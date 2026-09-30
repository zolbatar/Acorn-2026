use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use acorn_2026::{
    error::RuntimeError,
    host::HostConsole,
    memory::{MemoryError, Task},
    swi::{
        ACORN_MODULE_INFO, ACORN_MODULE_LOOKUP, ACORN_SWI_INFO, DisplayEvent, SwiContext,
        SwiDispatcher,
    },
};

const OS_CLI: u32 = 0x05;
const OS_MODULE: u32 = 0x1E;
const INSPECT_RUN: u32 = 0x4FF60;
const ACORN_MODULE_EXPORT: u32 = 0x4FF14;
const ACORN_DEFINITION_SOURCE: u32 = 0x4FF15;
const CLI_ADDRESS: u32 = 0x2100;
const QUERY_BUFFER: u32 = 0x5000;

const V1_SOURCE: &str = concat!(
    r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE InspectGuest 1.2.3
REM @STATE COUNT% UINT32
REM @SWI Inspect_Run &4FF60 Run REGISTERS=R0:U32:INOUT
REM @PRIVATE PROC DetailHelper
DEF PROC Run
    REM retained marker: café
    REM retained control source: "#,
    "\x1b",
    r#"[31mred
    COUNT% = COUNT% + 1
    R0% = R0% + COUNT% + 101
ENDPROC
DEF PROC DetailHelper
    REM helper-source-marker-v1
ENDPROC
DEF FN DetailFunction() AS UINT32
    REM helper-function-source-marker-v1
=1
"#
);

const V2_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE InspectGuest 1.2.3
REM @STATE COUNT% UINT32
REM @SWI Inspect_Run &4FF60 Run REGISTERS=R0:U32:INOUT
REM @PRIVATE PROC DetailHelper
DEF PROC Run
    REM retained marker: naïve
    COUNT% = COUNT% + 1
    R0% = R0% + COUNT% + 201
ENDPROC
DEF PROC DetailHelper
    REM helper-source-marker-v2
ENDPROC
DEF FN DetailFunction() AS UINT32
    REM helper-function-source-marker-v2
=1
"#;

const UNPRIVILEGED_MANAGER_IMPORT: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE UnprivilegedInspector 1.0.0
REM @CAPABILITY ModuleManagement
REM @IMPORT Host.ModuleManager.Unload ModuleManagement
REM @SWI Unprivileged_Probe &4FF61 Entry
DEF PROC Entry
ENDPROC
"#;

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModuleRow {
    name: String,
    version: [u32; 3],
    state: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SwiIdentity {
    name: String,
    owner: String,
    definition: String,
    generation: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ModuleExport {
    name: String,
    number: u32,
    definition: String,
    generation: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceChunk {
    text: String,
    bytes_copied: u32,
    next_offset: u32,
    total_bytes: u32,
    generation: u32,
    source_path: String,
    definition_id: [u32; 2],
}

fn scratch_directory() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "acorn-trellis-mos-introspection-{}-{nanos}",
        std::process::id()
    ))
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
    if result.is_ok() {
        assert_eq!(
            context.registers[0], CLI_ADDRESS,
            "successful OS_CLI dispatch must preserve its input R0 command-line pointer for {command:?}"
        );
    }
    let output_bytes: Vec<u8> = receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect();
    (result, String::from_utf8_lossy(&output_bytes).into_owned())
}

fn read_modules(dispatcher: &mut SwiDispatcher, task: &mut Task) -> Vec<ModuleRow> {
    let mut cursor = 0;
    let mut modules = Vec::new();
    for _ in 0..64 {
        let mut context = SwiContext::default();
        context.registers[0] = 1;
        context.registers[1] = cursor;
        context.registers[2] = QUERY_BUFFER;
        context.registers[3] = 128;
        dispatcher
            .dispatch(ACORN_MODULE_INFO, task, &mut context)
            .unwrap();
        assert!(!context.overflow);
        if context.registers[4] == 0 {
            return modules;
        }
        let name =
            String::from_utf8(task.memory.read_c_string(QUERY_BUFFER, 128).unwrap()).unwrap();
        modules.push(ModuleRow {
            name,
            version: [
                context.registers[5],
                context.registers[6],
                context.registers[7],
            ],
            state: context.registers[8],
        });
        cursor = context.registers[1];
    }
    panic!("active module enumeration did not terminate within the capsule bound");
}

fn lookup_module(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    title: &str,
) -> Option<ModuleRow> {
    put_string(task, QUERY_BUFFER + 0x300, title);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = QUERY_BUFFER + 0x300;
    dispatcher
        .dispatch(ACORN_MODULE_LOOKUP, task, &mut context)
        .unwrap();
    assert!(!context.overflow);
    (context.registers[1] != 0).then(|| ModuleRow {
        name: title.to_owned(),
        version: [
            context.registers[3],
            context.registers[4],
            context.registers[5],
        ],
        state: context.registers[6],
    })
}

fn query_swi(dispatcher: &mut SwiDispatcher, task: &mut Task, number: u32) -> SwiIdentity {
    let name_address = QUERY_BUFFER + 0x400;
    let owner_address = QUERY_BUFFER + 0x500;
    let definition_address = QUERY_BUFFER + 0x600;
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = number;
    context.registers[2] = name_address;
    context.registers[3] = 128;
    context.registers[4] = owner_address;
    context.registers[5] = 128;
    context.registers[6] = definition_address;
    context.registers[7] = 128;
    dispatcher
        .dispatch(ACORN_SWI_INFO, task, &mut context)
        .unwrap();
    assert!(!context.overflow);
    SwiIdentity {
        name: String::from_utf8(task.memory.read_c_string(name_address, 128).unwrap()).unwrap(),
        owner: String::from_utf8(task.memory.read_c_string(owner_address, 128).unwrap()).unwrap(),
        definition: String::from_utf8(task.memory.read_c_string(definition_address, 128).unwrap())
            .unwrap(),
        generation: context.registers[8],
    }
}

fn query_module_exports(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    module: &str,
) -> Vec<ModuleExport> {
    let module_address = QUERY_BUFFER + 0x700;
    let name_address = QUERY_BUFFER + 0x800;
    let definition_address = QUERY_BUFFER + 0x900;
    put_string(task, module_address, module);
    let mut cursor = 0;
    let mut exports = Vec::new();
    for _ in 0..64 {
        let mut context = SwiContext::default();
        context.registers[0] = 1;
        context.registers[1] = module_address;
        context.registers[2] = cursor;
        context.registers[3] = name_address;
        context.registers[4] = 128;
        context.registers[6] = definition_address;
        context.registers[7] = 128;
        dispatcher
            .dispatch(ACORN_MODULE_EXPORT, task, &mut context)
            .unwrap();
        assert!(!context.overflow);
        if context.registers[9] == 0 {
            return exports;
        }
        exports.push(ModuleExport {
            name: String::from_utf8(task.memory.read_c_string(name_address, 128).unwrap()).unwrap(),
            number: context.registers[5],
            definition: String::from_utf8(
                task.memory.read_c_string(definition_address, 128).unwrap(),
            )
            .unwrap(),
            generation: context.registers[8],
        });
        cursor = context.registers[2];
    }
    panic!("module export enumeration did not terminate within the capsule bound");
}

fn query_definition_source(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    selector: &str,
    offset: u32,
    output_capacity: u32,
) -> Result<SourceChunk, RuntimeError> {
    let selector_address = QUERY_BUFFER + 0xA00;
    let output_address = QUERY_BUFFER + 0xB00;
    let source_path_address = QUERY_BUFFER + 0xC00;
    put_string(task, selector_address, selector);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = selector_address;
    context.registers[2] = offset;
    context.registers[3] = output_address;
    context.registers[4] = output_capacity;
    context.registers[7] = source_path_address;
    context.registers[8] = 128;
    dispatcher.dispatch(ACORN_DEFINITION_SOURCE, task, &mut context)?;
    Ok(SourceChunk {
        text: String::from_utf8(
            task.memory
                .read_c_string(output_address, output_capacity as usize)
                .unwrap(),
        )
        .unwrap(),
        bytes_copied: context.registers[0],
        next_offset: context.registers[2],
        total_bytes: context.registers[5],
        generation: context.registers[6],
        source_path: String::from_utf8(
            task.memory.read_c_string(source_path_address, 128).unwrap(),
        )
        .unwrap(),
        definition_id: [context.registers[9], context.registers[10]],
    })
}

fn call_guest(dispatcher: &mut SwiDispatcher, task: &mut Task, initial_r0: u32) -> u32 {
    let mut context = SwiContext::default();
    context.registers[0] = initial_r0;
    dispatcher
        .dispatch(INSPECT_RUN, task, &mut context)
        .unwrap();
    context.registers[0]
}

fn assert_cli_succeeded(result: Result<(), RuntimeError>, output: &str, command: &str) {
    assert!(
        result.is_ok(),
        "command {command:?} returned {result:?}; output was {output:?}"
    );
}

fn assert_text_has(output: &str, expected: &str, command: &str) {
    assert!(
        output
            .to_ascii_lowercase()
            .contains(&expected.to_ascii_lowercase()),
        "command {command:?} output {output:?} did not contain {expected:?}"
    );
}

#[test]
fn wp51_mos_introspection_matches_read_only_queries_and_tracks_live_generations() {
    let root = scratch_directory();
    fs::create_dir_all(&root).unwrap();
    let config_path = root.join("isolated-configure");
    unsafe {
        std::env::set_var("ACORN_CONFIG_PATH", &config_path);
        std::env::set_var("ACORN_DEMO_VOLUME", &root);
    }
    let v1_source_with_crlf = V1_SOURCE.replace('\n', "\r\n");
    write_guest_module(&root, "InspectV1", &v1_source_with_crlf);
    write_guest_module(&root, "InspectV2", V2_SOURCE);
    write_guest_module(&root, "UnprivilegedInspector", UNPRIVILEGED_MANAGER_IMPORT);
    fs::write(root.join("LegacyType"), "legacy-type-marker\n").unwrap();

    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut dispatcher =
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    let mut task = Task::trusted_mos_session(0x51_1601);

    // The existing CLI remains reachable while the new command family is
    // installed; its help and configuration query must not alter modules.
    let initial_modules = read_modules(&mut dispatcher, &mut task);
    let (help_result, help) = cli(&mut dispatcher, &mut task, &display_receiver, "*HELP");
    assert_cli_succeeded(help_result, &help, "*HELP");
    assert_text_has(&help, "CONFIGURE", "*HELP");
    assert_text_has(&help, "STATUS", "*HELP");
    let (status_result, status) = cli(&mut dispatcher, &mut task, &display_receiver, "*STATUS");
    assert_cli_succeeded(status_result, &status, "*STATUS");
    assert_text_has(&status, "BASIC", "*STATUS");
    assert_eq!(read_modules(&mut dispatcher, &mut task), initial_modules);
    let unterminated_cli = vec![b'X'; 256];
    task.memory
        .write_bytes(CLI_ADDRESS, &unterminated_cli)
        .unwrap();
    let mut unterminated_context = SwiContext::default();
    unterminated_context.registers[0] = CLI_ADDRESS;
    let unterminated_result = dispatcher.dispatch(OS_CLI, &mut task, &mut unterminated_context);
    assert!(
        matches!(
            unterminated_result,
            Err(RuntimeError::Memory(MemoryError::MissingNullTerminator(address)))
                if address == CLI_ADDRESS
        ),
        "unterminated 256-byte OS_CLI line returned {unterminated_result:?}"
    );
    let (legacy_type_result, legacy_type_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*T. LegacyType",
    );
    assert_cli_succeeded(legacy_type_result, &legacy_type_output, "*T. LegacyType");
    assert_text_has(&legacy_type_output, "legacy-type-marker", "*T. LegacyType");
    assert_eq!(read_modules(&mut dispatcher, &mut task), initial_modules);
    let mut invalid_tagged_cli = SwiContext::default();
    invalid_tagged_cli.registers[0] = 0x4000_2100;
    assert!(matches!(
        dispatcher.dispatch(OS_CLI, &mut task, &mut invalid_tagged_cli),
        Err(RuntimeError::Memory(MemoryError::AddressOutsideSpace(_)))
    ));
    assert_eq!(read_modules(&mut dispatcher, &mut task), initial_modules);

    let (load_result, load_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*RMLoad InspectV1",
    );
    assert_cli_succeeded(load_result, &load_output, "*RMLoad InspectV1");
    assert_text_has(&load_output, "InspectGuest", "*RMLoad InspectV1");
    let baseline_modules = read_modules(&mut dispatcher, &mut task);
    let module = lookup_module(&mut dispatcher, &mut task, "InspectGuest")
        .expect("loaded module is discoverable through the public query");
    assert_eq!(module.version, [1, 2, 3]);
    assert_eq!(module.state, 4, "module is Active");
    let identity = query_swi(&mut dispatcher, &mut task, INSPECT_RUN);
    assert_eq!(identity.name, "Inspect_Run");
    assert_eq!(identity.owner, "InspectGuest");
    assert_eq!(identity.definition, "RUN");
    assert_eq!(identity.generation, 1);
    let initial_exports = query_module_exports(&mut dispatcher, &mut task, "InspectGuest");
    assert_eq!(
        initial_exports,
        [ModuleExport {
            name: identity.name.clone(),
            number: INSPECT_RUN,
            definition: identity.definition.clone(),
            generation: identity.generation,
        }]
    );
    let module_name_address = QUERY_BUFFER + 0x700;
    let export_name_address = QUERY_BUFFER + 0x800;
    let export_definition_address = QUERY_BUFFER + 0x900;
    for (description, module_address, name_address, name_capacity, definition_capacity) in [
        (
            "invalid module-name pointer",
            u32::MAX,
            export_name_address,
            128,
            128,
        ),
        (
            "invalid export-name output pointer",
            module_name_address,
            u32::MAX,
            128,
            128,
        ),
        (
            "too-small export-name capacity",
            module_name_address,
            export_name_address,
            1,
            128,
        ),
        (
            "export-name capacity above the ABI bound",
            module_name_address,
            export_name_address,
            129,
            128,
        ),
    ] {
        let mut context = SwiContext::default();
        context.registers[0] = 1;
        context.registers[1] = module_address;
        context.registers[3] = name_address;
        context.registers[4] = name_capacity;
        context.registers[6] = export_definition_address;
        context.registers[7] = definition_capacity;
        let result = dispatcher.dispatch(ACORN_MODULE_EXPORT, &mut task, &mut context);
        if description.contains("pointer") {
            assert!(
                matches!(&result, Err(RuntimeError::Memory(_))),
                "{description} should be a checked logical-memory error, got {result:?}"
            );
        } else {
            assert!(
                matches!(&result, Err(RuntimeError::Structured { .. })),
                "{description} should be a structured buffer-bounds error, got {result:?}"
            );
        }
        if description == "too-small export-name capacity" {
            assert!(matches!(
                &result,
                Err(RuntimeError::Structured { type_name, .. })
                    if type_name == "ModuleInfoBufferError"
            ));
        }
    }
    let mut invalid_export_definition_output = SwiContext::default();
    invalid_export_definition_output.registers[0] = 1;
    invalid_export_definition_output.registers[1] = module_name_address;
    invalid_export_definition_output.registers[3] = export_name_address;
    invalid_export_definition_output.registers[4] = 128;
    invalid_export_definition_output.registers[6] = u32::MAX;
    invalid_export_definition_output.registers[7] = 128;
    assert!(matches!(
        dispatcher.dispatch(
            ACORN_MODULE_EXPORT,
            &mut task,
            &mut invalid_export_definition_output
        ),
        Err(RuntimeError::Memory(_))
    ));
    let retained_source =
        query_definition_source(&mut dispatcher, &mut task, "InspectGuest/RUN", 0, 1025).unwrap();
    assert!(retained_source.text.contains("R0% = R0% + COUNT% + 101"));
    assert!(retained_source.text.contains("café"));
    assert!(retained_source.text.contains("\r\n"));
    assert!(retained_source.text.ends_with("\r\n"));
    assert!(
        !retained_source.text.replace("\r\n", "").contains('\n'),
        "retained CRLF source must not normalize to bare LF"
    );
    assert!(
        retained_source.text.as_bytes().contains(&0x1b),
        "the structured source API should preserve the source's actual ESC byte"
    );
    assert_eq!(
        retained_source.bytes_copied as usize,
        retained_source.text.len()
    );
    assert_eq!(retained_source.next_offset, retained_source.total_bytes);
    assert_eq!(retained_source.generation, identity.generation);
    assert!(retained_source.source_path.contains("InspectV1"));
    assert_ne!(retained_source.definition_id, [0, 0]);
    let source_at_end = query_definition_source(
        &mut dispatcher,
        &mut task,
        "InspectGuest/RUN",
        retained_source.total_bytes,
        1,
    )
    .unwrap();
    assert_eq!(source_at_end.text, "");
    assert_eq!(source_at_end.bytes_copied, 0);
    assert_eq!(source_at_end.next_offset, retained_source.total_bytes);
    assert_eq!(source_at_end.generation, retained_source.generation);
    let source_past_end = query_definition_source(
        &mut dispatcher,
        &mut task,
        "InspectGuest/RUN",
        retained_source.total_bytes + 1,
        2,
    )
    .unwrap_err();
    assert!(matches!(
        source_past_end,
        RuntimeError::Structured { ref type_name, .. }
            if type_name == "DefinitionSourceOffsetError"
    ));
    for (description, selector_address, output_address, output_capacity, path_capacity) in [
        (
            "invalid source selector pointer",
            u32::MAX,
            QUERY_BUFFER + 0xB00,
            1025,
            128,
        ),
        (
            "invalid source output pointer",
            QUERY_BUFFER + 0xA00,
            u32::MAX,
            1025,
            128,
        ),
        (
            "zero source output capacity outside the ABI bound",
            QUERY_BUFFER + 0xA00,
            QUERY_BUFFER + 0xB00,
            0,
            128,
        ),
        (
            "one-byte source output capacity before EOF",
            QUERY_BUFFER + 0xA00,
            QUERY_BUFFER + 0xB00,
            1,
            128,
        ),
        (
            "source path capacity above the ABI bound",
            QUERY_BUFFER + 0xA00,
            QUERY_BUFFER + 0xB00,
            1025,
            129,
        ),
    ] {
        if selector_address != u32::MAX {
            put_string(&mut task, selector_address, "InspectGuest/RUN");
        }
        let mut context = SwiContext::default();
        context.registers[0] = 1;
        context.registers[1] = selector_address;
        context.registers[3] = output_address;
        context.registers[4] = output_capacity;
        context.registers[7] = QUERY_BUFFER + 0xC00;
        context.registers[8] = path_capacity;
        let result = dispatcher.dispatch(ACORN_DEFINITION_SOURCE, &mut task, &mut context);
        if description.contains("pointer") {
            assert!(
                matches!(&result, Err(RuntimeError::Memory(_))),
                "{description} should be a checked logical-memory error, got {result:?}"
            );
        } else {
            assert!(
                matches!(&result, Err(RuntimeError::Structured { .. })),
                "{description} should be a structured buffer-bounds error, got {result:?}"
            );
        }
        if description == "one-byte source output capacity before EOF" {
            assert!(matches!(
                &result,
                Err(RuntimeError::Structured { type_name, .. })
                    if type_name == "DefinitionSourceBufferError"
            ));
        }
    }
    put_string(&mut task, QUERY_BUFFER + 0xA00, "InspectGuest/RUN");
    let mut invalid_source_path_output = SwiContext::default();
    invalid_source_path_output.registers[0] = 1;
    invalid_source_path_output.registers[1] = QUERY_BUFFER + 0xA00;
    invalid_source_path_output.registers[3] = QUERY_BUFFER + 0xB00;
    invalid_source_path_output.registers[4] = 1025;
    invalid_source_path_output.registers[7] = u32::MAX;
    invalid_source_path_output.registers[8] = 128;
    assert!(matches!(
        dispatcher.dispatch(
            ACORN_DEFINITION_SOURCE,
            &mut task,
            &mut invalid_source_path_output
        ),
        Err(RuntimeError::Memory(_))
    ));
    // The current public introspection surface returns retained source for
    // private PROC/FN definitions too; this is observable read-only metadata,
    // not evidence of per-task authorization for mutation operations.
    let private_helper_source = query_definition_source(
        &mut dispatcher,
        &mut task,
        "InspectGuest/DETAILHELPER",
        0,
        1025,
    )
    .unwrap();
    assert!(
        private_helper_source
            .text
            .contains("helper-source-marker-v1")
    );
    assert_eq!(private_helper_source.generation, identity.generation);
    assert_eq!(
        private_helper_source.source_path,
        retained_source.source_path
    );
    let private_function_source = query_definition_source(
        &mut dispatcher,
        &mut task,
        "InspectGuest/fn:detailfunction",
        0,
        1025,
    )
    .unwrap();
    assert!(
        private_function_source
            .text
            .contains("helper-function-source-marker-v1")
    );
    assert_eq!(private_function_source.generation, identity.generation);

    // A chunk limit that would split the two-byte 'é' must stop at its UTF-8
    // boundary. The continuation offset is a byte offset, not a character
    // index, and concatenating the chunks reproduces the complete definition.
    let split_at = retained_source.text.find('é').unwrap() as u32;
    let first_source_chunk = query_definition_source(
        &mut dispatcher,
        &mut task,
        "InspectGuest/RUN",
        0,
        split_at + 2,
    )
    .unwrap();
    assert_eq!(
        first_source_chunk.text,
        retained_source.text[..split_at as usize]
    );
    assert_eq!(first_source_chunk.next_offset, split_at);
    assert_eq!(first_source_chunk.bytes_copied, split_at);
    let second_source_chunk = query_definition_source(
        &mut dispatcher,
        &mut task,
        "InspectGuest/RUN",
        first_source_chunk.next_offset,
        1025,
    )
    .unwrap();
    assert_eq!(
        first_source_chunk.text + &second_source_chunk.text,
        retained_source.text
    );
    assert_eq!(second_source_chunk.next_offset, retained_source.total_bytes);
    assert_eq!(second_source_chunk.generation, identity.generation);
    assert_eq!(call_guest(&mut dispatcher, &mut task, 0), 102);

    // The source query must return retained source rather than reopen the
    // original HostFS file. Removing that file after Load keeps this an
    // observable check of the active definition's retained source.
    fs::remove_file(root.join("InspectV1.bas64")).unwrap();
    let retained_after_unlink =
        query_definition_source(&mut dispatcher, &mut task, "InspectGuest/RUN", 0, 1025).unwrap();
    assert_eq!(retained_after_unlink, retained_source);
    let readonly_commands = [
        "*INSPECT MODULES InspectGuest",
        "*INSPECT MODULE InspectGuest",
        "*INSPECT MODULE. InspectGuest",
        "  *INSPECT MODULE InspectGuest",
        "*INSPECT SWI Inspect_Run",
        "*INSPECT SWI &4FF60",
        "*INSPE. MODULES InspectGuest",
        "*INSPE. SWI. Inspect_Run",
        "*INSPECT DEFINITION InspectGuest/RUN",
        "*INSPECT DEFINITION InspectGuest/DETAILHELPER",
        "*INSPECT DEFINITION InspectGuest/fn:detailfunction",
    ];
    for command in readonly_commands {
        let (result, output) = cli(&mut dispatcher, &mut task, &display_receiver, command);
        assert_cli_succeeded(result, &output, command);
        assert_text_has(&output, "InspectGuest", command);
        if command.ends_with("MODULES InspectGuest") || command.ends_with("MOD. InspectGuest") {
            assert_text_has(&output, "1.2.3", command);
            assert_text_has(&output, "Active", command);
        }
        if command.ends_with("MODULE InspectGuest") || command.ends_with("MODULE. InspectGuest") {
            assert_text_has(&output, "1.2.3", command);
            assert_text_has(&output, "Active", command);
            assert_text_has(&output, "Inspect_Run", command);
            assert_text_has(&output, "&4FF60", command);
            assert_text_has(&output, "RUN", command);
            assert_text_has(&output, "generation 1", command);
        }
        if command.ends_with("SWI Inspect_Run") || command.ends_with("SWI &4FF60") {
            assert_text_has(&output, "Inspect_Run", command);
            assert_text_has(&output, "&4FF60", command);
            assert_text_has(&output, "InspectGuest.RUN", command);
            assert_text_has(&output, "generation 1", command);
        }
        assert_eq!(
            read_modules(&mut dispatcher, &mut task),
            baseline_modules,
            "read-only command changed the public module inventory: {command}"
        );
        assert_eq!(
            query_swi(&mut dispatcher, &mut task, INSPECT_RUN),
            identity,
            "read-only command changed SWI ownership/generation: {command}"
        );
    }
    for command in ["*INSPECT MODULES\n", "*INSPECT MODULES\r"] {
        let (result, output) = cli(&mut dispatcher, &mut task, &display_receiver, command);
        assert_cli_succeeded(result, &output, command);
        assert_text_has(&output, "InspectGuest", command);
        assert_text_has(&output, "1.2.3", command);
        assert_text_has(&output, "Active", command);
        assert_eq!(read_modules(&mut dispatcher, &mut task), baseline_modules);
        assert_eq!(query_swi(&mut dispatcher, &mut task, INSPECT_RUN), identity);
    }

    for command in [
        "*INSPECT SWI 5X",
        "*INSPECT SWI 2147483648",
        "*INSPECT SWI 4294967296",
        "*INSPECT SWI &80000000",
        "*INSPECT SWI &100000000",
    ] {
        let (result, output) = cli(&mut dispatcher, &mut task, &display_receiver, command);
        assert_cli_succeeded(result, &output, command);
        assert_text_has(&output, "Invalid SWI number", command);
        assert!(
            !output.contains("OS_CLI"),
            "malformed numeric selector {command:?} resolved to OS_CLI: {output:?}"
        );
        assert_eq!(read_modules(&mut dispatcher, &mut task), baseline_modules);
        assert_eq!(query_swi(&mut dispatcher, &mut task, INSPECT_RUN), identity);
    }
    let (source_result, source_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert_cli_succeeded(
        source_result,
        &source_output,
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert_text_has(
        &source_output,
        "R0% = R0% + COUNT% + 101",
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert_text_has(
        &source_output,
        "generation 1",
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert_text_has(
        &source_output,
        "café",
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert!(
        !source_output.as_bytes().contains(&0x1b),
        "raw ESC must not reach the display event stream"
    );
    assert!(
        source_output.contains("^["),
        "the display projection should render ESC visibly as ^[; got {source_output:?}"
    );
    let (helper_source_result, helper_source_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT DEFINITION InspectGuest/DETAILHELPER",
    );
    assert_cli_succeeded(
        helper_source_result,
        &helper_source_output,
        "*INSPECT DEFINITION InspectGuest/DETAILHELPER",
    );
    assert_text_has(
        &helper_source_output,
        "helper-source-marker-v1",
        "*INSPECT DEFINITION InspectGuest/DETAILHELPER",
    );
    let (function_source_result, function_source_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT DEFINITION InspectGuest/fn:detailfunction",
    );
    assert_cli_succeeded(
        function_source_result,
        &function_source_output,
        "*INSPECT DEFINITION InspectGuest/fn:detailfunction",
    );
    assert_text_has(
        &function_source_output,
        "helper-function-source-marker-v1",
        "*INSPECT DEFINITION InspectGuest/FN:detailfunction",
    );

    // Malformed and unknown entity requests are bounded, observable failures
    // and never turn a query command into an implicit mutation route.
    for command in [
        "*INSPECT MODULE",
        "*INSPECT MODULE MissingGuest",
        "*INSPECT SWI NoSuch_Service",
        "*INSPECT DEFINITION InspectGuest/MissingDefinition",
        "*INSPECT DEFINITION InspectGuest/",
    ] {
        let (result, output) = cli(&mut dispatcher, &mut task, &display_receiver, command);
        assert!(
            result.is_err() || !output.trim().is_empty(),
            "malformed/unknown command {command:?} produced neither an error nor feedback"
        );
        assert_eq!(read_modules(&mut dispatcher, &mut task), baseline_modules);
        assert_eq!(query_swi(&mut dispatcher, &mut task, INSPECT_RUN), identity);
    }
    for (offset, feedback) in [
        ("nonsense", "Invalid source byte offset"),
        ("12X", "Invalid source byte offset"),
        ("-1", "Invalid source byte offset"),
        ("2147483648", "Source byte offset is too large"),
    ] {
        let command = format!("*INSPECT DEFINITION InspectGuest/RUN {offset}");
        let (result, output) = cli(&mut dispatcher, &mut task, &display_receiver, &command);
        assert_cli_succeeded(result, &output, &command);
        assert_text_has(&output, feedback, &command);
        assert!(
            !output.contains("R0% = R0% + COUNT%"),
            "invalid source offset {offset:?} unexpectedly emitted source bytes"
        );
        assert_eq!(read_modules(&mut dispatcher, &mut task), baseline_modules);
        assert_eq!(query_swi(&mut dispatcher, &mut task, INSPECT_RUN), identity);
    }
    let too_long = format!("*INSPECT MODULE {}", "X".repeat(300));
    let (bounded_result, bounded_output) =
        cli(&mut dispatcher, &mut task, &display_receiver, &too_long);
    assert!(
        bounded_result.is_err() || !bounded_output.trim().is_empty(),
        "overlong OS_CLI query was neither rejected nor reported"
    );
    assert_eq!(read_modules(&mut dispatcher, &mut task), baseline_modules);
    assert_eq!(query_swi(&mut dispatcher, &mut task, INSPECT_RUN), identity);

    let too_large_source_capacity =
        query_definition_source(&mut dispatcher, &mut task, "InspectGuest/RUN", 0, 1026)
            .unwrap_err();
    assert!(matches!(
        too_large_source_capacity,
        RuntimeError::Structured { .. }
    ));
    let missing_source = query_definition_source(
        &mut dispatcher,
        &mut task,
        "InspectGuest/MissingDefinition",
        0,
        1025,
    )
    .unwrap_err();
    assert!(matches!(
        missing_source,
        RuntimeError::Structured { ref type_name, .. }
            if type_name == "DefinitionSourceNotFound"
    ));
    assert_eq!(
        call_guest(&mut dispatcher, &mut task, 0),
        103,
        "read-only commands did not change guest workspace state"
    );
    // Read-only MOS inspection does not grant guest source a protected
    // ModuleManagement primitive. An unprivileged guest import is rejected
    // without publishing any module or disturbing the active owner's SWI.
    put_string(&mut task, CLI_ADDRESS, "UnprivilegedInspector");
    let mut denied_manager_import = SwiContext::default();
    denied_manager_import.registers[0] = 1;
    denied_manager_import.registers[1] = CLI_ADDRESS;
    let denied_manager_result =
        dispatcher.dispatch(OS_MODULE, &mut task, &mut denied_manager_import);
    assert!(
        matches!(
            denied_manager_result,
            Err(RuntimeError::Structured { ref type_name, .. })
                if type_name == "ModuleCapabilityDenied"
        ),
        "unprivileged manager import returned {denied_manager_result:?}"
    );
    assert!(lookup_module(&mut dispatcher, &mut task, "UnprivilegedInspector").is_none());
    assert_eq!(read_modules(&mut dispatcher, &mut task), baseline_modules);
    assert_eq!(query_swi(&mut dispatcher, &mut task, INSPECT_RUN), identity);

    // Reload is a separate explicit mutation route. Command identity and the
    // structured query must agree on the newly active definition generation.
    let (reload_result, reload_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*RMLoad InspectV2",
    );
    assert_cli_succeeded(reload_result, &reload_output, "*RMLoad InspectV2");
    assert_text_has(&reload_output, "InspectGuest", "*RMLoad InspectV2");
    assert_eq!(
        lookup_module(&mut dispatcher, &mut task, "InspectGuest")
            .expect("replacement remains discoverable")
            .version,
        [1, 2, 3]
    );
    let replacement_identity = query_swi(&mut dispatcher, &mut task, INSPECT_RUN);
    assert_eq!(replacement_identity.name, identity.name);
    assert_eq!(replacement_identity.owner, identity.owner);
    assert_eq!(replacement_identity.definition, identity.definition);
    assert_eq!(replacement_identity.generation, identity.generation + 1);
    let (replacement_module_result, replacement_module_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT MODULE InspectGuest",
    );
    assert_cli_succeeded(
        replacement_module_result,
        &replacement_module_output,
        "*INSPECT MODULE InspectGuest",
    );
    for expected in [
        "InspectGuest",
        "1.2.3",
        "Active",
        "Inspect_Run",
        "&4FF60",
        "RUN",
        "generation 2",
    ] {
        assert_text_has(
            &replacement_module_output,
            expected,
            "*INSPECT MODULE InspectGuest",
        );
    }
    assert_eq!(
        query_module_exports(&mut dispatcher, &mut task, "InspectGuest"),
        [ModuleExport {
            name: replacement_identity.name.clone(),
            number: INSPECT_RUN,
            definition: replacement_identity.definition.clone(),
            generation: replacement_identity.generation,
        }]
    );
    let (replacement_swi_result, replacement_swi_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT SWI Inspect_Run",
    );
    assert_cli_succeeded(
        replacement_swi_result,
        &replacement_swi_output,
        "*INSPECT SWI Inspect_Run",
    );
    assert_text_has(
        &replacement_swi_output,
        &replacement_identity.name,
        "*INSPECT SWI Inspect_Run",
    );
    assert_text_has(
        &replacement_swi_output,
        "&4FF60",
        "*INSPECT SWI Inspect_Run",
    );
    assert_text_has(
        &replacement_swi_output,
        &replacement_identity.owner,
        "*INSPECT SWI Inspect_Run",
    );
    assert_text_has(
        &replacement_swi_output,
        &replacement_identity.definition,
        "*INSPECT SWI Inspect_Run",
    );
    assert_text_has(
        &replacement_swi_output,
        &replacement_identity.generation.to_string(),
        "*INSPECT SWI Inspect_Run",
    );
    let (replacement_source_result, replacement_source_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert_cli_succeeded(
        replacement_source_result,
        &replacement_source_output,
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert_text_has(
        &replacement_source_output,
        "R0% = R0% + COUNT% + 201",
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert_text_has(
        &replacement_source_output,
        "generation 2",
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    let replacement_source =
        query_definition_source(&mut dispatcher, &mut task, "InspectGuest/RUN", 0, 1025).unwrap();
    assert!(replacement_source.text.contains("R0% = R0% + COUNT% + 201"));
    assert!(replacement_source.text.contains("naïve"));
    assert_eq!(
        replacement_source.generation,
        replacement_identity.generation
    );
    assert!(replacement_source.source_path.contains("InspectV2"));
    assert_eq!(
        replacement_source.total_bytes,
        replacement_source.text.len() as u32
    );
    assert_eq!(
        replacement_source.next_offset,
        replacement_source.total_bytes
    );
    let replacement_private_helper = query_definition_source(
        &mut dispatcher,
        &mut task,
        "InspectGuest/DETAILHELPER",
        0,
        1025,
    )
    .unwrap();
    assert!(
        replacement_private_helper
            .text
            .contains("helper-source-marker-v2")
    );
    assert_eq!(
        replacement_private_helper.generation,
        replacement_identity.generation
    );
    let replacement_private_function = query_definition_source(
        &mut dispatcher,
        &mut task,
        "inspectguest/fn:detailfunction",
        0,
        1025,
    )
    .unwrap();
    assert!(
        replacement_private_function
            .text
            .contains("helper-function-source-marker-v2")
    );
    assert_eq!(
        replacement_private_function.generation,
        replacement_identity.generation
    );
    let (replacement_helper_result, replacement_helper_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT DEFINITION InspectGuest/DETAILHELPER",
    );
    assert_cli_succeeded(
        replacement_helper_result,
        &replacement_helper_output,
        "*INSPECT DEFINITION InspectGuest/DETAILHELPER",
    );
    assert_text_has(
        &replacement_helper_output,
        "helper-source-marker-v2",
        "*INSPECT DEFINITION InspectGuest/DETAILHELPER",
    );
    let (replacement_function_result, replacement_function_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT DEFINITION inspectguest/fn:detailfunction",
    );
    assert_cli_succeeded(
        replacement_function_result,
        &replacement_function_output,
        "*INSPECT DEFINITION inspectguest/fn:detailfunction",
    );
    assert_text_has(
        &replacement_function_output,
        "helper-function-source-marker-v2",
        "*INSPECT DEFINITION inspectguest/fn:detailfunction",
    );
    assert_eq!(
        call_guest(&mut dispatcher, &mut task, 0),
        204,
        "replacement keeps compatible workspace while changing source behavior"
    );

    // Delete is also explicit. Subsequent identity/list/source queries no
    // longer report the retired owner or its public SWI.
    let (delete_result, delete_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*RMKill InspectGuest",
    );
    assert_cli_succeeded(delete_result, &delete_output, "*RMKill InspectGuest");
    assert!(lookup_module(&mut dispatcher, &mut task, "InspectGuest").is_none());
    assert!(
        read_modules(&mut dispatcher, &mut task)
            .iter()
            .all(|module| !module.name.eq_ignore_ascii_case("InspectGuest"))
    );
    let (deleted_swi_result, deleted_swi_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT SWI Inspect_Run",
    );
    assert!(deleted_swi_result.is_err() || !deleted_swi_output.trim().is_empty());
    let deleted_source =
        query_definition_source(&mut dispatcher, &mut task, "InspectGuest/RUN", 0, 1025)
            .unwrap_err();
    assert!(matches!(
        deleted_source,
        RuntimeError::Structured { ref type_name, .. }
            if type_name == "ModuleNotFound"
    ));
    let mut missing_module_exports = SwiContext::default();
    missing_module_exports.registers[0] = 1;
    missing_module_exports.registers[1] = module_name_address;
    missing_module_exports.registers[3] = export_name_address;
    missing_module_exports.registers[4] = 128;
    missing_module_exports.registers[6] = export_definition_address;
    missing_module_exports.registers[7] = 128;
    let missing_module_export_result =
        dispatcher.dispatch(ACORN_MODULE_EXPORT, &mut task, &mut missing_module_exports);
    assert!(matches!(
        missing_module_export_result,
        Err(RuntimeError::Structured { ref type_name, .. })
            if type_name == "ModuleNotFound"
    ));
    let mut missing_swi = SwiContext::default();
    missing_swi.registers[0] = 1;
    missing_swi.registers[1] = INSPECT_RUN;
    missing_swi.registers[2] = QUERY_BUFFER + 0x400;
    missing_swi.registers[3] = 128;
    missing_swi.registers[4] = QUERY_BUFFER + 0x500;
    missing_swi.registers[5] = 128;
    missing_swi.registers[6] = QUERY_BUFFER + 0x600;
    missing_swi.registers[7] = 128;
    let missing_swi_result = dispatcher.dispatch(ACORN_SWI_INFO, &mut task, &mut missing_swi);
    assert!(matches!(
        missing_swi_result,
        Err(RuntimeError::Structured { ref type_name, .. })
            if type_name == "SwiIdentityNotFound"
    ));
    let (deleted_module_result, deleted_module_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT MODULE InspectGuest",
    );
    assert!(deleted_module_result.is_err() || !deleted_module_output.trim().is_empty());
    assert!(!deleted_module_output.contains("R0% = R0% + COUNT%"));
    let (deleted_source_result, deleted_source_output) = cli(
        &mut dispatcher,
        &mut task,
        &display_receiver,
        "*INSPECT DEFINITION InspectGuest/RUN",
    );
    assert!(deleted_source_result.is_err() || !deleted_source_output.trim().is_empty());
    assert!(!deleted_source_output.contains("R0% = R0% + COUNT%"));

    drop(task);
    drop(dispatcher);
    fs::remove_dir_all(root).unwrap();
}
