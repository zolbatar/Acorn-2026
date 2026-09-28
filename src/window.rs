//! Single-window MOS console and graphics display.

use std::{
    collections::HashMap,
    error::Error,
    sync::{Arc, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowId},
};

use crate::{
    desktop_scene::{DesktopSceneBuilder, Viewport},
    graphics::{GraphicsService, GraphicsSnapshot},
    renderer,
    runtime::Runtime,
    swi::DisplayEvent,
    vello_backend::VelloSurface,
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
                if proxy.send_event(WindowUserEvent::Display(event)).is_err() {
                    break;
                }
            }
        })?;

    let mut guest_threads = Vec::new();
    if let Some(programs) = programs {
        for (task_id, source, name) in programs {
            wimp.task_started(task_id, name)?;
            let task_wimp = wimp.clone();
            let task_display = display_sender.clone();
            let cleanup_wimp = task_wimp.clone();
            guest_threads.push(thread::Builder::new().name(name.into()).spawn(move || {
                let (_input_sender, input_receiver) = mpsc::channel();
                let mut runtime =
                    Runtime::desktop_task(task_id, input_receiver, task_display, task_wimp);
                if let Err(error) = runtime.run_application(&source) {
                    if !matches!(&error, crate::error::RuntimeError::EndOfInput) {
                        let _ = runtime.report_error(&error);
                        cleanup_wimp.post_notice(format!("{name}: {error}"));
                        eprintln!("{name} stopped: {error}");
                    }
                }
                cleanup_wimp.task_exited(task_id);
            })?);
        }
    } else {
        let exit_sender = display_sender.clone();
        let runtime_display_sender = display_sender.clone();
        let runtime_wimp = wimp.clone();
        thread::Builder::new()
            .name("acorn-basic-runtime".into())
            .spawn(move || {
                let mut runtime = Runtime::windowed_with_desktop(
                    input_receiver,
                    runtime_display_sender,
                    runtime_wimp,
                );
                if let Err(error) = runtime.run() {
                    let _ = runtime.report_error(&error);
                }
                let _ = exit_sender.send(DisplayEvent::RuntimeExited);
            })?;
    }

    let mut app = if desktop_demo {
        WindowApp::new_desktop(input_sender, wimp, display_sender)
    } else {
        WindowApp::new_windowed(input_sender, wimp, display_sender)
    };
    let event_loop_result = event_loop.run_app(&mut app);
    if let Some(wimp) = &app.wimp_service {
        wimp.stop();
    }
    for guest in guest_threads {
        let _ = guest.join();
    }
    for guest in app.guest_threads.drain(..) {
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
    gpu: Option<VelloSurface>,
    desktop_scene: DesktopSceneBuilder,
    frame_size: (u32, u32),
    next_teletext_flash: Option<Instant>,
    desktop: Option<std::sync::Arc<WimpServer>>,
    wimp_service: Option<std::sync::Arc<WimpServer>>,
    task_graphics: HashMap<u64, GraphicsService>,
    window_graphics: HashMap<(u64, u32), GraphicsService>,
    display_sender: mpsc::Sender<DisplayEvent>,
    guest_threads: Vec<JoinHandle<()>>,
    pointer: Option<(i32, i32)>,
    drag: Option<WindowDrag>,
}

impl WindowApp {
    fn new(input: mpsc::Sender<u8>, display_sender: mpsc::Sender<DisplayEvent>) -> Self {
        Self {
            graphics: GraphicsService::default(),
            input,
            modifiers: ModifiersState::empty(),
            window: None,
            gpu: None,
            desktop_scene: DesktopSceneBuilder::new(),
            frame_size: (renderer::SCREEN_WIDTH, renderer::SCREEN_HEIGHT),
            next_teletext_flash: None,
            desktop: None,
            wimp_service: None,
            task_graphics: HashMap::new(),
            window_graphics: HashMap::new(),
            display_sender,
            guest_threads: Vec::new(),
            pointer: None,
            drag: None,
        }
    }

    fn new_desktop(
        input: mpsc::Sender<u8>,
        wimp: std::sync::Arc<WimpServer>,
        display_sender: mpsc::Sender<DisplayEvent>,
    ) -> Self {
        let mut app = Self::new(input, display_sender);
        app.frame_size = (DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT);
        app.desktop = Some(wimp.clone());
        app.wimp_service = Some(wimp);
        app
    }

    fn new_windowed(
        input: mpsc::Sender<u8>,
        wimp: std::sync::Arc<WimpServer>,
        display_sender: mpsc::Sender<DisplayEvent>,
    ) -> Self {
        let mut app = Self::new(input, display_sender);
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
        if let Some(wimp) = &self.wimp_service
            && let Err(error) = wimp.start_system_task("$.System.Desktop", "Acorn Desktop")
        {
            wimp.post_notice(format!("Could not start Acorn Desktop: {error}"));
        }
        self.request_redraw();
    }

    fn start_pending_tasks(&mut self) {
        let Some(wimp) = self.wimp_service.clone() else {
            return;
        };
        for request in wimp.take_pending_launches() {
            let (input_sender, input_receiver) = mpsc::channel();
            wimp.set_task_input(request.task_id, input_sender);
            let task_wimp = wimp.clone();
            let display_sender = self.display_sender.clone();
            let title = request.title.clone();
            let guest_path = request.guest_path.clone();
            match thread::Builder::new()
                .name(format!("acorn-task-{}", request.task_id))
                .spawn(move || {
                    let mut runtime = Runtime::desktop_task(
                        request.task_id,
                        input_receiver,
                        display_sender,
                        task_wimp.clone(),
                    );
                    let result = match request.kind {
                        crate::wimp::DesktopTaskKind::File => runtime.run_guest_file(&guest_path),
                        crate::wimp::DesktopTaskKind::Commands => {
                            runtime.run_desktop_console(false)
                        }
                        crate::wimp::DesktopTaskKind::BasicWindow => {
                            runtime.run_desktop_console(true)
                        }
                    };
                    if let Err(error) = result {
                        task_wimp.post_notice(format!("{title}: {error}"));
                    }
                    task_wimp.task_exited(request.task_id);
                }) {
                Ok(thread) => self.guest_threads.push(thread),
                Err(error) => {
                    wimp.post_notice(format!("Could not start {}: {error}", request.title));
                    wimp.task_exited(request.task_id);
                }
            }
        }
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
            DisplayEvent::WriteByte {
                task_id,
                window_handle,
                byte,
            } => {
                if self.desktop.is_some() && !self.is_active_desktop_task(task_id) {
                    return;
                }
                let graphics = if self.desktop.is_some() {
                    match window_handle {
                        Some(handle) => self.window_graphics.entry((task_id, handle)).or_default(),
                        None => self.task_graphics.entry(task_id).or_default(),
                    }
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
                window_handle,
                code,
                x,
                y,
            } => {
                if self.desktop.is_some() && !self.is_active_desktop_task(task_id) {
                    return;
                }
                let graphics = if self.desktop.is_some() {
                    match window_handle {
                        Some(handle) => self.window_graphics.entry((task_id, handle)).or_default(),
                        None => self.task_graphics.entry(task_id).or_default(),
                    }
                } else {
                    &mut self.graphics
                };
                let result = if graphics.snapshot().raster_surface.is_some() {
                    // The task updates the shared authoritative CPU raster
                    // before publishing this event. Replaying the plot here
                    // would apply XOR/logical actions a second time.
                    graphics.note_external_plot(code & 3 != 0);
                    Ok(())
                } else {
                    graphics.plot(code, x, y)
                };
                if let Err(error) = result {
                    eprintln!("Acorn-2026 graphics state error: {error}");
                }
            }
            DisplayEvent::GraphicsSnapshot {
                task_id,
                window_handle,
                snapshot,
            } => {
                if self.desktop.is_some() && !self.is_active_desktop_task(task_id) {
                    return;
                }
                let size = (snapshot.mode.pixel_width, snapshot.mode.pixel_height);
                if self.desktop.is_some() {
                    match window_handle {
                        Some(handle) => self
                            .window_graphics
                            .entry((task_id, handle))
                            .or_default()
                            .replace_snapshot(snapshot),
                        None => self
                            .task_graphics
                            .entry(task_id)
                            .or_default()
                            .replace_snapshot(snapshot),
                    }
                } else {
                    self.graphics.replace_snapshot(snapshot);
                    self.resize_buffer(size);
                }
            }
            DisplayEvent::DesktopStarted => self.activate_desktop(),
            DisplayEvent::DesktopChanged => {}
            DisplayEvent::RuntimeExited => {
                if self.should_exit_on_runtime_exit() {
                    event_loop.exit();
                    return;
                }
            }
        }
        self.request_redraw();
    }

    fn is_active_desktop_task(&self, task_id: u64) -> bool {
        self.wimp_service
            .as_ref()
            .is_some_and(|wimp| wimp.is_guest_task_active(task_id))
    }

    fn prune_finished_task_graphics(&mut self) {
        let active = self
            .wimp_service
            .as_ref()
            .map(|wimp| wimp.active_guest_task_ids())
            .unwrap_or_default();
        self.task_graphics
            .retain(|task_id, _| active.contains(task_id));
        self.window_graphics
            .retain(|(task_id, _), _| active.contains(task_id));
        let mut active_surfaces = active
            .iter()
            .map(|task_id| (*task_id, None))
            .collect::<Vec<_>>();
        if let Some(wimp) = &self.wimp_service {
            active_surfaces.extend(
                wimp.desktop_windows()
                    .into_iter()
                    .map(|window| (window.owner_task_id, Some(window.handle))),
            );
        }
        self.desktop_scene.retain_active_surfaces(&active_surfaces);
    }

    fn should_exit_on_runtime_exit(&self) -> bool {
        self.desktop.is_none()
    }

    fn resize_buffer(&mut self, size: (u32, u32)) {
        if self.desktop.is_some() {
            return;
        }
        if self.frame_size == size || size.0 == 0 || size.1 == 0 {
            return;
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

    fn update_pointer(&mut self, physical_x: f64, physical_y: f64) {
        let Some(wimp) = &self.desktop else {
            return;
        };
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        let viewport = Viewport::new(size.width, size.height);
        let point = match viewport.desktop_point(physical_x, physical_y) {
            Some(point) => point,
            None if self.drag.is_some() => viewport.clamped_desktop_point(physical_x, physical_y),
            None => {
                self.pointer = None;
                if wimp.mouse_move(-1, -1) {
                    self.request_redraw();
                }
                return;
            }
        };
        let (desktop_x, desktop_y) = point;
        let desktop_x = desktop_x * DESKTOP_OS_UNITS_PER_PIXEL_X + DESKTOP_OS_UNITS_PER_PIXEL_X / 2;
        let desktop_y = desktop_y * DESKTOP_OS_UNITS_PER_PIXEL_Y + DESKTOP_OS_UNITS_PER_PIXEL_Y / 2;
        self.pointer = Some((desktop_x, desktop_y));
        if wimp.mouse_move(desktop_x, desktop_y) {
            self.request_redraw();
        }
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
        } else {
            wimp.mouse_button_up(buttons);
            if let Some(drag) = self.drag.take() {
                wimp.finish_drag(drag);
            }
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

        let gpu = match VelloSurface::new(window.clone()) {
            Ok(gpu) => gpu,
            Err(error) => {
                eprintln!("Acorn-2026 could not initialize Vello/wgpu: {error}");
                event_loop.exit();
                return;
            }
        };

        self.gpu = Some(gpu);
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
                    if let Some(gpu) = &mut self.gpu {
                        gpu.resize(size.width, size.height);
                    }
                    self.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                self.handle_key(event);
                self.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Winit's CursorMoved position is already in physical pixels.
                self.update_pointer(position.x, position.y)
            }
            WindowEvent::CursorLeft { .. } => {
                if self.drag.is_none() {
                    self.pointer = None;
                    if let Some(wimp) = &self.desktop {
                        if wimp.mouse_move(-1, -1) {
                            self.request_redraw();
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_button(button, state)
            }
            WindowEvent::RedrawRequested => {
                let Some(window) = &self.window else {
                    return;
                };
                let size = window.inner_size();
                let scene = if let Some(wimp) = &self.desktop {
                    let windows = wimp.desktop_windows();
                    let mut scenes = self
                        .task_graphics
                        .iter()
                        .map(|(task_id, graphics)| ((*task_id, None), graphics.snapshot().clone()))
                        .collect::<HashMap<(u64, Option<u32>), GraphicsSnapshot>>();
                    for visible in &windows {
                        if let Some(graphics) = self
                            .window_graphics
                            .get(&(visible.owner_task_id, visible.handle))
                        {
                            scenes.insert(
                                (visible.owner_task_id, Some(visible.handle)),
                                graphics.snapshot().clone(),
                            );
                        }
                    }
                    self.desktop_scene
                        .retain_active_surfaces(&scenes.keys().copied().collect::<Vec<_>>());
                    self.desktop_scene.build(
                        &windows,
                        &scenes,
                        &wimp.desktop_icons(),
                        &wimp.desktop_window_icons(),
                        &wimp.desktop_menus(),
                        wimp.desktop_notice().as_deref(),
                        Viewport::new(size.width, size.height),
                    )
                } else {
                    self.desktop_scene.build_classic(
                        self.graphics.snapshot(),
                        size.width,
                        size.height,
                    )
                };
                if let Some(gpu) = &mut self.gpu {
                    if let Err(error) = gpu.render(&scene) {
                        eprintln!("Acorn-2026 could not present its Vello scene: {error}");
                        event_loop.exit();
                    }
                }
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: WindowUserEvent) {
        match event {
            WindowUserEvent::Display(event) => self.apply_display_event(event, event_loop),
            WindowUserEvent::DesktopChanged => {
                self.start_pending_tasks();
                self.prune_finished_task_graphics();
                self.request_redraw();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(wimp) = &self.desktop {
            if wimp.advance_menu_hover_at(Instant::now()) {
                self.request_redraw();
            }
            event_loop.set_control_flow(match wimp.next_menu_hover_deadline() {
                Some(deadline) => ControlFlow::WaitUntil(deadline),
                None => ControlFlow::Wait,
            });
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
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut app = WindowApp::new_windowed(input_sender, wimp.clone(), display_sender);

        assert!(app.desktop.is_none());
        assert!(app.should_exit_on_runtime_exit());
        app.activate_desktop();

        assert_eq!(app.frame_size, (DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT));
        assert!(!app.should_exit_on_runtime_exit());
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

    #[test]
    fn completed_desktop_tasks_release_their_cached_display_snapshot() {
        let (updates, _update_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        wimp.task_started(77, "Snapshot task").unwrap();
        let (input_sender, _input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut app = WindowApp::new_desktop(input_sender, wimp.clone(), display_sender);
        app.task_graphics.insert(77, GraphicsService::default());
        app.window_graphics
            .insert((77, 9), GraphicsService::default());

        wimp.task_exited(77);
        app.prune_finished_task_graphics();

        assert!(!app.task_graphics.contains_key(&77));
        assert!(!app.window_graphics.contains_key(&(77, 9)));
    }
}
