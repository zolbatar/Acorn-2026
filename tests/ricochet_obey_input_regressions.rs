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
    old_capsule: Option<std::ffi::OsString>,
}

impl Environment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ricochet-obey-input-{nonce}"));
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

    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn script(&self, name: &str, bytes: &[u8]) {
        fs::write(self.file(name), bytes).unwrap();
    }

    fn guest(&self, name: &str) -> String {
        format!("HostFS::DemoDisk.$.{name}")
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        restore_environment("RICOCHET_DEMO_VOLUME", &self.old_volume);
        restore_environment("RICOCHET_CONFIG_PATH", &self.old_config);
        restore_environment("RICOCHET_BOOT_CAPSULE", &self.old_capsule);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn restore_environment(name: &str, old: &Option<std::ffi::OsString>) {
    unsafe {
        if let Some(value) = old {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
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
) -> (Result<(), RuntimeError>, String) {
    let _ = display.try_iter().count();
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
    let output = display
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
    display: &mpsc::Receiver<DisplayEvent>,
    guest_path: &str,
) -> (Result<(), RuntimeError>, String) {
    cli(dispatcher, task, display, &format!("*OBEY {guest_path}"))
}

fn show(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    display: &mpsc::Receiver<DisplayEvent>,
    key: &str,
) -> String {
    let (result, output) = cli(dispatcher, task, display, &format!("*SHOW {key}"));
    result.expect("SHOW succeeds");
    output
}

fn assert_no_value(output: &str, key: &str, forbidden: &str) {
    assert!(
        !output.contains(&format!("{key} (String): {forbidden}")),
        "unexpected truncated or later value for {key}: {output:?}"
    );
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
        if child.try_wait().expect("poll stdio child").is_some() {
            break child.wait_with_output().expect("collect stdio output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("isolated stdio process exceeded its 15-second deadline");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn obey_normalizes_star_and_whitespace_comments_without_eating_pipe_values() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0BE8);

    for command in [
        "*| direct comment",
        "**\t| multiple stars with a tab",
        "  ***   | stars and spaces",
    ] {
        let (result, output) = cli(&mut dispatcher, &mut task, &display, command);
        result.expect("direct CLI comment is a no-op");
        assert!(
            !output.contains("Bad command"),
            "comment fell through: {output:?}"
        );
    }

    environment.script(
        "comments",
        b"| plain comment\n  | space comment\n\t| tab comment\n*| star comment\n**| multiple stars\n* \t| stars and whitespace\n\n*\n***\nSET\tEdgeTab\ttab-value\nSET EdgeComment reached\nSET EdgePipe left||right\n",
    );
    let (result, output) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest("comments"),
    );
    result.expect("each normalized comment and blank line is skipped");
    assert!(
        !output.contains("Bad command"),
        "comment dispatched as a command: {output:?}"
    );

    let comment = show(&mut dispatcher, &mut task, &display, "EdgeComment");
    assert!(
        comment.contains("EdgeComment (String): reached"),
        "{comment:?}"
    );
    let tab_value = show(&mut dispatcher, &mut task, &display, "EdgeTab");
    assert!(
        tab_value.contains("EdgeTab (String): tab-value"),
        "{tab_value:?}"
    );
    let pipe_value = show(&mut dispatcher, &mut task, &display, "EdgePipe");
    assert!(
        pipe_value.contains("EdgePipe (String): left|right"),
        "{pipe_value:?}"
    );
}

#[test]
fn embedded_nul_anywhere_in_a_source_is_rejected_before_its_first_effect() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0BE9);

    let cases: &[(&str, &[u8], &str, u32, Option<&str>)] = &[
        (
            "nul-start",
            b"\0SET EdgeNulStart before\nSET EdgeAfterStart wrong\n",
            "EdgeNulStart",
            1,
            None,
        ),
        (
            "nul-middle",
            b"SET EdgeNulMiddle before\0discarded\nSET EdgeAfterMiddle wrong\n",
            "EdgeNulMiddle",
            1,
            None,
        ),
        (
            "nul-end",
            b"SET EdgeNulEnd before\0\nSET EdgeAfterEnd wrong\n",
            "EdgeNulEnd",
            1,
            None,
        ),
        (
            "nul-second-line",
            b"SET EdgeNulFirstLine forbidden\nSET EdgeNulSecond before\0discarded\nSET EdgeAfterSecond wrong\n",
            "EdgeNulSecond",
            2,
            Some("EdgeNulFirstLine"),
        ),
        (
            "nul-comment",
            b"*| apparent comment\0discarded\nSET EdgeAfterComment wrong\n",
            "EdgeAfterComment",
            1,
            None,
        ),
        (
            "control-byte",
            b"SET EdgeControlByte before\x07discarded\nSET EdgeAfterControl wrong\n",
            "EdgeControlByte",
            1,
            None,
        ),
    ];

    for (file, bytes, target_key, error_line, prior_key) in cases {
        environment.script(file, bytes);
        let guest_path = environment.guest(file);
        let scratch_before = task.memory.dynamic_area_count();
        let (result, output) = obey(&mut dispatcher, &mut task, &display, &guest_path);
        let error = result.expect_err("embedded NUL is rejected before OS_CLI sees a prefix");
        let message = error.to_string();
        assert!(
            message.contains(&format!("{guest_path}:{error_line}:")),
            "missing guest path/line provenance: {message}"
        );
        assert!(
            !message.contains(environment.root.to_string_lossy().as_ref()),
            "host path leaked: {message}"
        );
        assert!(
            !output.contains("Bad command"),
            "malformed shortened command reached dispatch: {output:?}"
        );
        assert_eq!(task.memory.dynamic_area_count(), scratch_before);
        assert!(
            task.file_system.open_files.is_empty(),
            "guest file handle leaked"
        );
        let shown = show(&mut dispatcher, &mut task, &display, target_key);
        assert_no_value(&shown, target_key, "before");
        assert_no_value(&shown, target_key, "shortened");
        assert_no_value(&shown, target_key, "forbidden");
        assert_no_value(&shown, target_key, "wrong");
        let later_key = match *file {
            "nul-start" => Some("EdgeAfterStart"),
            "nul-middle" => Some("EdgeAfterMiddle"),
            "nul-end" => Some("EdgeAfterEnd"),
            "nul-second-line" => Some("EdgeAfterSecond"),
            "control-byte" => Some("EdgeAfterControl"),
            _ => None,
        };
        if let Some(later_key) = later_key {
            assert_no_value(
                &show(&mut dispatcher, &mut task, &display, later_key),
                later_key,
                "wrong",
            );
        }
        if let Some(prior_key) = prior_key {
            assert_no_value(
                &show(&mut dispatcher, &mut task, &display, prior_key),
                prior_key,
                "forbidden",
            );
        }
    }

    let healthy = cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET EdgeHealthy after-error",
    );
    healthy
        .0
        .expect("dispatcher accepts a healthy command after NUL errors");
    assert!(
        show(&mut dispatcher, &mut task, &display, "EdgeHealthy")
            .contains("EdgeHealthy (String): after-error")
    );
}

#[test]
fn malformed_nested_source_keeps_parent_effects_and_stops_outer_lines() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0BEA);
    environment.script("nested-nul", b"SET EdgeNestedNul before\0discarded\n");
    environment.script(
        "outer-nul",
        format!(
            "SET EdgeParentBefore retained\n*OBEY {}\nSET EdgeParentAfter wrong\n",
            environment.guest("nested-nul")
        )
        .as_bytes(),
    );

    let guest_path = environment.guest("outer-nul");
    let (result, output) = obey(&mut dispatcher, &mut task, &display, &guest_path);
    let error = result.expect_err("malformed nested source unwinds both active scripts");
    let message = error.to_string();
    assert!(
        message.contains(&format!("{}:1:", environment.guest("nested-nul"))),
        "nested source/line provenance missing: {message}"
    );
    assert!(!message.contains(environment.root.to_string_lossy().as_ref()));
    assert!(
        !output.contains("Bad command"),
        "NUL prefix was dispatched: {output:?}"
    );
    assert!(
        show(&mut dispatcher, &mut task, &display, "EdgeParentBefore")
            .contains("EdgeParentBefore (String): retained")
    );
    assert_no_value(
        &show(&mut dispatcher, &mut task, &display, "EdgeNestedNul"),
        "EdgeNestedNul",
        "before",
    );
    assert_no_value(
        &show(&mut dispatcher, &mut task, &display, "EdgeParentAfter"),
        "EdgeParentAfter",
        "wrong",
    );
    assert!(
        task.file_system.open_files.is_empty(),
        "nested guest handle leaked"
    );
    assert_eq!(task.memory.dynamic_area_count(), 0);

    let (healthy, _) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET EdgeNestedRecovered yes",
    );
    healthy.expect("dispatcher context is usable after nested preflight failure");
    assert!(
        show(&mut dispatcher, &mut task, &display, "EdgeNestedRecovered")
            .contains("EdgeNestedRecovered (String): yes")
    );
}

#[test]
fn stdio_runs_normalized_comments_and_recovers_after_preflight_nul_error() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    environment.script(
        "stdio-comments",
        b"*| stdout comment\nSET EdgeStdioComment reached\nSET EdgeStdioPipe left||right\n",
    );
    environment.script(
        "stdio-nul",
        b"SET EdgeStdioNul before\0discarded\nSET EdgeStdioLater wrong\n",
    );

    let input = format!(
        "*OBEY {}\r*SHOW EdgeStdioComment\r*SHOW EdgeStdioPipe\r*OBEY {}\r*SET EdgeStdioRecovered queued\r*SHOW EdgeStdioRecovered\r*QUIT\r",
        environment.guest("stdio-comments"),
        environment.guest("stdio-nul"),
    );
    let output = run_stdio(
        &environment.file("stdio-configure"),
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
        stdout.contains("EdgeStdioComment (String): reached"),
        "{stdout}"
    );
    assert!(
        stdout.contains("EdgeStdioPipe (String): left|right"),
        "{stdout}"
    );
    assert!(
        stdout.contains(&format!("{}:1:", environment.guest("stdio-nul"))),
        "{stdout}"
    );
    assert!(
        !stdout.contains("EdgeStdioNul (String): before"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("EdgeStdioLater (String): wrong"),
        "{stdout}"
    );
    assert!(
        stdout.contains("EdgeStdioRecovered (String): queued"),
        "queued input lost: {stdout}"
    );
    assert!(
        !stdout.contains("Bad command"),
        "comment/NUL fell through to bad-command handling: {stdout}"
    );
}
