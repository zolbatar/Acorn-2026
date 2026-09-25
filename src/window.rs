//! Single-window MOS console and graphics display.

use std::{
    error::Error,
    sync::{Arc, mpsc},
    thread,
};

use pixels::{Pixels, SurfaceTexture};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, KeyEvent, WindowEvent},
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{Window, WindowId},
};

use crate::{graphics::GraphicsService, renderer, runtime::Runtime, swi::DisplayEvent};

const INITIAL_SCALE: f64 = 1.5;

pub fn run() -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::<WindowUserEvent>::with_user_event().build()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let (input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    let proxy = event_loop.create_proxy();

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

    let exit_sender = display_sender.clone();
    thread::Builder::new()
        .name("acorn-basic-runtime".into())
        .spawn(move || {
            let mut runtime = Runtime::windowed(input_receiver, display_sender);
            if let Err(error) = runtime.run() {
                let _ = runtime.report_error(&error);
            }
            let _ = exit_sender.send(DisplayEvent::RuntimeExited);
        })?;

    let mut app = WindowApp::new(input_sender);
    event_loop.run_app(&mut app)?;
    Ok(())
}

enum WindowUserEvent {
    Display(DisplayEvent),
}

struct WindowApp {
    graphics: GraphicsService,
    input: mpsc::Sender<u8>,
    modifiers: ModifiersState,
    window: Option<Arc<Window>>,
    pixels: Option<Pixels<'static>>,
    frame_size: (u32, u32),
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
        let _ = self.input.send(byte);
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn apply_display_event(&mut self, event: DisplayEvent, event_loop: &ActiveEventLoop) {
        match event {
            DisplayEvent::WriteByte(byte) => {
                if let Err(error) = self.graphics.write_byte(byte) {
                    eprintln!("Acorn-2026 display state error: {error}");
                }
            }
            DisplayEvent::Plot { code, x, y } => {
                if let Err(error) = self.graphics.plot(code, x, y) {
                    eprintln!("Acorn-2026 graphics state error: {error}");
                }
            }
            DisplayEvent::GraphicsSnapshot(snapshot) => {
                let size = (snapshot.mode.pixel_width, snapshot.mode.pixel_height);
                self.graphics.replace_snapshot(snapshot);
                self.resize_buffer(size);
            }
            DisplayEvent::RuntimeExited => {
                event_loop.exit();
                return;
            }
        }
        self.request_redraw();
    }

    fn resize_buffer(&mut self, size: (u32, u32)) {
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
            WindowEvent::CloseRequested => event_loop.exit(),
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
            WindowEvent::RedrawRequested => {
                let Some(pixels) = &mut self.pixels else {
                    return;
                };
                renderer::render(self.graphics.snapshot(), pixels.frame_mut());
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
        }
    }
}
