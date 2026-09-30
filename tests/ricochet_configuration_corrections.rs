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

use ricochet::{
    display::{DesktopResolution, DisplayColour, DisplaySettings},
    error::RuntimeError,
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
}

impl IsolatedEnvironment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ricochet-ricochet-configuration-corrections-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("configure");
        let old_config_path = std::env::var_os("RICOCHET_CONFIG_PATH");
        let old_demo_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        unsafe {
            std::env::set_var("RICOCHET_CONFIG_PATH", &config_path);
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
        }
        Self {
            root,
            config_path,
            old_config_path,
            old_demo_volume,
        }
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.old_config_path {
                std::env::set_var("RICOCHET_CONFIG_PATH", value);
            } else {
                std::env::remove_var("RICOCHET_CONFIG_PATH");
            }
            if let Some(value) = &self.old_demo_volume {
                std::env::set_var("RICOCHET_DEMO_VOLUME", value);
            } else {
                std::env::remove_var("RICOCHET_DEMO_VOLUME");
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn new_dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    let dispatcher = SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    (dispatcher, display_receiver)
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
) -> (Result<(), RuntimeError>, String) {
    let _ = receiver.try_iter().count();
    put_string(task, CLI_ADDRESS, command);
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    assert_eq!(
        context.registers[0], CLI_ADDRESS,
        "OS_CLI must preserve R0 for {command:?}"
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

fn normalized_output(output: &str) -> String {
    output
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn run_application(source: &str, wimp: Option<Arc<WimpServer>>) -> Result<String, RuntimeError> {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    let mut runtime = match wimp {
        Some(wimp) => Runtime::windowed_with_desktop(input_receiver, display_sender, wimp),
        None => Runtime::windowed(input_receiver, display_sender),
    };
    runtime.run_application(source)?;
    let output = display_receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(String::from_utf8_lossy(&output).into_owned())
}

fn run_ordinary_display_application(
    source: &str,
    wimp: Arc<WimpServer>,
    task_id: u64,
) -> Result<String, RuntimeError> {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    let mut runtime = Runtime::desktop_task(task_id, input_receiver, display_sender, wimp);
    runtime.run_application(source)?;
    let output = display_receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    Ok(String::from_utf8_lossy(&output).into_owned())
}

fn run_configured_startup(
    input: &[u8],
) -> (Result<(), RuntimeError>, Vec<DisplayEvent>, Arc<WimpServer>) {
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
        let _ = finished_sender.send(runtime.run());
    });

    let mut events = Vec::new();
    let startup_deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut finished = None;
    while std::time::Instant::now() < startup_deadline {
        match display_receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(event @ DisplayEvent::DesktopStarted) => {
                events.push(event);
                break;
            }
            Ok(event) => events.push(event),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(result) = finished_receiver.try_recv() {
                    finished = Some(result);
                    break;
                }
            }
        }
    }
    if events
        .iter()
        .any(|event| matches!(event, DisplayEvent::DesktopStarted))
    {
        wimp.stop();
    }
    if finished.is_none() {
        finished = Some(
            finished_receiver
                .recv_timeout(Duration::from_secs(3))
                .expect("configured startup should finish after its selected path"),
        );
    }
    runtime_thread.join().unwrap();
    (finished.expect("runtime result recorded"), events, wimp)
}

fn seed(path: &Path, source: &str) -> Vec<u8> {
    fs::write(path, source).unwrap();
    source.as_bytes().to_vec()
}

fn run_stdio_process(root: &Path, config_path: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ricochet"))
        .arg("--stdio")
        .env("RICOCHET_CONFIG_PATH", config_path)
        .env("RICOCHET_DEMO_VOLUME", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the public stdio runtime");
    child
        .stdin
        .take()
        .expect("child stdin is piped")
        .write_all(input.as_bytes())
        .expect("write the bounded public command sequence");
    child
        .wait_with_output()
        .expect("wait for public stdio runtime")
}

#[test]
fn configure_routes_are_single_shot_and_wimp_mode_is_the_only_display_setting() {
    let isolated = IsolatedEnvironment::new();
    let (mut dispatcher, receiver) = new_dispatcher();
    let mut ordinary = Task::new(0xC8_001);
    let mut mos = Task::trusted_mos_session(0xC8_002);

    // Whole-output snapshots catch the old bug where a valid CONFIGURE branch
    // printed its result, then fell through and claimed the command was an
    // unsupported ROM/RMA operation.
    let (help_result, help) = cli(&mut dispatcher, &mut mos, &receiver, "*CONFIGURE");
    help_result.expect("CONFIGURE help is a normal OS_CLI command");
    let expected_help = [
        "Syntax: *CONFIGURE <option> <value>",
        "  Language 0|3 (MOS prompt|desktop on load)",
        "  WimpMode (or Mode) Auto|X<width> Y<height> C/G<depth>",
        "    Supported: X640 Y480, X800 Y600, X1024 Y768, X1152 Y864, X1280 Y1024, X1600 Y1200",
        "    Depths: C2, C16, C256, C32K, C16M, G4, G16, G256; no monitor mode table is hosted",
        "  BASICMode Auto|Classic|BASIC64|Hybrid",
        "  BASICProfile Auto|<profile up to 232 bytes>",
        "  BASICTarget Auto|Hosted|RISCOS|Agon",
        "  BASICEngine Interpreter|Hybrid|Strict",
        "  *CONFIGURE DEFAULTS resets all configuration preferences.",
    ]
    .join("\n");
    assert_eq!(normalized_output(&help), expected_help);
    assert!(!help.contains("Unsupported"), "help fell through: {help:?}");
    assert!(!help.contains("ROM/RMA"), "help fell through: {help:?}");

    let (status_result, defaults) = cli(&mut dispatcher, &mut ordinary, &receiver, "*STATUS");
    status_result.expect("STATUS is public-read");
    let expected_defaults = [
        "Language=0",
        "BASICMode=AUTO",
        "BASICProfile=AUTO",
        "BASICTarget=AUTO",
        "BASICEngine=INTERPRETER",
        "WimpMode=AUTO",
    ]
    .join("\n");
    assert_eq!(normalized_output(&defaults), expected_defaults);

    let (write_result, write_output) = cli(
        &mut dispatcher,
        &mut mos,
        &receiver,
        "*CONFIGURE WimpMode X800 Y600 C32K",
    );
    write_result.expect("trusted MOS may configure WimpMode");
    assert_eq!(
        normalized_output(&write_output),
        "WimpMode set to X800 Y600 C32K."
    );
    assert!(
        !write_output.contains("Unsupported"),
        "write fell through: {write_output:?}"
    );
    let (mode_result, mode_status) = cli(&mut dispatcher, &mut ordinary, &receiver, "*STATUS Mode");
    mode_result.expect("STATUS Mode alias is public-read");
    assert_eq!(normalized_output(&mode_status), "WimpMode=X800 Y600 C32K");

    let (auto_result, auto_output) = cli(
        &mut dispatcher,
        &mut mos,
        &receiver,
        "*CONFIGURE WimpMode Auto",
    );
    auto_result.expect("trusted MOS may restore automatic WimpMode");
    assert_eq!(normalized_output(&auto_output), "WimpMode set to Auto.");
    let (auto_status_result, auto_status) = cli(
        &mut dispatcher,
        &mut ordinary,
        &receiver,
        "*STATUS WimpMode",
    );
    auto_status_result.expect("STATUS WimpMode is public-read");
    assert_eq!(normalized_output(&auto_status), "WimpMode=AUTO");

    for (command, expected) in [
        (
            "*CONFIGURE Language 4",
            "CONFIGURE error: Language must select module 0 (MOS prompt) or 3 (desktop); use decimal, &hex, or base_num notation",
        ),
        (
            "*CONFIGURE Language 3 extra",
            "Syntax: *CONFIGURE <option> <value>",
        ),
        ("*STATUS Language extra", "Syntax: *STATUS [option]"),
        (
            "*CONFIGURE WimpMode X800 Y600 C4",
            "CONFIGURE error: Unsupported WimpMode depth; use C2, C16, C256, C32K, C16M, G4, G16, or G256",
        ),
    ] {
        let (result, output) = cli(&mut dispatcher, &mut mos, &receiver, command);
        result.expect("user syntax/value rejection is a normal CLI result");
        assert_eq!(normalized_output(&output), expected, "{command}");
        assert!(
            !output.contains("Unsupported *CONFIGURE:"),
            "{command} fell through: {output:?}"
        );
        assert!(
            !output.contains("ROM/RMA"),
            "{command} fell through: {output:?}"
        );
    }

    for command in [
        "*CONFIGURE RicochetOutputProfile 16MRGB888",
        "*STATUS RicochetOutputProfile",
        "*CONFIGURE DisplayColour BW",
        "*STATUS DisplayResolution",
    ] {
        let (result, output) = cli(&mut dispatcher, &mut mos, &receiver, command);
        result.expect("removed setting is a normal CLI diagnostic");
        let normalized = normalized_output(&output);
        assert_eq!(normalized.lines().count(), 1, "{command}: {normalized:?}");
        assert!(
            normalized.to_ascii_lowercase().contains("unknown")
                || normalized.to_ascii_lowercase().contains("unsupported"),
            "removed setting was not rejected: {command}: {normalized:?}"
        );
        assert!(
            !normalized.contains("Unsupported *CONFIGURE:"),
            "{normalized:?}"
        );
        assert!(!normalized.contains("ROM/RMA"), "{normalized:?}");
    }

    let (defaults_seed_result, _) = cli(
        &mut dispatcher,
        &mut mos,
        &receiver,
        "*CONFIGURE WimpMode X640 Y480 C16",
    );
    defaults_seed_result.expect("authorized setup for DEFAULTS regression");
    let (reset_result, reset_output) =
        cli(&mut dispatcher, &mut mos, &receiver, "*CONFIGURE DEFAULTS");
    reset_result.expect("trusted MOS DEFAULTS is a normal command");
    assert_eq!(
        normalized_output(&reset_output),
        "Configuration preferences restored to defaults."
    );
    let (reset_status_result, reset_status) =
        cli(&mut dispatcher, &mut ordinary, &receiver, "*STATUS");
    reset_status_result.expect("reset defaults remain publicly readable");
    assert_eq!(normalized_output(&reset_status), expected_defaults);
    assert_eq!(
        fs::read_to_string(&isolated.config_path).unwrap(),
        "# Ricochet MOS configuration v3\nLanguage=0\nBASICMode=AUTO\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=INTERPRETER\nWimpMode=AUTO\n"
    );

    let (denied_result, denied_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &receiver,
        "*CONFIGURE WimpMode X640 Y480 G16",
    );
    denied_result.expect("CONFIGURE reports caller denial as CLI output");
    assert_eq!(
        normalized_output(&denied_output),
        "CONFIGURE error: TaskAuthorizationDenied (&00000004): caller task lacks configuration-write authority"
    );
    assert_eq!(
        fs::read_to_string(&isolated.config_path).unwrap(),
        "# Ricochet MOS configuration v3\nLanguage=0\nBASICMode=AUTO\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=INTERPRETER\nWimpMode=AUTO\n"
    );

    // These ordinary INSPECT and classic routes also return exactly once.
    // MODULES and *Modules share their actual registry projection, so comparing
    // the complete outputs detects any accidental downstream command routing.
    let (inspect_help_result, inspect_help) =
        cli(&mut dispatcher, &mut ordinary, &receiver, "*INSPECT");
    inspect_help_result.expect("INSPECT help is read-only");
    let expected_inspect_help = [
        "Read-only inspection commands:",
        "  *INSPECT MODULES [filter]",
        "  *INSPECT MODULE <title>",
        "  *INSPECT SWI <name|number>",
        "  *INSPECT DEFINITION <module>/<definition> [byte-offset]",
        "  *INSPECT SOURCE is a read-only alias for DEFINITION",
        "Module changes use *RMLoad, *RMKill, and *RMEnsure.",
    ]
    .join("\n");
    assert_eq!(normalized_output(&inspect_help), expected_inspect_help);

    let (inspect_result, inspect_modules) = cli(
        &mut dispatcher,
        &mut ordinary,
        &receiver,
        "*INSPECT MODULES",
    );
    let (modules_result, modules) = cli(&mut dispatcher, &mut ordinary, &receiver, "*Modules");
    inspect_result.expect("INSPECT MODULES is public read-only");
    modules_result.expect("*Modules is public read-only");
    assert_eq!(
        normalized_output(&inspect_modules),
        normalized_output(&modules)
    );
    assert!(
        !modules.contains("Unsupported"),
        "*Modules fell through: {modules:?}"
    );

    let (ensure_result, ensure_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &receiver,
        "*RMEnsure System 0.0.0",
    );
    ensure_result.expect("satisfied RMEnsure version check is a normal no-op");
    assert!(ensure_output.is_empty());
    let (rom_result, rom_output) = cli(&mut dispatcher, &mut ordinary, &receiver, "*ROMModules");
    rom_result.expect("unsupported ROM inventory is an explicit CLI diagnostic");
    assert_eq!(
        normalized_output(&rom_output),
        "Unsupported *ROMMODULES: the hosted module manager has no native ROM/RMA implementation; no state changed."
    );

    // A procedure return inside an inline IF must resume the caller's remaining
    // colon-separated statements, rather than aborting its enclosing line.
    let inline_source = "REM @BASIC64 MODE=BASIC64\n10 A%=40\n20 IF 1 THEN PROC Inner:PROC Inner:GOTO 50:A%=-1\n30 A%=-2\n40 GOTO 60\n50 PRINT A%\n60 PRINT \"done\"\n70 END\n100 DEF PROC Inner\n110 A%=A%+1\n120 ENDPROC\n";
    let inline_output = run_application(inline_source, None)
        .expect("inline IF/PROC continuation program should execute");
    assert_eq!(normalized_output(&inline_output), "42\ndone");

    // Old v1 display rows migrate to the one WimpMode value. Read-only status
    // never rewrites the old file; the first permitted write normalizes it.
    let legacy_v1 = "# Ricochet MOS configuration v1\nLanguage=3\nBASICMode=BASIC64\nBASICProfile=V1-Profile\nBASICTarget=Agon\nBASICEngine=Strict\nWindowFurniture=Bevelled\nDisplayResolution=800x600\nDisplayColour=32KRGB555\n";
    let v1_bytes = seed(&isolated.config_path, legacy_v1);
    let (v1_result, v1_status) = cli(&mut dispatcher, &mut ordinary, &receiver, "*STATUS");
    v1_result.expect("v1 migration must remain readable");
    assert_eq!(
        normalized_output(&v1_status),
        "Language=3\nBASICMode=BASIC64\nBASICProfile=V1-Profile\nBASICTarget=AGON\nBASICEngine=STRICT\nWimpMode=X800 Y600 C32K"
    );
    assert_eq!(fs::read(&isolated.config_path).unwrap(), v1_bytes);
    let (v1_startup, v1_events, v1_wimp) = run_configured_startup(&[]);
    v1_startup.expect("a legacy v1 Language=3 file should complete desktop startup");
    assert!(
        v1_events
            .iter()
            .any(|event| matches!(event, DisplayEvent::DesktopStarted))
    );
    assert_eq!(
        v1_wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::R800x600,
            colour: DisplayColour::Rgb555,
        },
        "startup should apply v1 DisplayResolution/DisplayColour migration"
    );
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        v1_bytes,
        "booting a legacy v1 file must not rewrite it before an authorized save"
    );

    let (v1_save_result, v1_save_output) = cli(
        &mut dispatcher,
        &mut mos,
        &receiver,
        "*CONFIGURE Language 0",
    );
    v1_save_result.expect("authorized write should persist normalized v1 migration");
    assert_eq!(normalized_output(&v1_save_output), "Language set to 0.");
    let v1_saved = fs::read_to_string(&isolated.config_path).unwrap();
    assert!(v1_saved.starts_with("# Ricochet MOS configuration v3\n"));
    for retired in [
        "RicochetOutputProfile=",
        "DisplayResolution=",
        "DisplayColour=",
        "WindowFurniture=",
    ] {
        assert!(
            !v1_saved.contains(retired),
            "retired row survived migration: {v1_saved}"
        );
    }
    assert!(v1_saved.contains("WimpMode=X800 Y600 C32K"));

    // v2 had both WimpMode and a separate renderer profile. WimpMode is now
    // authoritative even when the retired profile disagrees with its depth.
    let legacy_v2 = "# Ricochet MOS configuration v2\nLanguage=3\nBASICMode=Hybrid\nBASICProfile=AUTO\nBASICTarget=Hosted\nBASICEngine=Interpreter\nWimpMode=X1024 Y768 G16\nRicochetOutputProfile=16MRGB888\n";
    let v2_bytes = seed(&isolated.config_path, legacy_v2);
    let (v2_result, v2_status) = cli(&mut dispatcher, &mut ordinary, &receiver, "*STATUS");
    v2_result.expect("v2 migration must remain readable");
    assert_eq!(
        normalized_output(&v2_status),
        "Language=3\nBASICMode=HYBRID\nBASICProfile=AUTO\nBASICTarget=HOSTED\nBASICEngine=INTERPRETER\nWimpMode=X1024 Y768 G16"
    );
    assert_eq!(fs::read(&isolated.config_path).unwrap(), v2_bytes);
    let (v2_save_result, _) = cli(
        &mut dispatcher,
        &mut mos,
        &receiver,
        "*CONFIGURE WimpMode Auto",
    );
    v2_save_result.expect("successful save should remove conflicting old profile");
    let v2_saved = fs::read_to_string(&isolated.config_path).unwrap();
    assert!(v2_saved.starts_with("# Ricochet MOS configuration v3\n"));
    assert!(v2_saved.contains("WimpMode=AUTO"));
    assert!(!v2_saved.contains("RicochetOutputProfile="));

    let legacy_v2_auto = "# Ricochet MOS configuration v2\nLanguage=3\nBASICMode=Auto\nBASICProfile=Auto\nBASICTarget=Auto\nBASICEngine=Interpreter\nWimpMode=Auto\nRicochetOutputProfile=BW\n";
    let v2_auto_bytes = seed(&isolated.config_path, legacy_v2_auto);
    let (v2_auto_result, v2_auto_status) =
        cli(&mut dispatcher, &mut ordinary, &receiver, "*STATUS");
    v2_auto_result.expect("v2 Auto/profile conflict remains readable");
    let expected_v2_auto = expected_defaults.replacen("Language=0", "Language=3", 1);
    assert_eq!(normalized_output(&v2_auto_status), expected_v2_auto);
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        v2_auto_bytes,
        "read-only status must not rewrite the v2 file"
    );
    let (auto_updates, _auto_update_receiver) = mpsc::channel();
    let auto_wimp = WimpServer::new(auto_updates);
    let auto_query = run_application(
        "REM @BASIC64 MODE=BASIC64\nSYS \"RICOCHET_DISPLAY\", 1, 0\n",
        Some(Arc::clone(&auto_wimp)),
    )
    .expect("Auto WimpMode can be queried through RICOCHET_DISPLAY");
    assert!(auto_query.is_empty());
    assert_eq!(
        auto_wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::Window,
            colour: DisplayColour::Rgb888,
        },
        "retired v2 BW profile must not override Auto's C16M default"
    );
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        v2_auto_bytes,
        "read-only display query must not rewrite the v2 file"
    );
    let (v2_startup, v2_events, v2_wimp) = run_configured_startup(&[]);
    v2_startup.expect("legacy v2 Auto config should complete desktop startup");
    assert!(
        v2_events
            .iter()
            .any(|event| matches!(event, DisplayEvent::DesktopStarted))
    );
    assert_eq!(
        v2_wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::Window,
            colour: DisplayColour::Rgb888,
        },
        "boot must ignore the retired v2 BW profile under WimpMode=Auto"
    );
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        v2_auto_bytes,
        "booting a legacy v2 Auto file must not rewrite it before an authorized save"
    );
    let (v2_auto_save_result, _) = cli(
        &mut dispatcher,
        &mut mos,
        &receiver,
        "*CONFIGURE WimpMode Auto",
    );
    v2_auto_save_result.expect("successful WimpMode save should canonicalize v2 Auto");
    let v2_auto_saved = fs::read_to_string(&isolated.config_path).unwrap();
    assert!(v2_auto_saved.starts_with("# Ricochet MOS configuration v3\n"));
    assert!(v2_auto_saved.contains("WimpMode=AUTO"));
    assert!(!v2_auto_saved.contains("RicochetOutputProfile="));

    // A legacy Auto/Window setting with a non-full-colour profile normalizes
    // to host-following size plus C16M, never to a hidden independent colour.
    let legacy_window = "# Ricochet MOS configuration v1\nLanguage=0\nDisplayResolution=Window\nDisplayColour=BW\nWindowFurniture=Flat\n";
    let _ = seed(&isolated.config_path, legacy_window);
    let (window_result, window_status) = cli(
        &mut dispatcher,
        &mut ordinary,
        &receiver,
        "*STATUS WimpMode",
    );
    window_result.expect("legacy Window remains readable");
    assert_eq!(normalized_output(&window_status), "WimpMode=AUTO");

    // RICOCHET_DISPLAY and CONFIGURE share one WimpMode persistence/state source.
    let (updates, _update_receiver) = mpsc::channel();
    let wimp = WimpServer::new(updates);
    let auto_settings = DisplaySettings {
        resolution: DesktopResolution::Window,
        colour: DisplayColour::Rgb888,
    };
    assert_eq!(wimp.display_settings(), auto_settings);
    let ordinary_apply = "REM @BASIC64 MODE=BASIC64\nSYS \"RICOCHET_DISPLAY\", 1, 1, 0, 0\n";
    let persisted_before_denial = fs::read(&isolated.config_path).unwrap();
    let denied_apply =
        run_ordinary_display_application(ordinary_apply, Arc::clone(&wimp), 0xC8_004);
    assert!(matches!(
        denied_apply,
        Err(RuntimeError::Structured {
            type_name,
            code: 4,
            ..
        }) if type_name == "TaskAuthorizationDenied"
    ));
    assert_eq!(wimp.display_settings(), auto_settings);
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        persisted_before_denial
    );

    let apply_fixed = format!(
        "REM @BASIC64 MODE=BASIC64\nSYS \"RICOCHET_DISPLAY\", 1, 1, {}, {}\n",
        DesktopResolution::R800x600.id(),
        DisplayColour::Grey16.id()
    );
    run_application(&apply_fixed, Some(Arc::clone(&wimp)))
        .expect("trusted MOS RICOCHET_DISPLAY apply should persist a fixed mode");
    assert_eq!(
        wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::R800x600,
            colour: DisplayColour::Grey16,
        }
    );
    let query_source = "REM @BASIC64 MODE=BASIC64\nSYS \"RICOCHET_DISPLAY\", 1, 0 TO ABI%, ACTION%, RESOLUTION%, COLOUR%, UNUSED4%, UNUSED5%, UNUSED6%, UNUSED7%, SAVE_STATUS%\nPRINT RESOLUTION%; \",\"; COLOUR%\n";
    let query_output = run_ordinary_display_application(query_source, Arc::clone(&wimp), 0xC8_005)
        .expect("ordinary task may query RICOCHET_DISPLAY");
    assert_eq!(normalized_output(&query_output), "2,2");
    let (fixed_status_result, fixed_status) = cli(
        &mut dispatcher,
        &mut ordinary,
        &receiver,
        "*STATUS WimpMode",
    );
    fixed_status_result.expect("status should observe RICOCHET_DISPLAY fixed apply");
    assert_eq!(normalized_output(&fixed_status), "WimpMode=X800 Y600 G16");

    let bad_auto_apply = format!(
        "REM @BASIC64 MODE=BASIC64\nSYS \"RICOCHET_DISPLAY\", 1, 1, {}, {} TO ABI%, ACTION%, RESOLUTION%, COLOUR%, UNUSED4%, UNUSED5%, UNUSED6%, UNUSED7%, SAVE_STATUS%\nPRINT SAVE_STATUS%\n",
        DesktopResolution::Window.id(),
        DisplayColour::BW.id()
    );
    let persisted_before_bad_auto = fs::read(&isolated.config_path).unwrap();
    let bad_auto_output = run_application(&bad_auto_apply, Some(Arc::clone(&wimp)));
    let bad_auto_output =
        bad_auto_output.expect("unsupported Window plus BW must use RICOCHET_DISPLAY's status return");
    assert_eq!(
        normalized_output(&bad_auto_output),
        "1",
        "Window plus BW must return a failed-apply status, not claim success"
    );
    assert_eq!(
        wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::R800x600,
            colour: DisplayColour::Grey16,
        },
        "invalid Auto/colour application must not change live settings"
    );
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        persisted_before_bad_auto,
        "invalid Window+BW apply must not change persisted settings"
    );
    let (after_bad_apply_result, after_bad_apply_status) = cli(
        &mut dispatcher,
        &mut ordinary,
        &receiver,
        "*STATUS WimpMode",
    );
    after_bad_apply_result.expect("status remains available after rejected apply");
    assert_eq!(
        normalized_output(&after_bad_apply_status),
        "WimpMode=X800 Y600 G16"
    );

    // The public stdio process exercises the same adjacent-command sequence
    // that exposed the original fallthrough, not just isolated dispatcher calls.
    let process_config = isolated.root.join("stdio-configure");
    let process = run_stdio_process(
        &isolated.root,
        &process_config,
        "*CONFIGURE\r*CONFIGURE Language 3\r*STATUS Language\r*INSPECT\r*Modules\rQUIT\r",
    );
    assert!(
        process.status.success(),
        "stdio runtime failed: {}",
        String::from_utf8_lossy(&process.stderr)
    );
    let process_output = normalized_output(&String::from_utf8_lossy(&process.stdout));
    let expected_process_output = format!(
        "*{expected_help}\n*Language set to 3.\n*Language=3\n*{expected_inspect_help}\n*{}\n*",
        normalized_output(&modules)
    );
    assert_eq!(process_output, expected_process_output);
    assert!(
        !process_output.contains("Unsupported *CONFIGURE"),
        "{process_output:?}"
    );
    assert!(
        !process_output.contains("Unsupported *STATUS"),
        "{process_output:?}"
    );
    assert!(!process_output.contains("ROM/RMA"), "{process_output:?}");
    assert!(process_output.contains("Syntax: *CONFIGURE <option> <value>"));
    assert!(process_output.contains("Language set to 3."));
    assert!(process_output.contains("Language=3"));
    assert!(process_output.contains("Read-only inspection commands:"));
    assert!(process_output.contains("MODULES\nBoot 1.0.0 Active"));
}
