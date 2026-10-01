use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ricochet::{
    basic_compat,
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, OS_CLI, OS_READ_C, OS_READ_LINE, SwiContext, SwiDispatcher},
};

const CLI_ADDRESS: u32 = 0x2100;
const INPUT_ADDRESS: u32 = 0x2400;
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

struct Environment {
    root: PathBuf,
    old_volume: Option<std::ffi::OsString>,
    old_config: Option<std::ffi::OsString>,
    old_capsule: Option<std::ffi::OsString>,
}

impl Environment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ricochet-exec-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        let old_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        let old_config = std::env::var_os("RICOCHET_CONFIG_PATH");
        let old_capsule = std::env::var_os("RICOCHET_BOOT_CAPSULE");
        unsafe {
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
            std::env::set_var("RICOCHET_CONFIG_PATH", root.join("configure"));
            std::env::remove_var("RICOCHET_BOOT_CAPSULE");
        }
        Self {
            root,
            old_volume,
            old_config,
            old_capsule,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn file(&self, name: &str, bytes: &[u8]) {
        fs::write(self.path(name), bytes).unwrap();
    }

    fn guest(&self, name: &str) -> String {
        format!("HostFS::DemoDisk.$.{name}")
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.old_volume {
                std::env::set_var("RICOCHET_DEMO_VOLUME", value);
            } else {
                std::env::remove_var("RICOCHET_DEMO_VOLUME");
            }
            if let Some(value) = &self.old_config {
                std::env::set_var("RICOCHET_CONFIG_PATH", value);
            } else {
                std::env::remove_var("RICOCHET_CONFIG_PATH");
            }
            if let Some(value) = &self.old_capsule {
                std::env::set_var("RICOCHET_BOOT_CAPSULE", value);
            } else {
                std::env::remove_var("RICOCHET_BOOT_CAPSULE");
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_tx, input_rx) = mpsc::channel();
    let (display_tx, display_rx) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_rx), display_tx),
        display_rx,
    )
}

fn cli(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    display: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> Result<(), RuntimeError> {
    task.memory
        .write_bytes(CLI_ADDRESS, command.as_bytes())
        .unwrap();
    task.memory
        .write_byte(CLI_ADDRESS + command.len() as u32, 0)
        .unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    let _ = display.try_iter().count();
    result
}

fn read_byte(dispatcher: &mut SwiDispatcher, task: &mut Task) -> Result<u8, RuntimeError> {
    let mut context = SwiContext::default();
    dispatcher.dispatch(OS_READ_C, task, &mut context)?;
    Ok(context.registers[0] as u8)
}

fn read_line(dispatcher: &mut SwiDispatcher, task: &mut Task) -> Result<Vec<u8>, RuntimeError> {
    let mut context = SwiContext::default();
    context.registers[0] = INPUT_ADDRESS;
    context.registers[1] = 255;
    context.registers[2] = 0;
    context.registers[3] = 255;
    dispatcher.dispatch(OS_READ_LINE, task, &mut context)?;
    task.memory
        .read_bytes(INPUT_ADDRESS, context.registers[1] as usize)
        .map_err(Into::into)
}

fn run_stdio(config: &Path, volume: &Path, input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ricochet"))
        .arg("--stdio")
        .env("RICOCHET_CONFIG_PATH", config)
        .env("RICOCHET_DEMO_VOLUME", volume)
        .env_remove("RICOCHET_BOOT_CAPSULE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start isolated stdio runtime");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().expect("poll runtime child").is_some() {
            break child.wait_with_output().expect("collect runtime output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("isolated stdio runtime exceeded its 15-second test deadline");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn exec_installs_a_task_scoped_shared_mos_basic_input_stream() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0E01);
    let mut other_task = Task::new(0x0E02);

    // CRLF and LF normalize to one carriage return; lone CR remains a line
    // break. The final line has no terminator and must still be delivered.
    environment.file("mixed-input", b"first\r\nsecond\nthird\rfinal");
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*EXEC {}", environment.guest("mixed-input")),
    )
    .expect("Exec installs the source and returns without reading it");

    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'f');
    assert_eq!(read_line(&mut dispatcher, &mut task).unwrap(), b"irst");
    assert_eq!(read_line(&mut dispatcher, &mut task).unwrap(), b"second");
    assert_eq!(read_line(&mut dispatcher, &mut task).unwrap(), b"third");
    assert_eq!(read_line(&mut dispatcher, &mut task).unwrap(), b"final");
    assert!(matches!(
        read_byte(&mut dispatcher, &mut task),
        Err(RuntimeError::EndOfInput)
    ));

    environment.file("utf8", "é☃".as_bytes());
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*EXEC {}", environment.guest("utf8")),
    )
    .unwrap();
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), 0xC3);
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), 0xA9);
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), 0xE2);
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), 0x98);
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), 0x83);
    assert!(matches!(
        read_byte(&mut dispatcher, &mut task),
        Err(RuntimeError::EndOfInput)
    ));

    environment.file("basic-data", b"value-for-input\r");
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*EXEC {}", environment.guest("basic-data")),
    )
    .unwrap();
    basic_compat::run_source("10 INPUT A$:PRINT A$:END", &mut task, &mut dispatcher)
        .expect("BASIC INPUT consumes from the same Exec stream");
    let output = display
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        String::from_utf8_lossy(&output).contains("value-for-input"),
        "BASIC INPUT output: {:?}",
        String::from_utf8_lossy(&output)
    );

    environment.file("owned", b"task-owned");
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*EXEC {}", environment.guest("owned")),
    )
    .unwrap();
    assert!(matches!(
        read_byte(&mut dispatcher, &mut other_task),
        Err(RuntimeError::EndOfInput)
    ));
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b't');

    environment.file("denied-command", b"*SET EXEC_UNAUTHORIZED yes\r");
    cli(
        &mut dispatcher,
        &mut other_task,
        &display,
        &format!("*EXEC {}", environment.guest("denied-command")),
    )
    .expect("ordinary caller may read a guest Exec file");
    let command = read_line(&mut dispatcher, &mut other_task).unwrap();
    let command = String::from_utf8(command).unwrap();
    assert!(cli(&mut dispatcher, &mut other_task, &display, &command).is_err());
}

#[test]
fn basic_inkey_polling_does_not_steal_queued_host_input_during_exec() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (input_tx, input_rx) = mpsc::channel();
    input_tx.send(b'K').unwrap();
    let (display_tx, display_rx) = mpsc::channel();
    let mut dispatcher = SwiDispatcher::windowed(HostConsole::windowed(input_rx), display_tx);
    let mut task = Task::trusted_mos_session(0x0E03);

    environment.file("poll-input", b"data\r");
    cli(
        &mut dispatcher,
        &mut task,
        &display_rx,
        &format!("*EXEC {}", environment.guest("poll-input")),
    )
    .unwrap();
    basic_compat::run_source(
        "10 K%=INKEY:IF K%=-256 THEN PRINT \"no-key\":END",
        &mut task,
        &mut dispatcher,
    )
    .expect("BASIC polling executes while Exec input is active");

    let output = display_rx
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        String::from_utf8_lossy(&output).contains("no-key"),
        "INKEY consumed the queued host key during Exec: {:?}",
        String::from_utf8_lossy(&output)
    );

    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'd');
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'a');
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b't');
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'a');
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'\r');
    assert_eq!(
        read_byte(&mut dispatcher, &mut task).unwrap(),
        b'K',
        "queued host input remains available after Exec EOF"
    );
}

#[test]
fn exec_stop_replacement_failed_preflight_and_bounds_are_atomic() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0E11);

    environment.file("old", b"abcdefghijklmnop");
    environment.file("new", b"replacement");
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*EXEC {}", environment.guest("old")),
    )
    .unwrap();
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'a');
    assert!(
        cli(
            &mut dispatcher,
            &mut task,
            &display,
            &format!("*EXEC {}", environment.guest("missing")),
        )
        .is_err()
    );
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'b');

    let host_path = environment.path("new").to_string_lossy().into_owned();
    let host_path_error = cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*EXEC {host_path}"),
    )
    .unwrap_err()
    .to_string();
    assert!(
        !host_path_error.contains(&host_path),
        "host path leaked: {host_path_error}"
    );
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'c');

    assert!(
        cli(
            &mut dispatcher,
            &mut task,
            &display,
            &format!("*EXEC {} unexpected", environment.guest("new")),
        )
        .is_err()
    );
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'd');

    assert!(
        cli(
            &mut dispatcher,
            &mut task,
            &display,
            &format!("*EXEC \"{}", environment.guest("new")),
        )
        .is_err()
    );
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'e');

    environment.file("nul", b"bad\0source");
    let nul_error = cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*EXEC {}", environment.guest("nul")),
    )
    .unwrap_err()
    .to_string();
    assert!(
        nul_error.contains(&environment.guest("nul")),
        "guest source path absent: {nul_error}"
    );
    assert!(
        nul_error.contains(":1:"),
        "physical line number absent: {nul_error}"
    );
    assert!(
        !nul_error.contains(environment.root.to_string_lossy().as_ref()),
        "host path leaked: {nul_error}"
    );
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'f');

    let mut oversized_source = Vec::new();
    for _ in 0..257 {
        oversized_source.extend([b'x'; 255]);
        oversized_source.push(b'\n');
    }
    let mut expected_old_byte = b'g';
    for (name, bytes) in [
        ("too-long-line", vec![b'x'; 256]),
        ("too-many-lines", vec![b'\n'; 4097]),
        ("too-large", oversized_source),
        ("bad-control", b"ok\x07bad".to_vec()),
        ("invalid-utf8", vec![0xC3, 0x28]),
    ] {
        environment.file(name, &bytes);
        let error = cli(
            &mut dispatcher,
            &mut task,
            &display,
            &format!("*EXEC {}", environment.guest(name)),
        );
        assert!(error.is_err(), "invalid source {name} was accepted");
        assert_eq!(
            read_byte(&mut dispatcher, &mut task).unwrap(),
            expected_old_byte
        );
        expected_old_byte += 1;
    }

    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*EXEC {}", environment.guest("new")),
    )
    .unwrap();
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'r');
    cli(&mut dispatcher, &mut task, &display, "*EXEC").expect("bare Exec stops the active source");
    assert!(matches!(
        read_byte(&mut dispatcher, &mut task),
        Err(RuntimeError::EndOfInput)
    ));
}

#[test]
fn exec_cli_consumes_file_lines_and_preserves_queued_stdio_after_eof() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    environment.file(
        "commands",
        b"*SET EXEC_STDIO old\r\n*EXEC replacement\n*SET EXEC_OLD_TAIL wrong\n",
    );
    environment.file(
        "replacement",
        b"*SET EXEC_STDIO from-replacement\n*SHOW EXEC_STDIO\n",
    );
    let input = format!(
        "*EXEC {}\n*SHOW EXEC_STDIO\n*QUIT\n",
        environment.guest("commands")
    );
    let output = run_stdio(
        &environment.path("configure-stdio"),
        &environment.root,
        input.as_bytes(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("from-replacement"),
        "stdio output: {stdout}"
    );
    assert!(
        stdout.matches("from-replacement").count() >= 2,
        "replacement source or queued command wasn't processed: {stdout}"
    );
    assert!(
        !stdout.contains("EXEC_OLD_TAIL"),
        "old source continued after replacement: {stdout}"
    );
}

#[test]
fn bare_exec_stops_the_source_and_returns_to_the_queued_stdio_stream() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    environment.file(
        "stop-source",
        b"*SET EXEC_STOP before-stop\n*EXEC\n*SET EXEC_STOP wrong-tail\n",
    );
    let input = format!(
        "*EXEC {}\n*SHOW EXEC_STOP\n*QUIT\n",
        environment.guest("stop-source")
    );
    let output = run_stdio(
        &environment.path("configure-stop"),
        &environment.root,
        input.as_bytes(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("before-stop"),
        "queued command wasn't read: {stdout}"
    );
    assert!(
        !stdout.contains("wrong-tail"),
        "source continued after bare Exec: {stdout}"
    );
}

#[test]
fn exec_command_errors_report_guest_source_line_without_host_paths() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    environment.file("bad-command", b"*NO_SUCH_EXEC_COMMAND\r\n");
    let input = format!("*EXEC {}\n*QUIT\n", environment.guest("bad-command"));
    let output = run_stdio(
        &environment.path("configure-provenance"),
        &environment.root,
        input.as_bytes(),
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("bad-command:1:"),
        "missing guest path/line provenance: {stdout}"
    );
    assert!(
        !stdout.contains(environment.root.to_string_lossy().as_ref()),
        "host path leaked in diagnostic: {stdout}"
    );
}
