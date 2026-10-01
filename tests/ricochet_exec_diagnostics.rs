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
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, OS_CLI, OS_READ_C, SwiContext, SwiDispatcher},
};

const CLI_ADDRESS: u32 = 0x2100;
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
        let root = std::env::temp_dir().join(format!("ricochet-exec-diagnostics-{nonce}"));
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

    fn write(&self, name: &str, bytes: &[u8]) {
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

fn cli(dispatcher: &mut SwiDispatcher, task: &mut Task, command: &str) -> Result<(), RuntimeError> {
    task.memory
        .write_bytes(CLI_ADDRESS, command.as_bytes())
        .unwrap();
    task.memory
        .write_byte(CLI_ADDRESS + command.len() as u32, 0)
        .unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    dispatcher.dispatch(OS_CLI, task, &mut context)
}

fn read_byte(dispatcher: &mut SwiDispatcher, task: &mut Task) -> Result<u8, RuntimeError> {
    let mut context = SwiContext::default();
    dispatcher.dispatch(OS_READ_C, task, &mut context)?;
    Ok(context.registers[0] as u8)
}

fn assert_preflight_error(
    environment: &Environment,
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    bytes: &[u8],
    line: u32,
    expected_class: &str,
) -> String {
    environment.write(name, bytes);
    let error = cli(
        dispatcher,
        task,
        &format!("*EXEC {}", environment.guest(name)),
    )
    .expect_err("malformed Exec source must fail before replacing the active source")
    .to_string();
    let location = format!("{}:{line}:", environment.guest(name));
    assert!(error.contains(expected_class), "wrong error class: {error}");
    assert!(error.contains(&location), "wrong source location: {error}");
    assert!(
        !error.contains(environment.root.to_string_lossy().as_ref()),
        "host path leaked: {error}"
    );
    error
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
fn control_and_nul_diagnostics_count_crlf_once_across_blank_mixed_and_final_lines() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, _display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0E41);

    let cases: &[(&str, &[u8], u32)] = &[
        ("bel-lf", b"ok\n\x07bad", 2),
        ("bel-cr", b"ok\r\x07bad", 2),
        ("bel-crlf", b"ok\r\n\x07bad", 2),
        ("bel-mixed-blank", b"ok\r\n\r\nsecond\n\x07", 4),
        ("nul-first", b"\0start", 1),
        ("nul-middle", b"one\r\nmid\0dle", 2),
        ("nul-final-unterminated", b"one\r\nlast\r\0", 3),
    ];
    for (name, bytes, line) in cases {
        assert_preflight_error(
            &environment,
            &mut dispatcher,
            &mut task,
            name,
            bytes,
            *line,
            "ExecSourceControlCharacter",
        );
    }
}

#[test]
fn invalid_utf8_and_line_ceiling_use_the_same_physical_line_counting() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, _display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0E42);

    assert_preflight_error(
        &environment,
        &mut dispatcher,
        &mut task,
        "utf8-mixed",
        b"first\r\n\rsecond\n\xC3\x28",
        4,
        "ExecSourceError",
    );

    let mut overlong = b"valid\r\n".to_vec();
    overlong.extend([b'x'; 256]);
    assert_preflight_error(
        &environment,
        &mut dispatcher,
        &mut task,
        "overlong-second-line",
        &overlong,
        2,
        "ExecLineLength",
    );

    // A 255-byte line is accepted even when its CRLF terminator is present.
    // The next candidate has the same line at physical line 2 and is rejected.
    let mut accepted = vec![b'x'; 255];
    accepted.extend_from_slice(b"\r\nshort\r\n");
    environment.write("valid-boundary", &accepted);
    cli(
        &mut dispatcher,
        &mut task,
        &format!("*EXEC {}", environment.guest("valid-boundary")),
    )
    .expect("255 content bytes are permitted before a CRLF terminator");
}

#[test]
fn crlf_line_work_limit_counts_physical_lines_not_separator_bytes() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, _display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0E44);

    environment.write("line-limit-valid", &b"\r\n".repeat(4096));
    cli(
        &mut dispatcher,
        &mut task,
        &format!("*EXEC {}", environment.guest("line-limit-valid")),
    )
    .expect("4,096 CRLF physical lines are within the hosted limit");

    assert_preflight_error(
        &environment,
        &mut dispatcher,
        &mut task,
        "line-limit-over",
        &b"\r\n".repeat(4097),
        4097,
        "ExecLineLimit",
    );
}

#[test]
fn malformed_replacement_preserves_prior_source_cursor_and_has_no_partial_effects() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, _display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0E43);

    environment.write("active", b"abcdef\r");
    cli(
        &mut dispatcher,
        &mut task,
        &format!("*EXEC {}", environment.guest("active")),
    )
    .unwrap();
    assert_eq!(read_byte(&mut dispatcher, &mut task).unwrap(), b'a');

    environment.write("malformed", b"*SET EXEC_PARTIAL wrong\r\n\x07");
    let error = assert_preflight_error(
        &environment,
        &mut dispatcher,
        &mut task,
        "malformed",
        b"*SET EXEC_PARTIAL wrong\r\n\x07",
        2,
        "ExecSourceControlCharacter",
    );
    assert!(
        error.contains("U+0007"),
        "control byte unidentified: {error}"
    );
    assert_eq!(
        read_byte(&mut dispatcher, &mut task).unwrap(),
        b'b',
        "failed candidate changed the active source cursor"
    );
}

#[test]
fn stdio_reports_physical_line_for_crlf_preflight_error_then_reads_queued_commands() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    environment.write(
        "bad-crlf",
        b"*SET EXEC_DIAG_FIRST first-value\r\n*SET EXEC_DIAG_SECOND second-value\x07\r\n",
    );
    let input = format!(
        "*EXEC {}\n*SET EXEC_DIAG_QUEUED recovered\n*SHOW EXEC_DIAG_QUEUED\n*QUIT\n",
        environment.guest("bad-crlf")
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
        stdout.contains("bad-crlf:2:"),
        "CRLF preflight error should identify physical line 2: {stdout}"
    );
    assert!(
        !stdout.contains(environment.root.to_string_lossy().as_ref()),
        "host path leaked: {stdout}"
    );
    assert!(
        stdout.contains("recovered"),
        "queued healthy command was lost after rejection: {stdout}"
    );
    assert!(
        !stdout.contains("first-value") && !stdout.contains("second-value"),
        "malformed candidate had partial effects: {stdout}"
    );
}
