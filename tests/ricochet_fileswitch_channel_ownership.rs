use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    basic_compat,
    error::RuntimeError,
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{
        DisplayEvent, OS_ARGS, OS_BGET, OS_BPUT, OS_FIND, OS_GBPB, SwiContext, SwiDispatchRoute,
        SwiDispatcher,
    },
};

const STRING_ADDRESS: u32 = 0x2400;
const BUFFER_ADDRESS: u32 = 0x4000;
const RESULT_ADDRESS: u32 = 0x5000;
const X_BIT: u32 = 1 << 17;

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
            .expect("system time is valid")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ricochet-fileswitch-ownership-{}-{nonce}",
            std::process::id()
        ));
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

    fn guest_path(&self, name: &str) -> String {
        format!("HostFS::DemoDisk.$.{name}")
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.old_volume {
                std::env::set_var("RICOCHET_DEMO_VOLUME", value);
            } else {
                std::env::remove_var("RICOCHET_DEMO_VOLUME");
            }
            if let Some(value) = &self.old_config {
                std::env::set_var("RICOCHET_CONFIG_PATH", value);
            } else {
                std::env::remove_var("RICOCHET_CONFIG_PATH");
            }
            if let Some(value) = &self.old_capsule {
                std::env::set_var("RICOCHET_BOOT_CAPSULE", value);
            } else {
                std::env::remove_var("RICOCHET_BOOT_CAPSULE");
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender),
        display_receiver,
    )
}

fn dispatch(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    number: u32,
    registers: &[u32],
) -> (Result<(), RuntimeError>, SwiContext) {
    let mut context = SwiContext::default();
    context.registers[..registers.len()].copy_from_slice(registers);
    let result = dispatcher.dispatch(number, task, &mut context);
    (result, context)
}

fn assert_owner(dispatcher: &SwiDispatcher, number: u32, definition: &str) {
    let name = match number {
        OS_ARGS => "OS_Args",
        OS_BGET => "OS_BGet",
        OS_BPUT => "OS_BPut",
        OS_GBPB => "OS_GBPB",
        OS_FIND => "OS_Find",
        _ => panic!("unexpected FileSwitch SWI &{number:X}"),
    };
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned {
                number: routed_number,
                name: routed_name,
                module,
                definition: routed_definition,
                ..
            }) if *routed_number == number
                && routed_name.eq_ignore_ascii_case(name)
                && module.eq_ignore_ascii_case("FileSwitch")
                && routed_definition.eq_ignore_ascii_case(definition)
        ),
        "SWI &{number:X} did not use FileSwitch::{definition}: {:?}",
        dispatcher.last_dispatch_route()
    );
}

fn write_guest_string(task: &mut Task, address: u32, value: &str) {
    task.memory.write_bytes(address, value.as_bytes()).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn invoke_find(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    reason: u32,
    path: &str,
) -> (Result<(), RuntimeError>, SwiContext) {
    write_guest_string(task, STRING_ADDRESS, path);
    dispatch(dispatcher, task, OS_FIND, &[reason, STRING_ADDRESS, 0])
}

fn get_file_pointer(dispatcher: &mut SwiDispatcher, task: &mut Task, handle: u32) -> u32 {
    let (result, context) = dispatch(dispatcher, task, OS_ARGS, &[0, handle, 0]);
    result.expect("OS_Args 0 reads the task-local sequential pointer");
    assert_owner(dispatcher, OS_ARGS, "ArgsService");
    context.registers[2]
}

#[test]
fn fileswitch_services_are_module_owned_checked_and_task_scoped() {
    let environment = Environment::new();
    fs::write(environment.root.join("Alpha"), b"ABC").unwrap();
    fs::write(environment.root.join("Beta"), b"xyz").unwrap();
    fs::write(environment.root.join("CallFile"), b"Q").unwrap();

    let (mut dispatcher, _display) = dispatcher();
    let mut caller = Task::new(0x0D01);
    let mut other = Task::new(0x0D02);
    let active_modules = dispatcher
        .module_registry()
        .active_modules_sorted()
        .into_iter()
        .map(|module| module.manifest.name.as_str())
        .collect::<Vec<_>>();
    assert!(
        active_modules
            .iter()
            .any(|module| module.eq_ignore_ascii_case("FileSwitch")),
        "FileSwitch is not published in the normal runtime: {active_modules:?}"
    );

    // An ordinary Task uses the same public FileSwitch entrypoints, without
    // borrowing host authority from the interactive MOS task.
    let (open, open_context) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0x40,
        &environment.guest_path("Alpha"),
    );
    open.expect("OS_Find &40 opens an existing guest file for reading");
    assert_owner(&dispatcher, OS_FIND, "FindService");
    let alpha = open_context.registers[0];
    assert!((0x80..=0xFF).contains(&alpha));
    assert_eq!(open_context.registers[1], STRING_ADDRESS);
    assert_eq!(open_context.registers[2], 0);

    let (current_fs, current_fs_context) =
        dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[0, 0, 0]);
    current_fs.expect("OS_Args 0 with a null handle reports the current filing system");
    assert_eq!(current_fs_context.registers[0], 1);
    assert_owner(&dispatcher, OS_ARGS, "ArgsService");

    let (first_byte, first_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_BGET,
        &[0xAAAA_5555, alpha, 0xFACE_B00C],
    );
    first_byte.expect("OS_BGet reads the first byte from an open channel");
    assert_eq!(first_context.registers[0], u32::from(b'A'));
    assert_eq!(first_context.registers[1], alpha, "R1 handle is preserved");
    assert_owner(&dispatcher, OS_BGET, "ByteGetService");
    dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[1, alpha, 0])
        .0
        .expect("rewind before checking the X-form success path");
    let (x_success, x_success_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_BGET | X_BIT,
        &[0, alpha, 0],
    );
    x_success.expect("X-form successful FileSwitch call returns normally");
    assert!(!x_success_context.overflow);
    assert_eq!(x_success_context.registers[0], u32::from(b'A'));
    assert_owner(&dispatcher, OS_BGET, "ByteGetService");
    assert_eq!(get_file_pointer(&mut dispatcher, &mut caller, alpha), 1);

    // The BASIC64-owned OS_GBPB route shares the same task channel/cursor with
    // the byte SWIs.
    let (group_read, group_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_GBPB,
        &[4, alpha, BUFFER_ADDRESS, 2, 0, 0],
    );
    group_read.expect("OS_GBPB reason 4 continues from the OS_BGet cursor");
    assert_owner(&dispatcher, OS_GBPB, "BulkTransferService");
    assert_eq!(caller.memory.read_bytes(BUFFER_ADDRESS, 2).unwrap(), b"BC");
    assert_eq!(group_context.registers[4], 3);
    assert_eq!(get_file_pointer(&mut dispatcher, &mut caller, alpha), 3);

    let (first_eof, eof_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_BGET,
        &[0x1234_5678, alpha, 0],
    );
    first_eof.expect("first read at EOF reports carry instead of an error");
    assert!(eof_context.carry);
    assert_eq!(eof_context.registers[1], alpha);
    assert_owner(&dispatcher, OS_BGET, "ByteGetService");
    let (eof_status, eof_status_context) =
        dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[5, alpha, 0]);
    eof_status.expect("OS_Args 5 reports the file's EOF state");
    assert_eq!(eof_status_context.registers[0], 5);
    assert_eq!(eof_status_context.registers[1], alpha);
    assert_ne!(eof_status_context.registers[2], 0);
    let (second_eof, _) = dispatch(&mut dispatcher, &mut caller, OS_BGET, &[0, alpha, 0]);
    assert!(
        second_eof.is_err(),
        "second EOF read must raise the delayed error"
    );
    assert_owner(&dispatcher, OS_BGET, "ByteGetService");

    // Seeking clears the delayed EOF state and remains a U32 position.
    let (seek, seek_context) = dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[1, alpha, 1]);
    seek.expect("OS_Args 1 seeks within an open file");
    assert_eq!(&seek_context.registers[..3], &[1, alpha, 1]);
    assert_eq!(get_file_pointer(&mut dispatcher, &mut caller, alpha), 1);
    let (last_byte, last_context) = dispatch(&mut dispatcher, &mut caller, OS_BGET, &[0, alpha, 0]);
    last_byte.expect("seek clears the pending EOF error");
    assert_eq!(last_context.registers[0], u32::from(b'B'));

    // A numeric handle from another Task does not authorize access to the
    // first Task's channel. Handles may have the same integer in each Task,
    // but each resolves to that caller's own file and cursor.
    let (cross_task, _) = dispatch(&mut dispatcher, &mut other, OS_BGET, &[0, alpha, 0]);
    assert!(
        cross_task.is_err(),
        "foreign Task handle must not be usable"
    );
    let (beta_open, beta_context) = invoke_find(
        &mut dispatcher,
        &mut other,
        0x40,
        &environment.guest_path("Beta"),
    );
    beta_open.expect("second Task opens its own guest channel");
    assert_owner(&dispatcher, OS_FIND, "FindService");
    let beta = beta_context.registers[0];
    assert_eq!(
        beta, alpha,
        "test expects independent handles to overlap numerically"
    );
    let (beta_byte, beta_byte_context) =
        dispatch(&mut dispatcher, &mut other, OS_BGET, &[0, beta, 0]);
    beta_byte.expect("same numeric handle resolves in the second Task's table");
    assert_eq!(beta_byte_context.registers[0], u32::from(b'x'));

    // Find mode &80 creates/truncates with read/write access; &C0 opens an
    // existing file read/write. BPut preserves its register values while
    // storing only the low byte, and a subsequent BGet can read it back.
    let (create, create_context) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0x80,
        &environment.guest_path("Output"),
    );
    create.expect("OS_Find &80 creates a new readable/writable guest file");
    assert_owner(&dispatcher, OS_FIND, "FindService");
    let output = create_context.registers[0];
    let (put_a, put_a_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_BPUT,
        &[0xABCD_0041, output, 0xFACE_1234],
    );
    put_a.expect("OS_BPut writes a byte to the writable channel");
    assert_eq!(
        &put_a_context.registers[..3],
        &[0xABCD_0041, output, 0xFACE_1234]
    );
    assert_owner(&dispatcher, OS_BPUT, "BytePutService");
    dispatch(&mut dispatcher, &mut caller, OS_BPUT, &[0x100, output, 0])
        .0
        .expect("second byte write stores the low byte");
    assert_eq!(get_file_pointer(&mut dispatcher, &mut caller, output), 2);
    let (output_seek, _) = dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[1, output, 0]);
    output_seek.expect("seek output channel to the beginning");
    let (output_byte, output_byte_context) =
        dispatch(&mut dispatcher, &mut caller, OS_BGET, &[0, output, 0]);
    output_byte.expect("create mode is readable as well as writable");
    assert_eq!(output_byte_context.registers[0], u32::from(b'A'));

    let (extent, extent_context) = dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[2, output, 0]);
    extent.expect("OS_Args 2 reads file extent");
    assert_eq!(extent_context.registers[2], 2);
    let (allocated, allocated_context) =
        dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[4, output, 0]);
    allocated.expect("hosted Args 4 reports extent as allocated size");
    assert_eq!(allocated_context.registers[2], 2);
    let (resize, resize_context) = dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[3, output, 4]);
    resize.expect("OS_Args 3 extends with zero-filled bytes");
    assert_eq!(&resize_context.registers[..3], &[3, output, 4]);
    assert_eq!(get_file_pointer(&mut dispatcher, &mut caller, output), 1);
    assert_eq!(
        caller
            .file_system
            .open_files
            .get(&output)
            .unwrap()
            .file
            .metadata()
            .unwrap()
            .len(),
        4
    );

    let (readonly_put, _) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_BPUT,
        &[b'Q' as u32, alpha, 0],
    );
    assert!(
        readonly_put.is_err(),
        "read-only OS_Find channel cannot be written"
    );

    let (reopen_truncate, reopen_context) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0x80,
        &environment.guest_path("Output"),
    );
    reopen_truncate.expect("OS_Find &80 truncates an existing file");
    let truncated = reopen_context.registers[0];
    let (empty_extent, empty_extent_context) =
        dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[2, truncated, 0]);
    empty_extent.expect("truncated file stays open");
    assert_eq!(empty_extent_context.registers[2], 0);
    dispatch(&mut dispatcher, &mut caller, OS_FIND, &[0, truncated, 0])
        .0
        .expect("close truncated handle");
    assert_eq!(fs::read(environment.root.join("Output")).unwrap(), b"");

    let (open_existing_rw, rw_context) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0xC0,
        &environment.guest_path("Alpha"),
    );
    open_existing_rw.expect("OS_Find &C0 opens an existing file read/write");
    let rw = rw_context.registers[0];
    dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[1, rw, 1])
        .0
        .expect("seek existing read/write channel");
    dispatch(&mut dispatcher, &mut caller, OS_BPUT, &[b'Z' as u32, rw, 0])
        .0
        .expect("read/write channel accepts BPut");
    dispatch(&mut dispatcher, &mut caller, OS_FIND, &[0, rw, 0])
        .0
        .expect("close read/write handle");
    assert_eq!(fs::read(environment.root.join("Alpha")).unwrap(), b"AZC");

    // Args supports the documented host subset, U32 position/extent values,
    // and exact two-pass canonical-name buffer negotiation.
    let (missing_rw, missing_rw_context) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0xC0,
        &environment.guest_path("DoesNotExist"),
    );
    missing_rw.expect("missing existing-file open returns handle zero without bit 3");
    assert_eq!(missing_rw_context.registers[0], 0);
    let (missing_error, _) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0xC8,
        &environment.guest_path("DoesNotExist"),
    );
    assert!(
        missing_error.is_err(),
        "bit 3 requests an error for missing files"
    );
    for reason in [6, 8, 254, 255] {
        let (unsupported_args, _) =
            dispatch(&mut dispatcher, &mut caller, OS_ARGS, &[reason, alpha, 0]);
        assert!(
            unsupported_args.is_err(),
            "unsupported OS_Args reason {reason} must fail"
        );
        assert_owner(&dispatcher, OS_ARGS, "ArgsService");
    }
    let (reserved_flags, _) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0x50,
        &environment.guest_path("Alpha"),
    );
    assert!(
        reserved_flags.is_err(),
        "unsupported Find selector bits are rejected"
    );
    let (inert_bit_two, inert_bit_two_context) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0x44,
        &environment.guest_path("Alpha"),
    );
    inert_bit_two.expect("OS_Find bit 2 is inert for a regular file");
    assert!((0x80..=0xFF).contains(&inert_bit_two_context.registers[0]));
    assert_eq!(inert_bit_two_context.registers[1], STRING_ADDRESS);
    assert_eq!(inert_bit_two_context.registers[2], 0);
    dispatch(
        &mut dispatcher,
        &mut caller,
        OS_FIND,
        &[0, inert_bit_two_context.registers[0], 0],
    )
    .0
    .expect("close regular file opened with inert bit 2");
    let (directory, _) = invoke_find(&mut dispatcher, &mut caller, 0x40, "HostFS::DemoDisk.$");
    assert!(
        directory.is_err(),
        "the bounded channel subset does not open directories"
    );
    let (traversal, _) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0x40,
        "HostFS::DemoDisk.$.folder/../outside",
    );
    let traversal = traversal.expect_err("guest paths cannot traverse into host paths");
    assert!(
        !traversal
            .to_string()
            .contains(environment.root.to_string_lossy().as_ref()),
        "FileSwitch errors must not expose host volume paths: {traversal}"
    );

    let alpha_canonical = "HostFS::DemoDisk.$.Alpha";
    let canonical_len = alpha_canonical.len();
    let (canonical_handle_result, canonical_context) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0x40,
        &environment.guest_path("Alpha"),
    );
    canonical_handle_result.expect("reopen Alpha to query its canonical name");
    let canonical_handle = canonical_context.registers[0];
    let capacity = (canonical_len + 4) as u32;
    caller
        .memory
        .write_bytes(BUFFER_ADDRESS, &vec![0xA5; capacity as usize])
        .unwrap();
    let (canonical, canonical_result) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[7, canonical_handle, BUFFER_ADDRESS, 0, 0, capacity],
    );
    canonical.expect("OS_Args 7 returns canonical guest name");
    assert_eq!(canonical_result.registers[0], 7);
    assert_eq!(canonical_result.registers[1], canonical_handle);
    assert_eq!(canonical_result.registers[5], 4);
    let mut expected_name = alpha_canonical.as_bytes().to_vec();
    expected_name.push(0);
    assert_eq!(
        caller
            .memory
            .read_bytes(BUFFER_ADDRESS, expected_name.len())
            .unwrap(),
        expected_name
    );

    let too_small = (canonical_len - 2) as u32;
    caller
        .memory
        .write_bytes(BUFFER_ADDRESS, &vec![0x5A; too_small as usize])
        .unwrap();
    let position_before_negotiation =
        get_file_pointer(&mut dispatcher, &mut caller, canonical_handle);
    let (negotiation, negotiation_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[7, canonical_handle, BUFFER_ADDRESS, 0, 0, too_small],
    );
    negotiation.expect("undersized canonical-name buffer uses PRM deficit negotiation");
    assert_eq!(
        negotiation_context.registers[5],
        too_small.wrapping_sub(canonical_len as u32)
    );
    assert_eq!(
        caller
            .memory
            .read_bytes(BUFFER_ADDRESS, too_small as usize)
            .unwrap(),
        vec![0x5A; too_small as usize],
        "too-small buffer is left untouched"
    );
    assert_eq!(
        get_file_pointer(&mut dispatcher, &mut caller, canonical_handle),
        position_before_negotiation,
        "buffer negotiation does not move the file cursor"
    );

    caller
        .memory
        .write_bytes(BUFFER_ADDRESS, &vec![0x6B; expected_name.len()])
        .unwrap();
    let (large_capacity, large_capacity_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[7, canonical_handle, BUFFER_ADDRESS, 0, 0, u32::MAX],
    );
    large_capacity.expect("R5 capacity may exceed the actual canonical output span");
    assert_eq!(
        large_capacity_context.registers[5],
        u32::MAX.wrapping_sub(canonical_len as u32),
        "R5 reports capacity minus the canonical name length"
    );
    assert_eq!(
        caller
            .memory
            .read_bytes(BUFFER_ADDRESS, expected_name.len())
            .unwrap(),
        expected_name,
        "only the actual name plus NUL is written"
    );

    // Full output-span preflight prevents a canonical name from touching
    // the reserved X-error block or committing a prefix before failure.
    let guest_end = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32;
    let boundary = guest_end - 2;
    caller.memory.write_bytes(boundary, &[0x11, 0x22]).unwrap();
    caller
        .memory
        .write_bytes(
            caller.memory.swi_error_block_address(),
            &[0x33, 0x44, 0x55, 0x66],
        )
        .unwrap();
    let (bad_canonical, _) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[
            7,
            canonical_handle,
            boundary,
            0,
            0,
            (canonical_len + 1) as u32,
        ],
    );
    assert!(
        bad_canonical.is_err(),
        "OS_Args 7 rejects a caller span past guest memory"
    );
    assert_eq!(caller.memory.read_bytes(boundary, 2).unwrap(), [0x11, 0x22]);
    assert_eq!(
        caller
            .memory
            .read_bytes(caller.memory.swi_error_block_address(), 4)
            .unwrap(),
        [0x33, 0x44, 0x55, 0x66],
        "canonical-name write cannot leak into the reserved X-error block"
    );

    // The largest U32 seek/extent is representable without writing 4 GiB of
    // data; a sparse extent is immediately shrunk again for safe cleanup.
    let (boundary_file, boundary_context) = invoke_find(
        &mut dispatcher,
        &mut caller,
        0x80,
        &environment.guest_path("Boundary"),
    );
    boundary_file.expect("create a temporary boundary file");
    let boundary_handle = boundary_context.registers[0];
    let (maximum_extent, _) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[3, boundary_handle, u32::MAX],
    );
    maximum_extent.expect("U32::MAX extent is preserved without signed truncation");
    let (read_maximum, read_maximum_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[2, boundary_handle, 0],
    );
    read_maximum.expect("maximum U32 extent remains readable");
    assert_eq!(read_maximum_context.registers[2], u32::MAX);
    dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[1, boundary_handle, u32::MAX],
    )
    .0
    .expect("maximum U32 pointer remains representable");
    assert_eq!(
        get_file_pointer(&mut dispatcher, &mut caller, boundary_handle),
        u32::MAX
    );
    dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[3, boundary_handle, 0],
    )
    .0
    .expect("shrink sparse boundary fixture before cleanup");
    let (shrunken_pointer, shrunken_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_ARGS,
        &[0, boundary_handle, 0],
    );
    shrunken_pointer.expect("shrinking extent moves a pointer past EOF back to the new end");
    assert_eq!(shrunken_context.registers[2], 0);
    dispatch(
        &mut dispatcher,
        &mut caller,
        OS_FIND,
        &[0, boundary_handle, 0],
    )
    .0
    .expect("close boundary fixture");

    // Direct named SWIs remain usable from interpreted BASIC, not just Rust
    // tests. This exercises the public channel and returned byte in guest code.
    write_guest_string(&mut caller, STRING_ADDRESS, &environment.guest_path("Beta"));
    basic_compat::run_source(
        "10 SYS \"OS_Find\",&40,&2400 TO H%\n20 SYS \"OS_BGet\",0,H% TO B%\n30 !&5000=B%\n40 SYS \"OS_Args\",0,H% TO A%,H%,P%\n50 SYS \"OS_Find\",0,H%\n60 END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("interpreted BASIC SYS reaches the module-owned FileSwitch services");
    assert_eq!(caller.memory.read_byte(RESULT_ADDRESS).unwrap(), b'x');
    assert_owner(&dispatcher, OS_FIND, "FindService");

    // BBC CALL file adapters enter the same numeric FileSwitch services.
    // CALL does not provide a BASIC SYS-TO result list, so verify open-handle
    // creation, read cursor movement, and write output at the host fixture.
    let mut call_task = Task::new(0x0D03);
    write_guest_string(
        &mut call_task,
        STRING_ADDRESS,
        &environment.guest_path("CallFile"),
    );
    basic_compat::run_source(
        "10 A%=&C0:X%=0:Y%=&24:CALL &FFCE\n20 A%=0:X%=0:Y%=&80:CALL &FFD7\n30 A%=90:X%=0:Y%=&80:CALL &FFD4\n40 END",
        &mut call_task,
        &mut dispatcher,
    )
    .expect("BBC CALL &FFCE/&FFD7/&FFD4 use the owned FileSwitch endpoints");
    assert_owner(&dispatcher, OS_BPUT, "BytePutService");
    assert!(call_task.file_system.open_files.contains_key(&0x80));
    assert_eq!(
        get_file_pointer(&mut dispatcher, &mut call_task, 0x80),
        2,
        "BGet and BPut CALL adapters share the same open-channel cursor"
    );
    assert_eq!(fs::read(environment.root.join("CallFile")).unwrap(), b"QZ");
    dispatch(&mut dispatcher, &mut call_task, OS_FIND, &[0, 0x80, 0])
        .0
        .expect("close CALL-created channel");

    dispatch(&mut dispatcher, &mut caller, OS_FIND, &[0, 0, 0])
        .0
        .expect("close-all affects this caller's channels only");
    let (stale_caller_handle, _) = dispatch(&mut dispatcher, &mut caller, OS_BGET, &[0, alpha, 0]);
    assert!(
        stale_caller_handle.is_err(),
        "close-all invalidates caller's old handles"
    );
    let (other_task_byte, other_task_context) =
        dispatch(&mut dispatcher, &mut other, OS_BGET, &[0, beta, 0]);
    other_task_byte.expect("caller close-all must not close another Task's channel");
    assert_eq!(other_task_context.registers[0], u32::from(b'y'));
    dispatch(&mut dispatcher, &mut other, OS_FIND, &[0, 0, 0])
        .0
        .expect("other Task closes its own channels");

    // X-form failure is a standard caller-local V/error-block result, and no
    // numeric Rust implementation remains to answer after FileSwitch stops.
    let (invalid_handle_x, x_context) = dispatch(
        &mut dispatcher,
        &mut caller,
        OS_BGET | X_BIT,
        &[0, 0xFFFF, 0],
    );
    invalid_handle_x.expect("X-form FileSwitch failure is returned in V/error block");
    assert!(x_context.overflow);
    assert_ne!(x_context.registers[0], 0);
    assert_owner(&dispatcher, OS_BGET, "ByteGetService");

    let mut module_manager = Task::trusted_mos_session(0x0DFF);
    dispatcher
        .basic64_module_manager()
        .quiesce("FileSwitch", &mut module_manager)
        .expect("FileSwitch can be quiesced by the explicit trusted MOS session");
    let (inactive, _) = dispatch(&mut dispatcher, &mut caller, OS_FIND, &[0, 0, 0]);
    assert!(
        matches!(inactive, Err(RuntimeError::Program(ref message)) if message.contains("published") && message.contains("owning module")),
        "inactive published FileSwitch SWI must not fall through to the old Rust handler: {inactive:?}"
    );
}
