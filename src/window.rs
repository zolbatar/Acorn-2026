//! Single-window MOS console and graphics display.

use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use pixels::{Pixels, SurfaceTexture};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowId},
};

use crate::{
    graphics::{GraphicsService, GraphicsSnapshot},
    renderer,
    runtime::Runtime,
    swi::DisplayEvent,
    wimp::{
        DESKTOP_OS_UNITS_PER_PIXEL_X, DESKTOP_OS_UNITS_PER_PIXEL_Y, DESKTOP_PIXEL_HEIGHT,
        DESKTOP_PIXEL_WIDTH, WimpServer, WindowDrag,
    },
};

const INITIAL_SCALE: f64 = 1.5;

pub fn run() -> Result<(), Box<dyn Error>> {
    run_frontend(false)
}

/// Launch the editable BASIC two-task Wimp demonstration in one host window.
pub fn run_desktop_demo() -> Result<(), Box<dyn Error>> {
    run_frontend(true)
}

fn run_frontend(desktop_demo: bool) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::<WindowUserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let (input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    let proxy = event_loop.create_proxy();
    let programs = if desktop_demo {
        Some([
            (
                101,
                std::fs::read_to_string(format!(
                    "{}/examples/wimp/two-windows/alpha.bas64",
                    env!("CARGO_MANIFEST_DIR")
                ))?,
                "acorn-basic-alpha",
            ),
            (
                102,
                std::fs::read_to_string(format!(
                    "{}/examples/wimp/two-windows/beta.bas64",
                    env!("CARGO_MANIFEST_DIR")
                ))?,
                "acorn-basic-beta",
            ),
        ])
    } else {
        None
    };
    let (updates, update_receiver) = mpsc::channel();
    let update_proxy = proxy.clone();
    thread::Builder::new()
        .name("acorn-wimp-desktop-updates".into())
        .spawn(move || {
            while update_receiver.recv().is_ok() {
                if update_proxy
                    .send_event(WindowUserEvent::DesktopChanged)
                    .is_err()
                {
                    break;
                }
            }
        })?;
    let wimp = WimpServer::new(updates);

    thread::Builder::new()
        .name("acorn-window-events".into())
        .spawn(move || {
            while let Ok(event) = display_receiver.recv() {
                let finished = matches!(&event, DisplayEvent::RuntimeExited);
                if proxy.send_event(WindowUserEvent::Display(event)).is_err() || finished {
                    break;
                }
            }
        })?;

    let mut guest_threads = Vec::new();
    if let Some(programs) = programs {
        for (task_id, source, name) in programs {
            let task_wimp = wimp.clone();
            let task_display = display_sender.clone();
            guest_threads.push(thread::Builder::new().name(name.into()).spawn(move || {
                let (_input_sender, input_receiver) = mpsc::channel();
                let mut runtime =
                    Runtime::desktop_task(task_id, input_receiver, task_display, task_wimp);
                if let Err(error) = runtime.run_application(&source) {
                    if !matches!(&error, crate::error::RuntimeError::EndOfInput) {
                        let _ = runtime.report_error(&error);
                        eprintln!("{name} stopped: {error}");
                    }
                }
            })?);
        }
    } else {
        let exit_sender = display_sender.clone();
        let runtime_wimp = wimp.clone();
        thread::Builder::new()
            .name("acorn-basic-runtime".into())
            .spawn(move || {
                let mut runtime =
                    Runtime::windowed_with_desktop(input_receiver, display_sender, runtime_wimp);
                if let Err(error) = runtime.run() {
                    let _ = runtime.report_error(&error);
                }
                let _ = exit_sender.send(DisplayEvent::RuntimeExited);
            })?;
    }

    let mut app = if desktop_demo {
        WindowApp::new_desktop(input_sender, wimp)
    } else {
        WindowApp::new_windowed(input_sender, wimp)
    };
    let event_loop_result = event_loop.run_app(&mut app);
    if let Some(wimp) = &app.wimp_service {
        wimp.stop();
    }
    for guest in guest_threads {
        let _ = guest.join();
    }
    event_loop_result?;
    Ok(())
}

enum WindowUserEvent {
    Display(DisplayEvent),
    DesktopChanged,
}

struct WindowApp {
    graphics: GraphicsService,
    input: mpsc::Sender<u8>,
    modifiers: ModifiersState,
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,
    frame_size: (u32, u32),
    next_teletext_flash: Option<Instant>,
    desktop: Option<std::sync::Arc<WimpServer>>,
    wimp_service: Option<std::sync::Arc<WimpServer>>,
    task_graphics: HashMap<u64, GraphicsService>,
    pointer: Option<(i32, i32)>,
    drag: Option<WindowDrag>,
}

impl WindowApp {
    fn new(input: mpsc::Sender<u8>) -> Self {
        Self {
            graphics: GraphicsService::default(),
            input,
            modifiers: ModifiersState::empty(),
            window: None,
            pixels: None,
            frame_size: (renderer::SCREEN_WIDTH, renderer::SCREEN_HEIGHT),
            next_teletext_flash: None,
            desktop: None,
            wimp_service: None,
            task_graphics: HashMap::new(),
            pointer: None,
            drag: None,
        }
    }

    fn new_desktop(input: mpsc::Sender<u8>, wimp: std::sync::Arc<WimpServer>) -> Self {
        let mut app = Self::new(input);
        app.frame_size = (DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT);
        app.desktop = Some(wimp.clone());
        app.wimp_service = Some(wimp);
        app
    }

    fn new_windowed(input: mpsc::Sender<u8>, wimp: std::sync::Arc<WimpServer>) -> Self {
        let mut app = Self::new(input);
        app.wimp_service = Some(wimp);
        app
    }

    fn activate_desktop(&mut self) {
        if self.desktop.is_some() {
            return;
        }
        let Some(wimp) = self.wimp_service.clone() else {
            eprintln!("Acorn-2026 received DESKTOP without a hosted Wimp service");
            return;
        };
        // Resize while this is still the MOS display; resize_buffer intentionally
        // ignores requests after desktop composition becomes active.
        self.resize_buffer((DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT));
        self.task_graphics.clear();
        self.pointer = None;
        self.drag = None;
        self.desktop = Some(wimp);
        self.request_redraw();
    }

    fn handle_key(&self, event: KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }

        if self.is_paste_shortcut(&event.logical_key) {
            self.paste_clipboard();
            return;
        }

        match event.logical_key {
            Key::Named(NamedKey::Enter) => self.send_input(b'\r'),
            Key::Named(NamedKey::Backspace) => self.send_input(0x08),
            Key::Named(NamedKey::Escape) => self.send_input(0x1B),
            _ => {
                let Some(text) = event.text else {
                    return;
                };
                if text.is_ascii() {
                    for byte in text.bytes().filter(|byte| (b' '..=b'~').contains(byte)) {
                        self.send_input(byte);
                    }
                }
            }
        }
    }

    fn is_paste_shortcut(&self, key: &Key) -> bool {
        let Key::Character(character) = key else {
            return false;
        };
        if !character.eq_ignore_ascii_case("v") {
            return false;
        }

        if cfg!(target_os = "macos") {
            self.modifiers.super_key()
        } else {
            self.modifiers.control_key()
        }
    }

    fn paste_clipboard(&self) {
        let result = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text());
        match result {
            Ok(text) => self.send_pasted_text(&text),
            Err(error) => eprintln!("Acorn-2026 clipboard paste failed: {error}"),
        }
    }

    fn send_pasted_text(&self, text: &str) {
        let mut characters = text.chars().peekable();
        while let Some(character) = characters.next() {
            match character {
                '\r' => {
                    if characters.peek() == Some(&'\n') {
                        characters.next();
                    }
                    self.send_input(b'\r');
                }
                '\n' | '\u{2028}' | '\u{2029}' => self.send_input(b'\r'),
                '\t' => self.send_input(b' '),
                ' '..='~' => self.send_input(character as u8),
                _ => {}
            }
        }
    }

    fn send_input(&self, byte: u8) {
        if let Some(wimp) = &self.desktop {
            wimp.key_pressed(u32::from(byte));
        } else {
            let _ = self.input.send(byte);
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn apply_display_event(&mut self, event: DisplayEvent, event_loop: &ActiveEventLoop) {
        match event {
            DisplayEvent::WriteByte { task_id, byte } => {
                let graphics = if self.desktop.is_some() {
                    self.task_graphics.entry(task_id).or_default()
                } else {
                    &mut self.graphics
                };
                if let Err(error) = graphics.write_byte(byte) {
                    eprintln!("Acorn-2026 display state error: {error}");
                }
                if self.desktop.is_none() {
                    let mode = self.graphics.snapshot().mode;
                    self.resize_buffer((mode.pixel_width, mode.pixel_height));
                }
            }
            DisplayEvent::Plot {
                task_id,
                code,
                x,
                y,
            } => {
                let graphics = if self.desktop.is_some() {
                    self.task_graphics.entry(task_id).or_default()
                } else {
                    &mut self.graphics
                };
                if let Err(error) = graphics.plot(code, x, y) {
                    eprintln!("Acorn-2026 graphics state error: {error}");
                }
            }
            DisplayEvent::GraphicsSnapshot { task_id, snapshot } => {
                let size = (snapshot.mode.pixel_width, snapshot.mode.pixel_height);
                if self.desktop.is_some() {
                    self.task_graphics
                        .entry(task_id)
                        .or_default()
                        .replace_snapshot(snapshot);
                } else {
                    self.graphics.replace_snapshot(snapshot);
                    self.resize_buffer(size);
                }
            }
            DisplayEvent::DesktopStarted => self.activate_desktop(),
            DisplayEvent::DesktopChanged => {}
            DisplayEvent::RuntimeExited => {
                event_loop.exit();
                return;
            }
        }
        self.request_redraw();
    }

    fn resize_buffer(&mut self, size: (u32, u32)) {
        if self.desktop.is_some() {
            return;
        }
        if self.frame_size == size || size.0 == 0 || size.1 == 0 {
            return;
        }
        if let Some(pixels) = &mut self.pixels {
            if let Err(error) = pixels.resize_buffer(size.0, size.1) {
                eprintln!("Acorn-2026 could not resize its pixel buffer: {error}");
                return;
            }
        }
        self.frame_size = size;
        if let Some(window) = &self.window {
            let scale = (INITIAL_SCALE)
                .min(960.0 / f64::from(size.0))
                .min(720.0 / f64::from(size.1));
            let _ = window.request_inner_size(LogicalSize::new(
                f64::from(size.0) * scale,
                f64::from(size.1) * scale,
            ));
        }
    }

    fn update_pointer(&mut self, x: f64, y: f64) {
        let Some(wimp) = &self.desktop else {
            return;
        };
        let Some(pixels) = &self.pixels else {
            return;
        };
        let position = (x as f32, y as f32);
        let (pixel_x, pixel_y) = match pixels.window_pos_to_pixel(position) {
            Ok(position) => position,
            Err(outside) if self.drag.is_some() => pixels.clamp_pixel_pos(outside),
            Err(_) => {
                self.pointer = None;
                return;
            }
        };
        let desktop_x =
            pixel_x as i32 * DESKTOP_OS_UNITS_PER_PIXEL_X + DESKTOP_OS_UNITS_PER_PIXEL_X / 2;
        let desktop_y = (DESKTOP_PIXEL_HEIGHT as i32 - 1 - pixel_y as i32)
            * DESKTOP_OS_UNITS_PER_PIXEL_Y
            + DESKTOP_OS_UNITS_PER_PIXEL_Y / 2;
        self.pointer = Some((desktop_x, desktop_y));
        if let Some(drag) = self.drag {
            wimp.drag_to(drag, desktop_x, desktop_y);
            self.request_redraw();
        }
    }

    fn handle_mouse_button(&mut self, button: MouseButton, state: ElementState) {
        let Some(wimp) = &self.desktop else {
            return;
        };
        let buttons = mouse_button_mask(button, self.modifiers, true);
        if buttons == 0 {
            return;
        }
        if state == ElementState::Pressed {
            self.drag = self
                .pointer
                .and_then(|(x, y)| wimp.mouse_down(x, y, buttons));
        } else if let Some(drag) = self.drag.take() {
            wimp.finish_drag(drag);
        }
        self.request_redraw();
    }
}

fn mouse_button_mask(button: MouseButton, modifiers: ModifiersState, desktop_active: bool) -> u32 {
    match button {
        MouseButton::Left if desktop_active && modifiers.contains(ModifiersState::ALT) => 2,
        MouseButton::Left => 4,
        MouseButton::Middle => 2,
        MouseButton::Right => 1,
        _ => 0,
    }
}

impl ApplicationHandler<WindowUserEvent> for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let frame_size = self.frame_size;
        let scale = INITIAL_SCALE
            .min(960.0 / f64::from(frame_size.0))
            .min(720.0 / f64::from(frame_size.1));
        let attributes = Window::default_attributes()
            .with_title("Acorn-2026")
            .with_inner_size(LogicalSize::new(
                f64::from(frame_size.0) * scale,
                f64::from(frame_size.1) * scale,
            ))
            .with_min_inner_size(LogicalSize::new(
                f64::from(renderer::SCREEN_WIDTH),
                f64::from(renderer::SCREEN_HEIGHT),
            ));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("Acorn-2026 could not create its window: {error}");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        let surface = SurfaceTexture::new(size.width, size.height, window.clone());
        let pixels = match Pixels::new(frame_size.0, frame_size.1, surface) {
            Ok(pixels) => pixels,
            Err(error) => {
                eprintln!("Acorn-2026 could not create its pixel surface: {error}");
                event_loop.exit();
                return;
            }
        };

        self.pixels = Some(pixels);
        self.window = Some(window.clone());
        window.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if !self
            .window
            .as_ref()
            .is_some_and(|window| window.id() == window_id)
        {
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                if let Some(wimp) = &self.wimp_service {
                    wimp.stop();
                }
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    if let Some(pixels) = &mut self.pixels {
                        if let Err(error) = pixels.resize_surface(size.width, size.height) {
                            eprintln!("Acorn-2026 could not resize its pixel surface: {error}");
                            event_loop.exit();
                        }
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => self.handle_key(event),
            WindowEvent::CursorMoved { position, .. } => {
                self.update_pointer(position.x, position.y)
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_button(button, state)
            }
            WindowEvent::RedrawRequested => {
                let Some(pixels) = &mut self.pixels else {
                    return;
                };
                if let Some(wimp) = &self.desktop {
                    let scenes = self
                        .task_graphics
                        .iter()
                        .map(|(task_id, graphics)| (*task_id, graphics.snapshot().clone()))
                        .collect::<HashMap<u64, GraphicsSnapshot>>();
                    renderer::render_desktop(&wimp.desktop_windows(), &scenes, pixels.frame_mut());
                } else {
                    renderer::render(self.graphics.snapshot(), pixels.frame_mut());
                }
                if let Err(error) = pixels.render() {
                    eprintln!("Acorn-2026 could not render its pixel surface: {error}");
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WindowUserEvent) {
        match event {
            WindowUserEvent::Display(event) => self.apply_display_event(event, event_loop),
            WindowUserEvent::DesktopChanged => self.request_redraw(),
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.desktop.is_some() {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        let snapshot = self.graphics.snapshot();
        let is_teletext = matches!(snapshot.mode.number, 7 | 135);
        let has_flash = is_teletext && snapshot.text_cells.contains(&0x88);
        if !has_flash {
            self.next_teletext_flash = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }

        let now = Instant::now();
        let deadline = self
            .next_teletext_flash
            .unwrap_or_else(|| now + Duration::from_millis(500));
        if now >= deadline {
            self.request_redraw();
            self.next_teletext_flash = Some(now + Duration::from_millis(500));
        } else {
            self.next_teletext_flash = Some(deadline);
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            self.next_teletext_flash.expect("flash deadline is set"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{memory::Task, swi::SwiContext, wimp::WIMP_INITIALISE};

    #[test]
    fn desktop_mouse_mapping_supports_option_menu_and_preserves_button_roles() {
        let alt = ModifiersState::ALT;
        let no_modifiers = ModifiersState::empty();

        assert_eq!(mouse_button_mask(MouseButton::Left, no_modifiers, true), 4);
        assert_eq!(mouse_button_mask(MouseButton::Left, alt, true), 2);
        assert_eq!(mouse_button_mask(MouseButton::Left, alt, false), 4);
        assert_eq!(
            mouse_button_mask(MouseButton::Middle, no_modifiers, true),
            2
        );
        assert_eq!(mouse_button_mask(MouseButton::Right, no_modifiers, true), 1);
        assert_eq!(
            mouse_button_mask(MouseButton::Other(8), no_modifiers, true),
            0
        );
    }

    #[test]
    fn desktop_activation_switches_the_mos_frame_to_an_empty_shared_wimp() {
        let (updates, _update_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        let (input_sender, input_receiver) = mpsc::channel();
        let mut app = WindowApp::new_windowed(input_sender, wimp.clone());

        assert!(app.desktop.is_none());
        app.activate_desktop();

        assert_eq!(app.frame_size, (DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT));
        assert!(
            app.desktop
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(active, &wimp)),
            "the visible desktop must use the shared Wimp service"
        );
        assert!(wimp.desktop_windows().is_empty());

        let mut task = Task::new(42);
        task.memory.write_bytes(0x1000, b"desktop task\r").unwrap();
        let mut initialise = SwiContext::default();
        initialise.registers[0] = 310;
        initialise.registers[1] = u32::from_le_bytes(*b"TASK");
        initialise.registers[2] = 0x1000;
        wimp.dispatch(WIMP_INITIALISE, &mut task, &mut initialise)
            .unwrap();
        assert_eq!(initialise.registers[0], 310);
        assert!(wimp.desktop_windows().is_empty());

        app.send_input(b'X');
        assert!(input_receiver.try_recv().is_err());
    }
}
