use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use acorn_2026::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    runtime::Runtime,
    swi::{DisplayEvent, SwiContext, SwiDispatcher},
    wimp::WimpServer,
};

const OS_CLI: u32 = 0x05;
const CLI_ADDRESS: u32 = 0x2100;
const OLD_CONFIG_SCRATCH_START: u32 = 0x5000;
const OLD_CONFIG_SCRATCH_LENGTH: usize = 0x0C00;

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
            "acorn-trellis-standard-config-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("configure");
        let old_config_path = std::env::var_os("ACORN_CONFIG_PATH");
        let old_demo_volume = std::env::var_os("ACORN_DEMO_VOLUME");
        unsafe {
            std::env::set_var("ACORN_CONFIG_PATH", &config_path);
            std::env::set_var("ACORN_DEMO_VOLUME", &root);
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
                std::env::set_var("ACORN_CONFIG_PATH", value);
            } else {
                std::env::remove_var("ACORN_CONFIG_PATH");
            }
            if let Some(value) = &self.old_demo_volume {
                std::env::set_var("ACORN_DEMO_VOLUME", value);
            } else {
                std::env::remove_var("ACORN_DEMO_VOLUME");
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn new_dispatcher() -> (
    SwiDispatcher,
    mpsc::Receiver<DisplayEvent>,
    mpsc::Sender<u8>,
) {
    let (input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    let dispatcher = SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    (dispatcher, display_receiver, input_sender)
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
    let bytes = receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    (result, String::from_utf8_lossy(&bytes).into_owned())
}

fn status(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    option: Option<&str>,
) -> String {
    let command = option.map_or_else(|| "*STATUS".to_string(), |name| format!("*STATUS {name}"));
    let (result, output) = cli(dispatcher, task, receiver, &command);
    result.expect("STATUS is public-read for every caller profile");
    output
}

fn status_table(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect()
}

fn assert_configure_diagnostic(output: &str) {
    assert!(
        output.contains("CONFIGURE error:") || output.contains("Syntax: *CONFIGURE"),
        "expected a visible CONFIGURE rejection, got {output:?}"
    );
}

fn run_configured_startup(input: Vec<u8>) -> (Result<(), RuntimeError>, Vec<DisplayEvent>) {
    let (input_sender, input_receiver) = mpsc::channel();
    for byte in input {
        input_sender.send(byte).unwrap();
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
                .expect("configured startup should finish after the selected path"),
        );
    }
    runtime_thread.join().unwrap();
    (finished.expect("runtime result recorded"), events)
}

fn seed_legacy_configuration(
    path: &Path,
    furniture: &str,
    resolution: &str,
    colour: &str,
) -> Vec<u8> {
    let contents = format!(
        "# Acorn-2026 MOS configuration v1\nLanguage=3\nWindowFurniture={furniture}\nBASICMode=BASIC64\nBASICProfile=Legacy-Profile\nBASICTarget=Agon\nBASICEngine=Strict\nDisplayResolution={resolution}\nDisplayColour={colour}\n"
    );
    fs::write(path, &contents).unwrap();
    contents.into_bytes()
}

#[test]
fn standard_configure_names_modes_and_flat_only_legacy_migration() {
    let isolated = IsolatedEnvironment::new();
    let (mut dispatcher, receiver, _input_sender) = new_dispatcher();
    let mut ordinary = Task::new(0xC7_001);
    let mut source_only = Task::trusted_source_inspector(0xC7_002);
    let mut module_only = Task::trusted_module_manager(0xC7_003);
    let mut config_manager = Task::trusted_configuration_manager(0xC7_004);
    let mut mos_session = Task::trusted_mos_session(0xC7_005);

    // The empty-file table and DEFAULTS must agree on the new schema. The
    // old hosted display fields and furniture preference are no longer public.
    let old_scratch = vec![0xA5; OLD_CONFIG_SCRATCH_LENGTH];
    ordinary
        .memory
        .write_bytes(OLD_CONFIG_SCRATCH_START, &old_scratch)
        .unwrap();
    let area_count = ordinary.memory.dynamic_area_count();
    let defaults = status(&mut dispatcher, &mut ordinary, &receiver, None);
    assert_eq!(ordinary.memory.dynamic_area_count(), area_count);
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_CONFIG_SCRATCH_START, OLD_CONFIG_SCRATCH_LENGTH)
            .unwrap(),
        old_scratch,
        "STATUS must not overwrite old fixed scratch addresses"
    );
    let defaults_table = status_table(&defaults);
    assert_eq!(
        defaults_table,
        BTreeMap::from([
            ("Language".into(), "0".into()),
            ("WimpMode".into(), "AUTO".into()),
            ("BASICMode".into(), "AUTO".into()),
            ("BASICProfile".into(), "AUTO".into()),
            ("BASICTarget".into(), "AUTO".into()),
            ("BASICEngine".into(), "INTERPRETER".into()),
        ]),
        "unexpected no-file configuration schema/defaults: {defaults:?}"
    );
    for removed in ["WindowFurniture", "DisplayResolution", "DisplayColour"] {
        assert!(!defaults.contains(removed), "obsolete key in {defaults:?}");
    }

    let (configure_help_result, configure_help) =
        cli(&mut dispatcher, &mut ordinary, &receiver, "*CONFIGURE");
    configure_help_result.unwrap();
    for expected in ["Language", "WimpMode", "BASICEngine"] {
        assert!(configure_help.contains(expected), "{configure_help:?}");
    }
    for removed in ["WindowFurniture", "DisplayResolution", "DisplayColour"] {
        assert!(
            !configure_help.contains(removed),
            "obsolete option in {configure_help:?}"
        );
    }

    // Public STATUS is available to ordinary, source-only, module-only, and
    // write-authorized tasks. Only a ConfigurationWrite grant can change it.
    for task in [
        &mut source_only,
        &mut module_only,
        &mut config_manager,
        &mut mos_session,
    ] {
        assert!(
            status(&mut dispatcher, task, &receiver, Some("WimpMode")).contains("WimpMode=AUTO")
        );
    }
    for task in [&mut ordinary, &mut source_only, &mut module_only] {
        for command in ["*CONFIGURE WimpMode X800 Y600 C16M"] {
            let bytes_before = fs::read(&isolated.config_path).ok();
            let (result, output) = cli(&mut dispatcher, task, &receiver, command);
            result.expect("denied CONFIGURE uses readable MOS diagnostics");
            assert!(
                output.contains("CONFIGURE error:")
                    && output.contains("caller task lacks configuration-write authority"),
                "expected caller-specific write denial for {command:?}: {output:?}"
            );
            assert_eq!(fs::read(&isolated.config_path).ok(), bytes_before);
        }
    }

    // `Language` is a real PRM option. Trellis intentionally supports only
    // the historical MOS/desktop module numbers, but accepts PRM numeric
    // spellings and canonicalizes them in STATUS.
    for (command, expected) in [
        ("*CONFIGURE Language 3", "Language=3"),
        ("*CONF. lAnGuAgE &3", "Language=3"),
        ("*CONFIGURE LANGUAGE 2_11", "Language=3"),
        ("*CONFIGURE Language 0", "Language=0"),
    ] {
        let (result, output) = cli(&mut dispatcher, &mut config_manager, &receiver, command);
        result.unwrap_or_else(|error| panic!("{command:?} failed: {error:?}"));
        assert!(!output.contains("CONFIGURE error:"), "{output:?}");
        assert!(
            status(&mut dispatcher, &mut ordinary, &receiver, Some("Language")).contains(expected)
        );
    }
    for command in [
        "*CONFIGURE Language 4",
        "*CONFIGURE Language &4",
        "*CONFIGURE Language 2_100",
        "*CONFIGURE Language &nothex",
    ] {
        let before = fs::read(&isolated.config_path).unwrap();
        let (result, output) = cli(&mut dispatcher, &mut config_manager, &receiver, command);
        result.unwrap();
        assert_configure_diagnostic(&output);
        assert_eq!(fs::read(&isolated.config_path).unwrap(), before);
    }

    // PRM Mode and WimpMode name the same setting. Trellis implements a
    // bounded hosted selector: standard X/Y plus one C/G depth, with explicit
    // rejection of physical mode IDs and unsupported monitor/refresh fields.
    let mode_cases = [
        (640, 480),
        (800, 600),
        (1024, 768),
        (1152, 864),
        (1280, 1024),
        (1600, 1200),
    ];
    for (width, height) in mode_cases {
        let command = format!("*CONFIGURE WimpMode X{width} Y{height} C16");
        let (result, output) = cli(&mut dispatcher, &mut config_manager, &receiver, &command);
        result.unwrap_or_else(|error| panic!("{command:?} failed: {error:?}"));
        assert!(!output.contains("CONFIGURE error:"), "{output:?}");
        let expected = format!("WimpMode=X{width} Y{height} C16");
        assert!(
            status(&mut dispatcher, &mut ordinary, &receiver, Some("Mode")).contains(&expected)
        );
    }
    for colour in ["C2", "C16", "C256", "C32K", "C16M", "G4", "G16", "G256"] {
        let command = format!("*CONFIGURE WimpMode X800 Y600 {colour}");
        let (result, output) = cli(&mut dispatcher, &mut config_manager, &receiver, &command);
        result.unwrap_or_else(|error| panic!("{command:?} failed: {error:?}"));
        assert!(!output.contains("CONFIGURE error:"), "{output:?}");
        let expected = format!("WimpMode=X800 Y600 {colour}");
        assert!(
            status(&mut dispatcher, &mut ordinary, &receiver, Some("WimpMode")).contains(&expected)
        );
    }
    let (padded_mode_result, padded_mode_output) = cli(
        &mut dispatcher,
        &mut config_manager,
        &receiver,
        "*CONFIGURE WimpMode X0640 Y0480 C16",
    );
    padded_mode_result.unwrap();
    assert!(
        !padded_mode_output.contains("CONFIGURE error:"),
        "PRM three/four digit X/Y values should accept zero-padded equivalents: {padded_mode_output:?}"
    );
    assert!(
        status(&mut dispatcher, &mut ordinary, &receiver, Some("WimpMode"))
            .contains("WimpMode=X640 Y480 C16")
    );
    let (auto_result, auto_output) = cli(
        &mut dispatcher,
        &mut config_manager,
        &receiver,
        "*conf. mode auto",
    );
    auto_result.unwrap();
    assert!(!auto_output.contains("CONFIGURE error:"), "{auto_output:?}");
    assert!(
        status(&mut dispatcher, &mut ordinary, &receiver, Some("Mode")).contains("WimpMode=AUTO")
    );

    for (command, expected_mode) in [
        (
            "*CONFIGURE WimpMode X800 Y600 C16M",
            "WimpMode=X800 Y600 C16M",
        ),
        ("*CONFIGURE Mode X640 Y480 G16", "WimpMode=X640 Y480 G16"),
    ] {
        let (result, output) = cli(&mut dispatcher, &mut config_manager, &receiver, command);
        result.unwrap_or_else(|error| panic!("{command:?} failed: {error:?}"));
        assert!(!output.contains("CONFIGURE error:"), "{output:?}");
        assert!(
            status(&mut dispatcher, &mut ordinary, &receiver, Some("WimpMode"))
                .contains(expected_mode)
        );
    }
    for command in [
        "*CONFIGURE WimpMode 15",
        "*CONFIGURE WimpMode X800 Y480 C16",
        "*CONFIGURE WimpMode X800 Y600 C16M G256",
        "*CONFIGURE WimpMode X800 Y600 C4",
        "*CONFIGURE WimpMode X800 Y600 C64",
        "*CONFIGURE WimpMode X800 Y600 C32T",
        "*CONFIGURE WimpMode X800 Y600 EIG4",
        "*CONFIGURE WimpMode X800 Y600 C16M EX2",
        "*CONFIGURE WimpMode X800 Y600 C16M EY2",
        "*CONFIGURE WimpMode X800 Y600 C16M F60",
    ] {
        let before = fs::read(&isolated.config_path).unwrap();
        let (result, output) = cli(&mut dispatcher, &mut config_manager, &receiver, command);
        result.unwrap();
        assert_configure_diagnostic(&output);
        assert_eq!(fs::read(&isolated.config_path).unwrap(), before);
    }

    // The old colour/profile and appearance keys are migration-only. Depth
    // now belongs solely to WimpMode, and Flat is the only furniture style.
    for command in [
        "*CONFIGURE WindowFurniture Flat",
        "*CONFIGURE DisplayResolution 640x480",
        "*CONFIGURE DisplayColour 32KRGB555",
        "*CONFIGURE TrellisOutputProfile 32KRGB555",
        "*STATUS WindowFurniture",
        "*STATUS DisplayResolution",
        "*STATUS DisplayColour",
        "*STATUS TrellisOutputProfile",
    ] {
        let before = fs::read(&isolated.config_path).unwrap();
        let (result, output) = cli(&mut dispatcher, &mut config_manager, &receiver, command);
        result.unwrap();
        if command.starts_with("*STATUS") {
            assert!(output.contains("STATUS error:"), "{output:?}");
        } else {
            assert_configure_diagnostic(&output);
        }
        assert_eq!(fs::read(&isolated.config_path).unwrap(), before);
    }

    // A legacy v1 file with either appearance value still loads; only the
    // next successful write migrates it. Other valid values survive, and the
    // old independent host resolution/palette are converted into one
    // WimpMode selector; Auto always restores the full-colour palette.
    let legacy_mode_migrations = [
        ("BW", "X640 Y480 C2"),
        ("4Grey", "X640 Y480 G4"),
        ("16Grey", "X640 Y480 G16"),
        ("16Colour", "X640 Y480 C16"),
        ("256Grey", "X640 Y480 G256"),
        ("256Colour", "X640 Y480 C256"),
        ("32KRGB555", "X640 Y480 C32K"),
        ("16MRGB888", "X640 Y480 C16M"),
    ];
    let migrations = legacy_mode_migrations
        .into_iter()
        .enumerate()
        .map(|(index, (colour, mode))| {
            (
                if index == 0 { "Bevelled" } else { "Flat" },
                "640x480",
                colour,
                mode,
            )
        })
        .chain(std::iter::once(("LegacyLook", "Window", "BW", "AUTO")));
    for (furniture, resolution, colour, expected_mode) in migrations {
        let original =
            seed_legacy_configuration(&isolated.config_path, furniture, resolution, colour);
        let (mut legacy_dispatcher, legacy_receiver, _legacy_input) = new_dispatcher();
        let mut legacy_reader = Task::new(0xC7_100);
        let migrated_view = status(
            &mut legacy_dispatcher,
            &mut legacy_reader,
            &legacy_receiver,
            None,
        );
        let migrated_table = status_table(&migrated_view);
        assert_eq!(
            migrated_table.get("Language").map(String::as_str),
            Some("3")
        );
        assert_eq!(
            migrated_table.get("BASICMode").map(String::as_str),
            Some("BASIC64")
        );
        assert_eq!(
            migrated_table.get("BASICProfile").map(String::as_str),
            Some("Legacy-Profile")
        );
        assert_eq!(
            migrated_table.get("BASICTarget").map(String::as_str),
            Some("AGON")
        );
        assert_eq!(
            migrated_table.get("BASICEngine").map(String::as_str),
            Some("STRICT")
        );
        assert_eq!(
            migrated_table.get("WimpMode").map(String::as_str),
            Some(expected_mode)
        );
        assert!(!migrated_table.contains_key("TrellisOutputProfile"));
        assert!(
            !migrated_view.contains("WindowFurniture"),
            "{migrated_view:?}"
        );
        assert!(
            !migrated_view.contains("DisplayResolution"),
            "{migrated_view:?}"
        );
        assert!(
            !migrated_view.contains("DisplayColour"),
            "{migrated_view:?}"
        );
        assert_eq!(
            fs::read(&isolated.config_path).unwrap(),
            original,
            "read-only migration should not rewrite a user's config file"
        );
        if furniture == "Bevelled" {
            let (startup, events) = run_configured_startup(Vec::new());
            startup.expect("a legacy v1 file with obsolete furniture must still boot");
            assert!(
                events
                    .iter()
                    .any(|event| matches!(event, DisplayEvent::DesktopStarted))
            );
        }

        let mut legacy_writer = Task::trusted_configuration_manager(0xC7_101);
        let (write_result, write_output) = cli(
            &mut legacy_dispatcher,
            &mut legacy_writer,
            &legacy_receiver,
            "*CONFIGURE BASICMode Hybrid",
        );
        write_result.unwrap();
        assert!(
            !write_output.contains("CONFIGURE error:"),
            "{write_output:?}"
        );
        let saved = fs::read_to_string(&isolated.config_path).unwrap();
        for obsolete in [
            "WindowFurniture=",
            "DisplayResolution=",
            "DisplayColour=",
            "TrellisOutputProfile=",
        ] {
            assert!(
                !saved.contains(obsolete),
                "legacy key {obsolete:?} remained: {saved}"
            );
        }
        for expected in [
            "Language=3",
            "WimpMode=",
            "BASICMode=HYBRID",
            "BASICProfile=Legacy-Profile",
            "BASICTarget=AGON",
            "BASICEngine=STRICT",
        ] {
            assert!(
                saved.contains(expected),
                "migration lost {expected:?}: {saved}"
            );
        }
    }

    // DEFAULTS writes the full new schema and matches the no-file effective
    // table. It cannot reintroduce Flat/Bevelled as a mutable preference.
    let (reset_result, reset_output) = cli(
        &mut dispatcher,
        &mut mos_session,
        &receiver,
        "*CONFIGURE DEFAULTS",
    );
    reset_result.unwrap();
    assert!(
        !reset_output.contains("CONFIGURE error:"),
        "{reset_output:?}"
    );
    let reset_status = status(&mut dispatcher, &mut ordinary, &receiver, None);
    assert_eq!(status_table(&reset_status), defaults_table);
    assert!(!reset_status.contains("WindowFurniture"));
    let reset_file = fs::read_to_string(&isolated.config_path).unwrap();
    for removed in [
        "WindowFurniture=",
        "DisplayResolution=",
        "DisplayColour=",
        "TrellisOutputProfile=",
    ] {
        assert!(!reset_file.contains(removed), "{reset_file}");
    }

    // Save the PRM Language selection and standard WimpMode through the
    // trusted MOS path, then verify both persisted reads and Boot's handoff.
    for command in [
        "*CONFIGURE Language &3",
        "*CONFIGURE WimpMode X1024 Y768 G256",
    ] {
        let (result, output) = cli(&mut dispatcher, &mut mos_session, &receiver, command);
        result.unwrap_or_else(|error| panic!("{command:?} failed: {error:?}"));
        assert!(!output.contains("CONFIGURE error:"), "{output:?}");
    }
    let (mut fresh_dispatcher, fresh_receiver, _fresh_input) = new_dispatcher();
    let mut fresh_reader = Task::new(0xC7_200);
    assert!(
        status(
            &mut fresh_dispatcher,
            &mut fresh_reader,
            &fresh_receiver,
            Some("Language")
        )
        .contains("Language=3")
    );
    assert!(
        status(
            &mut fresh_dispatcher,
            &mut fresh_reader,
            &fresh_receiver,
            Some("WimpMode")
        )
        .contains("WimpMode=X1024 Y768 G256")
    );
    let (startup, startup_events) = run_configured_startup(Vec::new());
    startup.expect("Language=3 must boot into the desktop path");
    assert!(
        startup_events
            .iter()
            .any(|event| matches!(event, DisplayEvent::DesktopStarted))
    );

    let (mos_result, mos_output) = cli(
        &mut dispatcher,
        &mut mos_session,
        &receiver,
        "*CONFIGURE Language 0",
    );
    mos_result.unwrap();
    assert!(!mos_output.contains("CONFIGURE error:"), "{mos_output:?}");
    let (mos_startup, mos_events) = run_configured_startup(b"QUIT\r".to_vec());
    mos_startup.expect("Language=0 must retain the MOS command prompt path");
    assert!(
        mos_events
            .iter()
            .any(|event| matches!(event, DisplayEvent::WriteByte { byte: b'*', .. }))
    );
}
