use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use acorn_2026::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiContext, SwiDispatcher},
};

const OS_CLI: u32 = 0x05;
const OS_MODULE: u32 = 0x1E;
const ACORN_MODULE_LOOKUP: u32 = 0x4FF12;
const ACORN_SWI_INFO: u32 = 0x4FF13;
const ACORN_DEFINITION_SOURCE: u32 = 0x4FF15;
const CLASSIC_PROBE: u32 = 0x4FF60;
const CLASSIC_FORWARD: u32 = 0x4FF61;

const CLI_ADDRESS: u32 = 0x2100;
const TITLE_ADDRESS: u32 = 0x3000;
const SOURCE_SELECTOR_ADDRESS: u32 = 0x3100;
const SOURCE_BUFFER_ADDRESS: u32 = 0x3200;
const SOURCE_PATH_ADDRESS: u32 = 0x3700;
const OLD_SCRATCH_SENTINEL_A: u32 = 0x5000;
const OLD_SCRATCH_SENTINEL_B: u32 = 0x5800;

const V1_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ClassicCommandsGuest 2.1.0
REM @STATE COUNT% UINT32
REM @SWI ClassicCommands_Probe &4FF60 Probe REGISTERS=R0:U32:OUT
REM @PRIVATE PROC SecretRoutine
DEF PROC Probe
    COUNT% = COUNT% + 1
    R0% = COUNT% + 100
ENDPROC
DEF PROC SecretRoutine
    REM classic-command-private-source-v1
ENDPROC
"#;

const V2_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ClassicCommandsGuest 2.1.0
REM @STATE COUNT% UINT32
REM @SWI ClassicCommands_Probe &4FF60 Probe REGISTERS=R0:U32:OUT
REM @PRIVATE PROC SecretRoutine
DEF PROC Probe
    COUNT% = COUNT% + 1
    R0% = COUNT% + 200
ENDPROC
DEF PROC SecretRoutine
    REM classic-command-private-source-v2
ENDPROC
"#;

const INCOMPATIBLE_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ClassicCommandsGuest 2.1.0
REM @STATE COUNT% UINT32
REM @STATE EXTRA% UINT32
REM @SWI ClassicCommands_Probe &4FF60 Probe REGISTERS=R0:U32:OUT
REM @PRIVATE PROC SecretRoutine
DEF PROC Probe
    COUNT% = COUNT% + 1
    R0% = COUNT% + 900
ENDPROC
DEF PROC SecretRoutine
    REM incompatible-candidate-source
ENDPROC
"#;

const RUN_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ClassicRunGuest 1.0.0
REM @SWI ClassicRun_Probe &4FF62 Probe REGISTERS=R0:U32:OUT
DEF PROC Probe
    R0% = 62
ENDPROC
"#;

const FORWARDER_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ClassicCommandForwarder 1.0.0
REM @SWI ClassicCommands_Forward &4FF61 Forward REGISTERS=R0:U32:OUT
DEF PROC Forward
    SYS "OS_CLI", &2100
    R0% = 77
ENDPROC
"#;

struct IsolatedEnvironment {
    root: PathBuf,
    old_config_path: Option<std::ffi::OsString>,
    old_demo_volume: Option<std::ffi::OsString>,
}

impl IsolatedEnvironment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "acorn-trellis-classic-commands-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let old_config_path = std::env::var_os("ACORN_CONFIG_PATH");
        let old_demo_volume = std::env::var_os("ACORN_DEMO_VOLUME");
        unsafe {
            std::env::set_var("ACORN_CONFIG_PATH", root.join("isolated-configure"));
            std::env::set_var("ACORN_DEMO_VOLUME", &root);
        }
        Self {
            root,
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

fn write_guest_module(root: &Path, file_stem: &str, source: &str) {
    fs::write(root.join(format!("{file_stem}.bas64")), source).unwrap();
    fs::write(
        root.join(format!("{file_stem}.bas64.acornmeta")),
        format!(
            "Acorn-2026 file metadata v1\nformat-version=1\nguest-name={file_stem}\nfile-type=0x00000064\nload-address=0x00000000\nexecution-address=0x00000000\nattributes=0x00000000\n"
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

fn read_string(task: &Task, address: u32, capacity: usize) -> String {
    String::from_utf8(task.memory.read_c_string(address, capacity).unwrap()).unwrap()
}

fn contains_pointer_looking_hex(output: &str) -> bool {
    output.split_whitespace().any(|token| {
        let hex = token.trim_start_matches('&').trim_start_matches("0x");
        hex.len() >= 7 && hex.chars().all(|character| character.is_ascii_hexdigit())
    })
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
        "OS_CLI must preserve R0 on normal and command-error paths for {command:?}"
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

fn module_info(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    title: &str,
) -> Option<([u32; 3], u32)> {
    put_string(task, TITLE_ADDRESS, title);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = TITLE_ADDRESS;
    dispatcher
        .dispatch(ACORN_MODULE_LOOKUP, task, &mut context)
        .unwrap();
    (context.registers[1] != 0).then_some((
        [
            context.registers[3],
            context.registers[4],
            context.registers[5],
        ],
        context.registers[6],
    ))
}

fn swi_generation(dispatcher: &mut SwiDispatcher, task: &mut Task, number: u32) -> Option<u32> {
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = number;
    context.registers[2] = SOURCE_BUFFER_ADDRESS;
    context.registers[3] = 128;
    context.registers[4] = SOURCE_BUFFER_ADDRESS + 0x100;
    context.registers[5] = 128;
    context.registers[6] = SOURCE_BUFFER_ADDRESS + 0x200;
    context.registers[7] = 128;
    match dispatcher.dispatch(ACORN_SWI_INFO, task, &mut context) {
        Ok(()) => {}
        Err(RuntimeError::Structured { type_name, .. }) if type_name == "SwiIdentityNotFound" => {
            return None;
        }
        Err(error) => panic!("unexpected SWI-info failure for {number:#x}: {error:?}"),
    }
    (context.registers[8] != 0).then_some(context.registers[8])
}

fn invoke(dispatcher: &mut SwiDispatcher, task: &mut Task, swi: u32) -> Result<u32, RuntimeError> {
    let mut context = SwiContext::default();
    dispatcher.dispatch(swi, task, &mut context)?;
    Ok(context.registers[0])
}

fn assert_cli_rejected(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> String {
    let (result, output) = cli(dispatcher, task, receiver, command);
    assert!(
        result.is_err()
            || [
                "unsupported",
                "unknown",
                "bad command",
                "syntax",
                "invalid",
                "not found",
                "ambiguous",
                "not available",
            ]
            .iter()
            .any(|term| output.to_ascii_lowercase().contains(term)),
        "{command:?} was neither rejected nor given an explicit diagnostic: {result:?}, {output:?}"
    );
    output
}

fn assert_explicitly_unsupported(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> String {
    let (result, output) = cli(dispatcher, task, receiver, command);
    let lower = output.to_ascii_lowercase();
    assert!(
        result.is_err()
            || [
                "unsupported",
                "not available",
                "unavailable",
                "requires native"
            ]
            .iter()
            .any(|term| lower.contains(term)),
        "{command:?} must fail explicitly as an unsupported hosted operation, got {result:?}, {output:?}"
    );
    output
}

#[test]
fn classic_module_commands_and_inspect_share_live_read_only_module_identity() {
    let environment = IsolatedEnvironment::new();
    write_guest_module(&environment.root, "ClassicV1", V1_SOURCE);
    write_guest_module(&environment.root, "ClassicV2", V2_SOURCE);
    write_guest_module(
        &environment.root,
        "ClassicIncompatible",
        INCOMPATIBLE_SOURCE,
    );
    write_guest_module(&environment.root, "ClassicRun", RUN_SOURCE);
    write_guest_module(&environment.root, "ClassicForwarder", FORWARDER_SOURCE);
    fs::write(
        environment.root.join("LegacyType"),
        "classic-cli-type-marker\n",
    )
    .unwrap();

    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut dispatcher =
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    let mut mos = Task::trusted_mos_session(0x53_0101);
    let mut ordinary = Task::new(0x53_0102);
    let mut source_inspector = Task::trusted_source_inspector(0x53_0103);
    let mut module_manager = Task::trusted_module_manager(0x53_0104);
    let mut same_id_ordinary = Task::new(source_inspector.id);

    for task in [
        &mut mos,
        &mut ordinary,
        &mut source_inspector,
        &mut module_manager,
        &mut same_id_ordinary,
    ] {
        task.memory
            .write_bytes(OLD_SCRATCH_SENTINEL_A, &[0xA1; 32])
            .unwrap();
        task.memory
            .write_bytes(OLD_SCRATCH_SENTINEL_B, &[0xB2; 32])
            .unwrap();
    }

    // Preserve the public bounded-string contract at the OS_CLI boundary.
    // A 256-byte unterminated line must not be parsed as a truncated command;
    // a high invalid logical pointer must not be aliased by masking.
    let unterminated = vec![b'X'; 256];
    mos.memory.write_bytes(CLI_ADDRESS, &unterminated).unwrap();
    let mut unterminated_context = SwiContext::default();
    unterminated_context.registers[0] = CLI_ADDRESS;
    let unterminated_result = dispatcher.dispatch(OS_CLI, &mut mos, &mut unterminated_context);
    assert!(
        matches!(unterminated_result, Err(RuntimeError::Memory(_)))
            || matches!(
                &unterminated_result,
                Err(RuntimeError::Program(message)) if message.contains("no terminator")
            ),
        "unterminated 256-byte OS_CLI input should be rejected before command parsing: {unterminated_result:?}"
    );
    assert_eq!(unterminated_context.registers[0], CLI_ADDRESS);
    let mut invalid_cli = SwiContext::default();
    invalid_cli.registers[0] = 0x4000_2100;
    assert!(matches!(
        dispatcher.dispatch(OS_CLI, &mut mos, &mut invalid_cli),
        Err(RuntimeError::Memory(_))
    ));
    assert_eq!(invalid_cli.registers[0], 0x4000_2100);
    let (empty_result, empty_output) = cli(&mut dispatcher, &mut mos, &display_receiver, "");
    assert!(empty_result.is_ok());
    assert!(empty_output.is_empty());
    let complete_256_byte_line = "X".repeat(255);
    let (bounded_result, bounded_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        &complete_256_byte_line,
    );
    assert!(
        bounded_result.is_ok(),
        "255-byte command plus NUL is within the PRM bound: {bounded_result:?}, {bounded_output:?}"
    );
    let _ = assert_cli_rejected(&mut dispatcher, &mut mos, &display_receiver, "*RMLoad");

    // Inspection is read-only and active module metadata is public. The new
    // command path must not make the interim *TRELLIS namespace public.
    let (_, before_any_guest) = cli(&mut dispatcher, &mut mos, &display_receiver, "*Modules");
    assert!(!before_any_guest.contains("ClassicCommandsGuest"));
    let (legacy_type_result, legacy_type_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*T. LegacyType",
    );
    assert!(legacy_type_result.is_ok());
    assert!(legacy_type_output.contains("classic-cli-type-marker"));
    let _ = assert_cli_rejected(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*TRELLIS MODULES ClassicCommandsGuest",
    );

    // RMLoad is the real reason-1 command route and is caller-authorized.
    // Metadata/entry identity remain absent after an ordinary denial.
    let (denied_load, denied_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RMLoad ClassicV1",
    );
    assert!(
        denied_load.is_err()
            || denied_output.to_ascii_lowercase().contains("authority")
            || denied_output.to_ascii_lowercase().contains("permission"),
        "ordinary *RMLoad was not explicitly denied: {denied_load:?}, {denied_output:?}"
    );
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert_eq!(
        module_info(&mut dispatcher, &mut mos, "ClassicCommandsGuest"),
        None
    );
    assert!(swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE).is_none());
    let mut direct_denied_load = SwiContext::default();
    direct_denied_load.registers[0] = 1;
    direct_denied_load.registers[1] = u32::MAX;
    assert!(matches!(
        dispatcher.dispatch(OS_MODULE, &mut ordinary, &mut direct_denied_load),
        Err(RuntimeError::Structured {
            ref type_name,
            code: 2,
            ..
        }) if type_name == "TaskAuthorizationDenied"
    ));
    assert_eq!(
        module_info(&mut dispatcher, &mut mos, "ClassicCommandsGuest"),
        None
    );

    let (load_result, load_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*RML. ClassicV1",
    );
    assert!(
        load_result.is_ok(),
        "authorized *RMLoad failed: {load_result:?}, {load_output:?}"
    );
    assert_eq!(
        module_info(&mut dispatcher, &mut mos, "ClassicCommandsGuest"),
        Some(([2, 1, 0], 4))
    );
    assert_eq!(
        swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE),
        Some(1)
    );
    assert_eq!(
        invoke(&mut dispatcher, &mut mos, CLASSIC_PROBE).unwrap(),
        101
    );
    assert_eq!(module_manager.memory.dynamic_area_count(), 0);

    let (source_only_load, source_only_load_output) = cli(
        &mut dispatcher,
        &mut source_inspector,
        &display_receiver,
        "*RMLoad ClassicV2",
    );
    assert!(
        source_only_load.is_err()
            || source_only_load_output
                .to_ascii_lowercase()
                .contains("authority"),
        "source-only task incorrectly managed a module: {source_only_load:?}, {source_only_load_output:?}"
    );
    assert_eq!(
        swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE),
        Some(1)
    );

    let (modules_result, modules_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*Modules",
    );
    assert!(modules_result.is_ok());
    assert!(modules_output.contains("ClassicCommandsGuest"));
    assert!(modules_output.contains("2.1.0"));
    assert!(modules_output.to_ascii_lowercase().contains("active"));
    assert!(
        !contains_pointer_looking_hex(&modules_output)
            && !modules_output.to_ascii_lowercase().contains("workspace"),
        "hosted *Modules must not fabricate RMA/workspace addresses: {modules_output:?}"
    );
    let (modules_abbrev_result, modules_abbrev_output) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*Mod.");
    assert!(
        modules_abbrev_result.is_ok(),
        "PRM example abbreviation *Mod. did not resolve: {modules_abbrev_output:?}"
    );
    assert!(modules_abbrev_output.contains("ClassicCommandsGuest"));
    let (modules_short_abbrev_result, modules_short_abbrev_output) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*M.");
    assert!(
        modules_short_abbrev_result.is_ok()
            && modules_short_abbrev_output.contains("ClassicCommandsGuest"),
        "with no competing M-command, the PRM any-prefix form *M. should resolve *Modules: {modules_short_abbrev_result:?}, {modules_short_abbrev_output:?}"
    );
    let (repeated_star_result, repeated_star_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "**Modules",
    );
    assert!(
        repeated_star_result.is_ok() && repeated_star_output.contains("ClassicCommandsGuest"),
        "PRM leading-star stripping should accept **Modules: {repeated_star_result:?}, {repeated_star_output:?}"
    );
    let (inspect_abbrev_result, inspect_abbrev_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPE. MODULES. ClassicCommandsGuest",
    );
    assert!(inspect_abbrev_result.is_ok());
    assert!(inspect_abbrev_output.contains("ClassicCommandsGuest"));
    let (inspect_short_abbrev_result, inspect_short_abbrev_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*I. MODULES. ClassicCommandsGuest",
    );
    assert!(
        inspect_short_abbrev_result.is_ok()
            && inspect_short_abbrev_output.contains("ClassicCommandsGuest"),
        "with no competing I-command, PRM any-prefix form *I. should resolve *INSPECT: {inspect_short_abbrev_result:?}, {inspect_short_abbrev_output:?}"
    );
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_A, 32)
            .unwrap(),
        vec![0xA1; 32]
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_B, 32)
            .unwrap(),
        vec![0xB2; 32]
    );

    let (inspect_list_result, inspect_list_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT MODULES ClassicCommandsGuest",
    );
    assert!(inspect_list_result.is_ok());
    assert!(inspect_list_output.contains("ClassicCommandsGuest"));
    assert!(inspect_list_output.contains("2.1.0"));
    assert!(inspect_list_output.to_ascii_lowercase().contains("active"));
    assert!(
        !inspect_list_output.contains("SWI ClassicCommands_Probe"),
        "MODULES is a list view and must not also dispatch the MODULE detail handler: {inspect_list_output:?}"
    );
    let (inspect_top_abbrev_result, inspect_top_abbrev_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPE. MODULES. ClassicCommandsGuest",
    );
    assert!(
        inspect_top_abbrev_result.is_ok(),
        "documented final-dot INSPECT abbreviation failed: {inspect_top_abbrev_output:?}"
    );
    assert!(inspect_top_abbrev_output.contains("ClassicCommandsGuest"));
    let (inspect_detail_abbrev_result, inspect_detail_abbrev_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT MODULE. ClassicCommandsGuest",
    );
    assert!(inspect_detail_abbrev_result.is_ok());
    assert!(inspect_detail_abbrev_output.contains("ClassicCommands_Probe"));
    let ambiguous_inspect = assert_cli_rejected(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT MOD. ClassicCommandsGuest",
    );
    assert!(!ambiguous_inspect.contains("ClassicCommands_Probe"));
    assert!(ambiguous_inspect.to_ascii_lowercase().contains("ambiguous"));
    let (inspect_module_result, inspect_module_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*inspect module ClassicCommandsGuest",
    );
    assert!(inspect_module_result.is_ok());
    assert!(inspect_module_output.contains("ClassicCommandsGuest"));
    assert!(inspect_module_output.contains("ClassicCommands_Probe"));
    assert!(inspect_module_output.contains("generation 1"));
    let (inspect_swi_result, inspect_swi_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT SWI ClassicCommands_Probe",
    );
    assert!(inspect_swi_result.is_ok());
    assert!(inspect_swi_output.contains("ClassicCommandsGuest"));
    assert!(inspect_swi_output.contains("Probe"));
    assert!(inspect_swi_output.contains("generation 1"));
    let (inspect_swi_number_result, inspect_swi_number_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT SWI &4FF60",
    );
    assert!(inspect_swi_number_result.is_ok());
    assert!(inspect_swi_number_output.contains("ClassicCommands_Probe"));
    assert!(inspect_swi_number_output.contains("generation 1"));
    for (command, expected_syntax) in [
        ("*INSPECT MODULE", "syntax: *inspect module"),
        ("*INSPECT SWI", "syntax: *inspect swi"),
        ("*INSPECT DEFINITION", "syntax: *inspect definition"),
    ] {
        let (result, output) = cli(&mut dispatcher, &mut ordinary, &display_receiver, command);
        assert!(
            result.is_err() || output.to_ascii_lowercase().contains(expected_syntax),
            "wrong-arity INSPECT did not return its public syntax for {command:?}: {result:?}, {output:?}"
        );
        assert!(!output.contains("TRELLIS"));
    }

    // Definition inspection requires SourceRead even though module/SWI
    // identity is public. The direct query has the same gate and preserves
    // the caller's output sentinels on denial.
    let (denied_definition, denied_definition_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT DEFINITION ClassicCommandsGuest/SecretRoutine",
    );
    assert!(
        matches!(
            &denied_definition,
            Err(RuntimeError::Structured { type_name, code: 1, .. })
                if type_name == "TaskAuthorizationDenied"
        ),
        "ordinary *INSPECT DEFINITION should match the source-query denial: {denied_definition:?}, {denied_definition_output:?}"
    );
    assert!(!denied_definition_output.contains("classic-command-private-source-v1"));
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_A, 32)
            .unwrap(),
        vec![0xA1; 32]
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_B, 32)
            .unwrap(),
        vec![0xB2; 32]
    );
    let (manager_source_denial, manager_source_denial_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*INSPECT DEFINITION ClassicCommandsGuest/SecretRoutine",
    );
    assert!(matches!(
        manager_source_denial,
        Err(RuntimeError::Structured { ref type_name, code: 1, .. })
            if type_name == "TaskAuthorizationDenied"
    ));
    assert!(!manager_source_denial_output.contains("classic-command-private-source-v1"));
    put_string(
        &mut ordinary,
        SOURCE_SELECTOR_ADDRESS,
        "ClassicCommandsGuest/SecretRoutine",
    );
    ordinary
        .memory
        .write_bytes(SOURCE_BUFFER_ADDRESS, &[0xA5; 32])
        .unwrap();
    ordinary
        .memory
        .write_bytes(SOURCE_PATH_ADDRESS, &[0x5A; 32])
        .unwrap();
    let mut direct_source = SwiContext::default();
    direct_source.registers[0] = 1;
    direct_source.registers[1] = SOURCE_SELECTOR_ADDRESS;
    direct_source.registers[3] = SOURCE_BUFFER_ADDRESS;
    direct_source.registers[4] = 128;
    direct_source.registers[7] = SOURCE_PATH_ADDRESS;
    direct_source.registers[8] = 128;
    let direct_source_error = dispatcher
        .dispatch(ACORN_DEFINITION_SOURCE, &mut ordinary, &mut direct_source)
        .unwrap_err();
    assert!(matches!(
        direct_source_error,
        RuntimeError::Structured { ref type_name, code: 1, .. }
            if type_name == "TaskAuthorizationDenied"
    ));
    assert_eq!(
        ordinary
            .memory
            .read_bytes(SOURCE_BUFFER_ADDRESS, 32)
            .unwrap(),
        vec![0xA5; 32]
    );
    assert_eq!(
        ordinary.memory.read_bytes(SOURCE_PATH_ADDRESS, 32).unwrap(),
        vec![0x5A; 32]
    );
    put_string(
        &mut same_id_ordinary,
        SOURCE_SELECTOR_ADDRESS,
        "ClassicCommandsGuest/SecretRoutine",
    );
    let mut same_id_source = SwiContext::default();
    same_id_source.registers[0] = 1;
    same_id_source.registers[1] = SOURCE_SELECTOR_ADDRESS;
    same_id_source.registers[3] = SOURCE_BUFFER_ADDRESS;
    same_id_source.registers[4] = 128;
    same_id_source.registers[7] = SOURCE_PATH_ADDRESS;
    same_id_source.registers[8] = 128;
    assert!(matches!(
        dispatcher.dispatch(ACORN_DEFINITION_SOURCE, &mut same_id_ordinary, &mut same_id_source),
        Err(RuntimeError::Structured { ref type_name, code: 1, .. })
            if type_name == "TaskAuthorizationDenied"
    ));
    let (allowed_definition, allowed_definition_output) = cli(
        &mut dispatcher,
        &mut source_inspector,
        &display_receiver,
        "*INSPECT DEFINITION ClassicCommandsGuest/SecretRoutine",
    );
    assert!(
        allowed_definition.is_ok(),
        "source inspector could not query definition: {allowed_definition:?}, {allowed_definition_output:?}"
    );
    assert!(allowed_definition_output.contains("classic-command-private-source-v1"));
    let (source_alias_result, source_alias_output) = cli(
        &mut dispatcher,
        &mut source_inspector,
        &display_receiver,
        "*INSPECT SOURCE ClassicCommandsGuest/SecretRoutine",
    );
    assert!(source_alias_result.is_ok());
    assert!(source_alias_output.contains("classic-command-private-source-v1"));
    assert_eq!(
        invoke(&mut dispatcher, &mut mos, CLASSIC_PROBE).unwrap(),
        102
    );
    assert_eq!(
        swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE),
        Some(1)
    );

    // INSPECT is strictly observational: former mutation-looking subcommands
    // fall back to the read-only help, with identity and behavior unchanged.
    for command in [
        "*INSPECT LOAD ClassicV2",
        "*INSPECT RELOAD ClassicV2",
        "*INSPECT DELETE ClassicCommandsGuest",
    ] {
        let (result, output) = cli(&mut dispatcher, &mut mos, &display_receiver, command);
        assert!(result.is_ok(), "read-only INSPECT help failed: {result:?}");
        let lower_output = output.to_ascii_lowercase();
        assert!(
            lower_output.contains("read-only inspection commands")
                || lower_output.contains("inspect is read-only")
                || lower_output.contains("unsupported *inspect"),
            "{command:?} must remain on the read-only command surface: {output:?}"
        );
        assert!(!lower_output.contains("loaded"));
        assert!(!lower_output.contains("removed"));
        assert_eq!(
            swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE),
            Some(1)
        );
        assert_eq!(
            module_info(&mut dispatcher, &mut mos, "ClassicCommandsGuest"),
            Some(([2, 1, 0], 4))
        );
    }
    assert_eq!(
        invoke(&mut dispatcher, &mut mos, CLASSIC_PROBE).unwrap(),
        103
    );

    // *RMEnsure compares the installed semantic version to a requested
    // threshold and executes the complete command tail only when missing or
    // too old. PRM examples 2.01 and 0.51 are pinned as dotted thresholds.
    let (equal_result, equal_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMEnsure ClassicCommandsGuest 2.01 *INSPECT MODULES ClassicCommandsGuest",
    );
    assert!(
        equal_result.is_ok(),
        "equal RMEnsure failed: {equal_output:?}"
    );
    assert!(equal_output.is_empty());
    assert!(
        !equal_output.contains("ClassicCommandsGuest"),
        "equal threshold should not execute RMEnsure tail: {equal_output:?}"
    );
    let (equal_no_tail_result, equal_no_tail_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RME. ClassicCommandsGuest 2.1",
    );
    assert!(
        equal_no_tail_result.is_ok(),
        "satisfied version without a tail should be a no-op: {equal_no_tail_output:?}"
    );
    assert!(equal_no_tail_output.is_empty());
    let insufficient_no_tail = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMEnsure ClassicCommandsGuest 2.02",
    );
    assert!(
        insufficient_no_tail.0.is_err(),
        "PRM RMEnsure must raise an error when the version is insufficient and no fallback tail is supplied: {insufficient_no_tail:?}"
    );
    assert!(
        insufficient_no_tail
            .1
            .to_ascii_lowercase()
            .contains("older")
    );
    let missing_no_tail = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMEnsure MissingClassicModule 1.00",
    );
    assert!(
        missing_no_tail.0.is_err(),
        "PRM RMEnsure must raise an error when the module is missing and no fallback tail is supplied: {missing_no_tail:?}"
    );
    assert!(missing_no_tail.1.to_ascii_lowercase().contains("absent"));
    let (older_result, older_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMEnsure ClassicCommandsGuest 0.51 *INSPECT MODULES ClassicCommandsGuest",
    );
    assert!(
        older_result.is_ok(),
        "older RMEnsure failed: {older_output:?}"
    );
    assert!(older_output.is_empty());
    assert!(
        !older_output.contains("ClassicCommandsGuest"),
        "a current module above 0.51 should not execute the tail: {older_output:?}"
    );
    let (newer_result, newer_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMEnsure ClassicCommandsGuest 2.02 *INSPECT MODULES ClassicCommandsGuest",
    );
    assert!(
        newer_result.is_ok(),
        "newer RMEnsure failed: {newer_output:?}"
    );
    assert!(newer_output.contains("ClassicCommandsGuest"));
    let (missing_result, missing_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMEnsure MissingClassicModule 1.00 *INSPECT MODULES ClassicCommandsGuest",
    );
    assert!(
        missing_result.is_ok(),
        "missing-module tail failed: {missing_output:?}"
    );
    assert!(missing_output.contains("ClassicCommandsGuest"));
    let bad_version_output = assert_cli_rejected(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMEnsure ClassicCommandsGuest 2x *INSPECT MODULES ClassicCommandsGuest",
    );
    assert!(!bad_version_output.contains("2.1.0"));

    // A command tail reaches the same task principal even from guest OS_CLI.
    // Ordinary callers cannot use RMEnsure to launder source-read authority;
    // an explicit source inspector can use the identical nested route.
    let (forwarder_load, forwarder_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*RMLoad ClassicForwarder",
    );
    assert!(
        forwarder_load.is_ok(),
        "could not load the nested OS_CLI fixture: {forwarder_load:?}, {forwarder_output:?}"
    );
    let nested = "*RMEnsure ClassicCommandsGuest 2.02 *INSPECT DEFINITION ClassicCommandsGuest/SecretRoutine";
    put_string(&mut ordinary, CLI_ADDRESS, nested);
    let _ = display_receiver.try_iter().count();
    let nested_ordinary_result = invoke(&mut dispatcher, &mut ordinary, CLASSIC_FORWARD);
    let nested_ordinary_bytes = display_receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    let nested_ordinary_output = String::from_utf8_lossy(&nested_ordinary_bytes);
    assert!(matches!(
        nested_ordinary_result,
        Err(RuntimeError::Structured { ref type_name, code: 1, .. })
            if type_name == "TaskAuthorizationDenied"
    ));
    assert!(!nested_ordinary_output.contains("classic-command-private-source-v1"));
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_A, 32)
            .unwrap(),
        vec![0xA1; 32]
    );
    put_string(&mut source_inspector, CLI_ADDRESS, nested);
    let _ = display_receiver.try_iter().count();
    assert_eq!(
        invoke(&mut dispatcher, &mut source_inspector, CLASSIC_FORWARD).unwrap(),
        77
    );
    let nested_source_output = display_receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        String::from_utf8_lossy(&nested_source_output)
            .contains("classic-command-private-source-v1")
    );
    assert_eq!(source_inspector.memory.dynamic_area_count(), 0);

    // The same nested path cannot launder ModuleManagement either. The
    // RMEnsure fallback is conditional, but RMLoad still sees the original
    // ordinary caller and must leave the candidate unpublished.
    let nested_management = "*RMEnsure MissingManagementGuard 1.00 *RMLoad ClassicRun";
    put_string(&mut ordinary, CLI_ADDRESS, nested_management);
    let _ = display_receiver.try_iter().count();
    let nested_management_result = invoke(&mut dispatcher, &mut ordinary, CLASSIC_FORWARD);
    assert!(matches!(
        nested_management_result,
        Err(RuntimeError::Structured { ref type_name, code: 2, .. })
            if type_name == "TaskAuthorizationDenied"
    ));
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert_eq!(
        module_info(&mut dispatcher, &mut mos, "ClassicRunGuest"),
        None
    );

    // RMLoad of a same-title source performs the documented hosted atomic
    // replacement, not duplicate publication. An incompatible candidate must
    // fail without changing current generation, source, or workspace value.
    let (replace_result, replace_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*RMLoad ClassicV2",
    );
    assert!(
        replace_result.is_ok(),
        "same-title RMLoad replacement failed: {replace_result:?}, {replace_output:?}"
    );
    assert_eq!(
        module_info(&mut dispatcher, &mut mos, "ClassicCommandsGuest"),
        Some(([2, 1, 0], 4))
    );
    assert_eq!(
        swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE),
        Some(2)
    );
    assert_eq!(
        invoke(&mut dispatcher, &mut mos, CLASSIC_PROBE).unwrap(),
        204
    );
    let module_list_after_replacement =
        cli(&mut dispatcher, &mut mos, &display_receiver, "*Modules");
    assert!(module_list_after_replacement.0.is_ok());
    assert_eq!(
        module_list_after_replacement
            .1
            .matches("ClassicCommandsGuest")
            .count(),
        1,
        "same-title RMLoad must not publish a second module instance"
    );
    let source_v2 = cli(
        &mut dispatcher,
        &mut source_inspector,
        &display_receiver,
        "*INSPECT DEFINITION ClassicCommandsGuest/SecretRoutine",
    );
    assert!(source_v2.0.is_ok());
    assert!(source_v2.1.contains("classic-command-private-source-v2"));
    assert!(!source_v2.1.contains("classic-command-private-source-v1"));
    let incompatible_result = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*RMLoad ClassicIncompatible",
    );
    assert!(
        incompatible_result.0.is_err()
            || incompatible_result
                .1
                .to_ascii_lowercase()
                .contains("incompat"),
        "incompatible same-title source was not rejected: {:?}",
        incompatible_result
    );
    assert_eq!(module_manager.memory.dynamic_area_count(), 0);
    assert_eq!(
        module_manager
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_A, 32)
            .unwrap(),
        vec![0xA1; 32]
    );
    assert_eq!(
        swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE),
        Some(2)
    );
    assert_eq!(
        invoke(&mut dispatcher, &mut mos, CLASSIC_PROBE).unwrap(),
        205
    );
    let source_after_reject = cli(
        &mut dispatcher,
        &mut source_inspector,
        &display_receiver,
        "*INSPECT DEFINITION ClassicCommandsGuest/SecretRoutine",
    );
    assert!(source_after_reject.0.is_ok());
    assert!(
        source_after_reject
            .1
            .contains("classic-command-private-source-v2")
    );
    assert!(
        !source_after_reject
            .1
            .contains("incompatible-candidate-source")
    );

    // RMRun is the hosted no-application-entry equivalent of RMLoad; it may
    // load this BASIC64 module, but does not pretend lifecycle Start is an
    // application entry point. Init strings are rejected on this profile.
    let (run_result, run_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*RMRun ClassicRun",
    );
    assert!(
        run_result.is_ok(),
        "hosted RMRun equivalent failed: {run_output:?}"
    );
    assert_eq!(
        module_info(&mut dispatcher, &mut mos, "ClassicRunGuest"),
        Some(([1, 0, 0], 4))
    );
    assert_eq!(invoke(&mut dispatcher, &mut mos, 0x4FF62).unwrap(), 62);

    // Classic mutation-looking commands without a safe hosted contract must
    // identify themselves as unsupported; they must not alias to another
    // action. RMKill's historical %instance suffix is likewise rejected.
    for (index, command) in [
        "*RMLoad ClassicRun InitialisationString",
        "*RMRun ClassicRun InitialisationString",
        "*RMKill ClassicCommandsGuest%Base",
        "*RMReInit ClassicCommandsGuest",
        "*RMInsert ClassicCommandsGuest",
        "*RMTidy",
        "*RMClear",
        "*RMFaster ClassicCommandsGuest",
        "*ROMModules",
        "*Unplug ClassicCommandsGuest",
    ]
    .into_iter()
    .enumerate()
    {
        let _ =
            assert_explicitly_unsupported(&mut dispatcher, &mut mos, &display_receiver, command);
        assert_eq!(
            swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE),
            Some(2)
        );
        assert_eq!(
            module_info(&mut dispatcher, &mut mos, "ClassicCommandsGuest"),
            Some(([2, 1, 0], 4))
        );
        assert_eq!(mos.memory.dynamic_area_count(), 0);
        assert_eq!(
            invoke(&mut dispatcher, &mut mos, CLASSIC_PROBE).unwrap(),
            206 + index as u32
        );
    }

    let (ambiguous_rm_abbrev, ambiguous_rm_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMR. ClassicCommandsGuest",
    );
    assert!(
        ambiguous_rm_abbrev.is_err()
            || ambiguous_rm_output
                .to_ascii_lowercase()
                .contains("ambiguous"),
        "ambiguous *RMR. must not silently choose RMRun or RMReInit: {ambiguous_rm_abbrev:?}, {ambiguous_rm_output:?}"
    );

    // The former command namespace is not retained as a hidden alias, and
    // existing status/configuration plus source inspection remain reachable.
    let old_namespace = assert_cli_rejected(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*TRELLIS MODULES ClassicCommandsGuest",
    );
    assert!(!old_namespace.contains("ClassicCommandsGuest"));
    let (status_result, status_output) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*STATUS");
    assert!(status_result.is_ok());
    assert!(status_output.to_ascii_lowercase().contains("basic"));

    let (kill_denied, kill_denied_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RMKill ClassicCommandsGuest",
    );
    assert!(
        kill_denied.is_err()
            || kill_denied_output
                .to_ascii_lowercase()
                .contains("authority")
            || kill_denied_output
                .to_ascii_lowercase()
                .contains("permission"),
        "ordinary *RMKill was not explicitly denied: {kill_denied:?}, {kill_denied_output:?}"
    );
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert_eq!(
        module_info(&mut dispatcher, &mut mos, "ClassicCommandsGuest"),
        Some(([2, 1, 0], 4))
    );
    let (kill_result, kill_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*RMK. ClassicCommandsGuest",
    );
    assert!(
        kill_result.is_ok(),
        "authorized *RMKill failed: {kill_output:?}"
    );
    assert_eq!(module_manager.memory.dynamic_area_count(), 0);
    assert_eq!(
        module_info(&mut dispatcher, &mut mos, "ClassicCommandsGuest"),
        None
    );
    assert!(swi_generation(&mut dispatcher, &mut mos, CLASSIC_PROBE).is_none());

    // The exact title and semver remain queryable until actual deletion;
    // no failed command above changed public registry ownership.
    assert!(read_string(&mos, TITLE_ADDRESS, 128).contains("ClassicCommandsGuest"));
}
