use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiContext, SwiDispatcher},
};

const X_BIT: u32 = 1 << 17;
const OS_CLI: u32 = 0x05;
const OS_MODULE: u32 = 0x1E;
const RICOCHET_MODULE_INFO: u32 = 0x4FF10;
const RICOCHET_MODULE_LOOKUP: u32 = 0x4FF12;
const RICOCHET_SWI_INFO: u32 = 0x4FF13;
const RICOCHET_MODULE_EXPORT: u32 = 0x4FF14;
const RICOCHET_DEFINITION_SOURCE: u32 = 0x4FF15;
const AUTH_PROBE: u32 = 0x4FF70;
const AUTH_FORWARD_SOURCE: u32 = 0x4FF71;
const AUTH_FORWARD_MANAGEMENT: u32 = 0x4FF72;

const CLI_ADDRESS: u32 = 0x2100;
const PATH_ADDRESS: u32 = 0x2200;
const TITLE_ADDRESS: u32 = 0x2300;
const SOURCE_SELECTOR_ADDRESS: u32 = 0x3000;
const SOURCE_BUFFER_ADDRESS: u32 = 0x4000;
const SOURCE_PATH_ADDRESS: u32 = 0x5000;
const FORWARD_SELECTOR_ADDRESS: u32 = 0x6100;
const FORWARD_SOURCE_BUFFER_ADDRESS: u32 = 0x6200;
const FORWARD_SOURCE_PATH_ADDRESS: u32 = 0x6600;

const AUTH_TARGET_V1: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE AuthorityTarget 1.0.0
REM @STATE COUNT% UINT32
REM @SWI Authority_Probe &4FF70 Probe REGISTERS=R0:U32:OUT
REM @PRIVATE PROC SecretRoutine
DEF PROC Probe
    COUNT% = COUNT% + 1
    R0% = COUNT%
ENDPROC
DEF PROC SecretRoutine
    REM private-source-marker-v1
ENDPROC
"#;

const AUTH_TARGET_V2: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE AuthorityTarget 1.0.0
REM @STATE COUNT% UINT32
REM @SWI Authority_Probe &4FF70 Probe REGISTERS=R0:U32:OUT
REM @PRIVATE PROC SecretRoutine
DEF PROC Probe
    COUNT% = COUNT% + 1
    R0% = COUNT% + 100
ENDPROC
DEF PROC SecretRoutine
    REM private-source-marker-v2
ENDPROC
"#;

// This guest has no ModuleManager primitive capability. Its only route to
// these operations is a nested public SWI call, so it tests whether the
// dispatcher preserves the originating Task through a privileged provider.
const AUTHORITY_FORWARDER: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE AuthorityForwarder 1.0.0
REM @SWI Authority_ForwardSource &4FF71 ForwardSource REGISTERS=R0:U32:OUT
REM @SWI Authority_ForwardManagement &4FF72 ForwardManagement REGISTERS=R0:U32:OUT
DEF PROC ForwardSource
    SYS "Ricochet_DefinitionSource", 1, &6100, 0, &6200, 256, 0, 0, &6600, 128, 0, 0
    R0% = 0
ENDPROC
DEF PROC ForwardManagement
    SYS "OS_Module", 4, &6100
    R0% = 0
ENDPROC
"#;

#[derive(Debug, Eq, PartialEq)]
struct ModuleInfo {
    name: String,
    version: [u32; 3],
    state: u32,
}

#[derive(Debug, Eq, PartialEq)]
struct SwiInfo {
    name: String,
    owner: String,
    definition: String,
    generation: u32,
}

fn scratch_directory() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "ricochet-ricochet-authorization-{}-{nanos}",
        std::process::id()
    ))
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

fn read_string(task: &Task, address: u32, capacity: usize) -> String {
    String::from_utf8(task.memory.read_c_string(address, capacity).unwrap()).unwrap()
}

fn assert_authorization_denied(error: RuntimeError, code: u32, message: &str) {
    assert!(
        matches!(
            &error,
            RuntimeError::Structured { type_name, code: actual_code, message: actual_message }
                if type_name == "TaskAuthorizationDenied"
                    && *actual_code == code
                    && actual_message == message
        ),
        "expected TaskAuthorizationDenied code {code} ({message}), got {error:?}"
    );
}

fn assert_error_block(task: &Task, address: u32, code: u32, message: &str) {
    let actual_code = u32::from_le_bytes(
        task.memory
            .read_bytes(address, 4)
            .unwrap()
            .try_into()
            .unwrap(),
    );
    assert_eq!(actual_code, code);
    let actual_message = read_string(task, address + 4, 252);
    assert_eq!(actual_message, message);
}

fn module_info(dispatcher: &mut SwiDispatcher, task: &mut Task, name: &str) -> Option<ModuleInfo> {
    put_string(task, TITLE_ADDRESS, name);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = TITLE_ADDRESS;
    dispatcher
        .dispatch(RICOCHET_MODULE_LOOKUP, task, &mut context)
        .unwrap();
    assert!(!context.overflow);
    (context.registers[1] != 0).then(|| ModuleInfo {
        name: name.to_owned(),
        version: [
            context.registers[3],
            context.registers[4],
            context.registers[5],
        ],
        state: context.registers[6],
    })
}

fn module_inventory_contains(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    wanted_name: &str,
) -> bool {
    let mut cursor = 0;
    for _ in 0..64 {
        let mut context = SwiContext::default();
        context.registers[0] = 1;
        context.registers[1] = cursor;
        context.registers[2] = SOURCE_BUFFER_ADDRESS;
        context.registers[3] = 128;
        dispatcher
            .dispatch(RICOCHET_MODULE_INFO, task, &mut context)
            .unwrap();
        assert!(!context.overflow);
        if context.registers[4] == 0 {
            return false;
        }
        let name = read_string(task, SOURCE_BUFFER_ADDRESS, 128);
        if name.eq_ignore_ascii_case(wanted_name) {
            return true;
        }
        cursor = context.registers[1];
    }
    panic!("active module enumeration did not end within 64 records");
}

fn swi_info(dispatcher: &mut SwiDispatcher, task: &mut Task, number: u32) -> SwiInfo {
    let name_address = SOURCE_BUFFER_ADDRESS;
    let owner_address = SOURCE_BUFFER_ADDRESS + 0x100;
    let definition_address = SOURCE_BUFFER_ADDRESS + 0x200;
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
        .dispatch(RICOCHET_SWI_INFO, task, &mut context)
        .unwrap();
    assert!(!context.overflow);
    SwiInfo {
        name: read_string(task, name_address, 128),
        owner: read_string(task, owner_address, 128),
        definition: read_string(task, definition_address, 128),
        generation: context.registers[8],
    }
}

fn module_export_generation(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    module_name: &str,
) -> Option<u32> {
    let module_address = SOURCE_BUFFER_ADDRESS;
    let export_name_address = SOURCE_BUFFER_ADDRESS + 0x100;
    let definition_address = SOURCE_BUFFER_ADDRESS + 0x200;
    put_string(task, module_address, module_name);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = module_address;
    context.registers[2] = 0;
    context.registers[3] = export_name_address;
    context.registers[4] = 128;
    context.registers[6] = definition_address;
    context.registers[7] = 128;
    dispatcher
        .dispatch(RICOCHET_MODULE_EXPORT, task, &mut context)
        .unwrap();
    assert!(!context.overflow);
    (context.registers[9] != 0).then_some(context.registers[8])
}

fn source_query(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    selector: &str,
    source_address: u32,
    source_capacity: u32,
    path_address: u32,
) -> Result<(String, String, u32), RuntimeError> {
    put_string(task, SOURCE_SELECTOR_ADDRESS, selector);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = SOURCE_SELECTOR_ADDRESS;
    context.registers[2] = 0;
    context.registers[3] = source_address;
    context.registers[4] = source_capacity;
    context.registers[7] = path_address;
    context.registers[8] = 128;
    dispatcher.dispatch(RICOCHET_DEFINITION_SOURCE, task, &mut context)?;
    Ok((
        read_string(task, source_address, source_capacity as usize),
        read_string(task, path_address, 128),
        context.registers[6],
    ))
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
            "OS_CLI must preserve R0 for successful command {command:?}"
        );
    }
    let bytes = receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    (
        result,
        context,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

fn load_direct(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    guest_path: &str,
) -> Result<(), RuntimeError> {
    put_string(task, PATH_ADDRESS, guest_path);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = PATH_ADDRESS;
    dispatcher.dispatch(OS_MODULE, task, &mut context)
}

fn invoke_probe(dispatcher: &mut SwiDispatcher, task: &mut Task) -> u32 {
    let mut context = SwiContext::default();
    dispatcher.dispatch(AUTH_PROBE, task, &mut context).unwrap();
    context.registers[0]
}

#[test]
fn ricochet_services_enforce_task_scoped_read_and_management_authority() {
    let root = scratch_directory();
    fs::create_dir_all(&root).unwrap();
    let config_path = root.join("isolated-configure");
    // Dispatcher configuration and HostFS are process-global inputs; isolate
    // them so this black-box policy test cannot touch a developer's settings.
    unsafe {
        std::env::set_var("RICOCHET_CONFIG_PATH", &config_path);
        std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
    }
    write_guest_module(&root, "AuthorityV1", AUTH_TARGET_V1);
    write_guest_module(&root, "AuthorityV2", AUTH_TARGET_V2);
    write_guest_module(&root, "AuthorityForwarder", AUTHORITY_FORWARDER);

    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut dispatcher =
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);

    // These are the production host bootstrap profiles: ordinary/spawned,
    // combined interactive MOS, source-only inspection, and management-only.
    let mut ordinary = Task::new(0xA001);
    let mut mos_session = Task::trusted_mos_session(0xA002);
    let mut source_inspector = Task::trusted_source_inspector(0xA003);
    let mut module_manager = Task::trusted_module_manager(0xA004);
    // Equal numeric identity is not an authority transfer; rights belong to
    // the private Task object, not an ID lookup table.
    let mut forged_same_id = Task::new(mos_session.id);

    // Both an explicitly management-authorized caller and the trusted MOS
    // session can use the published OS_Module boundary to install the target.
    load_direct(&mut dispatcher, &mut module_manager, "AuthorityV1").unwrap();
    assert_eq!(
        module_info(&mut dispatcher, &mut ordinary, "AuthorityTarget"),
        Some(ModuleInfo {
            name: "AuthorityTarget".into(),
            version: [1, 0, 0],
            state: 4,
        })
    );

    // Module inventory, title lookup and SWI identity remain public metadata
    // for every task profile; that is intentionally distinct from source read.
    for task in [
        &mut ordinary,
        &mut mos_session,
        &mut source_inspector,
        &mut module_manager,
        &mut forged_same_id,
    ] {
        assert!(module_inventory_contains(
            &mut dispatcher,
            task,
            "AuthorityTarget"
        ));
        let info = swi_info(&mut dispatcher, task, AUTH_PROBE);
        assert_eq!(info.name, "Authority_Probe");
        assert_eq!(info.owner, "AuthorityTarget");
        assert_eq!(info.definition, "PROBE");
        assert_eq!(info.generation, 1);
        assert_eq!(
            module_export_generation(&mut dispatcher, task, "AuthorityTarget"),
            Some(1)
        );
    }

    // Public command summaries are wrappers over those same metadata SWIs and
    // are readable even when the caller lacks source or management authority.
    let (list_result, _, list_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT MODULE AuthorityTarget",
    );
    assert!(
        list_result.is_ok(),
        "public metadata command failed: {list_result:?}"
    );
    assert!(list_output.contains("AuthorityTarget"));
    assert!(list_output.contains("1.0.0"));
    assert!(list_output.contains("Authority_Probe"));
    assert!(list_output.to_ascii_lowercase().contains("generation 1"));

    // The combined host-authorized interactive session and source-only profile
    // can read even a private definition. The result stays in each caller's
    // own logical address space.
    let ordinary_buffer_before = ordinary
        .memory
        .read_bytes(SOURCE_BUFFER_ADDRESS, 96)
        .unwrap();
    let (source, path, generation) = source_query(
        &mut dispatcher,
        &mut mos_session,
        "AuthorityTarget/SecretRoutine",
        SOURCE_BUFFER_ADDRESS,
        1025,
        SOURCE_PATH_ADDRESS,
    )
    .unwrap();
    assert!(source.contains("private-source-marker-v1"));
    assert!(path.contains("AuthorityV1"));
    assert_eq!(generation, 1);
    assert_eq!(
        ordinary
            .memory
            .read_bytes(SOURCE_BUFFER_ADDRESS, ordinary_buffer_before.len())
            .unwrap(),
        ordinary_buffer_before,
        "source read from the MOS task wrote into another task's logical memory"
    );
    let (inspected_source, _, inspected_generation) = source_query(
        &mut dispatcher,
        &mut source_inspector,
        "AuthorityTarget/SecretRoutine",
        SOURCE_BUFFER_ADDRESS,
        1025,
        SOURCE_PATH_ADDRESS,
    )
    .unwrap();
    assert!(inspected_source.contains("private-source-marker-v1"));
    assert_eq!(inspected_generation, 1);
    assert_eq!(
        ordinary
            .memory
            .read_bytes(SOURCE_BUFFER_ADDRESS, ordinary_buffer_before.len())
            .unwrap(),
        ordinary_buffer_before,
        "source read from the inspector task wrote into another task's logical memory"
    );

    // Ordinary and management-only tasks are denied direct source access. The
    // structured API must reject before touching either output buffer.
    let sentinel_source = vec![0xA5; 96];
    let sentinel_path = vec![0x5A; 64];
    ordinary
        .memory
        .write_bytes(SOURCE_BUFFER_ADDRESS, &sentinel_source)
        .unwrap();
    ordinary
        .memory
        .write_bytes(SOURCE_PATH_ADDRESS, &sentinel_path)
        .unwrap();
    let error = source_query(
        &mut dispatcher,
        &mut ordinary,
        "AuthorityTarget/SecretRoutine",
        SOURCE_BUFFER_ADDRESS,
        96,
        SOURCE_PATH_ADDRESS,
    )
    .unwrap_err();
    assert_authorization_denied(
        error,
        1,
        "caller task lacks definition-source read authority",
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(SOURCE_BUFFER_ADDRESS, sentinel_source.len())
            .unwrap(),
        sentinel_source,
        "denied source query changed the caller's source buffer"
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(SOURCE_PATH_ADDRESS, sentinel_path.len())
            .unwrap(),
        sentinel_path,
        "denied source query changed the caller's source-path buffer"
    );
    // The SWI contract prevalidates the declared logical-memory range, but an
    // ordinary caller must still be denied before the provider parses source
    // selector contents. A readable, unterminated selector therefore yields
    // the authority error, not a selector/terminator error.
    ordinary
        .memory
        .write_bytes(SOURCE_SELECTOR_ADDRESS, &vec![b'X'; 128])
        .unwrap();
    let mut denied_unterminated_source = SwiContext::default();
    denied_unterminated_source.registers[0] = 1;
    denied_unterminated_source.registers[1] = SOURCE_SELECTOR_ADDRESS;
    denied_unterminated_source.registers[2] = 0;
    denied_unterminated_source.registers[3] = SOURCE_BUFFER_ADDRESS;
    denied_unterminated_source.registers[4] = 96;
    denied_unterminated_source.registers[7] = SOURCE_PATH_ADDRESS;
    denied_unterminated_source.registers[8] = 64;
    assert_authorization_denied(
        dispatcher
            .dispatch(
                RICOCHET_DEFINITION_SOURCE,
                &mut ordinary,
                &mut denied_unterminated_source,
            )
            .unwrap_err(),
        1,
        "caller task lacks definition-source read authority",
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(SOURCE_BUFFER_ADDRESS, sentinel_source.len())
            .unwrap(),
        sentinel_source,
        "authorization rejection after pointer-range checking wrote source output"
    );
    let error = source_query(
        &mut dispatcher,
        &mut module_manager,
        "AuthorityTarget/SecretRoutine",
        SOURCE_BUFFER_ADDRESS,
        1025,
        SOURCE_PATH_ADDRESS,
    )
    .unwrap_err();
    assert_authorization_denied(
        error,
        1,
        "caller task lacks definition-source read authority",
    );

    // The same policy applies through OS_CLI's BASIC64 wrapper. A provider's
    // ModuleIntrospection capability must not launder source-read authority.
    let (source_denial, _, denied_source_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT DEFINITION AuthorityTarget/SecretRoutine",
    );
    assert_authorization_denied(
        source_denial.unwrap_err(),
        1,
        "caller task lacks definition-source read authority",
    );
    assert!(
        !denied_source_output.contains("private-source-marker-v1"),
        "denied CLI source request leaked private source: {denied_source_output:?}"
    );
    let (allowed_source, _, allowed_source_output) = cli(
        &mut dispatcher,
        &mut source_inspector,
        &display_receiver,
        "*INSPECT DEFINITION AuthorityTarget/SecretRoutine",
    );
    assert!(
        allowed_source.is_ok(),
        "source-inspector CLI failed: {allowed_source:?}"
    );
    assert!(allowed_source_output.contains("private-source-marker-v1"));

    // X-form authorization failures return the same structured code/message
    // in the caller's reserved error block and set V, rather than exposing a
    // provider's authority or leaving success-looking output.
    let mut x_source = SwiContext::default();
    x_source.registers[0] = 1;
    x_source.registers[1] = SOURCE_SELECTOR_ADDRESS;
    x_source.registers[2] = 0;
    x_source.registers[3] = SOURCE_BUFFER_ADDRESS;
    x_source.registers[4] = 128;
    x_source.registers[7] = SOURCE_PATH_ADDRESS;
    x_source.registers[8] = 128;
    put_string(
        &mut ordinary,
        SOURCE_SELECTOR_ADDRESS,
        "AuthorityTarget/SecretRoutine",
    );
    ordinary
        .memory
        .write_bytes(SOURCE_BUFFER_ADDRESS, &sentinel_source)
        .unwrap();
    dispatcher
        .dispatch(
            RICOCHET_DEFINITION_SOURCE | X_BIT,
            &mut ordinary,
            &mut x_source,
        )
        .unwrap();
    assert!(x_source.overflow);
    assert_eq!(
        x_source.registers[0],
        ordinary.memory.swi_error_block_address()
    );
    assert_error_block(
        &ordinary,
        x_source.registers[0],
        1,
        "caller task lacks definition-source read authority",
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(SOURCE_BUFFER_ADDRESS, sentinel_source.len())
            .unwrap(),
        sentinel_source
    );

    // Give the target persistent state, then try replacement and deletion
    // through both direct OS_Module and the command wrapper from callers with
    // no management grant. Rejection must preserve source, generation,
    // registry ownership, and the existing workspace value.
    assert_eq!(invoke_probe(&mut dispatcher, &mut ordinary), 1);
    let before_swi = swi_info(&mut dispatcher, &mut ordinary, AUTH_PROBE);
    assert_eq!(before_swi.generation, 1);
    let mut denied_load = SwiContext::default();
    denied_load.registers[0] = 1;
    denied_load.registers[1] = PATH_ADDRESS;
    put_string(&mut ordinary, PATH_ADDRESS, "AuthorityV2");
    let direct_management_error = dispatcher
        .dispatch(OS_MODULE, &mut ordinary, &mut denied_load)
        .unwrap_err();
    assert_authorization_denied(
        direct_management_error,
        2,
        "caller task lacks module-management authority",
    );
    // OS_Module's authorized path would dereference R1. The unprivileged
    // request is rejected before that pointer is read.
    let mut denied_invalid_pointer_load = SwiContext::default();
    denied_invalid_pointer_load.registers[0] = 1;
    denied_invalid_pointer_load.registers[1] = u32::MAX;
    assert_authorization_denied(
        dispatcher
            .dispatch(OS_MODULE, &mut ordinary, &mut denied_invalid_pointer_load)
            .unwrap_err(),
        2,
        "caller task lacks module-management authority",
    );
    put_string(&mut source_inspector, PATH_ADDRESS, "AuthorityV2");
    let mut source_only_direct_load = SwiContext::default();
    source_only_direct_load.registers[0] = 1;
    source_only_direct_load.registers[1] = PATH_ADDRESS;
    assert_authorization_denied(
        dispatcher
            .dispatch(
                OS_MODULE,
                &mut source_inspector,
                &mut source_only_direct_load,
            )
            .unwrap_err(),
        2,
        "caller task lacks module-management authority",
    );
    assert_eq!(
        module_info(&mut dispatcher, &mut ordinary, "AuthorityTarget")
            .unwrap()
            .state,
        4
    );
    assert_eq!(
        swi_info(&mut dispatcher, &mut ordinary, AUTH_PROBE),
        before_swi
    );
    assert_eq!(
        module_export_generation(&mut dispatcher, &mut ordinary, "AuthorityTarget"),
        Some(1)
    );
    assert!(
        source_query(
            &mut dispatcher,
            &mut mos_session,
            "AuthorityTarget/SecretRoutine",
            SOURCE_BUFFER_ADDRESS,
            1025,
            SOURCE_PATH_ADDRESS,
        )
        .unwrap()
        .0
        .contains("private-source-marker-v1")
    );

    let (denied_reload, _, reload_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RMLoad AuthorityV2",
    );
    assert_authorization_denied(
        denied_reload.unwrap_err(),
        2,
        "caller task lacks module-management authority",
    );
    assert!(!reload_output.contains("private-source-marker-v2"));
    let (denied_delete, _, _) = cli(
        &mut dispatcher,
        &mut source_inspector,
        &display_receiver,
        "*RMKill AuthorityTarget",
    );
    assert_authorization_denied(
        denied_delete.unwrap_err(),
        2,
        "caller task lacks module-management authority",
    );
    assert_eq!(invoke_probe(&mut dispatcher, &mut ordinary), 2);
    assert_eq!(
        swi_info(&mut dispatcher, &mut ordinary, AUTH_PROBE).generation,
        1
    );
    assert!(
        source_query(
            &mut dispatcher,
            &mut ordinary,
            "AuthorityTarget/SecretRoutine",
            SOURCE_BUFFER_ADDRESS,
            1025,
            SOURCE_PATH_ADDRESS,
        )
        .is_err()
    );
    assert!(
        source_query(
            &mut dispatcher,
            &mut mos_session,
            "AuthorityTarget/SecretRoutine",
            SOURCE_BUFFER_ADDRESS,
            1025,
            SOURCE_PATH_ADDRESS,
        )
        .unwrap()
        .0
        .contains("private-source-marker-v1")
    );

    let mut x_delete = SwiContext::default();
    x_delete.registers[0] = 4;
    x_delete.registers[1] = TITLE_ADDRESS;
    put_string(&mut forged_same_id, TITLE_ADDRESS, "AuthorityTarget");
    dispatcher
        .dispatch(OS_MODULE | X_BIT, &mut forged_same_id, &mut x_delete)
        .unwrap();
    assert!(x_delete.overflow);
    assert_eq!(
        x_delete.registers[0],
        forged_same_id.memory.swi_error_block_address()
    );
    assert_error_block(
        &forged_same_id,
        x_delete.registers[0],
        2,
        "caller task lacks module-management authority",
    );
    assert!(module_info(&mut dispatcher, &mut forged_same_id, "AuthorityTarget").is_some());

    // A source-only task cannot call the management service from guest code.
    // Loading this forwarding module does not grant it either capability; the
    // nested public service must still see the original Task.
    let (forwarder_load, _, forwarder_output) = cli(
        &mut dispatcher,
        &mut mos_session,
        &display_receiver,
        "*RMLoad AuthorityForwarder",
    );
    assert!(
        forwarder_load.is_ok(),
        "trusted MOS guest load failed: {forwarder_load:?} {forwarder_output:?}"
    );
    put_string(
        &mut ordinary,
        FORWARD_SELECTOR_ADDRESS,
        "AuthorityTarget/SecretRoutine",
    );
    ordinary
        .memory
        .write_bytes(FORWARD_SOURCE_BUFFER_ADDRESS, &sentinel_source)
        .unwrap();
    ordinary
        .memory
        .write_bytes(FORWARD_SOURCE_PATH_ADDRESS, &sentinel_path)
        .unwrap();
    let mut nested_source = SwiContext::default();
    let nested_source_error = dispatcher
        .dispatch(AUTH_FORWARD_SOURCE, &mut ordinary, &mut nested_source)
        .unwrap_err();
    assert_authorization_denied(
        nested_source_error,
        1,
        "caller task lacks definition-source read authority",
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(FORWARD_SOURCE_BUFFER_ADDRESS, sentinel_source.len())
            .unwrap(),
        sentinel_source,
        "nested denial changed caller source buffer"
    );
    put_string(
        &mut source_inspector,
        FORWARD_SELECTOR_ADDRESS,
        "AuthorityTarget/SecretRoutine",
    );
    let mut nested_source_allowed = SwiContext::default();
    dispatcher
        .dispatch(
            AUTH_FORWARD_SOURCE,
            &mut source_inspector,
            &mut nested_source_allowed,
        )
        .unwrap();
    assert!(
        read_string(&source_inspector, FORWARD_SOURCE_BUFFER_ADDRESS, 256)
            .contains("private-source-marker-v1"),
        "nested source call did not retain the source inspector's authority"
    );
    let mut nested_management = SwiContext::default();
    let nested_management_error = dispatcher
        .dispatch(
            AUTH_FORWARD_MANAGEMENT,
            &mut ordinary,
            &mut nested_management,
        )
        .unwrap_err();
    assert_authorization_denied(
        nested_management_error,
        2,
        "caller task lacks module-management authority",
    );
    assert!(module_info(&mut dispatcher, &mut ordinary, "AuthorityTarget").is_some());
    assert_eq!(
        swi_info(&mut dispatcher, &mut ordinary, AUTH_PROBE).generation,
        1
    );

    // The two authority dimensions are independent. The manager-only profile
    // can replace/delete but cannot inspect retained source; the source-only
    // profile can read private source but cannot mutate. Trusted MOS has both.
    let (manager_source_result, _, manager_source_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*INSPECT DEFINITION AuthorityTarget/SecretRoutine",
    );
    assert_authorization_denied(
        manager_source_result.unwrap_err(),
        1,
        "caller task lacks definition-source read authority",
    );
    assert!(!manager_source_output.contains("private-source-marker-v1"));

    let (allowed_reload, _, reload_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*RMLoad AuthorityV2",
    );
    assert!(
        allowed_reload.is_ok(),
        "authorized reload failed: {allowed_reload:?} {reload_output:?}"
    );
    assert_eq!(
        swi_info(&mut dispatcher, &mut module_manager, AUTH_PROBE).generation,
        2
    );
    assert_eq!(
        module_export_generation(&mut dispatcher, &mut module_manager, "AuthorityTarget"),
        Some(2)
    );
    assert!(
        source_query(
            &mut dispatcher,
            &mut mos_session,
            "AuthorityTarget/SecretRoutine",
            SOURCE_BUFFER_ADDRESS,
            1025,
            SOURCE_PATH_ADDRESS,
        )
        .unwrap()
        .0
        .contains("private-source-marker-v2")
    );
    assert_eq!(
        invoke_probe(&mut dispatcher, &mut module_manager),
        103,
        "compatible management-authorized reload should keep COUNT=2 workspace"
    );

    // Explicit command Delete is authorized only for a management-capable
    // task. The same manager-only task that could not see source can remove
    // the module, and public metadata then reports the owner as absent.
    let (delete_result, _, delete_output) = cli(
        &mut dispatcher,
        &mut module_manager,
        &display_receiver,
        "*RMKill AuthorityTarget",
    );
    assert!(
        delete_result.is_ok(),
        "authorized delete failed: {delete_result:?} {delete_output:?}"
    );
    assert!(module_info(&mut dispatcher, &mut ordinary, "AuthorityTarget").is_none());
    let unknown_export = dispatcher.dispatch(AUTH_PROBE, &mut ordinary, &mut SwiContext::default());
    assert!(
        unknown_export.is_err(),
        "deleted module SWI remained callable"
    );
}
