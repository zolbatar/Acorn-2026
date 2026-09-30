use std::{
    fs,
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use acorn_2026::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{
        ACORN_MODULE_LOOKUP, ACORN_SWI_INFO, DisplayEvent, OS_GENERATE_ERROR, SwiContext,
        SwiDispatcher,
    },
};

const X_BIT: u32 = 1 << 17;
const OS_MODULE: u32 = 0x1E;
const OS_SWI_NUMBER_TO_STRING: u32 = 0x38;
const OS_SWI_NUMBER_FROM_STRING: u32 = 0x39;
const OS_READ_MONOTONIC_TIME: u32 = 0x42;
const PROBE_SWI: u32 = 0x4FF30;
const ROLLBACK_SWI: u32 = 0x4FF31;
const NAMED_PROBE_SWI: u32 = 0x4FF32;
const PATH_ADDRESS: u32 = 0x2100;
const MODULE_NAME_ADDRESS: u32 = 0x2200;
const QUERY_BUFFER: u32 = 0x3000;

fn unique_scratch_directory() -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("acorn-trellis-wp51-{}-{nanos}", std::process::id()))
}

fn write_module(root: &Path, file_stem: &str, guest_name: &str, source: &str) {
    fs::write(root.join(format!("{file_stem}.bas64")), source).unwrap();
    fs::write(
        root.join(format!("{file_stem}.bas64.acornmeta")),
        format!(
            "Acorn-2026 file metadata v1\nformat-version=1\nguest-name={guest_name}\nfile-type=0x00000064\nload-address=0x00000000\nexecution-address=0x00000000\nattributes=0x00000000\n"
        ),
    )
    .unwrap();
}

fn put_string(task: &mut Task, address: u32, value: &[u8]) {
    task.memory.write_bytes(address, value).unwrap();
    if !value.ends_with(&[0]) {
        task.memory
            .write_byte(address + value.len() as u32, 0)
            .unwrap();
    }
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

fn assert_error_block(task: &Task, address: u32, expected_code: u32, message: &str) {
    assert_eq!(get_word(task, address), expected_code);
    let actual = String::from_utf8(task.memory.read_c_string(address + 4, 252).unwrap()).unwrap();
    assert!(
        actual
            .to_ascii_lowercase()
            .contains(&message.to_ascii_lowercase()),
        "error message {actual:?} did not contain {message:?}"
    );
}

#[test]
fn wp51_public_contracts_cover_modules_errors_identity_and_system_queries() {
    let root = unique_scratch_directory();
    fs::create_dir_all(&root).unwrap();
    let config_path = root.join("isolated-configure");
    // Each test process gets its own volume and configuration file; the values
    // are read when the dispatcher is constructed below.
    unsafe {
        std::env::set_var("ACORN_CONFIG_PATH", &config_path);
        std::env::set_var("ACORN_DEMO_VOLUME", &root);
    }

    let probe_source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE WP51Probe 1.2.3\nREM @LIFECYCLE START Start\nREM @PRIVATE PROC Start\nREM @SWI WP51_Probe &4FF30 Probe REGISTERS=R0:U32:INOUT\nREM @SWI WP51_NamedProbe &4FF32 NamedProbe REGISTERS=R0:U32:OUT\nDEF PROC Start\nENDPROC\nDEF PROC Probe\n    R0% = R0% + 1\nENDPROC\nDEF PROC NamedProbe\n    SYS \"XNoSuchNamedSwi\", TO ERRORADDRESS% ; FLAGS%\n    R0% = FLAGS%\nENDPROC\n";
    let rollback_source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE WP51Rollback 1.0.0\nREM @LIFECYCLE START Start\nREM @PRIVATE PROC Start\nREM @SWI WP51_Rollback &4FF31 Entry\nDEF PROC Start\n    SYS \"NoSuchStartupSwi\"\nENDPROC\nDEF PROC Entry\nENDPROC\n";
    write_module(&root, "WP51Probe", "WP51Probe", probe_source);
    write_module(&root, "WP51Rollback", "WP51Rollback", rollback_source);

    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, _display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut dispatcher =
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    assert!(
        dispatcher
            .module_registry()
            .swi_number("OS_Module")
            .is_some()
    );
    let mut task = Task::trusted_mos_session(0x51_0001);

    // Unknown numeric SWIs return the documented hosted generic code in both
    // ordinary error form and X error-returning form.
    let mut unknown = SwiContext::default();
    let error = dispatcher
        .dispatch(0x7FFE, &mut task, &mut unknown)
        .unwrap_err();
    assert!(matches!(
        error,
        RuntimeError::Structured { type_name, code: 1, .. } if type_name == "UnknownSwi"
    ));
    let mut unknown_x = SwiContext::default();
    unknown_x.registers[0] = 0xA1A2_A3A4;
    dispatcher
        .dispatch(0x7FFE | X_BIT, &mut task, &mut unknown_x)
        .unwrap();
    assert!(unknown_x.overflow);
    let unknown_error = unknown_x.registers[0];
    assert_eq!(unknown_error, task.memory.swi_error_block_address());
    assert_error_block(&task, unknown_error, 1, "no such swi");
    assert_eq!(
        Task::new(0x51_0002)
            .memory
            .read_byte(unknown_error)
            .unwrap(),
        0
    );

    // Unsupported OS_Module reasons, including pointer-bearing PRM reasons
    // 12 and 18, are rejected from R0 before R1 is interpreted as an address.
    for reason in (0..=20)
        .filter(|reason| !matches!(reason, 1 | 4))
        .chain([21, u32::MAX])
    {
        let mut call = SwiContext::default();
        call.registers[0] = reason;
        call.registers[1] = u32::MAX;
        let error = dispatcher
            .dispatch(OS_MODULE, &mut task, &mut call)
            .unwrap_err();
        assert!(
            matches!(
                error,
                RuntimeError::Structured { ref type_name, code, .. }
                    if type_name == "UnsupportedServiceReason" && code == reason
            ),
            "OS_Module reason {reason} returned {error:?}"
        );
    }
    let mut reason_18_x = SwiContext::default();
    reason_18_x.registers[0] = 18;
    reason_18_x.registers[1] = u32::MAX;
    dispatcher
        .dispatch(OS_MODULE | X_BIT, &mut task, &mut reason_18_x)
        .unwrap();
    assert!(reason_18_x.overflow);
    assert_error_block(&task, reason_18_x.registers[0], 18, "&1E");

    // Load and Delete accept the RISC OS reason/register positions and preserve
    // R0/R1 on success. The caller path is checked against task-local memory.
    put_string(&mut task, PATH_ADDRESS, b"WP51Probe");
    let mut load = SwiContext::default();
    load.registers[0] = 1;
    load.registers[1] = PATH_ADDRESS;
    load.registers[2] = 0xA2A3_A4A5;
    load.registers[3] = 0xB2B3_B4B5;
    dispatcher
        .dispatch(OS_MODULE, &mut task, &mut load)
        .unwrap();
    assert_eq!(load.registers[0], 1);
    assert_eq!(load.registers[1], PATH_ADDRESS);
    assert_eq!(load.registers[2], 0xA2A3_A4A5);
    assert_eq!(load.registers[3], 0xB2B3_B4B5);

    // A BASIC64 call by unknown SWI name follows the named X route and makes
    // the returned V flag visible to SYS's flag output.
    let mut named_unknown_x = SwiContext::default();
    dispatcher
        .dispatch(NAMED_PROBE_SWI, &mut task, &mut named_unknown_x)
        .unwrap();
    assert_eq!(named_unknown_x.registers[0], 1);

    // Safe module identity is active-manifest data and uses caller buffers.
    put_string(&mut task, MODULE_NAME_ADDRESS, b"wp51probe");
    let mut module_lookup = SwiContext::default();
    module_lookup.registers[0] = 1;
    module_lookup.registers[1] = MODULE_NAME_ADDRESS;
    dispatcher
        .dispatch(ACORN_MODULE_LOOKUP | X_BIT, &mut task, &mut module_lookup)
        .unwrap();
    assert!(!module_lookup.overflow);
    assert_eq!(module_lookup.registers[1], 1);
    assert!(module_lookup.registers[2] > 0);
    assert_eq!(module_lookup.registers[3..=5], [1, 2, 3]);
    assert_eq!(module_lookup.registers[6], 4);

    let name_buffer = QUERY_BUFFER;
    let owner_buffer = QUERY_BUFFER + 0x100;
    let definition_buffer = QUERY_BUFFER + 0x200;
    let mut swi_info = SwiContext::default();
    swi_info.registers[0] = 1;
    swi_info.registers[1] = PROBE_SWI;
    swi_info.registers[2] = name_buffer;
    swi_info.registers[3] = 128;
    swi_info.registers[4] = owner_buffer;
    swi_info.registers[5] = 128;
    swi_info.registers[6] = definition_buffer;
    swi_info.registers[7] = 128;
    dispatcher
        .dispatch(ACORN_SWI_INFO | X_BIT, &mut task, &mut swi_info)
        .unwrap();
    assert!(!swi_info.overflow);
    assert_eq!(
        task.memory.read_c_string(name_buffer, 128).unwrap(),
        b"WP51_Probe"
    );
    assert_eq!(
        task.memory.read_c_string(owner_buffer, 128).unwrap(),
        b"WP51Probe"
    );
    assert_eq!(
        task.memory.read_c_string(definition_buffer, 128).unwrap(),
        b"PROBE"
    );
    assert_eq!(swi_info.registers[8], 1);

    // OS_SWINumberToString requires room for the NUL terminator, preserves
    // R0/R1 and returns the visible byte length in R2. Bit 17 adds an X prefix.
    task.memory
        .write_bytes(QUERY_BUFFER + 0x300, &[0xA5; 16])
        .unwrap();
    let mut short_name = SwiContext::default();
    short_name.registers[0] = PROBE_SWI;
    short_name.registers[1] = QUERY_BUFFER + 0x300;
    short_name.registers[2] = 10;
    dispatcher
        .dispatch(OS_SWI_NUMBER_TO_STRING | X_BIT, &mut task, &mut short_name)
        .unwrap();
    assert!(short_name.overflow);
    assert_eq!(
        task.memory.read_bytes(QUERY_BUFFER + 0x300, 16).unwrap(),
        vec![0xA5; 16],
        "a too-small output buffer is not partially written"
    );

    let mut name = SwiContext::default();
    name.registers[0] = PROBE_SWI;
    name.registers[1] = QUERY_BUFFER + 0x300;
    name.registers[2] = 11;
    name.registers[3] = 0xD2D3_D4D5;
    dispatcher
        .dispatch(OS_SWI_NUMBER_TO_STRING | X_BIT, &mut task, &mut name)
        .unwrap();
    assert!(!name.overflow);
    assert_eq!(name.registers[0], PROBE_SWI);
    assert_eq!(name.registers[1], QUERY_BUFFER + 0x300);
    assert_eq!(name.registers[2], 10);
    assert_eq!(name.registers[3], 0xD2D3_D4D5);
    assert_eq!(
        task.memory.read_c_string(QUERY_BUFFER + 0x300, 11).unwrap(),
        b"WP51_Probe"
    );

    let mut x_name = SwiContext::default();
    x_name.registers[0] = PROBE_SWI | X_BIT;
    x_name.registers[1] = QUERY_BUFFER + 0x320;
    x_name.registers[2] = 12;
    dispatcher
        .dispatch(OS_SWI_NUMBER_TO_STRING | X_BIT, &mut task, &mut x_name)
        .unwrap();
    assert!(!x_name.overflow);
    assert_eq!(x_name.registers[0], PROBE_SWI | X_BIT);
    assert_eq!(x_name.registers[1], QUERY_BUFFER + 0x320);
    assert_eq!(x_name.registers[2], 11);
    assert_eq!(
        task.memory.read_c_string(QUERY_BUFFER + 0x320, 12).unwrap(),
        b"XWP51_Probe"
    );

    // OS_SWINumberFromString is case-sensitive, accepts a control/space
    // terminator, preserves R1, and maps a leading X to bit 17.
    let input_name = QUERY_BUFFER + 0x400;
    put_string(&mut task, input_name, b"XWP51_Probe\r");
    let mut from_name = SwiContext::default();
    from_name.registers[0] = 0xCAFE_BABE;
    from_name.registers[1] = input_name;
    from_name.registers[2] = 0xC2C3_C4C5;
    dispatcher
        .dispatch(OS_SWI_NUMBER_FROM_STRING | X_BIT, &mut task, &mut from_name)
        .unwrap();
    assert!(!from_name.overflow);
    assert_eq!(from_name.registers[0], PROBE_SWI | X_BIT);
    assert_eq!(from_name.registers[1], input_name);
    assert_eq!(from_name.registers[2], 0xC2C3_C4C5);

    put_string(&mut task, input_name, b"wp51_Probe");
    let mut wrong_case = SwiContext::default();
    wrong_case.registers[1] = input_name;
    dispatcher
        .dispatch(
            OS_SWI_NUMBER_FROM_STRING | X_BIT,
            &mut task,
            &mut wrong_case,
        )
        .unwrap();
    assert!(wrong_case.overflow);
    assert_eq!(
        wrong_case.registers[0],
        task.memory.swi_error_block_address()
    );
    let wrong_case_message = String::from_utf8(
        task.memory
            .read_c_string(wrong_case.registers[0] + 4, 252)
            .unwrap(),
    )
    .unwrap();
    assert!(wrong_case_message.contains("wp51_Probe"));

    // ReadMonotonicTime takes no inputs and returns centiseconds in R0.
    let mut first_time = SwiContext::default();
    first_time.registers[1] = 0xD1D2_D3D4;
    dispatcher
        .dispatch(OS_READ_MONOTONIC_TIME | X_BIT, &mut task, &mut first_time)
        .unwrap();
    assert!(!first_time.overflow);
    assert_eq!(first_time.registers[1], 0xD1D2_D3D4);
    thread::sleep(Duration::from_millis(25));
    let mut second_time = SwiContext::default();
    dispatcher
        .dispatch(OS_READ_MONOTONIC_TIME, &mut task, &mut second_time)
        .unwrap();
    let elapsed_centiseconds = second_time.registers[0].wrapping_sub(first_time.registers[0]);
    assert!(elapsed_centiseconds > 0);

    // Standard error blocks are checked in caller memory. The X form of
    // OS_GenerateError keeps the supplied block address in R0 and sets V.
    let error_block = QUERY_BUFFER + 0x500;
    let mut block = 0x1234_5678_u32.to_le_bytes().to_vec();
    block.extend_from_slice(b"WP5.1 probe error\0");
    task.memory.write_bytes(error_block, &block).unwrap();
    let mut generate_x = SwiContext::default();
    generate_x.registers[0] = error_block;
    generate_x.registers[1] = 0xE1E2_E3E4;
    dispatcher
        .dispatch(OS_GENERATE_ERROR | X_BIT, &mut task, &mut generate_x)
        .unwrap();
    assert!(generate_x.overflow);
    assert_eq!(generate_x.registers[0], error_block);
    assert_eq!(generate_x.registers[1], 0xE1E2_E3E4);

    let mut generate_normal = SwiContext::default();
    generate_normal.registers[0] = error_block;
    let error = dispatcher
        .dispatch(OS_GENERATE_ERROR, &mut task, &mut generate_normal)
        .unwrap_err();
    assert!(matches!(
        error,
        RuntimeError::Structured { type_name, code: 0x1234_5678, message }
            if type_name == "OSError" && message == "WP5.1 probe error"
    ));
    assert_eq!(generate_normal.registers[0], error_block);

    let mut invalid_error_block = SwiContext::default();
    invalid_error_block.registers[0] = u32::MAX;
    dispatcher
        .dispatch(
            OS_GENERATE_ERROR | X_BIT,
            &mut task,
            &mut invalid_error_block,
        )
        .unwrap();
    assert!(invalid_error_block.overflow);
    assert_eq!(
        invalid_error_block.registers[0],
        task.memory.swi_error_block_address()
    );
    assert_error_block(
        &task,
        invalid_error_block.registers[0],
        5,
        "outside this task",
    );

    let mut invalid_load = SwiContext::default();
    invalid_load.registers[0] = 1;
    invalid_load.registers[1] = u32::MAX;
    dispatcher
        .dispatch(OS_MODULE | X_BIT, &mut task, &mut invalid_load)
        .unwrap();
    assert!(invalid_load.overflow);
    assert_error_block(&task, invalid_load.registers[0], 5, "outside this task");

    let mut invalid_delete = SwiContext::default();
    invalid_delete.registers[0] = 4;
    invalid_delete.registers[1] = u32::MAX;
    dispatcher
        .dispatch(OS_MODULE | X_BIT, &mut task, &mut invalid_delete)
        .unwrap();
    assert!(invalid_delete.overflow);
    assert_error_block(&task, invalid_delete.registers[0], 5, "outside this task");

    let mut invalid_lookup = SwiContext::default();
    invalid_lookup.registers[0] = 1;
    invalid_lookup.registers[1] = u32::MAX;
    dispatcher
        .dispatch(ACORN_MODULE_LOOKUP | X_BIT, &mut task, &mut invalid_lookup)
        .unwrap();
    assert!(invalid_lookup.overflow);
    assert_error_block(&task, invalid_lookup.registers[0], 5, "outside this task");

    let mut invalid_identity_output = SwiContext::default();
    invalid_identity_output.registers[0] = 1;
    invalid_identity_output.registers[1] = PROBE_SWI;
    invalid_identity_output.registers[2] = u32::MAX;
    invalid_identity_output.registers[3] = 128;
    invalid_identity_output.registers[4] = owner_buffer;
    invalid_identity_output.registers[5] = 128;
    invalid_identity_output.registers[6] = definition_buffer;
    invalid_identity_output.registers[7] = 128;
    dispatcher
        .dispatch(
            ACORN_SWI_INFO | X_BIT,
            &mut task,
            &mut invalid_identity_output,
        )
        .unwrap();
    assert!(invalid_identity_output.overflow);
    assert_error_block(
        &task,
        invalid_identity_output.registers[0],
        5,
        "outside this task",
    );

    // A failing Start must leave neither an active manifest identity nor an
    // exported SWI. The error block is returned through the X contract.
    put_string(&mut task, PATH_ADDRESS, b"WP51Rollback");
    let mut failed_load = SwiContext::default();
    failed_load.registers[0] = 1;
    failed_load.registers[1] = PATH_ADDRESS;
    dispatcher
        .dispatch(OS_MODULE | X_BIT, &mut task, &mut failed_load)
        .unwrap();
    assert!(failed_load.overflow);
    let start_error = failed_load.registers[0];
    assert_eq!(get_word(&task, start_error), 2);
    assert!(
        String::from_utf8(task.memory.read_c_string(start_error + 4, 252).unwrap())
            .unwrap()
            .to_ascii_lowercase()
            .contains("nosuchstartupswi")
    );

    put_string(&mut task, MODULE_NAME_ADDRESS, b"WP51Rollback");
    let mut failed_lookup = SwiContext::default();
    failed_lookup.registers[0] = 1;
    failed_lookup.registers[1] = MODULE_NAME_ADDRESS;
    dispatcher
        .dispatch(ACORN_MODULE_LOOKUP, &mut task, &mut failed_lookup)
        .unwrap();
    assert_eq!(failed_lookup.registers[1..=6], [0; 6]);

    let mut rolled_back_identity = SwiContext::default();
    rolled_back_identity.registers[0] = 1;
    rolled_back_identity.registers[1] = ROLLBACK_SWI;
    rolled_back_identity.registers[2] = name_buffer;
    rolled_back_identity.registers[3] = 128;
    rolled_back_identity.registers[4] = owner_buffer;
    rolled_back_identity.registers[5] = 128;
    rolled_back_identity.registers[6] = definition_buffer;
    rolled_back_identity.registers[7] = 128;
    dispatcher
        .dispatch(ACORN_SWI_INFO | X_BIT, &mut task, &mut rolled_back_identity)
        .unwrap();
    assert!(rolled_back_identity.overflow);
    assert_error_block(
        &task,
        rolled_back_identity.registers[0],
        ROLLBACK_SWI,
        "active",
    );

    let mut rolled_back_call = SwiContext::default();
    dispatcher
        .dispatch(ROLLBACK_SWI | X_BIT, &mut task, &mut rolled_back_call)
        .unwrap();
    assert!(rolled_back_call.overflow);
    assert_error_block(&task, rolled_back_call.registers[0], 1, "no such swi");

    // Delete preserves its documented registers. Removal drops the active
    // identity and makes the same SWI number unknown again.
    put_string(&mut task, MODULE_NAME_ADDRESS, b"WP51Probe");
    let mut delete = SwiContext::default();
    delete.registers[0] = 4;
    delete.registers[1] = MODULE_NAME_ADDRESS;
    delete.registers[2] = 0xF2F3_F4F5;
    dispatcher
        .dispatch(OS_MODULE, &mut task, &mut delete)
        .unwrap();
    assert_eq!(delete.registers[0], 4);
    assert_eq!(delete.registers[1], MODULE_NAME_ADDRESS);
    assert_eq!(delete.registers[2], 0xF2F3_F4F5);

    let mut deleted_lookup = SwiContext::default();
    deleted_lookup.registers[0] = 1;
    deleted_lookup.registers[1] = MODULE_NAME_ADDRESS;
    dispatcher
        .dispatch(ACORN_MODULE_LOOKUP, &mut task, &mut deleted_lookup)
        .unwrap();
    assert_eq!(deleted_lookup.registers[1..=6], [0; 6]);

    let mut deleted_call = SwiContext::default();
    dispatcher
        .dispatch(PROBE_SWI | X_BIT, &mut task, &mut deleted_call)
        .unwrap();
    assert!(deleted_call.overflow);
    assert_error_block(&task, deleted_call.registers[0], 1, "no such swi");

    drop(task);
    drop(dispatcher);
    fs::remove_dir_all(&root).unwrap();
}
