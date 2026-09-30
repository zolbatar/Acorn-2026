use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use acorn_2026::{
    display::{DesktopResolution, DisplayColour, DisplaySettings},
    host::HostConsole,
    memory::Task,
    runtime::Runtime,
    swi::{DisplayEvent, SwiContext, SwiDispatcher},
    wimp::WimpServer,
};

const OS_CLI: u32 = 0x05;
const CLI_ADDRESS: u32 = 0x2100;

struct IsolatedEnvironment {
    root: PathBuf,
    config_path: PathBuf,
    old_config_path: Option<OsString>,
    old_demo_volume: Option<OsString>,
    old_boot_capsule: Option<OsString>,
}

impl IsolatedEnvironment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "acorn-trellis-config-recovery-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("configure");
        let old_config_path = std::env::var_os("ACORN_CONFIG_PATH");
        let old_demo_volume = std::env::var_os("ACORN_DEMO_VOLUME");
        let old_boot_capsule = std::env::var_os("ACORN_BOOT_CAPSULE");
        unsafe {
            std::env::set_var("ACORN_CONFIG_PATH", &config_path);
            std::env::set_var("ACORN_DEMO_VOLUME", &root);
            std::env::remove_var("ACORN_BOOT_CAPSULE");
        }
        Self {
            root,
            config_path,
            old_config_path,
            old_demo_volume,
            old_boot_capsule,
        }
    }

    fn select_config(&self, path: &Path) {
        unsafe { std::env::set_var("ACORN_CONFIG_PATH", path) };
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.old_config_path {
                std::env::set_var("ACORN_CONFIG_PATH", value);
            } else {
                std::env::remove_var("ACORN_CONFIG_PATH");
            }
            if let Some(value) = &self.old_demo_volume {
                std::env::set_var("ACORN_DEMO_VOLUME", value);
            } else {
                std::env::remove_var("ACORN_DEMO_VOLUME");
            }
            if let Some(value) = &self.old_boot_capsule {
                std::env::set_var("ACORN_BOOT_CAPSULE", value);
            } else {
                std::env::remove_var("ACORN_BOOT_CAPSULE");
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_stdio(root: &Path, config_path: &Path, input: &[u8], capsule: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_acorn-2026"));
    command
        .arg("--stdio")
        .env("ACORN_CONFIG_PATH", config_path)
        .env("ACORN_DEMO_VOLUME", root)
        .env_remove("ACORN_BOOT_CAPSULE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(capsule) = capsule {
        command.env("ACORN_BOOT_CAPSULE", capsule);
    }
    let mut child = command.spawn().expect("spawn the public stdio runtime");
    child
        .stdin
        .take()
        .expect("child stdin is piped")
        .write_all(input)
        .expect("write the bounded public command sequence");
    child.wait_with_output().expect("wait for public runtime")
}

fn process_text(output: &Output) -> String {
    let mut bytes = output.stdout.clone();
    bytes.extend_from_slice(&output.stderr);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn normalized(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_stdio_checked(root: &Path, config_path: &Path, input: &[u8]) -> String {
    let output = run_stdio(root, config_path, input, None);
    let text = process_text(&output);
    assert!(
        output.status.success(),
        "public stdio runtime failed: {text:?}"
    );
    text
}

fn assert_safe_defaults(text: &str) {
    let text = normalized(text);
    for expected in [
        "Language=0",
        "BASICMode=AUTO",
        "BASICProfile=AUTO",
        "BASICTarget=AUTO",
        "BASICEngine=INTERPRETER",
        "WimpMode=AUTO",
    ] {
        assert!(
            text.lines()
                .any(|line| line.strip_prefix('*').unwrap_or(line) == expected),
            "safe default {expected:?} missing from {text:?}"
        );
    }
}

fn assert_status_sequence(text: &str, expected: &[&str]) {
    let normalized_text = normalized(text);
    let lines = normalized_text
        .lines()
        .map(|line| line.strip_prefix('*').unwrap_or(line))
        .collect::<Vec<_>>();
    assert!(
        lines
            .windows(expected.len())
            .any(|window| window == expected),
        "expected contiguous STATUS rows {expected:?} in {lines:?}"
    );
}

fn assert_recovery(text: &str, category: &str, startup_warning: &str) {
    let text = normalized(text);
    assert!(
        text.lines().any(|line| line == startup_warning),
        "startup must show the cause-specific recovery warning before the prompt: {text:?}"
    );
    assert!(
        text.lines().any(|line| {
            line == "Use *STATUS to inspect effective settings; a valid *CONFIGURE write or *CONFIGURE DEFAULTS repairs the file, preserving any recoverable original bytes."
        }),
        "startup omitted the explicit recovery route: {text:?}"
    );
    assert_recovery_status(&text, category);
}

fn assert_recovery_status(text: &str, category: &str) {
    let text = normalized(text);
    let status_line = format!(
        "ConfigurationRecovery={category}; effective safe defaults are active; saved file unchanged."
    );
    assert!(
        text.lines().any(|line| line == status_line),
        "STATUS omitted the recovery category: {text:?}"
    );
    assert_safe_defaults(&text);
}

fn new_dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender),
        display_receiver,
    )
}

fn put_string(task: &mut Task, address: u32, value: &str) {
    task.memory.write_bytes(address, value.as_bytes()).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn cli(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> (Result<(), acorn_2026::error::RuntimeError>, String) {
    let _ = receiver.try_iter().count();
    put_string(task, CLI_ADDRESS, command);
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    assert_eq!(context.registers[0], CLI_ADDRESS, "OS_CLI changed R0");
    let bytes = receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    (result, String::from_utf8_lossy(&bytes).into_owned())
}

fn run_application(source: &str, wimp: Option<Arc<WimpServer>>) -> Result<String, String> {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    let mut runtime = match wimp {
        Some(wimp) => Runtime::windowed_with_desktop(input_receiver, display_sender, wimp),
        None => Runtime::windowed(input_receiver, display_sender),
    };
    runtime
        .run_application(source)
        .map_err(|error| error.to_string())?;
    let bytes = display_receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn run_windowed_to_desktop(input: &[u8]) -> (Result<(), String>, String, Arc<WimpServer>) {
    let (input_sender, input_receiver) = mpsc::channel();
    for byte in input {
        input_sender.send(*byte).unwrap();
    }
    drop(input_sender);
    let (display_sender, display_receiver) = mpsc::channel();
    let (updates, _update_receiver) = mpsc::channel();
    let wimp = WimpServer::new(updates);
    let runtime_wimp = Arc::clone(&wimp);
    let (finished_sender, finished_receiver) = mpsc::channel();
    let runtime_thread = thread::spawn(move || {
        let mut runtime =
            Runtime::windowed_with_desktop(input_receiver, display_sender, runtime_wimp);
        let _ = finished_sender.send(runtime.run().map_err(|error| error.to_string()));
    });

    let mut bytes = Vec::new();
    let mut saw_desktop = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut finished = None;
    while std::time::Instant::now() < deadline {
        match display_receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(DisplayEvent::WriteByte { byte, .. }) => bytes.push(byte),
            Ok(DisplayEvent::DesktopStarted) => {
                saw_desktop = true;
                break;
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(result) = finished_receiver.try_recv() {
                    finished = Some(result);
                    break;
                }
            }
        }
    }
    if saw_desktop {
        wimp.stop();
    }
    if finished.is_none() {
        finished = Some(
            finished_receiver
                .recv_timeout(Duration::from_secs(3))
                .expect("windowed startup must finish after desktop handoff or prompt input"),
        );
    }
    runtime_thread.join().unwrap();
    let result = finished.expect("runtime result recorded");
    assert!(saw_desktop, "startup did not request desktop: {result:?}");
    (result, String::from_utf8_lossy(&bytes).into_owned(), wimp)
}

fn recovery_copies(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let prefix = format!("{}.recovery-", path.file_name().unwrap().to_string_lossy());
    fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| {
            let path = entry.path();
            (path.clone(), fs::read(path).unwrap())
        })
        .collect()
}

fn directory_snapshot(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries = fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            let value = if file_type.is_file() {
                fs::read(entry.path()).unwrap()
            } else {
                Vec::new()
            };
            (entry.file_name().to_string_lossy().into_owned(), value)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries
}

fn assert_no_temporary_config_files(root: &Path) {
    for entry in fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(
            !name.contains(".tmp-"),
            "configuration failure left a temporary file: {name}"
        );
    }
}

#[test]
fn malformed_configuration_starts_safely_and_recovers_only_on_explicit_write() {
    let isolated = IsolatedEnvironment::new();

    // A missing file is the ordinary first-run path: typed defaults are used
    // without presenting a corruption warning or creating a config file.
    let missing = run_stdio_checked(&isolated.root, &isolated.config_path, b"*STATUS\rQUIT\r");
    assert_safe_defaults(&missing);
    assert!(!missing.contains("Configuration recovery"), "{missing:?}");
    assert!(!missing.contains("ConfigurationRecovery="), "{missing:?}");
    assert!(!isolated.config_path.exists());

    let defaults_v3 = b"# Acorn-2026 MOS configuration v3\nLanguage=0\nBASICMode=AUTO\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=INTERPRETER\nWimpMode=AUTO\n";
    let truncated = b"# Acorn-2026 MOS configuration v3\nLanguage=3\nBASICMode=BASIC64\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=STRICT\nWimpMode=X800 Y600 G256\nthis final row is truncated\n";
    let duplicate_headers =
        b"# Acorn-2026 MOS configuration v3\n# Acorn-2026 MOS configuration v1\nLanguage=3\n";
    let unsupported_version = b"# Acorn-2026 MOS configuration v99\nLanguage=3\n";
    let unsupported_schema = b"# Acorn-2026 MOS configuration v3\nLanguage=3\nBASICMode=AUTO\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=STRICT\nWimpMode=X800 Y600 G256\nUnknownSetting=surprise\n";
    let invalid_utf8 = [
        b"# Acorn-2026 MOS configuration v3\nLanguage=0\nBASICMode=AUTO\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=INTERPRETER\nWimpMode=AUTO\n".as_slice(),
        &[0xFF],
    ]
    .concat();

    for (name, bytes, code, warning) in [
        (
            "truncated",
            truncated.as_slice(),
            "MALFORMED_OR_TRUNCATED",
            "Configuration recovery: settings are malformed or truncated; safe defaults are active and the saved file is unchanged.",
        ),
        (
            "duplicate-headers",
            duplicate_headers.as_slice(),
            "MALFORMED_OR_TRUNCATED",
            "Configuration recovery: settings are malformed or truncated; safe defaults are active and the saved file is unchanged.",
        ),
        (
            "unsupported-version",
            unsupported_version.as_slice(),
            "UNSUPPORTED_VERSION",
            "Configuration recovery: the saved format version is unsupported; safe defaults are active and the file is unchanged.",
        ),
        (
            "unsupported-schema",
            unsupported_schema.as_slice(),
            "UNSUPPORTED_SCHEMA",
            "Configuration recovery: the saved settings schema is unsupported; safe defaults are active and the file is unchanged.",
        ),
        (
            "invalid-utf8",
            invalid_utf8.as_slice(),
            "INVALID_UTF8",
            "Configuration recovery: the saved file is not valid UTF-8; safe defaults are active and the file is unchanged.",
        ),
    ] {
        let path = isolated.root.join(format!("configure-{name}"));
        fs::write(&path, bytes).unwrap();
        let original = fs::read(&path).unwrap();
        let output = run_stdio_checked(&isolated.root, &path, b"*STATUS\rQUIT\r");
        assert_recovery(&output, code, warning);
        assert!(
            !output.contains(path.to_string_lossy().as_ref()),
            "guest diagnostics must not disclose the host config path: {output:?}"
        );
        assert_eq!(fs::read(&path).unwrap(), original, "{name} was rewritten");
    }

    // Oversized input is rejected before parsing; reads are bounded, while an
    // explicit authorized repair streams a byte-exact sibling recovery copy.
    let oversized_path = isolated.root.join("configure-oversized");
    let oversized = vec![b'Z'; 64 * 1024 + 1];
    fs::write(&oversized_path, &oversized).unwrap();
    let oversized_status = run_stdio_checked(&isolated.root, &oversized_path, b"*STATUS\rQUIT\r");
    assert_recovery(
        &oversized_status,
        "OVERSIZED",
        "Configuration recovery: the saved configuration exceeds the 64 KiB safety limit; safe defaults are active and the file is unchanged.",
    );
    assert_eq!(fs::read(&oversized_path).unwrap(), oversized);
    let oversized_repair = run_stdio_checked(
        &isolated.root,
        &oversized_path,
        b"*STATUS\r*CONFIGURE DEFAULTS\r*STATUS\rQUIT\r",
    );
    assert!(oversized_repair.contains("ConfigurationRecovery=OVERSIZED"));
    assert_eq!(
        oversized_repair.matches("ConfigurationRecovery=").count(),
        1,
        "oversized repair did not clear recovery state: {oversized_repair:?}"
    );
    assert_eq!(fs::read(&oversized_path).unwrap(), defaults_v3);
    let oversized_backups = recovery_copies(&oversized_path);
    assert_eq!(oversized_backups.len(), 1, "{oversized_backups:?}");
    assert_eq!(oversized_backups[0].1, oversized);

    // The read-only public Status call and an ordinary Task's denied reset
    // leave corrupt bytes in place; only the trusted MOS process can repair.
    let denied_path = isolated.root.join("configure-denied");
    fs::write(&denied_path, truncated).unwrap();
    isolated.select_config(&denied_path);
    let denied_original = fs::read(&denied_path).unwrap();
    let (mut dispatcher, receiver) = new_dispatcher();
    let mut ordinary = Task::new(0xC9_001);
    let (status_result, status_output) = cli(&mut dispatcher, &mut ordinary, &receiver, "*STATUS");
    status_result.expect("ordinary callers can inspect effective recovery settings");
    assert_recovery_status(&status_output, "MALFORMED_OR_TRUNCATED");
    let (denied_result, denied_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &receiver,
        "*CONFIGURE DEFAULTS",
    );
    denied_result.expect("CONFIGURE renders authorization denial as an OS_CLI diagnostic");
    assert!(
        normalized(&denied_output).contains("caller task lacks configuration-write authority"),
        "ordinary reset was not denied: {denied_output:?}"
    );
    assert_eq!(fs::read(&denied_path).unwrap(), denied_original);
    assert!(recovery_copies(&denied_path).is_empty());

    // A damaged row must be all-or-nothing for every consumer. In particular,
    // it contains Language=3 and STRICT/fixed display preferences, but Boot
    // must reach the MOS prompt, BASIC must use its default Interpreter, and
    // the later desktop handoff must apply default Auto/full-colour settings.
    let boot_path = isolated.root.join("configure-boot");
    fs::write(&boot_path, truncated).unwrap();
    isolated.select_config(&boot_path);
    let boot_original = fs::read(&boot_path).unwrap();
    let (boot_result, boot_output, wimp) = run_windowed_to_desktop(b"*STATUS\rDESKTOP\r");
    boot_result.expect("malformed config must not fail Boot.Start");
    let boot_output = normalized(&boot_output);
    assert!(
        boot_output
            .lines()
            .next()
            .unwrap_or_default()
            .starts_with("Configuration recovery:"),
        "recovery warning must precede the first MOS prompt: {boot_output:?}"
    );
    assert!(
        boot_output.contains("Language=0"),
        "partial Language=3 leaked instead of safe defaults: {boot_output:?}"
    );
    assert!(
        boot_output.contains("ConfigurationRecovery=MALFORMED_OR_TRUNCATED; effective safe defaults are active; saved file unchanged."),
        "Status did not disclose the latched recovery state: {boot_output:?}"
    );
    assert_eq!(
        wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::Window,
            colour: DisplayColour::Rgb888,
        },
        "malformed fixed-mode input leaked into the desktop handoff"
    );
    assert_eq!(fs::read(&boot_path).unwrap(), boot_original);

    let basic_path = isolated.root.join("configure-basic");
    fs::write(&basic_path, truncated).unwrap();
    isolated.select_config(&basic_path);
    let basic_output = run_application("10 DIM A\n20 END", None)
        .expect("a corrupt partial STRICT preference must resolve to the Interpreter default");
    assert!(basic_output.contains("Configuration recovery:"));
    assert_eq!(fs::read(&basic_path).unwrap(), truncated);

    // The saved file is repaired only after an explicit authorized setting
    // write. The damaged bytes are copied to a unique sibling before replace,
    // the same session clears its recovery marker, and the next runtime sees
    // canonical v3 without a warning.
    let setting_path = isolated.root.join("configure-setting-repair");
    let setting_original = b"# Acorn-2026 MOS configuration v3\nLanguage=3\nBASICMode=BASIC64\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=STRICT\nWimpMode=X800 Y600 G256\ntruncated-option\n";
    fs::write(&setting_path, setting_original).unwrap();
    let repair_output = run_stdio_checked(
        &isolated.root,
        &setting_path,
        b"*STATUS\r*CONFIGURE Language 3\r*STATUS\rQUIT\r",
    );
    assert!(repair_output.contains("ConfigurationRecovery=MALFORMED_OR_TRUNCATED"));
    assert!(
        repair_output.contains("Language set to 3."),
        "{repair_output:?}"
    );
    assert_status_sequence(
        &repair_output,
        &[
            "Language=3",
            "BASICMode=AUTO",
            "BASICProfile=AUTO",
            "BASICTarget=AUTO",
            "BASICEngine=INTERPRETER",
            "WimpMode=AUTO",
        ],
    );
    assert_eq!(
        repair_output.matches("ConfigurationRecovery=").count(),
        1,
        "successful save did not clear the current recovery state: {repair_output:?}"
    );
    let expected_language3 = b"# Acorn-2026 MOS configuration v3\nLanguage=3\nBASICMode=AUTO\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=INTERPRETER\nWimpMode=AUTO\n";
    assert_eq!(fs::read(&setting_path).unwrap(), expected_language3);
    let setting_backups = recovery_copies(&setting_path);
    assert_eq!(setting_backups.len(), 1, "{setting_backups:?}");
    assert_eq!(setting_backups[0].1, setting_original);
    let fresh_setting_status = run_stdio_checked(&isolated.root, &setting_path, b"*STATUS\rQUIT\r");
    assert!(!fresh_setting_status.contains("Configuration recovery"));
    assert!(!fresh_setting_status.contains("ConfigurationRecovery="));
    assert!(normalized(&fresh_setting_status).contains("Language=3"));

    // Explicit DEFAULTS follows the same recoverable-write path.
    let defaults_path = isolated.root.join("configure-defaults-repair");
    let defaults_original = b"# Acorn-2026 MOS configuration v99\nLanguage=3\n";
    fs::write(&defaults_path, defaults_original).unwrap();
    let reset_output = run_stdio_checked(
        &isolated.root,
        &defaults_path,
        b"*STATUS\r*CONFIGURE DEFAULTS\r*STATUS\rQUIT\r",
    );
    assert!(reset_output.contains("ConfigurationRecovery=UNSUPPORTED_VERSION"));
    assert!(reset_output.contains("Configuration preferences restored to defaults."));
    assert_eq!(
        reset_output.matches("ConfigurationRecovery=").count(),
        1,
        "DEFAULTS did not clear the latched recovery marker: {reset_output:?}"
    );
    assert_eq!(fs::read(&defaults_path).unwrap(), defaults_v3);
    let default_backups = recovery_copies(&defaults_path);
    assert_eq!(default_backups.len(), 1, "{default_backups:?}");
    assert_eq!(default_backups[0].1, defaults_original);
    let fresh_defaults_status =
        run_stdio_checked(&isolated.root, &defaults_path, b"*STATUS\rQUIT\r");
    assert!(!fresh_defaults_status.contains("Configuration recovery"));
    assert!(!fresh_defaults_status.contains("ConfigurationRecovery="));
    assert_safe_defaults(&fresh_defaults_status);

    // Non-parse I/O failures are distinguishable, and a failed repair leaves
    // both the inaccessible target and its containing directory unchanged.
    let directory_path = isolated.root.join("configure-directory");
    fs::create_dir_all(&directory_path).unwrap();
    let sentinel_path = directory_path.join("sentinel");
    fs::write(&sentinel_path, b"do not replace directory contents").unwrap();
    let directory_before = directory_snapshot(&directory_path);
    let directory_output = run_stdio_checked(
        &isolated.root,
        &directory_path,
        b"*STATUS\r*CONFIGURE DEFAULTS\r*STATUS\rQUIT\r",
    );
    assert_recovery(
        &directory_output,
        "UNREADABLE_STORAGE",
        "Configuration recovery: saved configuration storage is unreadable; safe defaults are active and no file was changed.",
    );
    assert!(
        directory_output.contains("CONFIGURE error:"),
        "failed repair was not reported: {directory_output:?}"
    );
    assert_eq!(directory_snapshot(&directory_path), directory_before);
    assert!(recovery_copies(&directory_path).is_empty());

    // A regular file in place of the parent directory exercises a distinct
    // path-creation failure. No config, temporary, backup, or partial sibling
    // may appear, and the blocker bytes remain untouched.
    let blocker = isolated.root.join("not-a-directory");
    fs::write(&blocker, b"parent path blocker").unwrap();
    let blocked_path = blocker.join("configure");
    let blocker_before = fs::read(&blocker).unwrap();
    let blocked_output = run_stdio_checked(
        &isolated.root,
        &blocked_path,
        b"*STATUS\r*CONFIGURE DEFAULTS\r*STATUS\rQUIT\r",
    );
    assert_recovery(
        &blocked_output,
        "UNREADABLE_STORAGE",
        "Configuration recovery: saved configuration storage is unreadable; safe defaults are active and no file was changed.",
    );
    assert!(blocked_output.contains("CONFIGURE error:"));
    assert!(!blocked_path.exists());
    assert_eq!(fs::read(&blocker).unwrap(), blocker_before);
    assert_no_temporary_config_files(&isolated.root);

    // Even the separate public display writer must roll back when a damaged
    // store cannot be preserved/replaced: it may not publish a live Wimp
    // change before its configuration save succeeds.
    let (updates, _update_receiver) = mpsc::channel();
    let wimp = WimpServer::new(updates);
    let before_display = wimp.display_settings();
    isolated.select_config(&directory_path);
    let display_source = format!(
        "REM @BASIC64 MODE=BASIC64\nSYS \"ACORN_DISPLAY\", 1, 1, {}, {} TO ABI%, ACTION%, RESOLUTION%, COLOUR%, UNUSED4%, UNUSED5%, UNUSED6%, UNUSED7%, SAVE_STATUS%\nPRINT SAVE_STATUS%;\",\";RESOLUTION%;\",\";COLOUR%\n",
        DesktopResolution::R800x600.id(),
        DisplayColour::Grey16.id()
    );
    let display_output = run_application(&display_source, Some(Arc::clone(&wimp)))
        .expect("failed display save is reported through ACORN_DISPLAY status registers");
    assert!(
        normalized(&display_output).ends_with("1,0,7"),
        "failed ACORN_DISPLAY save should report refusal and prior Window/Rgb888 state: {display_output:?}"
    );
    assert_eq!(wimp.display_settings(), before_display);
    assert_eq!(directory_snapshot(&directory_path), directory_before);

    // Normal v3, v2-migrating and legacy v1 configurations remain readable
    // without recovery or eager rewrite. The v2 retired profile is ignored in
    // favour of its explicit WimpMode; v1 is the old display pair.
    let valid_v3_path = isolated.root.join("valid-v3");
    let valid_v3 = defaults_v3.to_vec();
    fs::write(&valid_v3_path, &valid_v3).unwrap();
    let v3_output = run_stdio_checked(&isolated.root, &valid_v3_path, b"*STATUS\rQUIT\r");
    assert_safe_defaults(&v3_output);
    assert!(!v3_output.contains("Configuration recovery"));
    assert_eq!(fs::read(&valid_v3_path).unwrap(), valid_v3);

    let valid_v2_path = isolated.root.join("valid-v2");
    let valid_v2 = b"# Acorn-2026 MOS configuration v2\nLanguage=0\nWimpMode=X800 Y600 G16\nTrellisOutputProfile=16MRGB888\n";
    fs::write(&valid_v2_path, valid_v2).unwrap();
    let v2_output = run_stdio_checked(&isolated.root, &valid_v2_path, b"*STATUS\rQUIT\r");
    assert!(normalized(&v2_output).contains("WimpMode=X800 Y600 G16"));
    assert!(!v2_output.contains("Configuration recovery"));
    assert_eq!(fs::read(&valid_v2_path).unwrap(), valid_v2);

    let valid_v1_path = isolated.root.join("valid-v1");
    let valid_v1 = b"# Acorn-2026 MOS configuration v1\nLanguage=3\nDisplayResolution=640x480\nDisplayColour=BW\nWindowFurniture=Bevelled\n";
    fs::write(&valid_v1_path, valid_v1).unwrap();
    let v1_status = run_stdio_checked(&isolated.root, &valid_v1_path, b"*STATUS\rQUIT\r");
    assert!(normalized(&v1_status).contains("WimpMode=X640 Y480 C2"));
    assert!(!v1_status.contains("Configuration recovery"));
    assert_eq!(fs::read(&valid_v1_path).unwrap(), valid_v1);
    isolated.select_config(&valid_v1_path);
    let (v1_boot_result, v1_boot_output, v1_wimp) = run_windowed_to_desktop(b"");
    v1_boot_result.expect("valid legacy Language=3 must retain desktop startup");
    assert!(!v1_boot_output.contains("Configuration recovery"));
    assert_eq!(
        v1_wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::R640x480,
            colour: DisplayColour::BW,
        }
    );
    assert_eq!(fs::read(&valid_v1_path).unwrap(), valid_v1);

    // Corrupting the boot capsule still takes the restricted native recovery
    // route; config recovery must not mask a foundation-capsule failure.
    let capsule_path = isolated.root.join("broken-capsule");
    fs::write(&capsule_path, b"not a boot capsule").unwrap();
    let capsule_config = isolated.root.join("capsule-config");
    let capsule_config_bytes = truncated.to_vec();
    fs::write(&capsule_config, &capsule_config_bytes).unwrap();
    let capsule_output = run_stdio(&isolated.root, &capsule_config, b"Q\r", Some(&capsule_path));
    let capsule_text = process_text(&capsule_output);
    assert!(capsule_output.status.success(), "{capsule_text:?}");
    assert!(
        capsule_text.contains("Trellis native recovery"),
        "{capsule_text:?}"
    );
    assert!(capsule_text.contains("Q exit"), "{capsule_text:?}");
    assert!(
        !capsule_text.contains("Configuration recovery:"),
        "{capsule_text:?}"
    );
    assert!(
        !capsule_text.contains("ConfigurationRecovery="),
        "{capsule_text:?}"
    );
    assert_eq!(fs::read(&capsule_config).unwrap(), capsule_config_bytes);
}
