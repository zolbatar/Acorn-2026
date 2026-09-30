use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use crate::{
    error::RuntimeError,
    graphics::GraphicsService,
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, Task},
    swi::{DisplayEvent, OS_CLI, OS_READ_LINE, OS_WRITE_C, SwiContext, SwiDispatcher},
    wimp::WimpServer,
};

const TASK_ID: u64 = 1;
const LINE_BUFFER: u32 = GUEST_MEMORY_BASE;
const LINE_BUFFER_SIZE: u32 = 256;

pub struct Runtime {
    task: Task,
    dispatcher: SwiDispatcher,
}

impl Runtime {
    /// Start the trusted interactive MOS session on the terminal host.
    /// The grant is task-scoped; applications spawned as independent tasks do
    /// not inherit it.
    pub fn stdio() -> Self {
        Self::new(HostConsole::stdio())
    }

    /// Construct the trusted interactive MOS shell for the windowed host.
    pub fn windowed(input: Receiver<u8>, display_events: Sender<DisplayEvent>) -> Self {
        let mut dispatcher = SwiDispatcher::windowed(HostConsole::windowed(input), display_events);
        dispatcher.initialize_mos_shell_console();
        Self {
            task: Task::trusted_mos_session(TASK_ID),
            dispatcher,
        }
    }

    pub fn windowed_with_desktop(
        input: Receiver<u8>,
        display_events: Sender<DisplayEvent>,
        wimp: Arc<WimpServer>,
    ) -> Self {
        let mut dispatcher = SwiDispatcher::windowed_with_desktop(
            HostConsole::windowed(input),
            display_events,
            wimp,
        );
        dispatcher.initialize_mos_shell_console();
        Self {
            task: Task::trusted_mos_session(TASK_ID),
            dispatcher,
        }
    }

    /// Construct an ordinary, unprivileged BASIC task attached to the shared
    /// hosted Wimp. It does not inherit the interactive MOS task's rights.
    pub fn desktop_task(
        task_id: u64,
        input: Receiver<u8>,
        display_events: Sender<DisplayEvent>,
        wimp: Arc<WimpServer>,
    ) -> Self {
        Self {
            task: Task::new(task_id),
            dispatcher: SwiDispatcher::desktop_task(
                HostConsole::windowed(input),
                display_events,
                task_id,
                wimp,
            ),
        }
    }

    /// Run one editable BASIC source program as this task.
    pub fn run_application(&mut self, source: &str) -> Result<(), RuntimeError> {
        let configuration = self.dispatcher.load_basic_configuration()?;
        crate::basic_compat::run_source_configured(
            source,
            &mut self.task,
            &mut self.dispatcher,
            &configuration,
        )
        .map(|_| ())
    }

    /// Load a guest BASIC program through the same FileSwitch path and saved
    /// execution preferences used by `BASIC` and `RUN`.
    pub fn run_guest_file(&mut self, path: &str) -> Result<(), RuntimeError> {
        self.dispatcher
            .set_program_working_directory(&mut self.task, path)?;
        let configuration = self.dispatcher.load_basic_configuration()?;
        self.dispatcher.begin_display_batch();
        let result = crate::basic64::run_guest_file_configured(
            path,
            &mut self.task,
            &mut self.dispatcher,
            &configuration,
        );
        self.dispatcher.finish_display_batch();
        result.map(|_| ())
    }

    /// Construct the host's trusted MOS session around a console.
    pub fn new(console: HostConsole) -> Self {
        let mut dispatcher = SwiDispatcher::new(console);
        dispatcher.initialize_mos_shell_console();
        Self {
            task: Task::trusted_mos_session(TASK_ID),
            dispatcher,
        }
    }

    pub fn graphics(&self) -> &GraphicsService {
        self.dispatcher.graphics()
    }

    pub fn run(&mut self) -> Result<(), RuntimeError> {
        if self.dispatcher.has_boot_failure() && !self.dispatcher.recover_boot()? {
            return Ok(());
        }
        if self.dispatcher.enter_boot_startup()? {
            return Ok(());
        }

        self.write_prompt()?;
        loop {
            match self.execute_console_line() {
                Err(RuntimeError::EndOfInput) => return Ok(()),
                Err(error) => return Err(error),
                Ok(()) => {}
            }

            if self.dispatcher.quit_requested() {
                return Ok(());
            }
            if self.dispatcher.desktop_requested() {
                return Ok(());
            }
            self.write_prompt()?;
        }
    }

    /// An isolated desktop prompt: do not re-run the saved startup Language.
    pub fn run_desktop_console(&mut self, basic: bool) -> Result<(), RuntimeError> {
        self.dispatcher.initialize_mos_shell_console();
        let mut lines = std::collections::BTreeMap::<u32, String>::new();
        loop {
            self.write_console_text(if basic { ">" } else { "*" })?;
            if !basic {
                match self.execute_console_line() {
                    Err(RuntimeError::EndOfInput) => return Ok(()),
                    Err(error) => self.report_error(&error)?,
                    Ok(()) => {}
                }
            } else {
                let mut input = SwiContext::default();
                input.registers[0] = LINE_BUFFER;
                input.registers[1] = LINE_BUFFER_SIZE - 1;
                input.registers[2] = 32;
                input.registers[3] = 126;
                match self
                    .dispatcher
                    .dispatch(OS_READ_LINE, &mut self.task, &mut input)
                {
                    Err(RuntimeError::EndOfInput) => return Ok(()),
                    Err(error) => return Err(error),
                    Ok(()) => {}
                }
                if input.carry {
                    continue;
                }
                let bytes = self
                    .task
                    .memory
                    .read_bytes(LINE_BUFFER, input.registers[1] as usize)?;
                let line = String::from_utf8_lossy(&bytes).trim().to_owned();
                if line.eq_ignore_ascii_case("QUIT") {
                    return Ok(());
                }
                if line.eq_ignore_ascii_case("NEW") {
                    lines.clear();
                    continue;
                }
                if line.eq_ignore_ascii_case("LIST") {
                    for source in lines.values() {
                        self.write_console_text(&format!("{source}\r\n"))?;
                    }
                    continue;
                }
                let digits = line.bytes().take_while(u8::is_ascii_digit).count();
                if digits > 0 {
                    let number = line[..digits]
                        .parse::<u32>()
                        .map_err(|_| RuntimeError::Program("line number is too large".into()))?;
                    if line[digits..].trim().is_empty() {
                        lines.remove(&number);
                    } else {
                        lines.insert(number, line);
                    }
                    continue;
                }
                if line.is_empty() {
                    continue;
                }
                let result = if let Some(command) = line.strip_prefix('*') {
                    self.task
                        .memory
                        .write_bytes(LINE_BUFFER, format!("{command}\0").as_bytes())?;
                    let mut context = SwiContext::default();
                    context.registers[0] = LINE_BUFFER;
                    self.dispatcher
                        .dispatch(OS_CLI, &mut self.task, &mut context)
                } else {
                    let source = if line.eq_ignore_ascii_case("RUN") {
                        lines.values().cloned().collect::<Vec<_>>().join("\n")
                    } else {
                        line
                    };
                    self.run_basic_console_source(&source)
                };
                if let Err(error) = result {
                    if matches!(error, RuntimeError::EndOfInput) {
                        return Ok(());
                    }
                    self.report_error(&error)?;
                }
            }
            if self.dispatcher.quit_requested() {
                return Ok(());
            }
        }
    }

    fn write_console_text(&mut self, text: &str) -> Result<(), RuntimeError> {
        for byte in text.bytes() {
            let mut context = SwiContext::default();
            context.registers[0] = u32::from(byte);
            self.dispatcher
                .dispatch(OS_WRITE_C, &mut self.task, &mut context)?;
        }
        self.dispatcher.flush()
    }

    fn run_basic_console_source(&mut self, source: &str) -> Result<(), RuntimeError> {
        let configuration = self.dispatcher.load_basic_configuration()?;
        crate::basic_compat::run_source_from_basic_console(
            source,
            &mut self.task,
            &mut self.dispatcher,
            &configuration,
        )
        .map(|_| ())
    }

    fn write_prompt(&mut self) -> Result<(), RuntimeError> {
        let mut prompt = SwiContext::default();
        prompt.registers[0] = u32::from(b'*');
        self.dispatcher
            .dispatch(OS_WRITE_C, &mut self.task, &mut prompt)?;
        self.dispatcher.flush()
    }

    fn execute_console_line(&mut self) -> Result<(), RuntimeError> {
        let mut input = SwiContext::default();
        input.registers[0] = LINE_BUFFER;
        input.registers[1] = LINE_BUFFER_SIZE - 1;
        input.registers[2] = u32::from(b' ');
        input.registers[3] = u32::from(b'~');
        self.dispatcher
            .dispatch(OS_READ_LINE, &mut self.task, &mut input)?;

        if input.carry || input.registers[1] == 0 {
            return Ok(());
        }

        let terminator = LINE_BUFFER
            .checked_add(input.registers[1])
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        self.task.memory.write_byte(terminator, 0)?;

        let mut command = SwiContext::default();
        command.registers[0] = LINE_BUFFER;
        match self
            .dispatcher
            .dispatch(OS_CLI, &mut self.task, &mut command)
        {
            Ok(()) => Ok(()),
            Err(error) => self.report_error(&error),
        }
    }

    pub fn report_error(&mut self, error: &RuntimeError) -> Result<(), RuntimeError> {
        // Errors shown by BASIC describe the fault, not the host application.
        let message = error.to_string();
        self.dispatcher
            .write_inline(&mut self.task, message.as_bytes())?;
        self.dispatcher.dispatch(
            crate::swi::OS_NEW_LINE,
            &mut self.task,
            &mut SwiContext::default(),
        )?;
        self.dispatcher.flush()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc::{self, TryRecvError},
        thread,
        time::Duration,
    };

    use super::*;
    use crate::{
        configure::{BasicEngine, ConfigureStore, StartupLanguage},
        filesystem::HostFileSystem,
        swi::{DisplayEvent, SwiDispatcher},
        wimp::{IconBarSide, WimpServer},
    };

    #[test]
    fn interactive_runtime_bootstrap_is_trusted_but_spawned_desktop_task_is_not() {
        let (_shell_input, shell_receiver) = mpsc::channel();
        let (shell_display, _shell_events) = mpsc::channel();
        let shell = Runtime::windowed(shell_receiver, shell_display);
        assert!(shell.task.require_source_read().is_ok());
        assert!(shell.task.require_module_management().is_ok());
        assert!(shell.task.require_configuration_write().is_ok());

        let (_task_input, task_receiver) = mpsc::channel();
        let (task_display, _task_events) = mpsc::channel();
        let (updates, _updates_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        let spawned = Runtime::desktop_task(77, task_receiver, task_display, wimp);
        assert!(matches!(
            spawned.task.require_source_read(),
            Err(RuntimeError::Structured { type_name, code: 1, .. })
                if type_name == "TaskAuthorizationDenied"
        ));
        assert!(matches!(
            spawned.task.require_module_management(),
            Err(RuntimeError::Structured { type_name, code: 2, .. })
                if type_name == "TaskAuthorizationDenied"
        ));
        assert!(matches!(
            spawned.task.require_configuration_write(),
            Err(RuntimeError::Structured { type_name, code: 4, .. })
                if type_name == "TaskAuthorizationDenied"
        ));

        // Rights are carried by the task object, not inferred from its ID.
        let same_id_untrusted = Task::new(shell.task.id);
        assert!(same_id_untrusted.require_source_read().is_err());
        assert!(same_id_untrusted.require_module_management().is_err());
        assert!(same_id_untrusted.require_configuration_write().is_err());

        let config_only = Task::trusted_configuration_manager(77);
        assert!(config_only.require_configuration_write().is_ok());
        assert!(config_only.require_source_read().is_err());
        assert!(config_only.require_module_management().is_err());
    }

    #[test]
    fn desktop_mandelbrot_publishes_extended_mode_and_coloured_raster() {
        // Exercise the actual desktop file-launch entry point with the original
        // listing, reducing only resolution/iteration count; a queued key ends its wait.
        let root = std::env::temp_dir().join(format!("ricochet-mandelbrot-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut bytes = include_bytes!("../demo-volume/mandelbrot.bbc").to_vec();
        for (from, to) in [(b"1680", b"0128"), (b"1050", b"0096"), (b"8192", b"0048")] {
            for offset in 0..bytes.len() - 3 {
                if &bytes[offset..offset + 4] == from {
                    bytes[offset..offset + 4].copy_from_slice(to);
                }
            }
        }
        std::fs::write(root.join("test"), bytes).unwrap();
        std::fs::write(
            root.join("test.ricochetmeta"),
            include_str!("../demo-volume/mandelbrot.bbc.ricochetmeta")
                .replace("guest-name=mandelbrot", "guest-name=test"),
        )
        .unwrap();
        let (input, rx) = mpsc::channel();
        input.send(b' ').unwrap();
        let (display, events) = mpsc::channel();
        let (updates, _updates) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        wimp.task_started(900, "mandelbrot").unwrap();
        let mut runtime = Runtime::desktop_task(900, rx, display, wimp.clone());
        runtime
            .dispatcher
            .set_file_system_for_test(HostFileSystem::new(&root));
        let configure = ConfigureStore::with_path(root.join("configure"));
        #[cfg(feature = "experimental-jit")]
        configure.set("BASICEngine", "HYBRID").unwrap();
        runtime.dispatcher.set_configure_store_for_test(configure);
        runtime.run_guest_file("$.test").unwrap();
        let snapshots: Vec<_> = events
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::GraphicsSnapshot { snapshot, .. } => Some(snapshot),
                DisplayEvent::Plot { .. } => panic!("desktop file launch must batch plots"),
                _ => None,
            })
            .collect();
        assert!(
            snapshots.len() >= 2,
            "MODE and final frame must both be published"
        );
        let snapshot = snapshots.last().unwrap();
        assert_eq!(
            (snapshot.mode.pixel_width, snapshot.mode.pixel_height),
            (128, 96)
        );
        let mut pixels = vec![0; 128 * 96 * 4];
        crate::renderer::render_for_vello(snapshot, &mut pixels);
        assert!(
            pixels
                .chunks_exact(4)
                .filter(|p| p[0] != p[1] || p[1] != p[2])
                .count()
                > 100,
            "Mandelbrot must draw a coloured image, not just a black background/cursor"
        );
        let window = &wimp.desktop_windows()[0];
        assert_eq!(window.work_area.max_x - window.work_area.min_x, 256);
        assert_eq!(window.work_area.max_y - window.work_area.min_y, 192);
        if let Ok(path) = std::env::var("RICOCHET_CONSOLE_SNAPSHOT")
            .or_else(|_| std::env::var("ACORN_CONSOLE_SNAPSHOT"))
        {
            let mut builder = crate::desktop_scene::DesktopSceneBuilder::new();
            let scene = builder.build(
                &wimp.desktop_windows(),
                &std::collections::HashMap::from([((900, None), snapshot.clone())]),
                &[],
                &[],
                &[],
                None,
                crate::desktop_scene::Viewport::new(1600, 1200),
            );
            let rgba = crate::vello_backend::snapshot_scene(&scene, 1600, 1200).unwrap();
            let mut encoder = png::Encoder::new(std::fs::File::create(path).unwrap(), 1600, 1200);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgba)
                .unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn desktop_prompts_accept_commands_and_basic_without_startup_handoff() {
        for (basic, input, expected) in [
            (false, "STATUS WimpMode\rQUIT\r", "WimpMode"),
            (true, "10 PRINT 12345\r20 END\rLIST\rRUN\rQUIT\r", "12345"),
        ] {
            let (tx, rx) = mpsc::channel();
            for byte in input.bytes() {
                tx.send(byte).unwrap();
            }
            drop(tx);
            let (display, events) = mpsc::channel();
            let mut runtime = Runtime::windowed(rx, display);
            runtime.run_desktop_console(basic).unwrap();
            let text: String = events
                .try_iter()
                .filter_map(|event| match event {
                    DisplayEvent::WriteByte { byte, .. } => Some(char::from(byte)),
                    _ => None,
                })
                .collect();
            assert!(text.contains(expected), "{text}");
            assert!(!text.contains("error"), "{text}");
        }
    }

    #[test]
    fn fullscreen_mos_shell_regrids_to_host_viewport_after_resize() {
        let (input_sender, input_receiver) = mpsc::channel();
        drop(input_sender);
        let (display_sender, _display_receiver) = mpsc::channel();
        let wimp = WimpServer::new(mpsc::channel().0);
        wimp.set_host_window_size(900, 400);
        let mut runtime =
            Runtime::windowed_with_desktop(input_receiver, display_sender, wimp.clone());

        let metrics = wimp.desktop_metrics();
        assert_eq!(
            runtime.dispatcher.graphics().snapshot().text_grid_size(),
            crate::graphics::modern_shell_grid_for_area(metrics.os_width(), metrics.os_height()),
        );

        wimp.set_host_window_size(700, 520);
        runtime
            .dispatcher
            .write_inline(&mut runtime.task, b"resized fullscreen")
            .unwrap();
        let metrics = wimp.desktop_metrics();
        assert_eq!(
            runtime.dispatcher.graphics().snapshot().text_grid_size(),
            crate::graphics::modern_shell_grid_for_area(metrics.os_width(), metrics.os_height()),
        );
        assert!(
            runtime
                .dispatcher
                .graphics()
                .snapshot()
                .modern_shell_console
        );
    }

    #[test]
    fn fullscreen_shell_grid_tracks_host_viewport_with_fixed_desktop_settings() {
        let (input_sender, input_receiver) = mpsc::channel();
        drop(input_sender);
        let (display_sender, _display_receiver) = mpsc::channel();
        let wimp = WimpServer::new(mpsc::channel().0);
        wimp.set_host_window_size(900, 400);
        let configure_root = std::env::temp_dir().join(format!(
            "ricochet-fixed-display-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&configure_root).unwrap();
        wimp.bind_configure_store(ConfigureStore::with_path(configure_root.join("configure")));

        let mut runtime =
            Runtime::windowed_with_desktop(input_receiver, display_sender, wimp.clone());
        wimp.apply_display_settings(crate::display::DisplaySettings {
            resolution: crate::display::DesktopResolution::R640x480,
            colour: crate::display::DisplayColour::Rgb888,
        })
        .unwrap();
        wimp.set_host_window_size(700, 520);
        runtime
            .dispatcher
            .write_inline(&mut runtime.task, b"resize")
            .unwrap();

        let metrics = wimp.desktop_metrics();
        assert_eq!(metrics.pixel_size(), (640, 480));
        assert_eq!(metrics.host_pixel_size(), (700, 520));
        assert_eq!(
            runtime.dispatcher.graphics().snapshot().text_grid_size(),
            crate::graphics::modern_shell_grid_for_area(1400, 1040),
        );
        assert_ne!(
            runtime.dispatcher.graphics().snapshot().text_grid_size(),
            crate::graphics::modern_shell_grid_for_area(metrics.os_width(), metrics.os_height()),
            "the fixed desktop extent must not size fullscreen host-console cells"
        );

        std::fs::remove_dir_all(configure_root).unwrap();
    }

    #[test]
    fn desktop_basic_shell_regrids_to_console_work_area_after_resize() {
        let (input_sender, input_receiver) = mpsc::channel();
        drop(input_sender);
        let (display_sender, _display_receiver) = mpsc::channel();
        let (updates, _updates_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        wimp.task_started(902, "BASIC window").unwrap();
        let mut runtime = Runtime::desktop_task(902, input_receiver, display_sender, wimp.clone());
        runtime.dispatcher.initialize_mos_shell_console();

        let initial_area = wimp.console_work_area(902).unwrap();
        assert_eq!(
            runtime.dispatcher.graphics().snapshot().text_grid_size(),
            crate::graphics::modern_shell_grid_for_area(
                initial_area.max_x - initial_area.min_x,
                initial_area.max_y - initial_area.min_y,
            ),
        );

        let window = wimp.desktop_windows()[0].clone();
        let grip = crate::wimp::desktop_window_furniture(&window)
            .adjust_size_icon
            .unwrap();
        let x = (grip.min_x + grip.max_x) / 2;
        let y = (grip.min_y + grip.max_y) / 2;
        let drag = wimp.mouse_down(x, y, 4).unwrap();
        wimp.drag_to(drag, x - 200, y + 100);
        wimp.finish_drag(drag);
        let resized_area = wimp.console_work_area(902).unwrap();
        assert_ne!(resized_area, initial_area);

        runtime
            .dispatcher
            .write_inline(&mut runtime.task, b"responsive shell")
            .unwrap();
        let snapshot = runtime.dispatcher.graphics().snapshot();
        assert_eq!(
            snapshot.text_grid_size(),
            crate::graphics::modern_shell_grid_for_area(
                resized_area.max_x - resized_area.min_x,
                resized_area.max_y - resized_area.min_y,
            ),
        );
        assert_eq!(
            snapshot.modern_text_cells.len(),
            usize::from(snapshot.text_grid_columns) * usize::from(snapshot.text_grid_rows),
        );
        assert!(snapshot.modern_shell_console);
    }

    #[test]
    fn immediate_basic_lines_keep_the_responsive_wimp_console_transcript() {
        let (updates, _updates_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        wimp.task_started(904, "BASIC window").unwrap();

        // Resize the host-owned console to a wide, short Wimp client area
        // before the BASIC prompt starts, so input, output and wrapping all
        // exercise the responsive host grid rather than MODE 20's guest grid.
        let window = wimp.desktop_windows()[0].clone();
        let grip = crate::wimp::desktop_window_furniture(&window)
            .adjust_size_icon
            .unwrap();
        let x = (grip.min_x + grip.max_x) / 2;
        let y = (grip.min_y + grip.max_y) / 2;
        let drag = wimp.mouse_down(x, y, 4).unwrap();
        wimp.drag_to(drag, x + 180, y + 360);
        wimp.finish_drag(drag);
        let area = wimp.console_work_area(904).unwrap();
        let (columns, rows) = crate::graphics::modern_shell_grid_for_area(
            area.max_x - area.min_x,
            area.max_y - area.min_y,
        );
        assert!(
            columns > 64,
            "expected a wide console, got {columns} columns"
        );
        assert!(rows < 25, "expected a short console, got {rows} rows");

        let long_text = "Z".repeat(usize::from(columns) + 8);
        let input = format!(
            "PRINT \"first\"\rPRINT \"second\"\rINPUT N$\rAda Lovelace\rPRINT N$\rPRINT \"{long_text}\"\rQUIT\r"
        );
        let (input_sender, input_receiver) = mpsc::channel();
        for byte in input.bytes() {
            input_sender.send(byte).unwrap();
        }
        drop(input_sender);
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut runtime = Runtime::desktop_task(904, input_receiver, display_sender, wimp);

        let config_root = std::env::temp_dir().join(format!(
            "ricochet-basic-shell-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&config_root).unwrap();
        let configure = ConfigureStore::with_path(config_root.join("configure"));
        configure.set("BASICEngine", "INTERPRETER").unwrap();
        runtime.dispatcher.set_configure_store_for_test(configure);

        runtime.run_desktop_console(true).unwrap();

        let snapshot = runtime.dispatcher.graphics().snapshot();
        assert!(snapshot.modern_shell_console);
        assert_eq!(snapshot.text_grid_size(), (columns, rows));
        assert_eq!(
            snapshot.modern_text_cells.len(),
            usize::from(columns) * usize::from(rows),
        );
        let transcript = snapshot.modern_text_cells.iter().collect::<String>();
        assert!(transcript.contains("first"), "{transcript:?}");
        assert!(transcript.contains("second"), "{transcript:?}");
        assert!(transcript.contains("Ada Lovelace"), "{transcript:?}");
        assert!(transcript.contains("Z"), "{transcript:?}");
        assert!(
            (0..usize::from(rows).saturating_sub(1)).any(|row| {
                let start = row * usize::from(columns);
                let end = start + usize::from(columns);
                snapshot.modern_text_cells[end - 1] == 'Z' && snapshot.modern_text_cells[end] == 'Z'
            }),
            "long PRINT output should wrap at the host-derived {columns}-cell width"
        );

        std::fs::remove_dir_all(config_root).unwrap();
    }

    #[test]
    fn desktop_task_prompts_initialize_a_modern_shell_for_mos_and_basic() {
        for basic in [false, true] {
            let (input_sender, input_receiver) = mpsc::channel();
            for byte in b"QUIT\r" {
                input_sender.send(*byte).unwrap();
            }
            drop(input_sender);
            let (display_sender, display_receiver) = mpsc::channel();
            let (updates, _updates_receiver) = mpsc::channel();
            let wimp = WimpServer::new(updates);
            let mut runtime = Runtime::desktop_task(901, input_receiver, display_sender, wimp);
            runtime.run_desktop_console(basic).unwrap();

            assert_eq!(
                runtime.dispatcher.graphics().snapshot().text_profile,
                crate::graphics::TextRenderingProfile::Modern
            );
            assert!(display_receiver.try_iter().any(|event| matches!(
                event,
                DisplayEvent::GraphicsSnapshot { snapshot, .. }
                    if snapshot.text_profile == crate::graphics::TextRenderingProfile::Modern
            )));
        }
    }

    #[test]
    fn desktop_command_preserves_configuration_for_later_basic_tasks() {
        let (input_sender, input_receiver) = mpsc::channel();
        for byte in b"*dEsK.\rHELP\r" {
            input_sender.send(*byte).unwrap();
        }
        drop(input_sender);

        let (display_sender, display_receiver) = mpsc::channel();
        let app_display_sender = display_sender.clone();
        let (updates, _update_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        let runtime_wimp = wimp.clone();
        let config_path = std::env::temp_dir().join(format!(
            "ricochet-desktop-engine-{}.configure",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&config_path);
        let configure = ConfigureStore::with_path(&config_path);
        configure.set("BASICENGINE", "STRICT").unwrap();
        configure.set("Language", "0").unwrap();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let runtime_thread = thread::spawn(move || {
            let mut runtime =
                Runtime::windowed_with_desktop(input_receiver, display_sender, runtime_wimp);
            runtime.dispatcher.set_configure_store_for_test(configure);
            let _ = finished_sender.send(runtime.run());
        });

        let mut initial_output = Vec::new();
        loop {
            match display_receiver.recv_timeout(Duration::from_secs(2)) {
                Ok(DisplayEvent::WriteByte { byte, .. }) => initial_output.push(byte),
                Ok(DisplayEvent::DesktopStarted) => break,
                Ok(_) => {}
                Err(error) => panic!("DESKTOP did not reach the display handoff: {error}"),
            }
        }
        assert_eq!(initial_output.first(), Some(&b'*'));
        assert!(wimp.desktop_windows().is_empty());
        assert_eq!(
            wimp.configure_store()
                .expect("DESKTOP should attach its preference store")
                .load()
                .unwrap()
                .engine,
            BasicEngine::StrictJit
        );
        assert!(
            display_receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "queued HELP input must not be consumed and a hidden MOS prompt must not be emitted"
        );
        assert!(matches!(
            finished_receiver.try_recv(),
            Err(TryRecvError::Empty)
        ));

        let (_app_input_sender, app_input_receiver) = mpsc::channel();
        let mut app =
            Runtime::desktop_task(2, app_input_receiver, app_display_sender, Arc::clone(&wimp));
        assert!(
            app.run_application("10 DIM A\n20 END").is_err(),
            "a desktop BASIC task should select the strict engine from session preferences"
        );
        drop(app);

        let (_reference_input_sender, reference_input_receiver) = mpsc::channel();
        let (reference_display_sender, _reference_display_receiver) = mpsc::channel();
        let mut reference_dispatcher = SwiDispatcher::windowed(
            HostConsole::windowed(reference_input_receiver),
            reference_display_sender,
        );
        let mut reference_task = Task::new(3);
        crate::basic_compat::run_source(
            "10 DIM A\n20 END",
            &mut reference_task,
            &mut reference_dispatcher,
        )
        .expect("the execution probe should run in the reference interpreter");

        wimp.stop();
        finished_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("closing the Wimp session must release the suspended MOS task")
            .expect("the MOS runtime should shut down cleanly");
        runtime_thread.join().unwrap();
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn boot_module_language_zero_enters_the_mos_command_prompt() {
        let (input_sender, input_receiver) = mpsc::channel();
        for byte in b"QUIT\r" {
            input_sender.send(*byte).unwrap();
        }
        drop(input_sender);
        let (display_sender, display_receiver) = mpsc::channel();
        let (updates, _update_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        let runtime_wimp = wimp.clone();
        let config_path = std::env::temp_dir().join(format!(
            "ricochet-language-mos-startup-{}.configure",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&config_path);
        let configure = ConfigureStore::with_path(&config_path);
        configure.set("Language", "0").unwrap();
        let (finished_sender, finished_receiver) = mpsc::channel();

        let runtime_thread = thread::spawn(move || {
            let mut runtime =
                Runtime::windowed_with_desktop(input_receiver, display_sender, runtime_wimp);
            runtime.dispatcher.set_configure_store_for_test(configure);
            let _ = finished_sender.send(runtime.run());
        });

        loop {
            match display_receiver.recv_timeout(Duration::from_secs(2)) {
                Ok(DisplayEvent::WriteByte { byte: b'*', .. }) => break,
                Ok(DisplayEvent::DesktopStarted) => {
                    panic!("configured Language 0 must not start the desktop")
                }
                Ok(_) => {}
                Err(error) => panic!("configured Language 0 did not show the MOS prompt: {error}"),
            }
        }
        finished_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("QUIT should leave the configured MOS command prompt")
            .expect("the MOS runtime should shut down cleanly");
        runtime_thread.join().unwrap();
        wimp.stop();
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn boot_module_language_three_starts_desktop_without_showing_a_mos_prompt() {
        let (input_sender, input_receiver) = mpsc::channel();
        drop(input_sender);
        let (display_sender, display_receiver) = mpsc::channel();
        let (updates, _update_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        let runtime_wimp = wimp.clone();
        let config_path = std::env::temp_dir().join(format!(
            "ricochet-language-startup-{}.configure",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&config_path);
        let configure = ConfigureStore::with_path(&config_path);
        configure.set("Language", "3").unwrap();

        let runtime_thread = thread::spawn(move || {
            let mut runtime =
                Runtime::windowed_with_desktop(input_receiver, display_sender, runtime_wimp);
            runtime.dispatcher.set_configure_store_for_test(configure);
            runtime.run()
        });

        let mut saw_prompt_output = false;
        loop {
            match display_receiver.recv_timeout(Duration::from_secs(2)) {
                Ok(DisplayEvent::WriteByte { .. }) => saw_prompt_output = true,
                Ok(DisplayEvent::DesktopStarted) => break,
                Ok(_) => {}
                Err(error) => panic!("configured Language 3 did not start the desktop: {error}"),
            }
        }
        assert!(
            !saw_prompt_output,
            "desktop startup must skip the MOS prompt"
        );
        wimp.stop();
        runtime_thread.join().unwrap().unwrap();
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn stdio_keeps_the_mos_recovery_path_when_desktop_startup_is_saved() {
        let config_path = std::env::temp_dir().join(format!(
            "ricochet-language-stdio-{}.configure",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&config_path);
        let configure = ConfigureStore::with_path(&config_path);
        configure.set("Language", "3").unwrap();

        let mut runtime = Runtime::stdio();
        runtime.dispatcher.set_configure_store_for_test(configure);
        assert_eq!(
            runtime
                .dispatcher
                .load_basic_configuration()
                .unwrap()
                .startup_language,
            StartupLanguage::Desktop
        );
        assert!(!runtime.dispatcher.desktop_is_configured_for_startup());
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn basic64_desktop_browses_scrolls_and_launches_mounted_programs() {
        let volume = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo-volume");
        let configure_path = std::env::temp_dir().join(format!(
            "ricochet-desktop-slice-{}.configure",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&configure_path);
        let configure = ConfigureStore::with_path(&configure_path);

        let (display_sender, display_receiver) = mpsc::channel();
        let (updates, _update_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        let desktop_wimp = wimp.clone();
        let desktop_volume = volume.clone();
        let desktop_configure = configure.clone();
        let (_desktop_input_sender, desktop_input_receiver) = mpsc::channel();
        let desktop_display = display_sender.clone();
        let (desktop_result_sender, desktop_result_receiver) = mpsc::channel();
        let desktop_worker = thread::spawn(move || {
            let mut runtime =
                Runtime::desktop_task(40, desktop_input_receiver, desktop_display, desktop_wimp);
            runtime
                .dispatcher
                .set_configure_store_for_test(desktop_configure);
            runtime
                .dispatcher
                .set_file_system_for_test(HostFileSystem::new(desktop_volume));
            let result = runtime.run_guest_file("$.System.Desktop");
            let summary = match &result {
                Ok(()) => Ok(()),
                Err(error) => Err(error.to_string()),
            };
            let _ = desktop_result_sender.send(summary);
            result
        });
        let mut filer_worker = None;
        let mut wimp_example_worker = None;
        let mut wimp_example_task_id = None;
        let mut echo_worker = None;
        let mut echo_task_id = None;

        let scenario = (|| -> Result<(), String> {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let volume_icon = loop {
                if let Some(icon) = wimp
                    .desktop_icons()
                    .into_iter()
                    .find(|icon| icon.side == IconBarSide::Devices)
                {
                    break icon;
                }
                if desktop_worker.is_finished() {
                    return Err(format!(
                        "Desktop BASIC task exited before creating the volume icon: {:?}",
                        desktop_result_receiver.try_recv().ok()
                    ));
                }
                if std::time::Instant::now() >= deadline {
                    return Err("Desktop BASIC task did not create the volume icon".into());
                }
                thread::sleep(Duration::from_millis(5));
            };
            let x = (volume_icon.bounds.min_x + volume_icon.bounds.max_x) / 2;
            let y = (volume_icon.bounds.min_y + volume_icon.bounds.max_y) / 2;
            wimp.mouse_down(x, y, 4);

            let request = loop {
                if let Some(request) = wimp.take_pending_launches().into_iter().next() {
                    break request;
                }
                if desktop_worker.is_finished() {
                    return Err("Desktop BASIC task exited after the volume click".into());
                }
                if std::time::Instant::now() >= deadline {
                    return Err("selecting the volume did not launch the Filer".into());
                }
                thread::sleep(Duration::from_millis(5));
            };
            if request.guest_path != "$.System.Filer" {
                return Err(format!("volume icon requested {}", request.guest_path));
            }

            let (filer_input_sender, filer_input_receiver) = mpsc::channel();
            let filer_wimp = wimp.clone();
            let filer_display = display_sender.clone();
            let filer_configure = configure.clone();
            let filer_volume = volume.clone();
            let filer_task_id = request.task_id;
            let filer_path = request.guest_path.clone();
            let (filer_result_sender, filer_result_receiver) = mpsc::channel();
            filer_worker = Some(thread::spawn(move || {
                let mut runtime = Runtime::desktop_task(
                    filer_task_id,
                    filer_input_receiver,
                    filer_display,
                    filer_wimp,
                );
                runtime
                    .dispatcher
                    .set_configure_store_for_test(filer_configure);
                runtime
                    .dispatcher
                    .set_file_system_for_test(HostFileSystem::new(filer_volume));
                let result = runtime.run_guest_file(&filer_path);
                let summary = match &result {
                    Ok(()) => Ok(()),
                    Err(error) => Err(error.to_string()),
                };
                let _ = filer_result_sender.send(summary);
                result
            }));
            drop(filer_input_sender);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let _filer_window = loop {
                if let Some(window) = wimp
                    .desktop_windows()
                    .into_iter()
                    .find(|window| window.owner_task_id == filer_task_id)
                {
                    break window;
                }
                if filer_worker
                    .as_ref()
                    .is_some_and(|worker| worker.is_finished())
                {
                    return Err(format!(
                        "Filer BASIC task exited before opening its window: {:?}",
                        filer_result_receiver.try_recv().ok()
                    ));
                }
                if std::time::Instant::now() >= deadline {
                    return Err("Filer BASIC task did not open its Wimp window".into());
                }
                thread::sleep(Duration::from_millis(5));
            };
            if wimp
                .desktop_icons()
                .iter()
                .any(|icon| icon.owner_task_id == filer_task_id)
            {
                return Err("opening the Filer created an implicit iconbar icon".into());
            }

            let mut output = Vec::new();
            let current_filer_window = || {
                wimp.desktop_windows()
                    .into_iter()
                    .find(|window| window.owner_task_id == filer_task_id)
            };
            let visible_catalogue_icon = |label: &str| {
                let window = current_filer_window()?;
                let visible = crate::wimp::desktop_window_furniture(&window).work_area;
                wimp.desktop_window_icons().into_iter().find(|icon| {
                    icon.owner_task_id == filer_task_id
                        && icon.label == label
                        && icon.bounds.min_x >= visible.min_x
                        && icon.bounds.max_x <= visible.max_x
                        && icon.bounds.min_y >= visible.min_y
                        && icon.bounds.max_y <= visible.max_y
                })
            };
            let click_icon = |icon: &crate::wimp::DesktopWindowIcon| {
                wimp.mouse_down(
                    (icon.bounds.min_x + icon.bounds.max_x) / 2,
                    (icon.bounds.min_y + icon.bounds.max_y) / 2,
                    4,
                );
            };
            let catalogue = HostFileSystem::new(&volume);
            let catalogue_task = Task::new(41);
            let entries = catalogue
                .enumerate(&catalogue_task.file_system, "$", "*")
                .map_err(|error| error.to_string())?;
            let examples_index = entries
                .iter()
                .position(|entry| entry.is_directory && entry.guest_name == "Examples")
                .ok_or_else(|| {
                    "the mounted root catalogue has no Examples directory".to_string()
                })?;
            if examples_index >= 56 {
                return Err("Examples is outside the Filer's first catalogue page".into());
            }
            let examples_deadline = std::time::Instant::now() + Duration::from_secs(5);
            let examples_icon = loop {
                if let Some(icon) = visible_catalogue_icon("Examples") {
                    break icon;
                }
                if std::time::Instant::now() >= examples_deadline {
                    let labels = wimp
                        .desktop_window_icons()
                        .into_iter()
                        .filter(|icon| icon.owner_task_id == filer_task_id)
                        .map(|icon| (icon.label, icon.bounds))
                        .collect::<Vec<_>>();
                    let windows = wimp
                        .desktop_windows()
                        .into_iter()
                        .filter(|window| window.owner_task_id == filer_task_id)
                        .map(|window| (window.work_area, window.scroll_y))
                        .collect::<Vec<_>>();
                    return Err(format!(
                        "Filer did not show the Examples directory icon: labels={labels:?}, windows={windows:?}, task={:?}",
                        filer_result_receiver.try_recv().ok()
                    ));
                }
                thread::sleep(Duration::from_millis(5));
            };
            click_icon(&examples_icon);
            click_icon(&examples_icon);

            let path_deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                // The Filer exposes catalogue entries in the work area; a
                // guest-path icon is not part of its current desktop UI. Wait
                // for an entry unique to Examples to prove navigation.
                if visible_catalogue_icon("Alpha").is_some() {
                    break;
                }
                if std::time::Instant::now() >= path_deadline {
                    let labels = wimp
                        .desktop_window_icons()
                        .into_iter()
                        .filter(|icon| icon.owner_task_id == filer_task_id)
                        .map(|icon| icon.label)
                        .collect::<Vec<_>>();
                    let windows = wimp
                        .desktop_windows()
                        .into_iter()
                        .filter(|window| window.owner_task_id == filer_task_id)
                        .map(|window| (window.work_area, window.scroll_y))
                        .collect::<Vec<_>>();
                    return Err(format!(
                        "double-clicking Examples did not show its Alpha child entry; Filer entries: {labels:?}; Filer window area/scroll: {windows:?}; task result: {:?}",
                        filer_result_receiver.try_recv().ok()
                    ));
                }
                thread::sleep(Duration::from_millis(5));
            }

            let close = wimp
                .furniture_layout()
                .into_iter()
                .find(|(window, _)| window.owner_task_id == filer_task_id)
                .and_then(|(_, furniture)| furniture.close_icon)
                .ok_or_else(|| "Filer window has no close control".to_string())?;
            wimp.mouse_down(
                close.min_x + (close.max_x - close.min_x) / 2,
                close.min_y + (close.max_y - close.min_y) / 2,
                1,
            );
            wimp.mouse_button_up(1);
            let parent_deadline = std::time::Instant::now() + Duration::from_secs(5);
            let _parent_examples = loop {
                if let Some(icon) = visible_catalogue_icon("Examples") {
                    break icon;
                }
                if filer_worker
                    .as_ref()
                    .is_some_and(|worker| worker.is_finished())
                {
                    return Err(format!(
                        "Adjust-close did not keep the Filer open at its parent: {:?}",
                        filer_result_receiver.try_recv().ok()
                    ));
                }
                if std::time::Instant::now() >= parent_deadline {
                    return Err("Adjust-close did not open the parent directory".into());
                }
                thread::sleep(Duration::from_millis(5));
            };
            let filer_window = current_filer_window().unwrap();
            let adjust_size = wimp
                .furniture_layout()
                .into_iter()
                .find(|(window, _)| window.owner_task_id == filer_task_id)
                .and_then(|(_, furniture)| furniture.adjust_size_icon)
                .ok_or_else(|| "Filer window has no resize control".to_string())?;
            let grab_x = adjust_size.min_x + (adjust_size.max_x - adjust_size.min_x) / 2;
            let grab_y = adjust_size.min_y + (adjust_size.max_y - adjust_size.min_y) / 2;
            let drag = wimp
                .mouse_down(grab_x, grab_y, 4)
                .ok_or_else(|| "Filer resize control did not start a drag".to_string())?;
            let narrow_width = 300;
            wimp.drag_to(
                drag,
                drag.start_x + narrow_width
                    - (filer_window.work_area.max_x - filer_window.work_area.min_x),
                drag.start_y,
            );
            wimp.finish_drag(drag);
            let resize_deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let Some(window) = current_filer_window() else {
                    return Err("Filer closed during its resize request".into());
                };
                if window.work_area.max_x - window.work_area.min_x == narrow_width {
                    let work = crate::wimp::desktop_window_furniture(&window).work_area;
                    let icons = wimp
                        .desktop_window_icons()
                        .into_iter()
                        .filter(|icon| icon.owner_task_id == filer_task_id)
                        .collect::<Vec<_>>();
                    if !icons.is_empty()
                        && icons.iter().all(|icon| {
                            icon.bounds.min_x >= work.min_x && icon.bounds.max_x <= work.max_x
                        })
                    {
                        break;
                    }
                }
                if std::time::Instant::now() >= resize_deadline {
                    return Err("Filer did not accept its narrow resize".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            let mut resize_scroll_steps = 0;
            while visible_catalogue_icon("Examples").is_none() {
                if resize_scroll_steps >= 12 {
                    return Err("reflowed root listing did not scroll Examples into view".into());
                }
                let (_, furniture) = wimp
                    .furniture_layout()
                    .into_iter()
                    .find(|(window, _)| window.owner_task_id == filer_task_id)
                    .ok_or_else(|| "Filer window disappeared after resize".to_string())?;
                let bar = furniture
                    .vertical_scrollbar
                    .ok_or_else(|| "Filer lost its vertical scrollbar after resize".to_string())?;
                if bar.slider.min_y <= bar.track.min_y {
                    return Err("reflowed root listing has no page-down area".into());
                }
                let scroll_x = (bar.track.min_x + bar.track.max_x) / 2;
                let scroll_y = (bar.track.min_y + bar.slider.min_y) / 2;
                let previous_scroll = current_filer_window().unwrap().scroll_y;
                wimp.mouse_down(scroll_x, scroll_y, 4);
                let scroll_deadline = std::time::Instant::now() + Duration::from_secs(2);
                loop {
                    let Some(window) = current_filer_window() else {
                        return Err("Filer window disappeared while scrolling after resize".into());
                    };
                    if window.scroll_y != previous_scroll {
                        break;
                    }
                    if std::time::Instant::now() >= scroll_deadline {
                        return Err("reflowed root listing did not scroll".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                resize_scroll_steps += 1;
            }
            let parent_examples = visible_catalogue_icon("Examples").unwrap();
            click_icon(&parent_examples);
            wimp.mouse_button_up(4);
            click_icon(&parent_examples);
            wimp.mouse_button_up(4);
            let reopened_deadline = std::time::Instant::now() + Duration::from_secs(5);
            while visible_catalogue_icon("Alpha").is_none() {
                if std::time::Instant::now() >= reopened_deadline {
                    return Err("Filer could not reopen Examples after Adjust-close".into());
                }
                thread::sleep(Duration::from_millis(5));
            }

            let example_entries = catalogue
                .enumerate(&catalogue_task.file_system, "$.Examples", "*")
                .map_err(|error| error.to_string())?;
            let echo_index = example_entries
                .iter()
                .position(|entry| !entry.is_directory && entry.guest_name == "Echo")
                .ok_or_else(|| "the Examples catalogue has no Echo program".to_string())?;
            let wimp_index = example_entries
                .iter()
                .position(|entry| !entry.is_directory && entry.guest_name == "WimpAlpha")
                .ok_or_else(|| "the Examples catalogue has no WimpAlpha program".to_string())?;

            let mut scroll_steps = 0;
            while visible_catalogue_icon("WimpAlpha").is_none() {
                if scroll_steps >= 30 {
                    return Err(format!(
                        "scrolling did not reveal WimpAlpha (catalogue index {wimp_index})"
                    ));
                }
                let (_, furniture) = wimp
                    .furniture_layout()
                    .into_iter()
                    .find(|(window, _)| window.owner_task_id == filer_task_id)
                    .ok_or_else(|| "Filer window disappeared before list scrolling".to_string())?;
                let bar = furniture
                    .vertical_scrollbar
                    .ok_or_else(|| "Filer has no vertical scrollbar".to_string())?;
                if bar.slider.min_y <= bar.track.min_y {
                    return Err("Filer scrollbar has no page-down area".into());
                }
                let scroll_x = (bar.track.min_x + bar.track.max_x) / 2;
                let scroll_y = (bar.track.min_y + bar.slider.min_y) / 2;
                let previous_scroll = current_filer_window().unwrap().scroll_y;
                wimp.mouse_down(scroll_x, scroll_y, 4);
                let scroll_deadline = std::time::Instant::now() + Duration::from_secs(2);
                loop {
                    let Some(window) = current_filer_window() else {
                        return Err(format!(
                            "Filer window disappeared while scrolling: {:?}",
                            filer_result_receiver.try_recv().ok()
                        ));
                    };
                    if window.scroll_y != previous_scroll {
                        break;
                    }
                    if std::time::Instant::now() >= scroll_deadline {
                        return Err("scrolling the Filer did not move its catalogue view".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                scroll_steps += 1;
            }
            if let Some(notice) = wimp.desktop_notice() {
                return Err(format!("Filer scrolling raised a desktop notice: {notice}"));
            }

            let wimp_icon = visible_catalogue_icon("WimpAlpha").unwrap();
            click_icon(&wimp_icon);
            let selected_deadline = std::time::Instant::now() + Duration::from_secs(2);
            while visible_catalogue_icon("WimpAlpha").is_none_or(|icon| icon.flags & (1 << 21) == 0)
            {
                if std::time::Instant::now() >= selected_deadline {
                    return Err("clicking WimpAlpha did not select its Filer icon".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            click_icon(&visible_catalogue_icon("WimpAlpha").unwrap());
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let wimp_request = loop {
                if let Some(request) = wimp.take_pending_launches().into_iter().next() {
                    break request;
                }
                if std::time::Instant::now() >= deadline {
                    return Err("double-clicking WimpAlpha did not start a BASIC task".into());
                }
                thread::sleep(Duration::from_millis(5));
            };
            if wimp_request.guest_path != "$.Examples.WimpAlpha" {
                return Err(format!(
                    "Filer launched {} instead of WimpAlpha",
                    wimp_request.guest_path
                ));
            }
            let wimp_example_id = wimp_request.task_id;
            wimp_example_task_id = Some(wimp_example_id);
            let (wimp_input_sender, wimp_input_receiver) = mpsc::channel();
            wimp.set_task_input(wimp_example_id, wimp_input_sender);
            let child_wimp = wimp.clone();
            let child_display = display_sender.clone();
            let child_configure = configure.clone();
            let child_volume = volume.clone();
            let child_path = wimp_request.guest_path.clone();
            wimp_example_worker = Some(thread::spawn(move || {
                let mut runtime = Runtime::desktop_task(
                    wimp_example_id,
                    wimp_input_receiver,
                    child_display,
                    child_wimp.clone(),
                );
                runtime
                    .dispatcher
                    .set_configure_store_for_test(child_configure);
                runtime
                    .dispatcher
                    .set_file_system_for_test(HostFileSystem::new(child_volume));
                let result = runtime.run_guest_file(&child_path);
                child_wimp.task_exited(wimp_example_id);
                result
            }));

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !wimp.desktop_windows().iter().any(|window| {
                window.owner_task_id == wimp_example_id && window.title == "Alpha App"
            }) {
                if wimp_example_worker
                    .as_ref()
                    .is_some_and(|worker| worker.is_finished())
                {
                    return Err("WimpAlpha exited before opening its shared Wimp window".into());
                }
                if std::time::Instant::now() >= deadline {
                    return Err("WimpAlpha did not open a window in the desktop".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            if wimp
                .desktop_icons()
                .iter()
                .any(|icon| icon.owner_task_id == wimp_example_id)
            {
                return Err("WimpAlpha received an iconbar icon without creating one".into());
            }
            let close = wimp
                .furniture_layout()
                .into_iter()
                .find(|(window, _)| window.owner_task_id == wimp_example_id)
                .and_then(|(_, furniture)| furniture.close_icon)
                .ok_or_else(|| "WimpAlpha window has no close control".to_string())?;
            wimp.mouse_down(
                (close.min_x + close.max_x) / 2,
                (close.min_y + close.max_y) / 2,
                4,
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while wimp
                .desktop_icons()
                .iter()
                .any(|icon| icon.owner_task_id == wimp_example_id)
            {
                if std::time::Instant::now() >= deadline {
                    return Err("closed WimpAlpha task kept an iconbar entry".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            if wimp
                .desktop_windows()
                .iter()
                .any(|window| window.owner_task_id == wimp_example_id)
            {
                return Err("closed WimpAlpha task kept its window".into());
            }

            let mut echo_scroll_steps = 0;
            while visible_catalogue_icon("Echo").is_none() {
                if echo_scroll_steps >= 30 {
                    return Err(format!(
                        "scrolling did not reveal Echo (catalogue index {echo_index})"
                    ));
                }
                let (_, furniture) = wimp
                    .furniture_layout()
                    .into_iter()
                    .find(|(window, _)| window.owner_task_id == filer_task_id)
                    .ok_or_else(|| "Filer window disappeared before finding Echo".to_string())?;
                let bar = furniture
                    .vertical_scrollbar
                    .ok_or_else(|| "Filer has no vertical scrollbar".to_string())?;
                if bar.track.max_y <= bar.slider.max_y {
                    return Err("Filer scrollbar has no page-up area".into());
                }
                let scroll_x = (bar.track.min_x + bar.track.max_x) / 2;
                let scroll_y = (bar.track.max_y + bar.slider.max_y) / 2;
                let previous_scroll = current_filer_window().unwrap().scroll_y;
                wimp.mouse_down(scroll_x, scroll_y, 4);
                let scroll_deadline = std::time::Instant::now() + Duration::from_secs(2);
                while current_filer_window().unwrap().scroll_y == previous_scroll {
                    if std::time::Instant::now() >= scroll_deadline {
                        return Err("Filer scrollbar did not page up".into());
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                echo_scroll_steps += 1;
            }

            let echo_icon = visible_catalogue_icon("Echo").unwrap();
            click_icon(&echo_icon);
            let selected_deadline = std::time::Instant::now() + Duration::from_secs(2);
            while visible_catalogue_icon("Echo").is_none_or(|icon| icon.flags & (1 << 21) == 0) {
                if std::time::Instant::now() >= selected_deadline {
                    return Err("clicking Echo did not select its Filer icon".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            click_icon(&visible_catalogue_icon("Echo").unwrap());

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let echo_request = loop {
                if let Some(request) = wimp.take_pending_launches().into_iter().next() {
                    break request;
                }
                if std::time::Instant::now() >= deadline {
                    return Err("double-clicking Echo did not start a BASIC task".into());
                }
                thread::sleep(Duration::from_millis(5));
            };
            if echo_request.guest_path != "$.Examples.Echo" {
                return Err(format!(
                    "Filer launched {} instead of Echo (catalogue index {echo_index}; output: {}; entries: {:?})",
                    echo_request.guest_path,
                    String::from_utf8_lossy(&output),
                    example_entries
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| (index, entry.guest_name.as_str()))
                        .collect::<Vec<_>>()
                ));
            }

            let (echo_input_sender, echo_input_receiver) = mpsc::channel();
            wimp.set_task_input(echo_request.task_id, echo_input_sender);
            let echo_wimp = wimp.clone();
            let echo_display = display_sender.clone();
            let echo_configure = configure.clone();
            let echo_volume = volume.clone();
            let task_id = echo_request.task_id;
            echo_task_id = Some(task_id);
            let echo_path = echo_request.guest_path.clone();
            let (echo_result_sender, echo_result_receiver) = mpsc::channel();
            echo_worker = Some(thread::spawn(move || {
                let cleanup_wimp = echo_wimp.clone();
                let mut runtime =
                    Runtime::desktop_task(task_id, echo_input_receiver, echo_display, echo_wimp);
                runtime
                    .dispatcher
                    .set_configure_store_for_test(echo_configure);
                runtime
                    .dispatcher
                    .set_file_system_for_test(HostFileSystem::new(echo_volume));
                let result = runtime.run_guest_file(&echo_path);
                cleanup_wimp.task_exited(task_id);
                let summary = match &result {
                    Ok(()) => Ok(()),
                    Err(error) => Err(error.to_string()),
                };
                let _ = echo_result_sender.send(summary);
                result
            }));

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !wimp.desktop_windows().iter().any(|window| {
                window.owner_task_id == task_id && window.title == "BASIC Output: Echo"
            }) {
                if echo_worker
                    .as_ref()
                    .is_some_and(|worker| worker.is_finished())
                {
                    return Err(format!(
                        "ordinary BASIC task exited before opening its output window: {:?}",
                        echo_result_receiver.try_recv().ok()
                    ));
                }
                if std::time::Instant::now() >= deadline {
                    return Err("ordinary BASIC task did not open its output window".into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            for byte in b"Mirror" {
                wimp.key_pressed(u32::from(*byte));
            }
            wimp.key_pressed(13);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !String::from_utf8_lossy(&output).contains("Mirror") {
                match display_receiver.recv_timeout(Duration::from_millis(20)) {
                    Ok(DisplayEvent::WriteByte {
                        task_id: id, byte, ..
                    }) if id == task_id => {
                        output.push(byte);
                    }
                    Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(error) => return Err(format!("display channel closed: {error}")),
                }
                if echo_worker
                    .as_ref()
                    .is_some_and(|worker| worker.is_finished())
                {
                    output.extend(display_receiver.try_iter().filter_map(|event| match event {
                        DisplayEvent::WriteByte {
                            task_id: id, byte, ..
                        } if id == task_id => Some(byte),
                        _ => None,
                    }));
                    if !String::from_utf8_lossy(&output).contains("Mirror") {
                        return Err(format!(
                            "ordinary BASIC task exited without echoing input: {:?}; output: {}",
                            echo_result_receiver.try_recv().ok(),
                            String::from_utf8_lossy(&output)
                        ));
                    }
                }
                if std::time::Instant::now() >= deadline {
                    return Err("input did not reach the ordinary BASIC task".into());
                }
            }

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while echo_worker
                .as_ref()
                .is_some_and(|worker| !worker.is_finished())
            {
                if std::time::Instant::now() >= deadline {
                    return Err(
                        "ordinary BASIC task did not finish after writing its output".into(),
                    );
                }
                thread::sleep(Duration::from_millis(5));
            }
            if wimp
                .desktop_icons()
                .iter()
                .any(|icon| icon.owner_task_id == task_id)
            {
                return Err("finished BASIC task kept an iconbar entry".into());
            }
            if wimp
                .desktop_windows()
                .iter()
                .any(|window| window.owner_task_id == task_id)
            {
                return Err("finished BASIC task kept its output window".into());
            }
            Ok(())
        })();

        wimp.stop();
        if let Some(task_id) = echo_task_id {
            wimp.task_exited(task_id);
        }
        if let Some(task_id) = wimp_example_task_id {
            wimp.task_exited(task_id);
        }
        let desktop_result = desktop_worker.join().unwrap();
        let filer_result = filer_worker.map(|worker| worker.join().unwrap());
        let wimp_example_result = wimp_example_worker.map(|worker| worker.join().unwrap());
        let echo_result = echo_worker.map(|worker| worker.join().unwrap());
        let _ = std::fs::remove_file(configure_path);
        scenario.unwrap_or_else(|error| panic!("desktop/Filer flow failed: {error}"));
        assert!(matches!(
            desktop_result,
            Err(RuntimeError::EndOfInput) | Ok(())
        ));
        assert!(matches!(
            filer_result,
            Some(Err(RuntimeError::EndOfInput)) | Some(Ok(()))
        ));
        assert!(matches!(wimp_example_result, Some(Ok(()))));
        assert!(matches!(echo_result, Some(Ok(()))));
    }
}
