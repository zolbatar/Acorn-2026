use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
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
const OS_MODULE: u32 = 0x1E;
const CONFIG_FORWARD: u32 = 0x4FF80;
const CLI_ADDRESS: u32 = 0x2100;
const MODULE_PATH_ADDRESS: u32 = 0x2200;
const OLD_CONFIG_SCRATCH_START: u32 = 0x5000;
const OLD_CONFIG_SCRATCH_LENGTH: usize = 0x0C00;

const CONFIG_FORWARDER: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ConfigureForwarder 1.0.0
REM @SWI Configure_Forward &4FF80 Forward REGISTERS=R0:U32:OUT
DEF PROC Forward
    REM private-configuration-source-marker
    SYS "OS_CLI", &2100
    R0% = 73
ENDPROC
"#;

const FORGED_CONFIG_WRITER: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ForgedConfigWriter 1.0.0
REM @CAPABILITY ConfigurationStoreWrite
REM @IMPORT Host.Configuration.WriteValue ConfigurationStoreWrite
REM @SWI ForgedConfig_Probe &4FF81 Probe REGISTERS=R0:U32:OUT
DEF PROC Probe
    R0% = 0
ENDPROC
"#;

struct IsolatedEnvironment {
    root: PathBuf,
    config_path: PathBuf,
    config_parent: PathBuf,
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
            "ricochet-ricochet-mos-config-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let config_parent = root.join("settings");
        fs::create_dir_all(&config_parent).unwrap();
        let config_path = config_parent.join("configure");
        let old_config_path = std::env::var_os("RICOCHET_CONFIG_PATH");
        let old_demo_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        // This test is one test per integration binary; isolate both process
        // inputs before any dispatcher or Runtime reads them.
        unsafe {
            std::env::set_var("RICOCHET_CONFIG_PATH", &config_path);
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
        }
        Self {
            root,
            config_path,
            config_parent,
            old_config_path,
            old_demo_volume,
        }
    }

    fn make_config_parent_read_only(&self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.config_parent, fs::Permissions::from_mode(0o555)).unwrap();
        }
    }

    fn restore_config_parent_writable(&self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&self.config_parent, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        self.restore_config_parent_writable();
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

fn write_guest_module(root: &Path, file_stem: &str, source: &str) {
    fs::write(root.join(format!("{file_stem}.bas64")), source).unwrap();
    fs::write(
        root.join(format!("{file_stem}.bas64.ricochetmeta")),
        format!(
            "Ricochet file metadata v1\nformat-version=1\nguest-name={file_stem}\nfile-type=0x00000064\nload-address=0x00000000\nexecution-address=0x00000000\nattributes=0x00000000\n"
        ),
    )
    .unwrap();
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
) -> (Result<(), RuntimeError>, SwiContext, String) {
    let _ = receiver.try_iter().count();
    put_string(task, CLI_ADDRESS, command);
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    if result.is_ok() {
        assert_eq!(
            context.registers[0], CLI_ADDRESS,
            "successful OS_CLI must preserve R0 for {command:?}"
        );
    }
    let output = receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    (
        result,
        context,
        String::from_utf8_lossy(&output).into_owned(),
    )
}

fn load_guest(
    dispatcher: &mut SwiDispatcher,
    manager: &mut Task,
    guest_path: &str,
) -> Result<(), RuntimeError> {
    put_string(manager, MODULE_PATH_ADDRESS, guest_path);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = MODULE_PATH_ADDRESS;
    dispatcher.dispatch(OS_MODULE, manager, &mut context)
}

fn assert_write_denied_output(output: &str) {
    assert!(
        output.contains("CONFIGURE error:")
            && output.contains("caller task lacks configuration-write authority"),
        "expected a visible configuration-write denial, got {output:?}"
    );
}

fn settings_status(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    option: &str,
) -> String {
    let (result, _, output) = cli(dispatcher, task, receiver, &format!("*STATUS {option}"));
    result.expect("STATUS is public-read for every caller profile");
    output
}

fn run_configured_startup(
    input: Vec<u8>,
) -> (Result<(), RuntimeError>, Vec<DisplayEvent>, Arc<WimpServer>) {
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
                .expect("configured startup should finish after its selected path")
                .map(|result| result),
        );
    }
    runtime_thread.join().unwrap();
    (finished.expect("runtime result recorded"), events, wimp)
}

#[test]
fn ricochet_configure_status_persist_and_enforce_task_scoped_write_authority() {
    let isolated = IsolatedEnvironment::new();
    write_guest_module(&isolated.root, "ConfigureForwarder", CONFIG_FORWARDER);
    write_guest_module(&isolated.root, "ForgedConfigWriter", FORGED_CONFIG_WRITER);
    // A no-dot fixture lets the legacy *CA. catalogue abbreviation be
    // distinguished from *C., whose current dispatcher order selects CONFIGURE.
    fs::write(
        isolated.root.join("ConfigMarker"),
        b"legacy-catalogue-route",
    )
    .unwrap();

    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut dispatcher =
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    let mut ordinary = Task::new(0xC0_001);
    let mut source_only = Task::trusted_source_inspector(0xC0_002);
    let mut module_only = Task::trusted_module_manager(0xC0_003);
    let mut config_only = Task::trusted_configuration_manager(0xC0_004);
    let mut mos_session = Task::trusted_mos_session(0xC0_005);
    let mut same_id_ordinary = Task::new(mos_session.id);

    // OS_CLI only declares its input string at R0. Scratch data therefore
    // must not be written to arbitrary fixed addresses in the caller's memory.
    let scratch_sentinel = vec![0xA5; OLD_CONFIG_SCRATCH_LENGTH];
    ordinary
        .memory
        .write_bytes(OLD_CONFIG_SCRATCH_START, &scratch_sentinel)
        .unwrap();
    let ordinary_area_count = ordinary.memory.dynamic_area_count();
    let (status_result, _, status_defaults) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*STATUS");
    assert_eq!(
        ordinary.memory.dynamic_area_count(),
        ordinary_area_count,
        "successful STATUS must release its temporary scratch area"
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_CONFIG_SCRATCH_START, OLD_CONFIG_SCRATCH_LENGTH)
            .unwrap(),
        scratch_sentinel,
        "*STATUS must not clobber unrelated caller memory at the former fixed scratch range"
    );
    status_result.expect("initial *STATUS must be available to an ordinary task");
    assert!(
        status_defaults.contains("Language=0"),
        "{status_defaults:?}"
    );
    assert!(
        status_defaults.contains("BASICMode=AUTO"),
        "{status_defaults:?}"
    );
    assert!(
        status_defaults.contains("BASICEngine=INTERPRETER"),
        "{status_defaults:?}"
    );

    // Public status reads do not require a ConfigurationRead grant; all
    // restrictive task profiles can inspect the saved settings.
    for task in [&mut source_only, &mut module_only, &mut config_only] {
        assert!(
            settings_status(&mut dispatcher, task, &display_receiver, "Language")
                .contains("Language=0")
        );
    }

    for task in [
        &mut ordinary,
        &mut source_only,
        &mut module_only,
        &mut same_id_ordinary,
    ] {
        let task_area_count = task.memory.dynamic_area_count();
        let (result, _, output) = cli(
            &mut dispatcher,
            task,
            &display_receiver,
            "*CONFIGURE BASICEngine STRICT",
        );
        result.expect("CONFIGURE reports policy denial through its MOS output path");
        assert_write_denied_output(&output);
        assert_eq!(
            task.memory.dynamic_area_count(),
            task_area_count,
            "denied CONFIGURE must release its temporary scratch area"
        );
        let task_area_count = task.memory.dynamic_area_count();
        let (_, _, defaults) = cli(
            &mut dispatcher,
            task,
            &display_receiver,
            "*CONFIGURE DEFAULTS",
        );
        assert_write_denied_output(&defaults);
        assert_eq!(
            task.memory.dynamic_area_count(),
            task_area_count,
            "denied DEFAULTS must release its temporary scratch area"
        );
    }
    assert!(
        !isolated.config_path.exists(),
        "denied write/default commands must not create a configuration file"
    );
    assert!(
        settings_status(
            &mut dispatcher,
            &mut ordinary,
            &display_receiver,
            "BASICEngine"
        )
        .contains("BASICEngine=INTERPRETER")
    );

    // A ConfigurationWrite-only host task can persist settings, while the
    // command/module provider cannot grant it extra source or module rights.
    config_only
        .memory
        .write_bytes(OLD_CONFIG_SCRATCH_START, &scratch_sentinel)
        .unwrap();
    let config_only_area_count = config_only.memory.dynamic_area_count();
    let (configured, _, output) = cli(
        &mut dispatcher,
        &mut config_only,
        &display_receiver,
        "*cOnF. bAsIcEnGiNe sTrIcT",
    );
    assert_eq!(
        config_only.memory.dynamic_area_count(),
        config_only_area_count,
        "successful CONFIGURE must release its temporary scratch area"
    );
    assert_eq!(
        config_only
            .memory
            .read_bytes(OLD_CONFIG_SCRATCH_START, OLD_CONFIG_SCRATCH_LENGTH)
            .unwrap(),
        scratch_sentinel,
        "*CONFIGURE must not clobber unrelated caller memory at the former fixed scratch range"
    );
    configured.expect("configuration manager write should succeed");
    assert!(output.contains("BASICEngine set to STRICT"), "{output:?}");
    let (language_set, _, language_output) = cli(
        &mut dispatcher,
        &mut mos_session,
        &display_receiver,
        "*CONFIGURE Language 3",
    );
    language_set.unwrap();
    assert!(language_output.contains("Language set to 3"));
    let (mode_set, _, mode_output) = cli(
        &mut dispatcher,
        &mut config_only,
        &display_receiver,
        "*CONFIGURE BASICMode BASIC64",
    );
    mode_set.unwrap();
    assert!(mode_output.contains("BASICMode set to BASIC64"));
    for command in [
        "*CONFIGURE BASICProfile BBCV-1.05",
        "*CONFIGURE BASICTarget Agon",
        "*CONFIGURE WimpMode X640 Y480 C32K",
    ] {
        let (result, _, output) = cli(
            &mut dispatcher,
            &mut config_only,
            &display_receiver,
            command,
        );
        result.unwrap_or_else(|error| panic!("valid setting {command:?} failed: {error:?}"));
        assert!(
            !output.contains("CONFIGURE error:"),
            "valid setting {command:?} was rejected: {output:?}"
        );
    }
    let max_profile_name = "P".repeat(232);
    let (max_profile_set, _, max_profile_output) = cli(
        &mut dispatcher,
        &mut config_only,
        &display_receiver,
        &format!("*C. BASICPROFILE {max_profile_name}"),
    );
    max_profile_set.expect("232-byte BASICProfile value fits the OS_CLI and config contract");
    assert!(
        max_profile_output.contains(&format!("BASICProfile set to {max_profile_name}")),
        "maximum BASICProfile should be accepted and preserved: {max_profile_output:?}"
    );
    let (profile_status, _, profile_status_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*STATUS BASICProfile",
    );
    profile_status.unwrap();
    assert!(
        profile_status_output.contains(&format!("BASICProfile={max_profile_name}")),
        "maximum BASICProfile did not round-trip through STATUS: {profile_status_output:?}"
    );
    let overlong_profile_name = "Q".repeat(233);
    let (overlong_profile_set, _, overlong_profile_output) = cli(
        &mut dispatcher,
        &mut config_only,
        &display_receiver,
        &format!("*C. BASICPROFILE {overlong_profile_name}"),
    );
    overlong_profile_set.expect("overlong BASICProfile is a user diagnostic, not dispatch error");
    assert!(
        overlong_profile_output.contains("CONFIGURE error:"),
        "233-byte BASICProfile should be rejected: {overlong_profile_output:?}"
    );
    let (profile_unchanged, _, profile_unchanged_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*STATUS BASICProfile",
    );
    profile_unchanged.unwrap();
    assert!(
        profile_unchanged_output.contains(&format!("BASICProfile={max_profile_name}")),
        "rejected overlong profile changed the stored value: {profile_unchanged_output:?}"
    );
    let (all_status, _, all_status_output) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*STATUS");
    all_status.unwrap();
    assert!(all_status_output.contains("Language=3"));
    assert!(!all_status_output.contains("WindowFurniture"));
    assert!(all_status_output.contains("BASICMode=BASIC64"));
    assert!(
        all_status_output.contains(&format!("BASICProfile={max_profile_name}")),
        "all-status truncated the maximum BASICProfile: {all_status_output:?}"
    );
    assert!(all_status_output.contains("BASICTarget=AGON"));
    assert!(all_status_output.contains("BASICEngine=STRICT"));
    assert!(all_status_output.contains("WimpMode=X640 Y480 C32K"));
    assert!(!all_status_output.contains("RicochetOutputProfile"));

    // Exact final-dot abbreviations follow the existing hosted command order:
    // *CONF. is the long command abbreviation, *C. claims CONFIGURE before CAT,
    // *CA. remains catalogue, and *S. selects STATUS.
    let (configure_syntax, _, configure_syntax_output) =
        cli(&mut dispatcher, &mut mos_session, &display_receiver, "*C.");
    configure_syntax.unwrap();
    assert!(configure_syntax_output.contains("Syntax: *CONFIGURE"));
    let (catalogue, _, catalogue_output) =
        cli(&mut dispatcher, &mut mos_session, &display_receiver, "*CA.");
    catalogue.unwrap();
    assert!(catalogue_output.contains("ConfigMarker"));
    let (status_abbrev, _, status_abbrev_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*S. BASICEngine",
    );
    status_abbrev.unwrap();
    assert!(status_abbrev_output.contains("BASICEngine=STRICT"));
    let (status_prefix, _, status_prefix_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*STAT. Language",
    );
    status_prefix.unwrap();
    assert!(status_prefix_output.contains("Language=3"));

    // Invalid option/value/arity diagnostics must not change either saved
    // field; option and value matching remain case-insensitive.
    for command in [
        "*CONFIGURE Language 4",
        "*CONFIGURE BASICEngine FAST",
        "*CONFIGURE WindowFurniture Flat",
        "*CONFIGURE DisplayResolution 640x480",
        "*CONFIGURE DisplayColour 32KRGB555",
        "*CONFIGURE RicochetOutputProfile 32KRGB555",
        "*CONFIGURE NoSuchOption 1",
        "*CONFIGURE Language",
        "*CONFIGURE Language 0 extra",
    ] {
        let (result, _, output) = cli(
            &mut dispatcher,
            &mut mos_session,
            &display_receiver,
            command,
        );
        result.expect("invalid configure syntax/value is a MOS diagnostic");
        assert!(
            output.contains("CONFIGURE error:") || output.contains("Syntax: *CONFIGURE"),
            "invalid input {command:?} was not diagnosed: {output:?}"
        );
    }
    assert!(
        settings_status(
            &mut dispatcher,
            &mut ordinary,
            &display_receiver,
            "Language"
        )
        .contains("Language=3")
    );
    assert!(
        settings_status(
            &mut dispatcher,
            &mut ordinary,
            &display_receiver,
            "BASICEngine"
        )
        .contains("BASICEngine=STRICT")
    );
    let (unknown_status, _, unknown_status_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*STATUS NoSuchOption",
    );
    unknown_status.unwrap();
    assert!(unknown_status_output.contains("STATUS error:"));
    let (status_extra, _, status_extra_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*STATUS Language extra",
    );
    status_extra.unwrap();
    assert!(status_extra_output.contains("Syntax: *STATUS [option]"));

    // Load a guest wrapper through the normal module service. Its nested OS_CLI
    // call must retain the original caller principal instead of inheriting the
    // privileged RicochetCommands provider's ConfigurationWrite right.
    let mut module_manager = Task::trusted_module_manager(0xC0_006);
    load_guest(&mut dispatcher, &mut module_manager, "ConfigureForwarder")
        .expect("the ordinary forwarder has no protected capability import");
    let config_before_forged_import = fs::read(&isolated.config_path).unwrap();
    let forged_import = load_guest(&mut dispatcher, &mut module_manager, "ForgedConfigWriter");
    assert!(
        matches!(
            &forged_import,
            Err(RuntimeError::Structured { type_name, .. })
                if type_name == "ModuleCapabilityDenied"
        ),
        "guest must not self-grant the configuration storage capability: {forged_import:?}"
    );
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        config_before_forged_import
    );
    put_string(
        &mut ordinary,
        CLI_ADDRESS,
        "*CONFIGURE BASICEngine INTERPRETER",
    );
    let before_nested = fs::read(&isolated.config_path).unwrap();
    let mut nested = SwiContext::default();
    dispatcher
        .dispatch(CONFIG_FORWARD, &mut ordinary, &mut nested)
        .expect("nested OS_CLI should return after printing a policy error");
    assert_eq!(nested.registers[0], 73);
    let nested_output = display_receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_write_denied_output(&String::from_utf8_lossy(&nested_output));
    assert_eq!(fs::read(&isolated.config_path).unwrap(), before_nested);

    let (forwarded, _, forwarded_output) = cli(
        &mut dispatcher,
        &mut config_only,
        &display_receiver,
        "*STATUS Language",
    );
    forwarded.unwrap();
    assert!(forwarded_output.contains("Language=3"));
    put_string(
        &mut config_only,
        CLI_ADDRESS,
        "*CONFIGURE BASICEngine INTERPRETER",
    );
    let mut nested_allowed = SwiContext::default();
    dispatcher
        .dispatch(CONFIG_FORWARD, &mut config_only, &mut nested_allowed)
        .expect("nested OS_CLI should retain an explicitly authorized caller's right");
    assert_eq!(nested_allowed.registers[0], 73);
    let nested_allowed_output = display_receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        String::from_utf8_lossy(&nested_allowed_output).contains("BASICEngine set to INTERPRETER")
    );

    // The public *INSPECT and non-Ricochet Help routes remain available after
    // moving Configure/Status policy into the BASIC64 command module.
    let ordinary_area_count = ordinary.memory.dynamic_area_count();
    let (modules, _, modules_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT MODULES",
    );
    assert_eq!(
        ordinary.memory.dynamic_area_count(),
        ordinary_area_count,
        "successful *INSPECT inspection must release its temporary scratch area"
    );
    modules.unwrap();
    assert!(modules_output.contains("RicochetCommands"));
    let (help, _, help_output) = cli(&mut dispatcher, &mut ordinary, &display_receiver, "*HELP");
    help.unwrap();
    assert!(help_output.contains("Ricochet MOS commands"));
    assert!(help_output.contains("*CONFIGURE"));

    // A new dispatcher reads the persisted values through the same public CLI
    // and demonstrates that Language and BASICEngine are independent fields.
    let (_fresh_input_sender, fresh_input_receiver) = mpsc::channel();
    let (fresh_display_sender, fresh_display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut fresh_dispatcher = SwiDispatcher::windowed(
        HostConsole::windowed(fresh_input_receiver),
        fresh_display_sender,
    );
    let mut fresh_task = Task::new(0xC0_007);
    assert!(
        settings_status(
            &mut fresh_dispatcher,
            &mut fresh_task,
            &fresh_display_receiver,
            "Language"
        )
        .contains("Language=3")
    );
    assert!(
        settings_status(
            &mut fresh_dispatcher,
            &mut fresh_task,
            &fresh_display_receiver,
            "BASICEngine"
        )
        .contains("BASICEngine=INTERPRETER")
    );

    // The DEFAULTS branch restores every documented field, not just startup
    // and engine controls. Then only the values needed for startup/engine
    // acceptance are changed again.
    let (prestartup_defaults, _, prestartup_defaults_output) = cli(
        &mut dispatcher,
        &mut config_only,
        &display_receiver,
        "*CONFIGURE DEFAULTS",
    );
    prestartup_defaults.unwrap();
    assert!(prestartup_defaults_output.contains("defaults"));
    let (default_table, _, default_table_output) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*STATUS");
    default_table.unwrap();
    for expected in [
        "Language=0",
        "BASICMode=AUTO",
        "BASICProfile=AUTO",
        "BASICTarget=AUTO",
        "BASICEngine=INTERPRETER",
        "WimpMode=AUTO",
    ] {
        assert!(
            default_table_output.contains(expected),
            "DEFAULTS omitted or drifted {expected:?}: {default_table_output:?}"
        );
    }
    assert_eq!(
        default_table_output, status_defaults,
        "BASIC64 *CONFIGURE DEFAULTS disagrees with the no-file typed defaults"
    );
    assert!(!default_table_output.contains("WindowFurniture"));
    for command in [
        "*CONFIGURE Language 3",
        "*CONFIGURE BASICMode BASIC64",
        "*CONFIGURE BASICEngine STRICT",
    ] {
        let (result, _, output) = cli(
            &mut dispatcher,
            &mut config_only,
            &display_receiver,
            command,
        );
        result
            .unwrap_or_else(|error| panic!("startup/engine setting {command:?} failed: {error:?}"));
        assert!(!output.contains("CONFIGURE error:"), "{output:?}");
    }

    // Verify the saved Language value still controls Boot's startup choice.
    // The Wimp handoff path emits DesktopStarted without first showing a MOS
    // prompt; the complementary zero path consumes QUIT from the prompt.
    let (desktop_start, desktop_events, desktop_wimp) = run_configured_startup(Vec::new());
    desktop_start.expect("Language 3 desktop startup should stop cleanly");
    assert!(
        desktop_events
            .iter()
            .any(|event| matches!(event, DisplayEvent::DesktopStarted))
    );
    assert!(
        !desktop_events
            .iter()
            .any(|event| matches!(event, DisplayEvent::WriteByte { .. })),
        "Language 3 displayed a MOS prompt before desktop handoff"
    );
    assert!(desktop_wimp.display_settings() == DisplaySettings::default());

    let (to_default, _, reset_output) = cli(
        &mut dispatcher,
        &mut mos_session,
        &display_receiver,
        "*CONFIGURE Language 0",
    );
    to_default.unwrap();
    assert!(reset_output.contains("Language set to 0"));
    let (mos_start, mos_events, _) = run_configured_startup(b"QUIT\r".to_vec());
    mos_start.expect("Language 0 should return cleanly through the MOS prompt");
    assert!(
        mos_events
            .iter()
            .any(|event| matches!(event, DisplayEvent::WriteByte { byte: b'*', .. }))
    );
    assert!(
        !mos_events
            .iter()
            .any(|event| matches!(event, DisplayEvent::DesktopStarted))
    );

    // BASICEngine remains independent from the startup Language and is used
    // by subsequently spawned BASIC tasks. Strict rejects this unsupported
    // dynamic allocation, while Interpreter accepts the same guest source.
    let (set_strict, _, _) = cli(
        &mut dispatcher,
        &mut mos_session,
        &display_receiver,
        "*CONFIGURE BASICEngine STRICT",
    );
    set_strict.unwrap();
    let (updates, _update_receiver) = mpsc::channel();
    let engine_wimp = WimpServer::new(updates);
    let (_strict_input_sender, strict_input_receiver) = mpsc::channel();
    let (strict_display_sender, _strict_display_receiver) = mpsc::channel();
    let mut strict_runtime = Runtime::desktop_task(
        0xC0_008,
        strict_input_receiver,
        strict_display_sender,
        Arc::clone(&engine_wimp),
    );
    assert!(
        strict_runtime.run_application("10 DIM A\n20 END").is_err(),
        "BASICEngine STRICT did not govern the next spawned program"
    );
    let (set_interpreter, _, _) = cli(
        &mut dispatcher,
        &mut mos_session,
        &display_receiver,
        "*CONFIGURE BASICEngine INTERPRETER",
    );
    set_interpreter.unwrap();
    let (_reference_input_sender, reference_input_receiver) = mpsc::channel();
    let (reference_display_sender, _reference_display_receiver) = mpsc::channel();
    let mut interpreter_runtime = Runtime::desktop_task(
        0xC0_009,
        reference_input_receiver,
        reference_display_sender,
        Arc::clone(&engine_wimp),
    );
    interpreter_runtime
        .run_application("10 DIM A\n20 END")
        .expect("the same program should run under the configured interpreter");

    // RICOCHET_DISPLAY is a second public persistence writer. Its query remains
    // readable to an ordinary desktop task, while APPLY uses the same
    // ConfigurationWrite right as CONFIGURE. The current public Runtime API
    // cannot combine a SourceRead-only/ModuleManagement-only Task with an
    // attached Wimp, so those profiles are checked through CONFIGURE above.
    let (display_updates, _display_updates_receiver) = mpsc::channel();
    let display_wimp = WimpServer::new(display_updates);
    let initial_display = display_wimp.display_settings();
    let bytes_before_denied_apply = fs::read(&isolated.config_path).unwrap();
    let (_ordinary_input_sender, ordinary_input_receiver) = mpsc::channel();
    let (ordinary_display_sender, _ordinary_display_receiver) = mpsc::channel();
    let mut ordinary_display_runtime = Runtime::desktop_task(
        0xC0_010,
        ordinary_input_receiver,
        ordinary_display_sender,
        Arc::clone(&display_wimp),
    );
    ordinary_display_runtime
        .run_application("REM @BASIC64 MODE=BASIC64\nSYS \"RICOCHET_DISPLAY\", 1, 0\n")
        .expect("ordinary desktop task may query public display settings");
    assert_eq!(display_wimp.display_settings(), initial_display);
    let apply_display_source = format!(
        "REM @BASIC64 MODE=BASIC64\nSYS \"RICOCHET_DISPLAY\", 1, 1, {}, {}\n",
        DesktopResolution::R640x480.id(),
        DisplayColour::Rgb555.id()
    );
    let denied_display_apply = ordinary_display_runtime
        .run_application(&apply_display_source)
        .expect_err("ordinary BASIC task must not persist display settings");
    assert!(
        matches!(
            &denied_display_apply,
            RuntimeError::Structured { type_name, code: 4, message }
                if type_name == "TaskAuthorizationDenied"
                    && message == "caller task lacks configuration-write authority"
        ),
        "display APPLY returned an unexpected authorization result: {denied_display_apply:?}"
    );
    assert_eq!(display_wimp.display_settings(), initial_display);
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        bytes_before_denied_apply
    );

    let (_mos_input_sender, mos_input_receiver) = mpsc::channel();
    let (mos_display_sender, _mos_display_receiver) = mpsc::channel();
    let mut mos_display_runtime = Runtime::windowed_with_desktop(
        mos_input_receiver,
        mos_display_sender,
        Arc::clone(&display_wimp),
    );
    mos_display_runtime
        .run_application(&apply_display_source)
        .expect("trusted interactive MOS session should retain display-write authority");
    assert_eq!(
        display_wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::R640x480,
            colour: DisplayColour::Rgb555,
        }
    );

    // A failed atomic replacement must not be reported as success or change
    // the in-memory view. A read-only settings directory makes creation of
    // the sibling temporary file fail before rename.
    let persisted_before_failure = fs::read(&isolated.config_path).unwrap();
    isolated.make_config_parent_read_only();
    let mos_session_area_count = mos_session.memory.dynamic_area_count();
    let (failed_write, _, failed_write_output) = cli(
        &mut dispatcher,
        &mut mos_session,
        &display_receiver,
        "*CONFIGURE BASICEngine STRICT",
    );
    assert_eq!(
        mos_session.memory.dynamic_area_count(),
        mos_session_area_count,
        "failed CONFIGURE storage write must release its temporary scratch area"
    );
    failed_write.expect("storage failure is reported as a MOS diagnostic");
    assert!(
        failed_write_output.contains("CONFIGURE error:"),
        "failed storage write looked successful: {failed_write_output:?}"
    );
    assert_eq!(
        fs::read(&isolated.config_path).unwrap(),
        persisted_before_failure
    );
    isolated.restore_config_parent_writable();
    assert!(
        settings_status(
            &mut dispatcher,
            &mut ordinary,
            &display_receiver,
            "BASICEngine"
        )
        .contains("BASICEngine=INTERPRETER")
    );
    let (_verify_input_sender, verify_input_receiver) = mpsc::channel();
    let (verify_display_sender, verify_display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut verify_dispatcher = SwiDispatcher::windowed(
        HostConsole::windowed(verify_input_receiver),
        verify_display_sender,
    );
    let mut verify_task = Task::new(0xC0_011);
    assert!(
        settings_status(
            &mut verify_dispatcher,
            &mut verify_task,
            &verify_display_receiver,
            "BASICEngine"
        )
        .contains("BASICEngine=INTERPRETER")
    );

    // DEFAULTS is also a write: a read-only task cannot reset the file, while
    // an explicitly authorized configuration manager can restore all defaults.
    let (_, _, denied_defaults) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*CONFIGURE DEFAULTS",
    );
    assert_write_denied_output(&denied_defaults);
    let (defaults_result, _, defaults_output) = cli(
        &mut dispatcher,
        &mut config_only,
        &display_receiver,
        "*CONFIGURE DEFAULTS",
    );
    defaults_result.unwrap();
    assert!(defaults_output.contains("defaults"));
    assert!(
        settings_status(
            &mut dispatcher,
            &mut ordinary,
            &display_receiver,
            "Language"
        )
        .contains("Language=0")
    );
    assert!(
        settings_status(
            &mut dispatcher,
            &mut ordinary,
            &display_receiver,
            "BASICEngine"
        )
        .contains("BASICEngine=INTERPRETER")
    );
}
