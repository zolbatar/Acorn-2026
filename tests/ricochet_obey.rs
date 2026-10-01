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
    swi::{DisplayEvent, OS_CLI, SwiContext, SwiDispatcher},
};

const CLI_ADDRESS: u32 = 0x2100;
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

struct Environment {
    root: PathBuf,
    old_volume: Option<std::ffi::OsString>,
    old_config: Option<std::ffi::OsString>,
    old_boot_capsule: Option<std::ffi::OsString>,
}

impl Environment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ricochet-obey-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        let old_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        let old_config = std::env::var_os("RICOCHET_CONFIG_PATH");
        let old_boot_capsule = std::env::var_os("RICOCHET_BOOT_CAPSULE");
        // Each integration test owns a unique fixture volume and config file.
        unsafe {
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
            std::env::set_var("RICOCHET_CONFIG_PATH", root.join("configure"));
            std::env::remove_var("RICOCHET_BOOT_CAPSULE");
        }
        Self {
            root,
            old_volume,
            old_config,
            old_boot_capsule,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn write_script(&self, name: &str, contents: &[u8]) {
        fs::write(self.path(name), contents).unwrap();
    }

    fn guest_path(&self, name: &str) -> String {
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
            if let Some(value) = &self.old_boot_capsule {
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
    receiver: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> (Result<(), RuntimeError>, String) {
    let _ = receiver.try_iter().count();
    let scratch_before = task.memory.dynamic_area_count();
    task.memory
        .write_bytes(CLI_ADDRESS, command.as_bytes())
        .unwrap();
    task.memory
        .write_byte(CLI_ADDRESS + command.len() as u32, 0)
        .unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    if result.is_ok() {
        assert_eq!(context.registers[0], CLI_ADDRESS, "OS_CLI preserves R0");
    }
    assert_eq!(
        task.memory.dynamic_area_count(),
        scratch_before,
        "command scratch released after {command:?}"
    );
    let output = receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    (result, String::from_utf8_lossy(&output).into_owned())
}

fn obey(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    path: &str,
) -> (Result<(), RuntimeError>, String) {
    cli(dispatcher, task, receiver, &format!("*OBEY {path}"))
}

fn show(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    name: &str,
) -> String {
    let (result, output) = cli(dispatcher, task, receiver, &format!("*SHOW {name}"));
    result.expect("SHOW succeeds");
    output
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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .expect("write bounded commands");
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
fn obey_runs_guest_lines_in_order_with_blank_comments_all_newlines_and_eof() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0BE1);

    environment.write_script(
        "mixed-lines",
        b"*SET OBEY_ORDER first\n \t| comment\r\n\r*SET OBEY_ORDER second\r*SET OBEY_TAIL no-newline",
    );
    let (result, output) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("mixed-lines"),
    );
    result.expect("mixed line endings, blank/comment lines, and EOF are accepted");
    let final_order = show(&mut dispatcher, &mut task, &display, "OBEY_ORDER");
    assert!(
        final_order.contains("second"),
        "script output={output:?}; final value={final_order:?}"
    );
    assert!(show(&mut dispatcher, &mut task, &display, "OBEY_TAIL").contains("no-newline"));

    environment.write_script("nested-inner", b"*SET OBEY_NESTED inner");
    environment.write_script(
        "nested-outer",
        format!(
            "*SET OBEY_NESTED before\n*OBEY {}\n*SET OBEY_NESTED after",
            environment.guest_path("nested-inner")
        )
        .as_bytes(),
    );
    let (nested_result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("nested-outer"),
    );
    nested_result.expect("outer script resumes after a successful nested script");
    assert!(show(&mut dispatcher, &mut task, &display, "OBEY_NESTED").contains("after"));

    environment.write_script("quoted script", b"*SET OBEY_QUOTED yes");
    let quoted = format!("\"{}\"", environment.guest_path("quoted script"));
    let (result, _) = obey(&mut dispatcher, &mut task, &display, &quoted);
    result.expect("one quoted guest pathname is accepted");
    assert!(show(&mut dispatcher, &mut task, &display, "OBEY_QUOTED").contains("yes"));
}

#[test]
fn nested_obey_errors_keep_guest_provenance_stop_later_lines_and_recover() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0BE2);

    environment.write_script(
        "inner",
        b"*SET NESTED_BEFORE kept\n*NO_SUCH_COMMAND\n*SET NESTED_AFTER wrong",
    );
    environment.write_script(
        "outer",
        format!(
            "*SET OUTER_BEFORE kept\n*OBEY {}\n*SET OUTER_AFTER wrong",
            environment.guest_path("inner")
        )
        .as_bytes(),
    );
    let (result, output) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("outer"),
    );
    let error = match result {
        Err(error) => error,
        Ok(()) => panic!("unknown nested command was ignored; output={output:?}"),
    };
    let message = error.to_string();
    assert!(
        message.contains("inner"),
        "missing nested guest path: {message}"
    );
    assert!(
        message.contains("2"),
        "missing nested source line: {message}"
    );
    assert!(
        !message.contains(environment.root.to_string_lossy().as_ref()),
        "host path leaked: {message}"
    );
    assert!(show(&mut dispatcher, &mut task, &display, "OUTER_BEFORE").contains("kept"));
    assert!(show(&mut dispatcher, &mut task, &display, "NESTED_BEFORE").contains("kept"));
    assert!(!show(&mut dispatcher, &mut task, &display, "NESTED_AFTER").contains("wrong"));
    assert!(!show(&mut dispatcher, &mut task, &display, "OUTER_AFTER").contains("wrong"));

    let dynamic_before = task.memory.dynamic_area_count();
    let (healthy, _) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET OBEY_RECOVERED yes",
    );
    healthy.expect("a later command succeeds after nested failure");
    assert_eq!(task.memory.dynamic_area_count(), dynamic_before);
    assert!(show(&mut dispatcher, &mut task, &display, "OBEY_RECOVERED").contains("yes"));
}

#[test]
fn obey_is_caller_scoped_and_prior_effects_survive_a_denied_nested_write() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut ordinary = Task::new(0x0BE3);
    environment.write_script("denied-inner", b"*SET OBEY_PRIVATE denied");
    environment.write_script(
        "denied",
        format!(
            "*OBEY {}\n*SET OBEY_LATER wrong",
            environment.guest_path("denied-inner")
        )
        .as_bytes(),
    );

    let (direct_result, _) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display,
        "*SET OBEY_DIRECT denied",
    );
    assert!(direct_result.is_err(), "ordinary direct write is denied");

    let (result, _) = obey(
        &mut dispatcher,
        &mut ordinary,
        &display,
        &environment.guest_path("denied"),
    );
    let nested_denial = result.expect_err("ordinary nested script cannot obtain write authority");
    assert!(nested_denial.to_string().contains("denied-inner:1:"));
    assert!(!show(&mut dispatcher, &mut ordinary, &display, "OBEY_DIRECT").contains("denied"));
    assert!(!show(&mut dispatcher, &mut ordinary, &display, "OBEY_PRIVATE").contains("denied"));
    assert!(!show(&mut dispatcher, &mut ordinary, &display, "OBEY_LATER").contains("wrong"));

    let mut trusted = Task::trusted_mos_session(0x0BE4);
    let (trusted_result, _) = obey(
        &mut dispatcher,
        &mut trusted,
        &display,
        &environment.guest_path("denied"),
    );
    trusted_result.expect("the explicit trusted session can write variables");
    assert!(show(&mut dispatcher, &mut trusted, &display, "OBEY_PRIVATE").contains("denied"));
}

#[test]
fn obey_rejects_missing_bad_and_oversized_inputs_without_truncating_or_leaking_paths() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0BE5);

    for operand in [
        "",
        "missing",
        "one extra-argument",
        "-v supported-looking-option",
        "HostFS::DemoDisk.$.../outside",
    ] {
        let (result, _) = obey(&mut dispatcher, &mut task, &display, operand);
        assert!(
            result.is_err(),
            "unsupported/missing operand accepted: {operand:?}"
        );
    }

    environment.write_script("oversized", &vec![b' '; 70 * 1024]);
    let (large_result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("oversized"),
    );
    assert!(
        large_result.is_err(),
        "oversized source must fail as a whole"
    );

    environment.write_script(
        "long-line",
        format!("*SET OBEY_LONG {}", "x".repeat(256)).as_bytes(),
    );
    let (line_result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("long-line"),
    );
    assert!(
        line_result.is_err(),
        "overlong line must fail, not truncate"
    );

    environment.write_script("too-many-lines", &vec![b'\n'; 4097]);
    let (work_result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("too-many-lines"),
    );
    let work_error = work_result.expect_err("physical line work is capped");
    assert!(work_error.to_string().contains(":4097:"));
    assert!(work_error.to_string().contains("4,096-line hosted limit"));

    let external = environment.root.with_extension("outside");
    fs::write(&external, b"*SET OBEY_ESCAPE bad").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&external, environment.path("outside-link")).unwrap();
    #[cfg(unix)]
    {
        let (link_result, _) = obey(
            &mut dispatcher,
            &mut task,
            &display,
            &environment.guest_path("outside-link"),
        );
        let message = link_result.unwrap_err().to_string();
        assert!(
            !message.contains(external.to_string_lossy().as_ref()),
            "host path leaked: {message}"
        );
        assert!(!show(&mut dispatcher, &mut task, &display, "OBEY_ESCAPE").contains("bad"));
    }

    environment.write_script("healthy", b"*SET OBEY_AFTER_LIMIT yes");
    let (healthy, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("healthy"),
    );
    healthy.expect("failed input leaves dispatcher usable");
    assert!(show(&mut dispatcher, &mut task, &display, "OBEY_AFTER_LIMIT").contains("yes"));
    assert!(
        task.file_system.open_files.is_empty(),
        "script file handles leaked"
    );
}

#[test]
fn obey_nesting_depth_is_bounded_and_reports_the_deep_guest_source() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0BE6);

    // Eight active frames are allowed by the hosted ceiling; the next nested
    // invocation fails with source provenance and leaves the caller usable.
    for index in 0..9 {
        let contents = if index == 8 {
            b"*SET OBEY_DEPTH_REACHED yes".to_vec()
        } else {
            format!(
                "*OBEY {}",
                environment.guest_path(&format!("depth-{:02}", index + 1))
            )
            .into_bytes()
        };
        environment.write_script(&format!("depth-{index:02}"), &contents);
    }
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("depth-00"),
    );
    let error = result.expect_err("the ninth active script frame is rejected");
    let message = error.to_string();
    assert!(
        message.contains("depth-07"),
        "deep source missing from error: {message}"
    );
    assert!(
        message.contains("limit of 8"),
        "depth cause missing: {message}"
    );
    assert!(!show(&mut dispatcher, &mut task, &display, "OBEY_DEPTH_REACHED").contains("yes"));
    assert!(
        task.file_system.open_files.is_empty(),
        "nested script handles leaked"
    );

    let (healthy, _) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET OBEY_DEPTH_RECOVERED yes",
    );
    healthy.expect("depth failure restores command context");
    assert!(show(&mut dispatcher, &mut task, &display, "OBEY_DEPTH_RECOVERED").contains("yes"));
}

#[test]
fn obey_quit_unwinds_script_and_stdio_preserves_commands_already_queued() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    environment.write_script(
        "quit-script",
        b"*SET OBEY_BEFORE_QUIT yes\n*QUIT\n*SET OBEY_AFTER_QUIT wrong\n",
    );
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0BE7);
    let (quit_result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("quit-script"),
    );
    quit_result.expect("QUIT ends the active script without an error");
    assert!(show(&mut dispatcher, &mut task, &display, "OBEY_BEFORE_QUIT").contains("yes"));
    assert!(!show(&mut dispatcher, &mut task, &display, "OBEY_AFTER_QUIT").contains("wrong"));

    // The public stdio sequence tests queued input with a returning script;
    // QUIT in that input belongs to the shell after the script completes.
    environment.write_script("stdio-script", b"*SET OBEY_STDIO_SCRIPT ran");
    let input = format!(
        "*OBEY {}\r*SET OBEY_QUEUED from-stdin\r*SHOW OBEY_QUEUED\r*QUIT\r",
        environment.guest_path("stdio-script")
    );
    let output = run_stdio(
        &environment.path("stdio-configure"),
        &environment.root,
        input.as_bytes(),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("OBEY_STDIO_SCRIPT set."),
        "script did not run: {stdout}"
    );
    assert!(
        stdout.contains("from-stdin"),
        "queued input lost or consumed early: {stdout}"
    );
    assert!(
        !stdout.contains("OBEY_AFTER_QUIT"),
        "QUIT did not unwind script: {stdout}"
    );
}
