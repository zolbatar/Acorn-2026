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
    swi::{
        RICOCHET_MODULE_LOOKUP, RICOCHET_SWI_INFO, DisplayEvent, OS_WRITE_C, SwiContext, SwiDispatcher,
    },
};

const X_BIT: u32 = 1 << 17;
const OS_MODULE: u32 = 0x1E;
const SWAP_RUN: u32 = 0x4FF40;
const SWAP_SECOND: u32 = 0x4FF41;
const SWAP_DURING: u32 = 0x4FF42;
const SWAP_OBSERVE: u32 = 0x4FF43;
const FN_CONSUMER_READ: u32 = 0x4FF50;
const LOAD_PATH_ADDRESS: u32 = 0x2100;
const NESTED_PATH_ADDRESS: u32 = 0x2200;
const MODULE_TITLE_ADDRESS: u32 = 0x2300;
const QUERY_NAME_ADDRESS: u32 = 0x3000;
const QUERY_OWNER_ADDRESS: u32 = 0x3100;
const QUERY_DEFINITION_ADDRESS: u32 = 0x3200;

const V1_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE SwapService 1.0.0
REM @LIFECYCLE START Start
REM @STATE COUNT% UINT32
REM @STATE STARTS% UINT32
REM @STATE STATS PersistentStats
REM @SWI Swap_Run &4FF40 Run REGISTERS=R0:U32:INOUT
REM @SWI Swap_Second &4FF41 Second REGISTERS=R0:U32:INOUT
REM @SWI Swap_During &4FF42 During REGISTERS=R0:U32:INOUT
REM @SWI Swap_Observe &4FF43 Observe REGISTERS=R0:U32:OUT
REM @EXPORT FN Adjust
RECORD PersistentStats
    Total AS UINT32
END RECORD
DEF PROC Start
    STARTS% = STARTS% + 1
ENDPROC
DEF PROC Run
    COUNT% = COUNT% + 1
    R0% = R0% + COUNT%
ENDPROC
DEF PROC Second
    COUNT% = COUNT% + 10
    R0% = R0% + COUNT%
ENDPROC
DEF PROC During
    SYS "OS_Module", 1, &2200
    R0% = R0% + 17
ENDPROC
DEF PROC Observe
    R0% = STARTS% * 1000 + COUNT%
ENDPROC
DEF FN Adjust(amount AS UINT32) AS UINT32
=amount + 1
"#;

const V2_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE SwapService 1.0.0
REM @LIFECYCLE START Start
REM @STATE COUNT% UINT32
REM @STATE STARTS% UINT32
REM @STATE STATS PersistentStats
REM @SWI Swap_Run &4FF40 Run REGISTERS=R0:U32:INOUT
REM @SWI Swap_Second &4FF41 Second REGISTERS=R0:U32:INOUT
REM @SWI Swap_During &4FF42 During REGISTERS=R0:U32:INOUT
REM @SWI Swap_Observe &4FF43 Observe REGISTERS=R0:U32:OUT
REM @EXPORT FN Adjust
RECORD PersistentStats
    Total AS UINT32
END RECORD
DEF PROC Start
    SYS "NoSuchReplacementStart"
ENDPROC
DEF PROC Run
    COUNT% = COUNT% + 1
    R0% = R0% + 1000 + COUNT%
ENDPROC
DEF PROC Second
    COUNT% = COUNT% + 10
    R0% = R0% + 2000 + COUNT%
ENDPROC
DEF PROC During
    R0% = R0% + 3000 + COUNT%
ENDPROC
DEF PROC Observe
    R0% = STARTS% * 1000 + COUNT%
ENDPROC
DEF FN Adjust(amount AS UINT32) AS UINT32
=amount + 1
"#;

const FN_PROVIDER_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE MetricProvider 1.0.0
REM @EXPORT FN ReadCount
DEF FN ReadCount() AS UINT32
=41
"#;

const FN_CONSUMER_V1_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE FnConsumer 1.0.0
REM @IMPORT_MODULE MetricProvider 1.0.0
REM @IMPORT_SYMBOL MetricProvider FN ReadCount
REM @SWI FnConsumer_Read &4FF50 ReadValue REGISTERS=R0:U32:OUT
DEF PROC ReadValue
    R0% = FN MetricProvider.ReadCount()
ENDPROC
"#;

const FN_CONSUMER_V2_SOURCE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE FnConsumer 1.0.0
REM @IMPORT_MODULE MetricProvider 1.0.0
REM @IMPORT_SYMBOL MetricProvider FN ReadCount
REM @SWI FnConsumer_Read &4FF50 ReadValue REGISTERS=R0:U32:OUT
DEF PROC ReadValue
    R0% = FN MetricProvider.ReadCount() + 1
ENDPROC
"#;

fn scratch_directory() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "ricochet-ricochet-wp51-replacement-{}-{nanos}",
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

fn get_word(task: &Task, address: u32) -> u32 {
    u32::from_le_bytes(
        task.memory
            .read_bytes(address, 4)
            .unwrap()
            .try_into()
            .unwrap(),
    )
}

fn error_text(task: &Task, address: u32) -> String {
    String::from_utf8(task.memory.read_c_string(address + 4, 252).unwrap()).unwrap()
}

fn assert_x_error(task: &Task, context: &SwiContext, expected_fragment: &str) {
    assert!(context.overflow, "expected V=1 for the X-form error");
    let address = context.registers[0];
    assert_eq!(get_word(task, address), 1, "hosted module error code");
    let message = error_text(task, address);
    assert!(
        message
            .to_ascii_lowercase()
            .contains(&expected_fragment.to_ascii_lowercase()),
        "error message {message:?} did not contain {expected_fragment:?}"
    );
}

fn load_guest_module(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    path: &str,
    x_form: bool,
) -> Result<SwiContext, RuntimeError> {
    put_string(task, LOAD_PATH_ADDRESS, path);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = LOAD_PATH_ADDRESS;
    dispatcher.dispatch(
        OS_MODULE | if x_form { X_BIT } else { 0 },
        task,
        &mut context,
    )?;
    Ok(context)
}

fn call_swi(dispatcher: &mut SwiDispatcher, task: &mut Task, number: u32, r0: u32) -> SwiContext {
    let mut context = SwiContext::default();
    context.registers[0] = r0;
    dispatcher.dispatch(number, task, &mut context).unwrap();
    context
}

fn query_guest_identity(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    number: u32,
) -> (String, String, String, u32) {
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = number;
    context.registers[2] = QUERY_NAME_ADDRESS;
    context.registers[3] = 128;
    context.registers[4] = QUERY_OWNER_ADDRESS;
    context.registers[5] = 128;
    context.registers[6] = QUERY_DEFINITION_ADDRESS;
    context.registers[7] = 128;
    dispatcher
        .dispatch(RICOCHET_SWI_INFO, task, &mut context)
        .unwrap();
    assert!(!context.overflow);
    (
        String::from_utf8(task.memory.read_c_string(QUERY_NAME_ADDRESS, 128).unwrap()).unwrap(),
        String::from_utf8(task.memory.read_c_string(QUERY_OWNER_ADDRESS, 128).unwrap()).unwrap(),
        String::from_utf8(
            task.memory
                .read_c_string(QUERY_DEFINITION_ADDRESS, 128)
                .unwrap(),
        )
        .unwrap(),
        context.registers[8],
    )
}

fn assert_replacement_rejected(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    candidate_path: &str,
    number: u32,
    owner_id: ricochet::ricochet::ModuleId,
    entry_id: ricochet::ricochet::SwiEntryId,
    generation: u64,
    source_path: &str,
    source_hash: &str,
    message_fragment: &str,
) {
    let context = load_guest_module(dispatcher, task, candidate_path, true).unwrap();
    assert_x_error(task, &context, message_fragment);
    let registry = dispatcher.module_registry();
    let module = registry.module_named("SwapService").unwrap();
    assert_eq!(module.id, owner_id, "rejection keeps module identity");
    assert_eq!(module.manifest.source_hash, source_hash);
    assert_eq!(registry.swi_entry_id(number), Some(entry_id));
    let identity = registry.active_swi_identity(number).unwrap();
    assert_eq!(identity.generation_number, generation);
    assert_eq!(identity.source_path, source_path);
    assert_eq!(identity.module, owner_id);
}

#[test]
fn wp51_os_module_replacement_is_atomic_compatible_and_preserves_active_calls() {
    let root = scratch_directory();
    fs::create_dir_all(&root).unwrap();
    let config_path = root.join("isolated-configure");
    // This integration process owns the only test in this file; give its
    // dispatcher both an isolated preference file and a private HostFS root.
    unsafe {
        std::env::set_var("RICOCHET_CONFIG_PATH", &config_path);
        std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
    }

    let bad_contract = V2_SOURCE.replacen("REGISTERS=R0:U32:INOUT", "REGISTERS=R0:BYTE:INOUT", 1);
    let bad_state = V2_SOURCE.replacen(
        "REM @STATE STARTS% UINT32\n",
        "REM @STATE STARTS% UINT32\nREM @STATE EXTRA% UINT32\n",
        1,
    );
    let bad_named_type_layout = V2_SOURCE.replacen(
        "    Total AS UINT32\nEND RECORD\n",
        "    Total AS UINT32\n    Epoch AS UINT32\nEND RECORD\n",
        1,
    );
    let bad_export_signature = V2_SOURCE.replacen(
        "DEF FN Adjust(amount AS UINT32) AS UINT32",
        "DEF FN Adjust(amount AS UINT64) AS UINT32",
        1,
    );
    assert_ne!(bad_named_type_layout, V2_SOURCE);
    assert_ne!(bad_export_signature, V2_SOURCE);
    let bad_capability = V2_SOURCE.replacen(
        "REM @MODULE SwapService 1.0.0\n",
        "REM @MODULE SwapService 1.0.0\nREM @CAPABILITY ConsoleOutput\nREM @IMPORT Host.Console.WriteByte ConsoleOutput\n",
        1,
    );
    let bad_dependency = V2_SOURCE.replacen(
        "REM @MODULE SwapService 1.0.0\n",
        "REM @MODULE SwapService 1.0.0\nREM @IMPORT_MODULE Console 1.0.0\n",
        1,
    );
    let bad_lifecycle = V2_SOURCE
        .replacen(
            "REM @LIFECYCLE START Start\n",
            "REM @LIFECYCLE START Start\nREM @LIFECYCLE QUIESCE Quiesce\n",
            1,
        )
        .to_owned()
        + "\nDEF PROC Quiesce\nENDPROC\n";
    let case_only_title = V2_SOURCE.replace(
        "REM @MODULE SwapService 1.0.0",
        "REM @MODULE swapservice 1.0.0",
    );

    write_guest_module(&root, "SwapV1", V1_SOURCE);
    write_guest_module(&root, "SwapV2", V2_SOURCE);
    write_guest_module(&root, "SwapCase", &case_only_title);
    write_guest_module(&root, "SwapBadContract", &bad_contract);
    write_guest_module(&root, "SwapBadState", &bad_state);
    write_guest_module(&root, "SwapBadNamedTypeLayout", &bad_named_type_layout);
    write_guest_module(&root, "SwapBadExportSignature", &bad_export_signature);
    write_guest_module(&root, "SwapBadCapability", &bad_capability);
    write_guest_module(&root, "SwapBadDependency", &bad_dependency);
    write_guest_module(&root, "SwapBadLifecycle", &bad_lifecycle);
    write_guest_module(&root, "MetricProvider", FN_PROVIDER_SOURCE);
    write_guest_module(&root, "FnConsumerV1", FN_CONSUMER_V1_SOURCE);
    write_guest_module(&root, "FnConsumerV2", FN_CONSUMER_V2_SOURCE);

    let console_original = include_str!("../modules/Console.bas64");
    let console_candidate = console_original.replace(
        "    PROC EmitByte(R0% AND &FF)",
        "    PROC EmitByte(R0% AND &FF): PROC EmitByte(33)",
    );
    assert_ne!(console_candidate, console_original);
    write_guest_module(&root, "ConsoleCandidate", &console_candidate);

    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut dispatcher =
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    let mut task = Task::trusted_mos_session(0x51_5101);

    // Stage all caller strings before the first SWI. During calls Swap_During
    // re-enters the public OS_Module entry using this task's checked memory.
    put_string(&mut task, NESTED_PATH_ADDRESS, "SwapV2");
    put_string(&mut task, MODULE_TITLE_ADDRESS, "SwapService");
    let first_load = load_guest_module(&mut dispatcher, &mut task, "SwapV1", false).unwrap();
    assert!(!first_load.overflow);
    assert_eq!(first_load.registers[0], 1);
    assert_eq!(first_load.registers[1], LOAD_PATH_ADDRESS);

    let initial_module = dispatcher
        .module_registry()
        .module_named("SwapService")
        .expect("guest module is active");
    let module_id = initial_module.id;
    let module_instance = initial_module.instance_id;
    let initial_source_hash = initial_module.manifest.source_hash.clone();
    let initial_generation = dispatcher
        .module_registry()
        .active_swi_identity(SWAP_RUN)
        .unwrap()
        .generation_number;
    assert_eq!(initial_generation, 1);
    let entry_ids = [SWAP_RUN, SWAP_SECOND, SWAP_DURING, SWAP_OBSERVE]
        .map(|number| dispatcher.module_registry().swi_entry_id(number).unwrap());

    // First mutate persistent module state through the public exported SWI.
    // During replacement, the old SWI frame remains active while its nested
    // OS_Module Load atomically publishes the candidate generation.
    assert_eq!(
        call_swi(&mut dispatcher, &mut task, SWAP_RUN, 0).registers[0],
        1
    );
    let old_call_result = call_swi(&mut dispatcher, &mut task, SWAP_DURING, 5);
    assert_eq!(
        old_call_result.registers[0], 22,
        "the old frame resumes with its original R0 and old +17 behavior"
    );

    let replacement_module = dispatcher
        .module_registry()
        .module_named("SwapService")
        .expect("replacement retains the active title");
    assert_eq!(
        replacement_module.id, module_id,
        "module identity is stable"
    );
    assert_eq!(
        replacement_module.instance_id, module_instance,
        "module-instance identity is stable"
    );
    assert!(replacement_module.manifest.source_path.contains("SwapV2"));
    assert_ne!(replacement_module.manifest.source_hash, initial_source_hash);

    let names = ["Swap_Run", "Swap_Second", "Swap_During", "Swap_Observe"];
    let definitions = ["RUN", "SECOND", "DURING", "OBSERVE"];
    let numbers = [SWAP_RUN, SWAP_SECOND, SWAP_DURING, SWAP_OBSERVE];
    for index in 0..numbers.len() {
        let number = numbers[index];
        let expected_name = names[index];
        let entry_id = entry_ids[index];
        let identity = dispatcher
            .module_registry()
            .active_swi_identity(number)
            .expect("all module exports remain active");
        assert_eq!(identity.name, expected_name);
        assert_eq!(identity.definition_name, definitions[index]);
        assert_eq!(identity.module, module_id);
        assert_eq!(identity.generation_number, 2);
        assert!(identity.source_path.contains("SwapV2"));
        assert_eq!(
            dispatcher.module_registry().swi_entry_id(number),
            Some(entry_id)
        );
        let (guest_name, guest_owner, guest_definition, guest_generation) =
            query_guest_identity(&mut dispatcher, &mut task, number);
        assert_eq!(guest_name, expected_name);
        assert_eq!(guest_owner, "SwapService");
        assert_eq!(guest_definition, definitions[index]);
        assert_eq!(guest_generation, 2);
    }
    let mut module_lookup = SwiContext::default();
    module_lookup.registers[0] = 1;
    module_lookup.registers[1] = MODULE_TITLE_ADDRESS;
    dispatcher
        .dispatch(RICOCHET_MODULE_LOOKUP, &mut task, &mut module_lookup)
        .unwrap();
    assert!(!module_lookup.overflow);
    assert_eq!(module_lookup.registers[1], 1);
    assert_eq!(module_lookup.registers[3..=5], [1, 0, 0]);
    assert_eq!(module_lookup.registers[6], 4, "same title remains active");

    // The shared workspace carried COUNT%=1 across the swap. The candidate's
    // Start hook deliberately errors, proving immediate replacement does not
    // invoke candidate Start (nor reset the retained workspace).
    assert_eq!(
        call_swi(&mut dispatcher, &mut task, SWAP_OBSERVE, 0).registers[0],
        1001
    );
    assert_eq!(
        call_swi(&mut dispatcher, &mut task, SWAP_RUN, 0).registers[0],
        1002
    );
    assert_eq!(
        call_swi(&mut dispatcher, &mut task, SWAP_SECOND, 0).registers[0],
        2012
    );
    assert_eq!(
        call_swi(&mut dispatcher, &mut task, SWAP_DURING, 5).registers[0],
        3017
    );

    let active_source_path = dispatcher
        .module_registry()
        .active_swi_identity(SWAP_RUN)
        .unwrap()
        .source_path;
    let active_export_identities = numbers.map(|number| {
        dispatcher
            .module_registry()
            .active_swi_identity(number)
            .unwrap()
    });
    let active_module_id = dispatcher
        .module_registry()
        .module_named("SwapService")
        .unwrap()
        .id;
    let active_source_hash = dispatcher
        .module_registry()
        .module_named("SwapService")
        .unwrap()
        .manifest
        .source_hash
        .clone();
    for (path, fragment) in [
        ("SwapBadContract", "replacement"),
        ("SwapBadState", "replacement"),
        ("SwapBadNamedTypeLayout", "named type layouts"),
        ("SwapBadExportSignature", "signature"),
        ("SwapBadCapability", "capabilit"),
        ("SwapBadDependency", "replacement"),
        ("SwapBadLifecycle", "replacement"),
    ] {
        assert_replacement_rejected(
            &mut dispatcher,
            &mut task,
            path,
            SWAP_RUN,
            active_module_id,
            entry_ids[0],
            2,
            &active_source_path,
            &active_source_hash,
            fragment,
        );
        for (number, old_identity) in numbers.iter().copied().zip(&active_export_identities) {
            assert_eq!(
                dispatcher
                    .module_registry()
                    .active_swi_identity(number)
                    .as_ref(),
                Some(old_identity),
                "rejection leaves every export owned by the current generation"
            );
        }
        // An incompatible candidate cannot change the behavior or ownership
        // of any already active export.
        assert!(call_swi(&mut dispatcher, &mut task, SWAP_RUN, 0).registers[0] >= 1000);
        assert_eq!(
            dispatcher
                .module_registry()
                .active_swi_identity(SWAP_RUN)
                .unwrap()
                .source_path,
            active_source_path
        );
    }
    assert_eq!(
        dispatcher
            .module_registry()
            .module_named("SwapService")
            .unwrap()
            .instance_id,
        module_instance
    );

    // PRM-style case-insensitive title matching keeps the established display
    // spelling while accepting a candidate that uses a different case.
    let case_reload = load_guest_module(&mut dispatcher, &mut task, "SwapCase", false).unwrap();
    assert!(!case_reload.overflow);
    assert_eq!(case_reload.registers[0], 1, "replacement preserves R0");
    assert_eq!(
        case_reload.registers[1], LOAD_PATH_ADDRESS,
        "replacement preserves the R1 pathname pointer"
    );
    let normalized_module = dispatcher
        .module_registry()
        .module_named("SwapService")
        .unwrap();
    assert_eq!(normalized_module.manifest.name, "SwapService");
    assert!(normalized_module.manifest.source_path.contains("SwapCase"));
    for (number, entry_id) in numbers.iter().copied().zip(entry_ids) {
        let identity = dispatcher
            .module_registry()
            .active_swi_identity(number)
            .unwrap();
        assert_eq!(identity.module_name, "SwapService");
        assert_eq!(identity.generation_number, 3);
        assert!(identity.source_path.contains("SwapCase"));
        assert_eq!(
            dispatcher.module_registry().swi_entry_id(number),
            Some(entry_id)
        );
    }
    assert!(call_swi(&mut dispatcher, &mut task, SWAP_RUN, 0).registers[0] >= 1000);

    // A compatible module with an active FN symbol import must survive the
    // dependency check during replacement. The `FN:` kind prefix is part of
    // the canonical manifest symbol identity, not a decoration to strip.
    assert!(
        !load_guest_module(&mut dispatcher, &mut task, "MetricProvider", false)
            .unwrap()
            .overflow
    );
    assert!(
        !load_guest_module(&mut dispatcher, &mut task, "FnConsumerV1", false)
            .unwrap()
            .overflow
    );
    assert_eq!(
        call_swi(&mut dispatcher, &mut task, FN_CONSUMER_READ, 0).registers[0],
        41,
        "consumer resolves its active provider FN import"
    );
    let fn_consumer = dispatcher
        .module_registry()
        .module_named("FnConsumer")
        .expect("FN consumer module loaded");
    let fn_consumer_id = fn_consumer.id;
    let fn_consumer_instance_id = fn_consumer.instance_id;
    let fn_entry_id = dispatcher
        .module_registry()
        .swi_entry_id(FN_CONSUMER_READ)
        .unwrap();
    let fn_replacement =
        load_guest_module(&mut dispatcher, &mut task, "FnConsumerV2", false).unwrap();
    assert!(
        !fn_replacement.overflow,
        "compatible replacement with active @IMPORT_SYMBOL MetricProvider FN ReadCount must be accepted"
    );
    assert_eq!(fn_replacement.registers[0], 1, "replacement preserves R0");
    assert_eq!(
        fn_replacement.registers[1], LOAD_PATH_ADDRESS,
        "replacement preserves the R1 pathname pointer"
    );
    let replaced_fn_consumer = dispatcher
        .module_registry()
        .module_named("FnConsumer")
        .expect("replaced FN consumer remains active");
    assert_eq!(replaced_fn_consumer.id, fn_consumer_id);
    assert_eq!(replaced_fn_consumer.instance_id, fn_consumer_instance_id);
    assert!(
        replaced_fn_consumer
            .manifest
            .source_path
            .contains("FnConsumerV2")
    );
    let fn_identity = dispatcher
        .module_registry()
        .active_swi_identity(FN_CONSUMER_READ)
        .unwrap();
    assert_eq!(fn_identity.generation_number, 2);
    assert!(fn_identity.source_path.contains("FnConsumerV2"));
    assert_eq!(
        dispatcher.module_registry().swi_entry_id(FN_CONSUMER_READ),
        Some(fn_entry_id),
        "same-title replacement retains the exported entry cell"
    );
    assert_eq!(
        call_swi(&mut dispatcher, &mut task, FN_CONSUMER_READ, 0).registers[0],
        42,
        "subsequent calls execute the replacement while its imported FN still resolves"
    );

    // The foundation Console module is protected even from a same-title,
    // contract-compatible source that would visibly append an exclamation.
    let console_cell = dispatcher
        .module_registry()
        .swi_entry_id(OS_WRITE_C)
        .unwrap();
    let console_identity = dispatcher
        .module_registry()
        .active_swi_identity(OS_WRITE_C)
        .unwrap();
    let console_load =
        load_guest_module(&mut dispatcher, &mut task, "ConsoleCandidate", true).unwrap();
    assert_x_error(&task, &console_load, "foundation");
    assert_eq!(
        dispatcher.module_registry().swi_entry_id(OS_WRITE_C),
        Some(console_cell)
    );
    assert_eq!(
        dispatcher
            .module_registry()
            .active_swi_identity(OS_WRITE_C)
            .unwrap(),
        console_identity,
        "failed foundation replacement leaves source and generation unchanged"
    );
    let _ = display_receiver.try_iter().collect::<Vec<_>>();
    let _ = call_swi(&mut dispatcher, &mut task, OS_WRITE_C, u32::from(b'K'));
    let emitted = display_receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(emitted, [b'K']);

    drop(task);
    drop(dispatcher);
    fs::remove_dir_all(&root).unwrap();
}
