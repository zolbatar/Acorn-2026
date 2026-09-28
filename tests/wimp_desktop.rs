use std::{
    collections::HashMap,
    sync::{
        Arc, mpsc,
        mpsc::{Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use acorn_2026::{
    error::RuntimeError,
    memory::Task,
    runtime::Runtime,
    swi::{DisplayEvent, SwiContext},
    wimp::{
        DESKTOP_HEIGHT, DESKTOP_ICONBAR_HEIGHT, DESKTOP_WIDTH, DesktopRect, DesktopWindow,
        VerticalScrollbarLayout, WIMP_CREATE_WINDOW, WIMP_GET_WINDOW_STATE, WIMP_INITIALISE,
        WIMP_OPEN_WINDOW, WIMP_POLL, WimpServer, WindowDragKind, WindowFurnitureLayout, WorkArea,
        desktop_window_furniture,
    },
};

const ALPHA_TASK: u64 = 101;
const BETA_TASK: u64 = 102;
const EVENT_SLICE: Duration = Duration::from_millis(20);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const INTERACTION_TIMEOUT: Duration = Duration::from_secs(5);
const TASK_MAGIC: u32 = 0x4B53_4154;
const DESCRIPTION_ADDRESS: u32 = 0x1100;
const WINDOW_BLOCK_ADDRESS: u32 = 0x1200;
const OPEN_BLOCK_ADDRESS: u32 = 0x1300;
const POLL_BLOCK_ADDRESS: u32 = 0x1400;
const STATE_BLOCK_ADDRESS: u32 = 0x1500;
const MODERN_CONTROLS: u32 = CONTROL_EXPLICIT
    | CONTROL_RESIZE
    | CONTROL_VERTICAL_SCROLL
    | CONTROL_TOGGLE
    | CONTROL_TITLE
    | CONTROL_CLOSE
    | CONTROL_BACK
    | (1 << 1);
const CONTROL_BACK: u32 = 1 << 24;
const CONTROL_CLOSE: u32 = 1 << 25;
const CONTROL_TITLE: u32 = 1 << 26;
const CONTROL_TOGGLE: u32 = 1 << 27;
const CONTROL_VERTICAL_SCROLL: u32 = 1 << 28;
const CONTROL_RESIZE: u32 = 1 << 29;
const CONTROL_EXPLICIT: u32 = 1 << 31;
const STATE_FULLY_VISIBLE: u32 = 1 << 17;
const STATE_MAXIMIZED: u32 = 1 << 18;
const STATE_TOGGLE_REQUEST: u32 = 1 << 19;
const STATE_FOCUS: u32 = 1 << 20;

struct WimpFixture {
    wimp: std::sync::Arc<WimpServer>,
    task: Task,
    _desktop_updates: Receiver<()>,
}

impl WimpFixture {
    fn new(task_id: u64) -> Self {
        let (desktop_updates, desktop_update_events) = mpsc::channel();
        let wimp = WimpServer::new(desktop_updates);
        let mut task = Task::new(task_id);
        task.memory
            .write_bytes(DESCRIPTION_ADDRESS, b"desktop acceptance\r")
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[0] = 310;
        context.registers[1] = TASK_MAGIC;
        context.registers[2] = DESCRIPTION_ADDRESS;
        wimp.dispatch(WIMP_INITIALISE, &mut task, &mut context)
            .unwrap();
        assert_eq!(context.registers[0], 310);

        Self {
            wimp,
            task,
            _desktop_updates: desktop_update_events,
        }
    }

    fn create_window(
        &mut self,
        title: &str,
        extent: WorkArea,
        flags: u32,
        min_width: u16,
        min_height: u16,
    ) -> u32 {
        self.create_window_with_button_type(title, extent, flags, min_width, min_height, 0)
    }

    fn create_window_with_button_type(
        &mut self,
        title: &str,
        extent: WorkArea,
        flags: u32,
        min_width: u16,
        min_height: u16,
        button_type: u32,
    ) -> u32 {
        let mut block = [0; 88];
        put_word(&mut block, 28, flags);
        put_word(&mut block, 40, extent.min_x as u32);
        put_word(&mut block, 44, extent.min_y as u32);
        put_word(&mut block, 48, extent.max_x as u32);
        put_word(&mut block, 52, extent.max_y as u32);
        put_word(&mut block, 56, 1);
        put_word(&mut block, 60, button_type << 12);
        block[68..70].copy_from_slice(&min_width.to_le_bytes());
        block[70..72].copy_from_slice(&min_height.to_le_bytes());
        let title_bytes = title.as_bytes();
        let title_length = title_bytes.len().min(11);
        block[72..72 + title_length].copy_from_slice(&title_bytes[..title_length]);
        put_word(&mut block, 84, 0);
        self.task
            .memory
            .write_bytes(WINDOW_BLOCK_ADDRESS, &block)
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = WINDOW_BLOCK_ADDRESS;
        self.wimp
            .dispatch(WIMP_CREATE_WINDOW, &mut self.task, &mut context)
            .unwrap();
        context.registers[0]
    }

    fn open_window(
        &mut self,
        handle: u32,
        area: WorkArea,
        scroll_x: i32,
        scroll_y: i32,
        behind: i32,
    ) {
        let mut block = [0; 32];
        put_word(&mut block, 0, handle);
        put_word(&mut block, 4, area.min_x as u32);
        put_word(&mut block, 8, area.min_y as u32);
        put_word(&mut block, 12, area.max_x as u32);
        put_word(&mut block, 16, area.max_y as u32);
        put_word(&mut block, 20, scroll_x as u32);
        put_word(&mut block, 24, scroll_y as u32);
        put_word(&mut block, 28, behind as u32);
        self.task
            .memory
            .write_bytes(OPEN_BLOCK_ADDRESS, &block)
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = OPEN_BLOCK_ADDRESS;
        self.wimp
            .dispatch(WIMP_OPEN_WINDOW, &mut self.task, &mut context)
            .unwrap();
    }

    fn poll(&mut self) -> (u32, Vec<u8>) {
        let mut context = SwiContext::default();
        // These fixture tests exercise desktop input and stacking. Mask redraw
        // requests here; the redraw protocol has its own focused unit coverage.
        context.registers[0] = 1 << 1;
        context.registers[1] = POLL_BLOCK_ADDRESS;
        self.wimp
            .dispatch(WIMP_POLL, &mut self.task, &mut context)
            .unwrap();
        let block = self.task.memory.read_bytes(POLL_BLOCK_ADDRESS, 40).unwrap();
        (context.registers[0], block)
    }

    fn accept_open_request(&mut self) {
        let mut context = SwiContext::default();
        context.registers[1] = POLL_BLOCK_ADDRESS;
        self.wimp
            .dispatch(WIMP_OPEN_WINDOW, &mut self.task, &mut context)
            .unwrap();
    }

    fn window(&self, handle: u32) -> DesktopWindow {
        self.wimp
            .desktop_windows()
            .into_iter()
            .find(|window| window.handle == handle)
            .unwrap_or_else(|| panic!("window {handle} is not open"))
    }

    fn layout(&self, handle: u32) -> WindowFurnitureLayout {
        desktop_window_furniture(&self.window(handle))
    }

    fn state(&mut self, handle: u32) -> Vec<u8> {
        self.task
            .memory
            .write_bytes(STATE_BLOCK_ADDRESS, &handle.to_le_bytes())
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK_ADDRESS;
        self.wimp
            .dispatch(WIMP_GET_WINDOW_STATE, &mut self.task, &mut context)
            .unwrap();
        self.task
            .memory
            .read_bytes(STATE_BLOCK_ADDRESS, 36)
            .unwrap()
    }
}

const TEST_AREA: WorkArea = WorkArea {
    min_x: 30,
    // Keep the resize cell and frame above the desktop's icon bar.
    // 48-unit resize cell plus the two-unit outer rule.
    min_y: DESKTOP_ICONBAR_HEIGHT + 50,
    max_x: 390,
    max_y: 400,
};
const TEST_EXTENT: WorkArea = WorkArea {
    min_x: 0,
    min_y: -2_000,
    max_x: 900,
    max_y: 0,
};

struct DesktopAcceptance {
    wimp: Arc<WimpServer>,
    display_events: Receiver<DisplayEvent>,
    completions: Receiver<(u64, Result<(), String>)>,
    workers: HashMap<u64, JoinHandle<Result<(), String>>>,
    completed: HashMap<u64, Result<(), String>>,
    output: HashMap<u64, Vec<u8>>,
}

impl DesktopAcceptance {
    fn start() -> Self {
        let (display_sender, display_events) = mpsc::channel();
        let (desktop_updates, _desktop_update_events) = mpsc::channel();
        let wimp = WimpServer::new(desktop_updates);
        let (completion_sender, completions) = mpsc::channel();
        let mut workers = HashMap::new();

        for (task_id, source, name) in [
            (
                ALPHA_TASK,
                include_str!("../examples/wimp/two-windows/alpha.bas64"),
                "wimp-alpha-acceptance",
            ),
            (
                BETA_TASK,
                include_str!("../examples/wimp/two-windows/beta.bas64"),
                "wimp-beta-acceptance",
            ),
        ] {
            let task_wimp = wimp.clone();
            let task_display = display_sender.clone();
            let task_completions = completion_sender.clone();
            let source = source.to_owned();
            let worker = thread::Builder::new()
                .name(name.into())
                .spawn(move || {
                    let (_input_sender, input_receiver) = mpsc::channel();
                    let mut runtime =
                        Runtime::desktop_task(task_id, input_receiver, task_display, task_wimp);
                    let result = match runtime.run_application(&source) {
                        Ok(()) | Err(RuntimeError::EndOfInput) => Ok(()),
                        Err(error) => Err(error.to_string()),
                    };
                    drop(runtime);
                    let _ = task_completions.send((task_id, result.clone()));
                    result
                })
                .expect("could not spawn Wimp guest task");
            workers.insert(task_id, worker);
        }

        Self {
            wimp,
            display_events,
            completions,
            workers,
            completed: HashMap::new(),
            output: HashMap::new(),
        }
    }

    fn wait_for_windows(&mut self, count: usize, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain_completions()?;
            if self.wimp.desktop_windows().len() >= count {
                return Ok(());
            }
            if let Some((task_id, result)) = self.completed.iter().next() {
                return Err(match result {
                    Ok(()) => format!("task {task_id} exited before opening its Wimp window"),
                    Err(error) => {
                        format!("task {task_id} failed before opening its Wimp window: {error}")
                    }
                });
            }
            self.wait_for_display_event(deadline, "waiting for Wimp windows")?;
        }
    }

    fn wait_for_output(
        &mut self,
        task_id: u64,
        expected: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            self.drain_completions()?;
            if self.task_output(task_id).contains(expected) {
                return Ok(());
            }
            if let Some(result) = self.completed.get(&task_id) {
                return Err(match result {
                    Ok(()) => format!("task {task_id} exited before displaying {expected:?}"),
                    Err(error) => {
                        format!("task {task_id} failed before displaying {expected:?}: {error}")
                    }
                });
            }
            if let Err(error) = self.wait_for_display_event(deadline, expected) {
                let windows = self.wimp.desktop_windows();
                return Err(format!(
                    "{error}; task {task_id} output so far: {:?}; windows: {:?}",
                    self.task_output(task_id),
                    windows
                ));
            }
        }
    }

    fn wait_for_completion(&mut self, task_id: u64, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        while !self.completed.contains_key(&task_id) {
            self.drain_display_events();
            self.receive_completions();
            if self.completed.contains_key(&task_id) {
                break;
            }
            self.wait_for_completion_event(deadline, task_id)?;
        }
        match self.completed.get(&task_id) {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => Err(format!("task {task_id} returned an error: {error}")),
            None => Err(format!("task {task_id} did not finish")),
        }
    }

    fn close_window(&self, task_id: u64) -> Result<(), String> {
        let window = self
            .wimp
            .desktop_windows()
            .into_iter()
            .find(|window| window.owner_task_id == task_id)
            .ok_or_else(|| format!("no open window for task {task_id}"))?;
        click_close_icon(&self.wimp, &window)
    }

    fn task_output(&self, task_id: u64) -> String {
        String::from_utf8_lossy(
            self.output
                .get(&task_id)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        )
        .into_owned()
    }

    fn drain_completions(&mut self) -> Result<(), String> {
        self.receive_completions();
        if let Some((task_id, Err(error))) =
            self.completed.iter().find(|(_, result)| result.is_err())
        {
            return Err(format!("task {task_id} returned an error: {error}"));
        }
        Ok(())
    }

    fn receive_completions(&mut self) {
        while let Ok((task_id, result)) = self.completions.try_recv() {
            self.completed.insert(task_id, result);
        }
    }

    fn drain_display_events(&mut self) {
        while let Ok(event) = self.display_events.try_recv() {
            self.record_display_event(event);
        }
    }

    fn wait_for_display_event(&mut self, deadline: Instant, operation: &str) -> Result<(), String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("timed out {operation}"));
        }
        match self.display_events.recv_timeout(remaining.min(EVENT_SLICE)) {
            Ok(event) => {
                self.record_display_event(event);
                Ok(())
            }
            Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => Ok(()),
            Err(RecvTimeoutError::Timeout) => Err(format!("timed out {operation}")),
            Err(RecvTimeoutError::Disconnected) => {
                Err(format!("display event channel closed while {operation}"))
            }
        }
    }

    fn wait_for_completion_event(&mut self, deadline: Instant, task_id: u64) -> Result<(), String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("timed out waiting for task {task_id} to close"));
        }
        match self.completions.recv_timeout(remaining.min(EVENT_SLICE)) {
            Ok((completed_id, result)) => {
                self.completed.insert(completed_id, result);
                Ok(())
            }
            Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => Ok(()),
            Err(RecvTimeoutError::Timeout) => {
                Err(format!("timed out waiting for task {task_id} to close"))
            }
            Err(RecvTimeoutError::Disconnected) => Err(format!(
                "completion channel closed while waiting for task {task_id}"
            )),
        }
    }

    fn shutdown_and_join(&mut self) -> Result<(), String> {
        self.wimp.stop();
        let mut errors = Vec::new();
        for task_id in [ALPHA_TASK, BETA_TASK] {
            if let Err(error) = self.wait_for_completion(task_id, INTERACTION_TIMEOUT) {
                errors.push(error);
            }
        }
        let all_completed = [ALPHA_TASK, BETA_TASK]
            .iter()
            .all(|task_id| self.completed.contains_key(task_id));
        for (task_id, worker) in self.workers.drain() {
            if all_completed {
                match worker.join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        errors.push(format!("task {task_id} returned an error: {error}"))
                    }
                    Err(_) => errors.push(format!("task {task_id} panicked")),
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn cleanup_open_windows(&self) {
        for _ in 0..3 {
            let windows = self.wimp.desktop_windows();
            if windows.is_empty() {
                break;
            }
            for window in windows {
                let _ = click_close_icon(&self.wimp, &window);
            }
        }
        self.wimp.stop();
    }

    fn record_display_event(&mut self, event: DisplayEvent) {
        if let DisplayEvent::WriteByte { task_id, byte, .. } = event {
            self.output.entry(task_id).or_default().push(byte);
        }
    }
}

#[test]
fn two_wimp_basic_tasks_poll_and_respond_independently() {
    let mut desktop = DesktopAcceptance::start();

    let scenario = (|| -> Result<(), String> {
        desktop.wait_for_windows(2, STARTUP_TIMEOUT)?;
        let windows = desktop.wimp.desktop_windows();
        require(
            windows.len() == 2,
            format!("expected two windows, found {windows:?}"),
        )?;
        require(
            windows
                .iter()
                .any(|window| window.owner_task_id == ALPHA_TASK && window.title == "Alpha App"),
            "Alpha App window was not opened",
        )?;
        require(
            windows
                .iter()
                .any(|window| window.owner_task_id == BETA_TASK && window.title == "Beta App"),
            "Beta App window was not opened",
        )?;
        for window in &windows {
            require(
                window.has_back_icon
                    && window.closable
                    && window.has_title
                    && window.has_toggle_size_icon
                    && window.has_vertical_scrollbar
                    && window.resizable,
                format!(
                    "{} did not request the RISC OS Back, Close, Title and Toggle controls",
                    window.title
                ),
            )?;
        }

        desktop.wait_for_output(ALPHA_TASK, "Alpha window", INTERACTION_TIMEOUT)?;
        desktop.wait_for_output(BETA_TASK, "Beta window", INTERACTION_TIMEOUT)?;
        require(
            desktop.task_output(ALPHA_TASK).contains("Alpha window"),
            "Alpha output was not captured",
        )?;
        require(
            desktop.task_output(BETA_TASK).contains("Beta window"),
            "Beta output was not captured",
        )?;
        require(
            !desktop.task_output(ALPHA_TASK).contains("Beta window"),
            "Beta output leaked to Alpha",
        )?;
        require(
            !desktop.task_output(BETA_TASK).contains("Alpha window"),
            "Alpha output leaked to Beta",
        )?;

        // A work-area click reaches Alpha's Wimp_Poll loop and causes output
        // tagged with Alpha's task identity.
        let alpha = desktop
            .wimp
            .desktop_windows()
            .into_iter()
            .find(|window| window.owner_task_id == ALPHA_TASK)
            .ok_or_else(|| "Alpha window disappeared before its click".to_owned())?;
        let alpha_work_point = rect_center(desktop_window_furniture(&alpha).work_area);
        require(
            desktop
                .wimp
                .mouse_down(alpha_work_point.0, alpha_work_point.1, 4)
                .is_none(),
            "Alpha work-area click started a drag",
        )?;
        desktop.wait_for_output(ALPHA_TASK, "Mouse clicks: 1", INTERACTION_TIMEOUT)?;
        require(
            !desktop.task_output(BETA_TASK).contains("Mouse clicks: 1"),
            "Alpha click output leaked to Beta",
        )?;

        // Keyboard input follows the window that received the previous click.
        desktop.wimp.key_pressed(65);
        desktop.wait_for_output(ALPHA_TASK, "Key code: 65", INTERACTION_TIMEOUT)?;

        // Close Alpha through its actual close-icon hit target and confirm
        // Beta remains alive and can still receive events.
        desktop.close_window(ALPHA_TASK)?;
        desktop.wait_for_completion(ALPHA_TASK, INTERACTION_TIMEOUT)?;
        let open_windows = desktop.wimp.desktop_windows();
        require(
            open_windows
                .iter()
                .any(|window| window.owner_task_id == BETA_TASK),
            "Beta window closed with Alpha",
        )?;
        require(
            !open_windows
                .iter()
                .any(|window| window.owner_task_id == ALPHA_TASK),
            "Alpha window remained open",
        )?;

        let beta = open_windows
            .iter()
            .find(|window| window.owner_task_id == BETA_TASK)
            .ok_or_else(|| "Beta window disappeared before its click".to_owned())?;
        let beta_work_point = rect_center(desktop_window_furniture(beta).work_area);
        require(
            desktop
                .wimp
                .mouse_down(beta_work_point.0, beta_work_point.1, 4)
                .is_none(),
            "Beta work-area click started a drag",
        )?;
        desktop.wait_for_output(BETA_TASK, "Mouse clicks: 1", INTERACTION_TIMEOUT)?;
        require(
            !desktop.task_output(BETA_TASK).contains("Key code: 65"),
            "Alpha key output leaked to Beta",
        )?;

        desktop.close_window(BETA_TASK)?;
        desktop.wait_for_completion(BETA_TASK, INTERACTION_TIMEOUT)?;
        require(
            desktop.wimp.desktop_windows().is_empty(),
            "Wimp windows remained after both tasks closed",
        )?;
        Ok(())
    })();

    if let Err(error) = scenario {
        desktop.cleanup_open_windows();
        let cleanup = desktop.shutdown_and_join();
        panic!("Wimp desktop acceptance failed: {error}; cleanup: {cleanup:?}");
    }

    desktop
        .shutdown_and_join()
        .unwrap_or_else(|error| panic!("Wimp desktop tasks did not shut down cleanly: {error}"));
}

#[test]
fn wimp_back_sends_window_to_stack_bottom_and_moves_focus() {
    let mut fixture = WimpFixture::new(201);
    let back = fixture.create_window("Back", TEST_EXTENT, MODERN_CONTROLS, 48, 48);
    fixture.open_window(back, TEST_AREA, 0, -800, -1);
    let front = fixture.create_window("Front", TEST_EXTENT, MODERN_CONTROLS, 48, 48);
    fixture.open_window(front, TEST_AREA, 0, -800, -2);
    let control_mask = CONTROL_EXPLICIT
        | CONTROL_RESIZE
        | CONTROL_VERTICAL_SCROLL
        | CONTROL_TOGGLE
        | CONTROL_TITLE
        | CONTROL_CLOSE
        | CONTROL_BACK;
    assert_eq!(
        read_word(&fixture.state(back), 32) & control_mask,
        MODERN_CONTROLS & control_mask
    );

    let work_point = rect_center(fixture.layout(back).work_area);
    assert!(
        fixture
            .wimp
            .mouse_down(work_point.0, work_point.1, 4)
            .is_none()
    );
    assert_ne!(read_word(&fixture.state(back), 32) & STATE_FOCUS, 0);
    assert_ne!(read_word(&fixture.state(back), 32) & STATE_FULLY_VISIBLE, 0);
    assert_eq!(
        read_word(&fixture.state(front), 32) & STATE_FULLY_VISIBLE,
        0
    );

    let back_point = rect_center(fixture.layout(back).back_icon.unwrap());
    assert!(
        fixture
            .wimp
            .mouse_down(back_point.0, back_point.1, 4)
            .is_none()
    );
    let windows = fixture.wimp.desktop_windows();
    assert_eq!(windows[0].handle, front);
    assert_eq!(windows[1].handle, back);
    assert_eq!(read_word(&fixture.state(back), 32) & STATE_FOCUS, 0);
    assert_ne!(read_word(&fixture.state(front), 32) & STATE_FOCUS, 0);
    assert_eq!(read_word(&fixture.state(back), 32) & STATE_FULLY_VISIBLE, 0);
    assert_ne!(
        read_word(&fixture.state(front), 32) & STATE_FULLY_VISIBLE,
        0
    );
    assert_eq!(fixture.poll().0, 0, "Back is handled by the Wimp stack");
}

#[test]
fn wimp_toggle_size_uses_open_requests_and_restores_bounds_and_depth() {
    for (index, first_button) in [(0_u64, 4_u32), (1, 1)] {
        let mut fixture = WimpFixture::new(210 + index);
        let target = fixture.create_window("Target", TEST_EXTENT, MODERN_CONTROLS, 48, 48);
        fixture.open_window(target, TEST_AREA, 0, -800, -1);
        let side_area = WorkArea {
            min_x: 500,
            max_x: 860,
            ..TEST_AREA
        };
        let sibling = fixture.create_window("Sibling", TEST_EXTENT, MODERN_CONTROLS, 48, 48);
        fixture.open_window(sibling, side_area, 0, -800, -1);
        let expected_max_behind = if first_button == 4 {
            -1
        } else {
            sibling as i32
        };

        let original = fixture.window(target).work_area;
        let toggle = fixture.layout(target).toggle_size_icon.unwrap();
        let toggle_point = rect_center(toggle);
        assert!(
            fixture
                .wimp
                .mouse_down(toggle_point.0, toggle_point.1, first_button)
                .is_none()
        );
        let (reason, request) = fixture.poll();
        assert_eq!(reason, 2);
        assert_eq!(read_word(&request, 0), target);
        assert_eq!(read_word(&request, 28) as i32, expected_max_behind);
        let pending = fixture.window(target);
        let maximized = pending
            .preview_area
            .expect("toggle requested maximum bounds");
        assert_eq!(read_word(&request, 4) as i32, maximized.min_x);
        assert_eq!(read_word(&request, 8) as i32, maximized.min_y);
        assert_eq!(read_word(&request, 12) as i32, maximized.max_x);
        assert_eq!(read_word(&request, 16) as i32, maximized.max_y);
        assert_eq!(
            read_word(&fixture.state(target), 32) & (STATE_MAXIMIZED | STATE_TOGGLE_REQUEST),
            STATE_MAXIMIZED | STATE_TOGGLE_REQUEST
        );
        assert_eq!(fixture.window(target).work_area, original);

        fixture.accept_open_request();
        let opened = fixture.window(target);
        assert!(opened.maximized);
        assert_eq!(opened.work_area, maximized);
        let state = fixture.state(target);
        assert_ne!(read_word(&state, 32) & STATE_MAXIMIZED, 0);
        assert_eq!(read_word(&state, 32) & STATE_TOGGLE_REQUEST, 0);
        if first_button == 4 {
            assert_eq!(fixture.wimp.desktop_windows()[0].handle, target);
            assert_eq!(read_word(&state, 28) as i32, -1);
        } else {
            assert_eq!(fixture.wimp.desktop_windows()[0].handle, sibling);
            assert_eq!(read_word(&state, 28), sibling);
        }

        let restore_point = rect_center(fixture.layout(target).toggle_size_icon.unwrap());
        assert!(
            fixture
                .wimp
                .mouse_down(restore_point.0, restore_point.1, 1)
                .is_none()
        );
        let (reason, restore) = fixture.poll();
        assert_eq!(reason, 2);
        assert_eq!(read_word(&restore, 0), target);
        assert_eq!(read_word(&restore, 4) as i32, original.min_x);
        assert_eq!(read_word(&restore, 8) as i32, original.min_y);
        assert_eq!(read_word(&restore, 12) as i32, original.max_x);
        assert_eq!(read_word(&restore, 16) as i32, original.max_y);
        assert_eq!(read_word(&restore, 28), sibling);
        assert_eq!(read_word(&fixture.state(target), 32) & STATE_MAXIMIZED, 0);
        assert_ne!(
            read_word(&fixture.state(target), 32) & STATE_TOGGLE_REQUEST,
            0
        );
        fixture.accept_open_request();
        let restored = fixture.window(target);
        assert!(!restored.maximized);
        assert_eq!(restored.work_area, original);
        assert_eq!((restored.scroll_x, restored.scroll_y), (0, -800));
        let state = fixture.state(target);
        assert_eq!(
            read_word(&state, 32) & (STATE_MAXIMIZED | STATE_TOGGLE_REQUEST),
            0
        );
        assert_eq!(read_word(&state, 28), sibling);
        assert_eq!(fixture.wimp.desktop_windows()[0].handle, sibling);
        assert_eq!(fixture.wimp.desktop_windows()[1].handle, target);
    }
}

#[test]
fn wimp_default_scrollbars_use_os_unit_steps_pages_and_extent_clamps() {
    let mut fixture = WimpFixture::new(220);
    let handle = fixture.create_window("Scroll", TEST_EXTENT, MODERN_CONTROLS, 48, 48);
    fixture.open_window(handle, TEST_AREA, 0, -800, -1);

    let up = rect_center(fixture.layout(handle).vertical_scrollbar.unwrap().up_arrow);
    fixture.wimp.mouse_down(up.0, up.1, 4);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, -768); // Up is +32 OS units.
    assert_eq!(read_word(&request, 32), 0);
    assert_eq!(read_word(&request, 36), 0);
    fixture.accept_open_request();

    let down = rect_center(
        fixture
            .layout(handle)
            .vertical_scrollbar
            .unwrap()
            .down_arrow,
    );
    fixture.wimp.mouse_down(down.0, down.1, 4);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, -800); // Down is -32.
    fixture.accept_open_request();

    let up = rect_center(fixture.layout(handle).vertical_scrollbar.unwrap().up_arrow);
    fixture.wimp.mouse_down(up.0, up.1, 1);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, -832); // Adjust reverses Select.
    fixture.accept_open_request();

    let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
    let page_up = page_track_point(bar, 1).expect("page-up area beside slider");
    fixture.wimp.mouse_down(page_up.0, page_up.1, 4);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    let page_step = (TEST_AREA.max_y - TEST_AREA.min_y) * 2 / 3;
    assert_eq!(read_word(&request, 24) as i32, -832 + page_step);
    fixture.accept_open_request();

    let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
    let page_down = page_track_point(bar, -1).expect("page-down area beside slider");
    fixture.wimp.mouse_down(page_down.0, page_down.1, 4);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, -832);
    fixture.accept_open_request();

    let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
    let page_up = page_track_point(bar, 1).unwrap();
    fixture.wimp.mouse_down(page_up.0, page_up.1, 1);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, -832 - page_step);
    fixture.accept_open_request();

    let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
    let page_down = page_track_point(bar, -1).unwrap();
    fixture.wimp.mouse_down(page_down.0, page_down.1, 1);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, -832);
    fixture.accept_open_request();

    let minimum_scroll_y = TEST_EXTENT.min_y + (TEST_AREA.max_y - TEST_AREA.min_y);
    fixture.open_window(handle, TEST_AREA, 0, minimum_scroll_y, -1);
    let down = rect_center(
        fixture
            .layout(handle)
            .vertical_scrollbar
            .unwrap()
            .down_arrow,
    );
    fixture.wimp.mouse_down(down.0, down.1, 4);
    assert_eq!(fixture.poll().0, 0, "scroll past extent end is a no-op");

    fixture.open_window(handle, TEST_AREA, 0, TEST_EXTENT.max_y, -1);
    let up = rect_center(fixture.layout(handle).vertical_scrollbar.unwrap().up_arrow);
    fixture.wimp.mouse_down(up.0, up.1, 4);
    assert_eq!(fixture.poll().0, 0, "scroll past extent start is a no-op");

    fixture.open_window(handle, TEST_AREA, 0, -1_500, -1);
    let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
    let page_down = page_track_point(bar, -1).unwrap();
    fixture.wimp.mouse_down(page_down.0, page_down.1, 4);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, -1_500 - page_step);
    fixture.accept_open_request();
}

#[test]
fn wimp_scroll_request_flags_report_arrow_and_page_directions() {
    for (index, flag) in [1 << 8, 1 << 9].into_iter().enumerate() {
        let mut fixture = WimpFixture::new(230 + index as u64);
        let handle = fixture.create_window(
            "Scroll request",
            TEST_EXTENT,
            MODERN_CONTROLS | flag,
            48,
            48,
        );
        fixture.open_window(handle, TEST_AREA, 0, -800, -1);
        let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
        for (point, button, expected) in [
            (rect_center(bar.up_arrow), 4, 1),
            (rect_center(bar.up_arrow), 1, -1),
            (page_track_point(bar, 1).unwrap(), 4, 2),
            (page_track_point(bar, -1).unwrap(), 4, -2),
            (page_track_point(bar, -1).unwrap(), 1, 2),
        ] {
            assert!(fixture.wimp.mouse_down(point.0, point.1, button).is_none());
            let (reason, request) = fixture.poll();
            assert_eq!(reason, 10);
            assert_eq!(read_word(&request, 0), handle);
            assert_eq!(read_word(&request, 20), 0);
            assert_eq!(read_word(&request, 24) as i32, -800);
            assert_eq!(read_word(&request, 32) as i32, 0);
            assert_eq!(read_word(&request, 36) as i32, expected);
            assert_eq!(fixture.window(handle).scroll_y, -800);
        }
    }
}

#[test]
fn wimp_slider_grab_is_stable_and_drag_reaches_both_extent_endpoints() {
    let mut fixture = WimpFixture::new(240);
    let handle = fixture.create_window("Slider", TEST_EXTENT, MODERN_CONTROLS, 48, 48);
    fixture.open_window(handle, TEST_AREA, 0, -800, -1);

    let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
    let (x, y) = rect_center(bar.slider);
    let drag = fixture
        .wimp
        .mouse_down(x, y, 4)
        .expect("slider thumb starts a drag");
    assert_eq!(drag.kind, WindowDragKind::ScrollSlider);
    fixture.wimp.drag_to(drag, x, y);
    assert_eq!(fixture.window(handle).preview_scroll, Some((0, -800)));
    fixture.wimp.finish_drag(drag);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, -800);
    fixture.accept_open_request();

    let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
    let (x, y) = rect_center(bar.slider);
    let drag = fixture.wimp.mouse_down(x, y, 4).unwrap();
    let pointer_from_thumb_top = y - bar.slider.max_y;
    fixture
        .wimp
        .drag_to(drag, x, bar.track.max_y + pointer_from_thumb_top);
    assert_eq!(
        fixture.window(handle).preview_scroll,
        Some((0, TEST_EXTENT.max_y))
    );
    fixture.wimp.finish_drag(drag);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, TEST_EXTENT.max_y);
    fixture.accept_open_request();

    let bar = fixture.layout(handle).vertical_scrollbar.unwrap();
    let (x, y) = rect_center(bar.slider);
    let drag = fixture.wimp.mouse_down(x, y, 4).unwrap();
    let pointer_from_thumb_top = y - bar.slider.max_y;
    let slider_travel = (bar.track.max_y - bar.track.min_y) - (bar.slider.max_y - bar.slider.min_y);
    let bottom_slider_top = bar.track.max_y - slider_travel;
    fixture
        .wimp
        .drag_to(drag, x, bottom_slider_top + pointer_from_thumb_top);
    let minimum_scroll_y = TEST_EXTENT.min_y + (TEST_AREA.max_y - TEST_AREA.min_y);
    assert_eq!(
        fixture.window(handle).preview_scroll,
        Some((0, minimum_scroll_y))
    );
    fixture.wimp.finish_drag(drag);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 24) as i32, minimum_scroll_y);
}

#[test]
fn wimp_resize_clamps_to_extent_screen_and_control_cell_geometry() {
    let mut fixture = WimpFixture::new(250);
    let handle = fixture.create_window("Resize", TEST_EXTENT, MODERN_CONTROLS, 48, 48);
    fixture.open_window(handle, TEST_AREA, 0, 0, -1);

    let adjust = rect_center(fixture.layout(handle).adjust_size_icon.unwrap());
    let drag = fixture
        .wimp
        .mouse_down(adjust.0, adjust.1, 4)
        .expect("Adjust Size icon starts resize");
    assert_eq!(drag.kind, WindowDragKind::Resize);
    fixture
        .wimp
        .drag_to(drag, DESKTOP_WIDTH * 4, -DESKTOP_HEIGHT * 4);
    let preview = fixture.window(handle).preview_area.unwrap();
    assert!(preview.max_x - preview.min_x <= TEST_EXTENT.max_x - TEST_EXTENT.min_x);
    assert!(preview.max_y - preview.min_y <= TEST_EXTENT.max_y - TEST_EXTENT.min_y);
    let preview_window = fixture.window(handle);
    let layout = desktop_window_furniture(&preview_window);
    assert!(layout.outer.min_x >= 0);
    assert!(layout.outer.min_y >= 0);
    assert!(layout.outer.max_x <= DESKTOP_WIDTH);
    assert!(layout.outer.max_y <= DESKTOP_HEIGHT);
    fixture.wimp.finish_drag(drag);
    let (reason, request) = fixture.poll();
    assert_eq!(reason, 2);
    assert_eq!(read_word(&request, 4) as i32, preview.min_x);
    assert_eq!(read_word(&request, 8) as i32, preview.min_y);
    assert_eq!(read_word(&request, 12) as i32, preview.max_x);
    assert_eq!(read_word(&request, 16) as i32, preview.max_y);

    let screen_extent = WorkArea {
        min_x: 0,
        min_y: -10_000,
        max_x: 10_000,
        max_y: 0,
    };
    let mut screen_fixture = WimpFixture::new(252);
    let screen_window =
        screen_fixture.create_window("Screen bound", screen_extent, MODERN_CONTROLS, 48, 48);
    screen_fixture.open_window(screen_window, TEST_AREA, 0, 0, -1);
    let adjust = rect_center(
        screen_fixture
            .layout(screen_window)
            .adjust_size_icon
            .unwrap(),
    );
    let drag = screen_fixture
        .wimp
        .mouse_down(adjust.0, adjust.1, 4)
        .expect("Adjust Size icon starts resize");
    screen_fixture
        .wimp
        .drag_to(drag, DESKTOP_WIDTH * 4, -DESKTOP_HEIGHT * 4);
    let screen_layout = screen_fixture.layout(screen_window);
    assert_eq!(screen_layout.outer.max_x, DESKTOP_WIDTH);
    assert_eq!(screen_layout.outer.min_y, DESKTOP_ICONBAR_HEIGHT);

    let mut minimum_fixture = WimpFixture::new(251);
    let narrow = minimum_fixture.create_window("Narrow", TEST_EXTENT, MODERN_CONTROLS, 48, 48);
    minimum_fixture.open_window(narrow, TEST_AREA, 0, 0, -1);
    let adjust = rect_center(minimum_fixture.layout(narrow).adjust_size_icon.unwrap());
    let drag = minimum_fixture
        .wimp
        .mouse_down(adjust.0, adjust.1, 4)
        .expect("Adjust Size icon starts resize");
    minimum_fixture.wimp.drag_to(
        drag,
        drag.start_x - DESKTOP_WIDTH,
        drag.start_y + DESKTOP_HEIGHT,
    );
    let layout = minimum_fixture.layout(narrow);
    let back_right = layout.back_icon.unwrap().max_x;
    let close_left = layout.close_icon.unwrap().min_x;
    let close_right = layout.close_icon.unwrap().max_x;
    let toggle_left = layout.toggle_size_icon.unwrap().min_x;
    assert!(
        back_right <= close_left,
        "Back and Close cells overlap at minimum size"
    );
    assert!(
        close_right <= toggle_left,
        "Close and Toggle cells overlap at minimum size"
    );
}

#[test]
fn wimp_menu_clicks_are_reported_in_work_area_but_ignored_on_furniture() {
    for (index, button_type) in [0, 3].into_iter().enumerate() {
        let mut fixture = WimpFixture::new(260 + index as u64);
        let handle = fixture.create_window_with_button_type(
            "Menu",
            TEST_EXTENT,
            MODERN_CONTROLS,
            48,
            48,
            button_type,
        );
        fixture.open_window(handle, TEST_AREA, 0, -800, -1);

        let work_point = rect_center(fixture.layout(handle).work_area);
        assert!(
            fixture
                .wimp
                .mouse_down(work_point.0, work_point.1, 2)
                .is_none()
        );
        let (reason, event) = fixture.poll();
        assert_eq!(
            reason, 6,
            "Menu must be reported for button type {button_type}"
        );
        assert_eq!(read_word(&event, 0) as i32, work_point.0);
        assert_eq!(read_word(&event, 4) as i32, work_point.1);
        assert_eq!(read_word(&event, 8), 2);
        assert_eq!(read_word(&event, 12), handle);
        assert_eq!(read_word(&event, 16) as i32, -1);

        let back_point = rect_center(fixture.layout(handle).back_icon.unwrap());
        assert!(
            fixture
                .wimp
                .mouse_down(back_point.0, back_point.1, 2)
                .is_none()
        );
        assert_eq!(fixture.poll().0, 0, "Menu is ignored over system furniture");
    }
}

fn put_word(block: &mut [u8], offset: usize, value: u32) {
    block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn read_word(block: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(block[offset..offset + 4].try_into().unwrap())
}

fn rect_center(rect: DesktopRect) -> (i32, i32) {
    (
        rect.min_x + (rect.max_x - rect.min_x) / 2,
        rect.min_y + (rect.max_y - rect.min_y) / 2,
    )
}

fn page_track_point(bar: VerticalScrollbarLayout, direction: i32) -> Option<(i32, i32)> {
    let (min_y, max_y) = if direction > 0 {
        (bar.slider.max_y, bar.track.max_y)
    } else {
        (bar.track.min_y, bar.slider.min_y)
    };
    (max_y > min_y).then(|| {
        (
            bar.track.min_x + (bar.track.max_x - bar.track.min_x) / 2,
            min_y + (max_y - min_y) / 2,
        )
    })
}

fn click_close_icon(wimp: &WimpServer, window: &DesktopWindow) -> Result<(), String> {
    if !window.has_title || !window.closable {
        return Err(format!("window {} has no close icon", window.handle));
    }
    let close_icon = desktop_window_furniture(window)
        .close_icon
        .ok_or_else(|| format!("window {} has no close-icon geometry", window.handle))?;
    let (x, y) = rect_center(close_icon);
    if wimp.mouse_down(x, y, 4).is_some() {
        return Err(format!(
            "close icon for window {} initiated a drag",
            window.handle
        ));
    }
    Ok(())
}

fn require(condition: bool, message: impl Into<String>) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message.into())
    }
}
