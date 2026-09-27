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
    pub fn stdio() -> Self {
        Self::new(HostConsole::stdio())
    }

    pub fn windowed(input: Receiver<u8>, display_events: Sender<DisplayEvent>) -> Self {
        Self {
            task: Task::new(TASK_ID),
            dispatcher: SwiDispatcher::windowed(HostConsole::windowed(input), display_events),
        }
    }

    pub fn windowed_with_desktop(
        input: Receiver<u8>,
        display_events: Sender<DisplayEvent>,
        wimp: Arc<WimpServer>,
    ) -> Self {
        Self {
            task: Task::new(TASK_ID),
            dispatcher: SwiDispatcher::windowed_with_desktop(
                HostConsole::windowed(input),
                display_events,
                wimp,
            ),
        }
    }

    /// Construct an independent BASIC task attached to the shared hosted Wimp.
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

    pub fn new(console: HostConsole) -> Self {
        Self {
            task: Task::new(TASK_ID),
            dispatcher: SwiDispatcher::new(console),
        }
    }

    pub fn graphics(&self) -> &GraphicsService {
        self.dispatcher.graphics()
    }

    pub fn run(&mut self) -> Result<(), RuntimeError> {
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
        let message = format!("Acorn-2026 error: {error}");
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
        configure::{BasicEngine, ConfigureStore},
        swi::{DisplayEvent, SwiDispatcher},
        wimp::WimpServer,
    };

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
            "acorn-2026-desktop-engine-{}.configure",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&config_path);
        let configure = ConfigureStore::with_path(&config_path);
        configure.set("BASICENGINE", "STRICTJIT").unwrap();
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
            "a desktop BASIC task should select StrictJIT from the session preferences"
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
}
