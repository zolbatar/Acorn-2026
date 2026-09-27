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
    runtime::Runtime,
    swi::DisplayEvent,
    wimp::{DesktopWindow, WimpServer},
};

const ALPHA_TASK: u64 = 101;
const BETA_TASK: u64 = 102;
const EVENT_SLICE: Duration = Duration::from_millis(20);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(8);
const INTERACTION_TIMEOUT: Duration = Duration::from_secs(5);

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
        if let DisplayEvent::WriteByte { task_id, byte } = event {
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

        desktop.wait_for_output(ALPHA_TASK, "Alpha is ready", INTERACTION_TIMEOUT)?;
        desktop.wait_for_output(BETA_TASK, "Beta is ready", INTERACTION_TIMEOUT)?;
        require(
            desktop.task_output(ALPHA_TASK).contains("Alpha is ready"),
            "Alpha output was not captured",
        )?;
        require(
            desktop.task_output(BETA_TASK).contains("Beta is ready"),
            "Beta output was not captured",
        )?;
        require(
            !desktop.task_output(ALPHA_TASK).contains("Beta is ready"),
            "Beta output leaked to Alpha",
        )?;
        require(
            !desktop.task_output(BETA_TASK).contains("Alpha is ready"),
            "Alpha output leaked to Beta",
        )?;

        // A work-area click reaches Alpha's Wimp_Poll loop and causes output
        // tagged with Alpha's task identity.
        require(
            desktop.wimp.mouse_down(120, 200, 4).is_none(),
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

        require(
            desktop.wimp.mouse_down(500, 200, 4).is_none(),
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

fn click_close_icon(wimp: &WimpServer, window: &DesktopWindow) -> Result<(), String> {
    if !window.has_title || !window.closable {
        return Err(format!("window {} has no close icon", window.handle));
    }
    // The modern close icon is inside the title bar; when a vertical scroll
    // bar is present, the title extends beyond the work area's right edge.
    let x = if window.has_vertical_scrollbar {
        window.work_area.max_x + 4
    } else {
        window.work_area.max_x - 10
    };
    let y = window.work_area.max_y + 10;
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
