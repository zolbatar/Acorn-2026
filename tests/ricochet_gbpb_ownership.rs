use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    filesystem::{FileMetadata, metadata_path, write_metadata},
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{
        DisplayEvent, OS_ARGS, OS_BGET, OS_FIND, OS_GBPB, SwiContext, SwiDispatchRoute,
        SwiDispatcher,
    },
};

const PATH_ADDRESS: u32 = 0x2400;
const WILDCARD_ADDRESS: u32 = 0x2600;
const BUFFER_ADDRESS: u32 = 0x4000;
const X_BIT: u32 = 1 << 17;
const MAX_GBPB_TRANSFER: u32 = 1024 * 1024;

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
            .expect("system time is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ricochet-gbpb-ownership-{}-{nonce}",
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

fn assert_gbpb_owner(dispatcher: &SwiDispatcher) {
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned {
                number,
                name,
                module,
                definition,
                ..
            }) if *number == OS_GBPB
                && name.eq_ignore_ascii_case("OS_GBPB")
                && module.eq_ignore_ascii_case("FileSwitch")
                && definition.eq_ignore_ascii_case("BulkTransferService")
        ),
        "OS_GBPB did not route through FileSwitch::BulkTransferService: {:?}",
        dispatcher.last_dispatch_route()
    );
}

fn write_guest_string(task: &mut Task, address: u32, value: &str) {
    task.memory.write_bytes(address, value.as_bytes()).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn open_file(dispatcher: &mut SwiDispatcher, task: &mut Task, path: &str, mode: u32) -> u32 {
    write_guest_string(task, PATH_ADDRESS, path);
    let mut context = SwiContext::default();
    context.registers[0] = mode;
    context.registers[1] = PATH_ADDRESS;
    dispatcher
        .dispatch(OS_FIND, task, &mut context)
        .expect("OS_Find opens fixture file");
    context.registers[0]
}

fn file_position(dispatcher: &mut SwiDispatcher, task: &mut Task, handle: u32) -> u32 {
    let mut context = SwiContext::default();
    context.registers[0] = 0;
    context.registers[1] = handle;
    dispatcher
        .dispatch(OS_ARGS, task, &mut context)
        .expect("OS_Args 0 returns current file position");
    context.registers[2]
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_prefix(path: &Path, maximum_bytes: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take(maximum_bytes as u64)
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

fn call_gbpb(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    registers: &[u32],
) -> (Result<(), RuntimeError>, SwiContext) {
    dispatch(dispatcher, task, OS_GBPB, registers)
}

fn close_file(dispatcher: &mut SwiDispatcher, task: &mut Task, handle: u32) {
    dispatch(dispatcher, task, OS_FIND, &[0, handle, 0])
        .0
        .expect("close task-local file channel");
}

#[test]
fn gbpb_services_are_module_owned_checked_and_preserve_file_and_directory_contracts() {
    let environment = Environment::new();
    fs::write(environment.root.join("Bulk"), b"012345").unwrap();
    fs::write(environment.root.join("Alpha"), b"a").unwrap();
    fs::write(environment.root.join("Beta"), b"b").unwrap();
    fs::write(environment.root.join("Bz"), b"z").unwrap();
    write_metadata(
        &metadata_path(&environment.root.join("Beta")),
        &FileMetadata {
            guest_name: "Beta".into(),
            file_type: 0xABC,
            load_address: 0x12345,
            execution_address: 0x0102_0304,
            attributes: 0xA5,
        },
    )
    .unwrap();
    fs::write(environment.root.join("Gamma"), b"g").unwrap();
    fs::create_dir(environment.root.join("Folder")).unwrap();

    let (mut dispatcher, _display) = dispatcher();
    let mut task = Task::new(0x0C01);
    let mut other = Task::new(0x0C02);
    assert!(
        dispatcher
            .module_registry()
            .active_modules_sorted()
            .iter()
            .any(|module| module.manifest.name.eq_ignore_ascii_case("FileSwitch"))
    );

    let handle = open_file(
        &mut dispatcher,
        &mut task,
        &environment.guest_path("Bulk"),
        0xC0,
    );
    assert!(
        (0x80..=0xFF).contains(&handle),
        "OS_Find returned {handle:#x}; route={:?}",
        dispatcher.last_dispatch_route()
    );

    // Zero-length transfers retain the PRM's reason-specific pointer rules.
    // Keep this matrix in the shared-environment test to avoid parallel tests
    // changing process-wide HostFS configuration.
    fs::write(environment.root.join("ZeroProbe"), b"abc").unwrap();
    let zero_handle = open_file(
        &mut dispatcher,
        &mut task,
        &environment.guest_path("ZeroProbe"),
        0xC0,
    );
    for (reason, offset) in [
        (3, 0),
        (3, 3),
        (3, 4),
        (3, u32::MAX),
        (1, 0),
        (1, 3),
        (1, 4),
        (2, 0),
        (2, 3),
        (2, 4),
        (2, u32::MAX),
        (4, 0),
        (4, 3),
        (4, 4),
        (4, u32::MAX),
    ] {
        let extent_before = fs::metadata(environment.root.join("ZeroProbe"))
            .unwrap()
            .len() as u32;
        let start_cursor = if reason == 1 || reason == 3 {
            extent_before
        } else {
            offset
        };
        let (seek_eof, _) = dispatch(
            &mut dispatcher,
            &mut task,
            OS_ARGS,
            &[1, zero_handle, start_cursor],
        );
        seek_eof.expect("position zero-probe channel before zero transfer");
        let extent_after_seek = fs::metadata(environment.root.join("ZeroProbe"))
            .unwrap()
            .len() as u32;
        assert_eq!(
            extent_after_seek,
            extent_before.max(start_cursor),
            "OS_Args 1 zero-fills when positioning beyond the extent"
        );
        let zero_file_before = read_prefix(&environment.root.join("ZeroProbe"), 8);
        let (first_eof, first_eof_context) =
            dispatch(&mut dispatcher, &mut task, OS_BGET, &[0, zero_handle, 0]);
        first_eof.expect("read at EOF establishes delayed EOF state");
        let had_eof_state = start_cursor >= extent_before;
        assert_eq!(first_eof_context.carry, had_eof_state);
        if !had_eof_state {
            let (restore_cursor, _) = dispatch(
                &mut dispatcher,
                &mut task,
                OS_ARGS,
                &[1, zero_handle, start_cursor],
            );
            restore_cursor.expect("restore the sequential cursor after the in-file probe");
        }

        let expected_cursor = match reason {
            1 => offset,
            2 | 4 => start_cursor,
            3 if offset <= extent_before => offset,
            3 => start_cursor,
            _ => unreachable!(),
        };
        let expected_extent = if reason == 1 {
            u64::from(extent_after_seek.max(offset))
        } else {
            u64::from(extent_after_seek)
        };
        let mut expected_prefix = zero_file_before.clone();
        if reason == 1 && offset > extent_after_seek && offset <= 8 {
            expected_prefix.resize(offset as usize, 0);
        }

        task.memory.write_bytes(BUFFER_ADDRESS, &[0xE7; 4]).unwrap();
        let (zero_transfer, zero_context) = call_gbpb(
            &mut dispatcher,
            &mut task,
            &[reason, zero_handle, BUFFER_ADDRESS, 0, offset],
        );
        zero_transfer.expect("zero-length transfer follows reason-specific PRM semantics");
        assert_gbpb_owner(&dispatcher);
        assert_eq!(zero_context.registers[0], reason);
        assert_eq!(zero_context.registers[1], zero_handle);
        assert_eq!(zero_context.registers[2], BUFFER_ADDRESS);
        assert_eq!(zero_context.registers[3], 0);
        assert_eq!(
            zero_context.registers[4],
            if reason == 1 || reason == 3 {
                offset
            } else {
                start_cursor
            }
        );
        assert!(!zero_context.carry);
        assert_eq!(
            file_position(&mut dispatcher, &mut task, zero_handle),
            expected_cursor
        );
        assert_eq!(
            fs::metadata(environment.root.join("ZeroProbe"))
                .unwrap()
                .len(),
            expected_extent
        );
        assert_eq!(
            read_prefix(&environment.root.join("ZeroProbe"), expected_prefix.len()),
            expected_prefix
        );
        assert_eq!(
            task.memory.read_bytes(BUFFER_ADDRESS, 4).unwrap(),
            [0xE7; 4]
        );

        let (rearmed_eof, rearmed_context) =
            dispatch(&mut dispatcher, &mut task, OS_BGET, &[0, zero_handle, 0]);
        rearmed_eof.expect("zero transfer clears the delayed EOF error");
        if expected_cursor < expected_extent as u32 {
            assert!(!rearmed_context.carry);
            assert_eq!(
                rearmed_context.registers[0],
                u32::from(expected_prefix[expected_cursor as usize])
            );
        } else {
            assert!(rearmed_context.carry);
            let (delayed_error, _) =
                dispatch(&mut dispatcher, &mut task, OS_BGET, &[0, zero_handle, 0]);
            assert!(
                delayed_error.is_err(),
                "a fresh EOF read rearms delayed error"
            );
        }
    }

    // At the U32 endpoint, reason 1's one-byte-length gap is represented by
    // a sparse file, avoiding a multi-gigabyte buffer or physical zero write.
    let sparse_path = environment.root.join("ZeroSparse");
    fs::write(&sparse_path, b"x").unwrap();
    OpenOptions::new()
        .write(true)
        .open(&sparse_path)
        .unwrap()
        .set_len(u64::from(u32::MAX) - 1)
        .unwrap();
    let sparse_handle = open_file(
        &mut dispatcher,
        &mut task,
        &environment.guest_path("ZeroSparse"),
        0xC0,
    );
    task.memory.write_bytes(BUFFER_ADDRESS, &[0x5D; 2]).unwrap();
    let (sparse_zero_write, sparse_zero_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[1, sparse_handle, BUFFER_ADDRESS, 0, u32::MAX],
    );
    sparse_zero_write.expect("zero-byte explicit write extends to the requested offset");
    assert_eq!(sparse_zero_context.registers[2], BUFFER_ADDRESS);
    assert_eq!(sparse_zero_context.registers[3], 0);
    assert_eq!(sparse_zero_context.registers[4], u32::MAX);
    assert!(!sparse_zero_context.carry);
    assert_eq!(
        file_position(&mut dispatcher, &mut task, sparse_handle),
        u32::MAX
    );
    assert_eq!(
        fs::metadata(&sparse_path).unwrap().len(),
        u64::from(u32::MAX)
    );
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 2).unwrap(),
        [0x5D; 2]
    );
    close_file(&mut dispatcher, &mut task, sparse_handle);
    fs::remove_file(sparse_path).unwrap();

    task.memory.write_bytes(BUFFER_ADDRESS, &[0x91; 4]).unwrap();
    let zero_file_before_missing = read_prefix(&environment.root.join("ZeroProbe"), 8);
    let (missing_handle_zero, _) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[1, 0x77, BUFFER_ADDRESS, 0, u32::MAX],
    );
    assert!(
        missing_handle_zero.is_err(),
        "zero count still validates the handle"
    );
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 4).unwrap(),
        [0x91; 4]
    );
    assert_eq!(
        read_prefix(
            &environment.root.join("ZeroProbe"),
            zero_file_before_missing.len()
        ),
        zero_file_before_missing
    );
    close_file(&mut dispatcher, &mut task, zero_handle);
    fs::remove_file(environment.root.join("ZeroProbe")).unwrap();

    // A BGet and the bulk transfer API share the same task-owned channel and
    // sequential cursor. Reason 2 writes at the cursor left by the byte read.
    let mut bget = SwiContext::default();
    bget.registers[1] = handle;
    dispatcher
        .dispatch(OS_BGET, &mut task, &mut bget)
        .expect("BGet reads the first byte");
    assert_eq!(bget.registers[0], u32::from(b'0'));
    task.memory.write_bytes(BUFFER_ADDRESS, b"xy").unwrap();
    let (reason_two, reason_two_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[2, handle, BUFFER_ADDRESS, 2, 0],
    );
    reason_two.expect("OS_GBPB reason 2 writes at the current pointer");
    assert_gbpb_owner(&dispatcher);
    assert_eq!(reason_two_context.registers[0], 2);
    assert_eq!(reason_two_context.registers[1], handle);
    assert_eq!(reason_two_context.registers[2], BUFFER_ADDRESS + 2);
    assert_eq!(reason_two_context.registers[3], 0);
    assert_eq!(reason_two_context.registers[4], 3);
    assert!(!reason_two_context.carry);
    assert_eq!(file_position(&mut dispatcher, &mut task, handle), 3);
    assert_eq!(fs::read(environment.root.join("Bulk")).unwrap(), b"0xy345");
    let (zero_write, zero_write_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[2, handle, BUFFER_ADDRESS, 0, 0],
    );
    zero_write.expect("zero-byte reason 2 write is a no-op");
    assert_eq!(zero_write_context.registers[2], BUFFER_ADDRESS);
    assert_eq!(zero_write_context.registers[3], 0);
    assert_eq!(zero_write_context.registers[4], 3);
    assert!(!zero_write_context.carry);
    assert_eq!(file_position(&mut dispatcher, &mut task, handle), 3);

    task.memory.write_bytes(BUFFER_ADDRESS, b"AB").unwrap();
    let (reason_one, reason_one_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[1, handle, BUFFER_ADDRESS, 2, 4],
    );
    reason_one.expect("OS_GBPB reason 1 writes at the explicit file pointer");
    assert_gbpb_owner(&dispatcher);
    assert_eq!(reason_one_context.registers[0], 1);
    assert_eq!(reason_one_context.registers[1], handle);
    assert_eq!(reason_one_context.registers[2], BUFFER_ADDRESS + 2);
    assert_eq!(reason_one_context.registers[3], 0);
    assert_eq!(reason_one_context.registers[4], 6);
    assert!(!reason_one_context.carry);
    assert_eq!(file_position(&mut dispatcher, &mut task, handle), 6);
    assert_eq!(fs::read(environment.root.join("Bulk")).unwrap(), b"0xy3AB");

    // Invalid caller-memory ranges fail before an explicit seek can alter the
    // channel pointer or file contents. Check normal and X-form failure paths.
    let file_before_invalid_write = fs::read(environment.root.join("Bulk")).unwrap();
    let pointer_before_invalid_write = file_position(&mut dispatcher, &mut task, handle);
    let guest_end = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32;
    let (invalid_write, _) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[1, handle, guest_end - 1, 4, 99],
    );
    assert!(invalid_write.is_err(), "write span must be preflighted");
    assert_eq!(
        file_position(&mut dispatcher, &mut task, handle),
        pointer_before_invalid_write
    );
    assert_eq!(
        fs::read(environment.root.join("Bulk")).unwrap(),
        file_before_invalid_write
    );

    // File-range end arithmetic is checked independently of the guest-memory
    // span. Both an explicit write and read at UINT32_MAX must reject a
    // nonempty transfer before seeking, changing the file, or touching memory.
    task.memory.write_bytes(BUFFER_ADDRESS, &[0xB6; 4]).unwrap();
    let buffer_before_offset_overflow = task.memory.read_bytes(BUFFER_ADDRESS, 4).unwrap();
    for reason in [1, 3] {
        let (offset_overflow, _) = call_gbpb(
            &mut dispatcher,
            &mut task,
            &[reason, handle, BUFFER_ADDRESS, 2, u32::MAX],
        );
        assert!(
            offset_overflow.is_err(),
            "reason {reason} rejects an overflowing U32 file range"
        );
        assert_eq!(
            file_position(&mut dispatcher, &mut task, handle),
            pointer_before_invalid_write,
            "reason {reason} leaves the channel cursor unchanged"
        );
        assert_eq!(
            fs::read(environment.root.join("Bulk")).unwrap(),
            file_before_invalid_write,
            "reason {reason} leaves file contents unchanged"
        );
        assert_eq!(
            task.memory.read_bytes(BUFFER_ADDRESS, 4).unwrap(),
            buffer_before_offset_overflow,
            "reason {reason} leaves caller memory unchanged"
        );
    }

    let (invalid_write_x, invalid_write_x_context) = dispatch(
        &mut dispatcher,
        &mut task,
        OS_GBPB | X_BIT,
        &[1, handle, guest_end - 1, 4, 77],
    );
    invalid_write_x.expect("X-form reports the checked range fault through V/error block");
    assert!(invalid_write_x_context.overflow);
    assert_eq!(
        file_position(&mut dispatcher, &mut task, handle),
        pointer_before_invalid_write
    );

    let (huge_write, _) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[2, handle, BUFFER_ADDRESS, MAX_GBPB_TRANSFER + 1, 0],
    );
    assert!(
        huge_write.is_err(),
        "oversized transfer is rejected before allocation"
    );
    assert_eq!(
        file_position(&mut dispatcher, &mut task, handle),
        pointer_before_invalid_write
    );
    assert_eq!(
        fs::read(environment.root.join("Bulk")).unwrap(),
        file_before_invalid_write
    );

    // Reason 3 beyond EOF transfers no bytes and preserves the old sequential
    // pointer; reason 4 then demonstrates partial EOF and R3/carry reporting.
    let (set_cursor, _) = dispatch(&mut dispatcher, &mut task, OS_ARGS, &[1, handle, 2]);
    set_cursor.expect("place cursor before specified beyond-EOF read");
    task.memory.write_bytes(BUFFER_ADDRESS, &[0xA5; 8]).unwrap();
    let (beyond_eof, beyond_eof_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[3, handle, BUFFER_ADDRESS, 3, 99],
    );
    beyond_eof.expect("reason 3 beyond EOF returns an empty transfer");
    assert_eq!(beyond_eof_context.registers[0], 3);
    assert_eq!(beyond_eof_context.registers[1], handle);
    assert_eq!(beyond_eof_context.registers[2], BUFFER_ADDRESS);
    assert_eq!(beyond_eof_context.registers[3], 3);
    assert_eq!(beyond_eof_context.registers[4], 99);
    assert!(beyond_eof_context.carry);
    assert_eq!(file_position(&mut dispatcher, &mut task, handle), 2);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 8).unwrap(),
        [0xA5; 8]
    );

    let (zero_read, zero_read_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[4, handle, BUFFER_ADDRESS, 0, 0],
    );
    zero_read.expect("zero-byte reason 4 read is a no-op");
    assert_eq!(zero_read_context.registers[2], BUFFER_ADDRESS);
    assert_eq!(zero_read_context.registers[3], 0);
    assert_eq!(zero_read_context.registers[4], 2);
    assert!(!zero_read_context.carry);
    assert_eq!(file_position(&mut dispatcher, &mut task, handle), 2);

    let (partial_read, partial_read_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[4, handle, BUFFER_ADDRESS, 10, 0],
    );
    partial_read.expect("reason 4 reads the current pointer through EOF");
    assert_gbpb_owner(&dispatcher);
    assert_eq!(partial_read_context.registers[0], 4);
    assert_eq!(partial_read_context.registers[1], handle);
    assert_eq!(partial_read_context.registers[2], BUFFER_ADDRESS + 4);
    assert_eq!(partial_read_context.registers[3], 6);
    assert_eq!(partial_read_context.registers[4], 6);
    assert!(partial_read_context.carry);
    assert_eq!(task.memory.read_bytes(BUFFER_ADDRESS, 4).unwrap(), b"y3AB");
    assert_eq!(file_position(&mut dispatcher, &mut task, handle), 6);

    let read_pointer_before_invalid = file_position(&mut dispatcher, &mut task, handle);
    let (invalid_read, _) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[3, handle, guest_end - 1, 4, 1],
    );
    assert!(
        invalid_read.is_err(),
        "read destination span must be preflighted"
    );
    assert_eq!(
        file_position(&mut dispatcher, &mut task, handle),
        read_pointer_before_invalid
    );

    let (wrong_task_handle, _) = call_gbpb(
        &mut dispatcher,
        &mut other,
        &[4, handle, BUFFER_ADDRESS, 1, 0],
    );
    assert!(
        wrong_task_handle.is_err(),
        "file handles remain caller-task scoped"
    );
    assert_eq!(
        file_position(&mut dispatcher, &mut task, handle),
        read_pointer_before_invalid
    );

    // Reasons 5–7 return bounded byte-oriented FS/directory information and
    // commit only after the entire output span has been checked.
    for (reason, expected) in [
        (5, b"\x08DemoDisk\0".as_slice()),
        (6, b"\0\x01$\0".as_slice()),
        (7, b"\0\x01$\0".as_slice()),
    ] {
        let (names, names_context) = call_gbpb(
            &mut dispatcher,
            &mut task,
            &[reason, 0xA1A2_A3A4, BUFFER_ADDRESS, 0x1234, 0x5678],
        );
        names.expect("OS_GBPB 5–7 writes a complete metadata record");
        assert_gbpb_owner(&dispatcher);
        assert_eq!(&names_context.registers[0..2], &[reason, 0xA1A2_A3A4]);
        assert_eq!(names_context.registers[2], BUFFER_ADDRESS);
        assert_eq!(
            task.memory
                .read_bytes(BUFFER_ADDRESS, expected.len())
                .unwrap(),
            expected
        );
    }
    let guest_end = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32;
    task.memory
        .write_bytes(guest_end - 2, &[0x61, 0x62])
        .unwrap();
    let (short_fs_info, _) = call_gbpb(&mut dispatcher, &mut task, &[5, 0, guest_end - 2, 0, 0]);
    assert!(
        short_fs_info.is_err(),
        "reason 5 validates its full output span"
    );
    assert_eq!(
        task.memory.read_bytes(guest_end - 2, 2).unwrap(),
        [0x61, 0x62]
    );

    // Reason 8 uses the current directory and length-prefixed names. It stages
    // the entire requested result before writing, with a stable sorted offset.
    task.memory
        .write_bytes(BUFFER_ADDRESS, &[0xCC; 64])
        .unwrap();
    let (reason_eight_first, reason_eight_first_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[8, 0x1111, BUFFER_ADDRESS, 2, 0],
    );
    reason_eight_first.expect("reason 8 enumerates the current directory");
    assert_gbpb_owner(&dispatcher);
    assert_eq!(reason_eight_first_context.registers[0], 8);
    assert_eq!(reason_eight_first_context.registers[1], 0x1111);
    assert_eq!(reason_eight_first_context.registers[2], BUFFER_ADDRESS);
    assert_eq!(reason_eight_first_context.registers[3], 0);
    assert_eq!(reason_eight_first_context.registers[4], 2);
    assert!(!reason_eight_first_context.carry);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 11).unwrap(),
        b"\x05Alpha\x04Beta"
    );

    let (reason_eight_second, reason_eight_second_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[8, 0x3333, BUFFER_ADDRESS, 2, 2],
    );
    reason_eight_second.expect("reason 8 continues after the prior filtered index");
    assert_eq!(reason_eight_second_context.registers[3], 0);
    assert_eq!(reason_eight_second_context.registers[4], 4);
    assert!(!reason_eight_second_context.carry);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 8).unwrap(),
        b"\x04Bulk\x02Bz"
    );

    let (reason_eight_last, reason_eight_last_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[8, 0x4444, BUFFER_ADDRESS, 2, 4],
    );
    reason_eight_last.expect("reason 8 signals residual request at end");
    assert_eq!(reason_eight_last_context.registers[3], 0);
    assert_eq!(reason_eight_last_context.registers[4], u32::MAX);
    assert!(!reason_eight_last_context.carry);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 13).unwrap(),
        b"\x06Folder\x05Gamma"
    );

    task.memory
        .write_bytes(guest_end - 2, &[0x11, 0x22])
        .unwrap();
    let (invalid_reason_eight, _) =
        call_gbpb(&mut dispatcher, &mut task, &[8, 0, guest_end - 2, 2, 0]);
    assert!(
        invalid_reason_eight.is_err(),
        "reason 8 preflights the complete output"
    );
    assert_eq!(
        task.memory.read_bytes(guest_end - 2, 2).unwrap(),
        [0x11, 0x22]
    );

    // Reasons 9 and 10 enumerate a path with wildcard filtering and bounded
    // continuation. Short buffers return complete records only, never a prefix.
    write_guest_string(&mut task, PATH_ADDRESS, "HostFS::DemoDisk.$");
    write_guest_string(&mut task, WILDCARD_ADDRESS, "B*");
    task.memory
        .write_bytes(BUFFER_ADDRESS, &[0xEE; 64])
        .unwrap();
    let (reason_nine_first, reason_nine_first_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[9, PATH_ADDRESS, BUFFER_ADDRESS, 2, 0, 5, WILDCARD_ADDRESS],
    );
    reason_nine_first.expect("reason 9 writes the first whole matching NUL name");
    assert_gbpb_owner(&dispatcher);
    assert_eq!(reason_nine_first_context.registers[0], 9);
    assert_eq!(reason_nine_first_context.registers[1], PATH_ADDRESS);
    assert_eq!(reason_nine_first_context.registers[2], BUFFER_ADDRESS);
    assert_eq!(reason_nine_first_context.registers[3], 1);
    assert_eq!(reason_nine_first_context.registers[4], 1);
    assert_eq!(reason_nine_first_context.registers[5], 5);
    assert_eq!(reason_nine_first_context.registers[6], WILDCARD_ADDRESS);
    assert!(reason_nine_first_context.carry);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 5).unwrap(),
        b"Beta\0"
    );
    assert_eq!(task.memory.read_byte(BUFFER_ADDRESS + 5).unwrap(), 0xEE);

    // A later shorter match must not be returned by skipping the first record
    // when the capacity cannot hold that first whole record.
    task.memory.write_bytes(BUFFER_ADDRESS, &[0xA7; 3]).unwrap();
    let (first_name_too_long, first_name_too_long_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[9, PATH_ADDRESS, BUFFER_ADDRESS, 3, 0, 3, WILDCARD_ADDRESS],
    );
    first_name_too_long.expect("short reason-9 capacity returns no partial names");
    assert_eq!(first_name_too_long_context.registers[3], 0);
    assert_eq!(first_name_too_long_context.registers[4], 0);
    assert!(!first_name_too_long_context.carry);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 3).unwrap(),
        [0xA7; 3]
    );
    task.memory
        .write_bytes(BUFFER_ADDRESS, &[0xEE; 64])
        .unwrap();

    let (reason_nine_next, reason_nine_next_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[9, PATH_ADDRESS, BUFFER_ADDRESS, 2, 1, 64, WILDCARD_ADDRESS],
    );
    reason_nine_next.expect("reason 9 retries the unreturned matching record");
    assert_eq!(reason_nine_next_context.registers[3], 2);
    assert_eq!(reason_nine_next_context.registers[4], u32::MAX);
    assert!(reason_nine_next_context.carry);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 8).unwrap(),
        b"Bulk\0Bz\0"
    );

    write_guest_string(
        &mut task,
        PATH_ADDRESS,
        "HostFS::DemoDisk.$.MissingDirectory",
    );
    task.memory
        .write_bytes(BUFFER_ADDRESS, &[0x7C; 16])
        .unwrap();
    let (invalid_directory, _) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[9, PATH_ADDRESS, BUFFER_ADDRESS, 2, 0, 16, WILDCARD_ADDRESS],
    );
    assert!(
        invalid_directory.is_err(),
        "reason 9 rejects a missing guest directory"
    );
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 16).unwrap(),
        [0x7C; 16]
    );
    write_guest_string(&mut task, PATH_ADDRESS, "HostFS::DemoDisk.$");

    // Reason 10 requires word alignment, returns fixed LE header fields plus
    // padded complete records, and preserves the next offset on short capacity.
    task.memory
        .write_bytes(BUFFER_ADDRESS, &[0xDD; 64])
        .unwrap();
    let (reason_ten_short, reason_ten_short_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[10, PATH_ADDRESS, BUFFER_ADDRESS, 2, 0, 27, WILDCARD_ADDRESS],
    );
    reason_ten_short.expect("reason 10 reports a short buffer without partial record");
    assert_eq!(reason_ten_short_context.registers[3], 0);
    assert_eq!(reason_ten_short_context.registers[4], 0);
    assert!(!reason_ten_short_context.carry);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 8).unwrap(),
        [0xDD; 8]
    );

    task.memory
        .write_bytes(BUFFER_ADDRESS, &[0xD3; 24])
        .unwrap();
    let (later_smaller_record, later_smaller_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[10, PATH_ADDRESS, BUFFER_ADDRESS, 3, 0, 24, WILDCARD_ADDRESS],
    );
    later_smaller_record.expect("reason 10 stops at the first whole record that cannot fit");
    assert_eq!(later_smaller_context.registers[3], 0);
    assert_eq!(later_smaller_context.registers[4], 0);
    assert!(!later_smaller_context.carry);
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS, 24).unwrap(),
        [0xD3; 24]
    );

    let (reason_ten_first, reason_ten_first_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[10, PATH_ADDRESS, BUFFER_ADDRESS, 2, 0, 28, WILDCARD_ADDRESS],
    );
    reason_ten_first.expect("reason 10 emits one whole aligned record");
    assert_eq!(reason_ten_first_context.registers[3], 1);
    assert_eq!(reason_ten_first_context.registers[4], 1);
    assert!(reason_ten_first_context.carry);
    let beta_record = task.memory.read_bytes(BUFFER_ADDRESS, 28).unwrap();
    assert_eq!(read_u32(&beta_record, 0), 0xABC1_2345);
    assert_eq!(read_u32(&beta_record, 4), 0x0102_0304);
    assert_eq!(read_u32(&beta_record, 8), 1);
    assert_eq!(read_u32(&beta_record, 12), 0xA5);
    assert_eq!(read_u32(&beta_record, 16), 1);
    assert_eq!(&beta_record[20..25], b"Beta\0");
    assert_eq!(&beta_record[25..28], &[0, 0, 0]);

    let (reason_ten_next, reason_ten_next_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[10, PATH_ADDRESS, BUFFER_ADDRESS, 2, 1, 32, WILDCARD_ADDRESS],
    );
    reason_ten_next.expect("reason 10 continuation returns the second matching record");
    assert_eq!(reason_ten_next_context.registers[3], 1);
    assert_eq!(reason_ten_next_context.registers[4], 2);
    assert!(reason_ten_next_context.carry);
    let bulk_record = task.memory.read_bytes(BUFFER_ADDRESS, 32).unwrap();
    assert_eq!(read_u32(&bulk_record, 8), 6);
    assert_eq!(&bulk_record[20..25], b"Bulk\0");

    let (reason_ten_final, reason_ten_final_context) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[10, PATH_ADDRESS, BUFFER_ADDRESS, 2, 2, 24, WILDCARD_ADDRESS],
    );
    reason_ten_final.expect("reason 10 continuation returns the final matching record");
    assert_eq!(reason_ten_final_context.registers[3], 1);
    assert_eq!(reason_ten_final_context.registers[4], u32::MAX);
    assert!(reason_ten_final_context.carry);
    let bz_record = task.memory.read_bytes(BUFFER_ADDRESS, 24).unwrap();
    assert_eq!(read_u32(&bz_record, 8), 1);
    assert_eq!(&bz_record[20..23], b"Bz\0");

    task.memory
        .write_bytes(BUFFER_ADDRESS + 1, &[0x88; 32])
        .unwrap();
    let (unaligned_reason_ten, _) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[
            10,
            PATH_ADDRESS,
            BUFFER_ADDRESS + 1,
            1,
            0,
            32,
            WILDCARD_ADDRESS,
        ],
    );
    assert!(
        unaligned_reason_ten.is_err(),
        "reason 10 enforces word alignment"
    );
    assert_eq!(
        task.memory.read_bytes(BUFFER_ADDRESS + 1, 8).unwrap(),
        [0x88; 8]
    );

    task.memory
        .write_bytes(guest_end - 4, &[0x51, 0x52, 0x53, 0x54])
        .unwrap();
    let (invalid_directory_output, _) = call_gbpb(
        &mut dispatcher,
        &mut task,
        &[9, PATH_ADDRESS, guest_end - 4, 2, 0, 64, WILDCARD_ADDRESS],
    );
    assert!(
        invalid_directory_output.is_err(),
        "directory output span is checked atomically"
    );
    assert_eq!(
        task.memory.read_bytes(guest_end - 4, 4).unwrap(),
        [0x51, 0x52, 0x53, 0x54]
    );

    for reason in [11, 12] {
        let (invalid_reason, _) = call_gbpb(&mut dispatcher, &mut task, &[reason, handle, 0, 0, 0]);
        assert!(
            invalid_reason.is_err(),
            "reason {reason} remains outside the hosted subset"
        );
    }

    close_file(&mut dispatcher, &mut task, handle);
    let mut manager = Task::trusted_mos_session(0x0C03);
    dispatcher
        .basic64_module_manager()
        .quiesce("FileSwitch", &mut manager)
        .expect("trusted manager may quiesce FileSwitch for route check");
    let (inactive, _) = call_gbpb(&mut dispatcher, &mut task, &[4, 0x80, BUFFER_ADDRESS, 0, 0]);
    assert!(
        inactive.is_err(),
        "inactive FileSwitch OS_GBPB must not fall through to a Rust public handler"
    );
}
