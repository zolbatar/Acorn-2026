use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    basic_compat,
    error::RuntimeError,
    filesystem::{FileMetadata, metadata_path, read_metadata, write_metadata},
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{DisplayEvent, OS_FILE, OS_FIND, SwiContext, SwiDispatchRoute, SwiDispatcher},
};

const PATH_ADDRESS: u32 = 0x2400;
const SOURCE_ADDRESS: u32 = 0x5000;
const X_BIT: u32 = 1 << 17;
const MAX_OS_FILE_TRANSFER: usize = 1024 * 1024;

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
            "ricochet-os-file-ownership-{}-{nonce}",
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

fn call(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    _reason: u32,
    registers: [u32; 10],
    x_form: bool,
) -> (Result<(), RuntimeError>, SwiContext) {
    let mut context = SwiContext::default();
    context.registers[..registers.len()].copy_from_slice(&registers);
    let number = OS_FILE | if x_form { X_BIT } else { 0 };
    let result = dispatcher.dispatch(number, task, &mut context);
    (result, context)
}

fn registers(reason: u32, path: u32) -> [u32; 10] {
    let mut registers = [0xA5A5_0000; 10];
    registers[0] = reason;
    registers[1] = path;
    registers
}

fn set_path(task: &mut Task, path: &str) {
    task.memory
        .write_bytes(PATH_ADDRESS, path.as_bytes())
        .unwrap();
    task.memory
        .write_byte(PATH_ADDRESS + path.len() as u32, 0)
        .unwrap();
}

fn assert_owner(dispatcher: &SwiDispatcher) {
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned {
                number,
                name,
                module,
                definition,
                ..
            }) if *number == OS_FILE
                && name.eq_ignore_ascii_case("OS_File")
                && module.eq_ignore_ascii_case("FileSwitch")
                && definition.eq_ignore_ascii_case("FileService")
        ),
        "OS_File did not route through FileSwitch::FileService: {:?}",
        dispatcher.last_dispatch_route()
    );
}

#[test]
fn os_file_is_module_owned_bounded_and_preserves_hosted_file_contracts() {
    let environment = Environment::new();
    fs::write(environment.root.join("Source"), b"hello").unwrap();
    write_metadata(
        &metadata_path(&environment.root.join("Source")),
        &FileMetadata {
            guest_name: "Source".into(),
            file_type: 0xABC,
            load_address: 0x12345,
            execution_address: 0x0102_0304,
            attributes: 0xA5,
        },
    )
    .unwrap();

    let (mut dispatcher, _display) = dispatcher();
    let mut task = Task::new(0x0F01);
    let mut other = Task::new(0x0F02);
    assert!(
        dispatcher
            .module_registry()
            .active_modules_sorted()
            .iter()
            .any(|module| module.manifest.name.eq_ignore_ascii_case("FileSwitch"))
    );

    // Metadata-only operations on a missing object must not create orphaned
    // sidecars or a phantom payload.
    for reason in [1, 2, 3, 4, 9, 18] {
        set_path(&mut task, &environment.guest_path("MissingMeta"));
        let mut update = registers(reason, PATH_ADDRESS);
        update[2] = 0x1234;
        update[3] = 0x5678;
        update[5] = 0x9A;
        let (result, _) = call(&mut dispatcher, &mut task, reason, update, false);
        assert!(
            result.is_err(),
            "metadata reason {reason} needs an existing file"
        );
        assert!(!environment.root.join("MissingMeta").exists());
        assert!(!metadata_path(&environment.root.join("MissingMeta")).exists());
    }

    // Reason 17 performs a direct no-search catalogue; the fully qualified
    // pathname selects its filing system explicitly.
    set_path(&mut task, &environment.guest_path("Source"));
    let mut input = registers(17, PATH_ADDRESS);
    input[6] = 0x6A6A_6A6A;
    let (catalogue, catalogue_context) = call(&mut dispatcher, &mut task, 17, input, false);
    catalogue.expect("reason 17 catalogues a regular guest file without search");
    assert_owner(&dispatcher);
    assert_eq!(catalogue_context.registers[0], 1);
    assert_eq!(catalogue_context.registers[2], (0xABC << 20) | 0x12345);
    assert_eq!(catalogue_context.registers[3], 0x0102_0304);
    assert_eq!(catalogue_context.registers[4], 5);
    assert_eq!(catalogue_context.registers[5], 0xA5);
    assert_eq!(catalogue_context.registers[1], PATH_ADDRESS);
    assert_eq!(catalogue_context.registers[6], 0x6A6A_6A6A);

    let (x_catalogue, _) = call(&mut dispatcher, &mut task, 17, input, true);
    x_catalogue.expect("X OS_File keeps the normal result convention");
    assert_owner(&dispatcher);
    let (no_search_catalogue, no_search_context) = call(
        &mut dispatcher,
        &mut task,
        17,
        registers(17, PATH_ADDRESS),
        false,
    );
    no_search_catalogue.expect("reason 17 uses the explicit path with no search");
    assert_eq!(no_search_context.registers[0], 1);

    // Reason 16 loads without path search. A zero low byte of R3 selects R2
    // as destination; result registers report catalogue metadata and length.
    task.memory.write_bytes(SOURCE_ADDRESS, &[0xCC; 8]).unwrap();
    let mut load = registers(16, PATH_ADDRESS);
    load[2] = SOURCE_ADDRESS;
    load[3] = 0;
    let (loaded, load_context) = call(&mut dispatcher, &mut task, 16, load, false);
    loaded.expect("reason 16 loads a fully qualified direct path");
    assert_owner(&dispatcher);
    assert_eq!(task.memory.read_bytes(SOURCE_ADDRESS, 5).unwrap(), b"hello");
    assert_eq!(task.memory.read_byte(SOURCE_ADDRESS + 5).unwrap(), 0xCC);
    assert_eq!(load_context.registers[0], 1);
    assert_eq!(load_context.registers[2], (0xABC << 20) | 0x12345);
    assert_eq!(load_context.registers[3], 0x0102_0304);
    assert_eq!(load_context.registers[4], 5);
    assert_eq!(load_context.registers[5], 0xA5);
    for reason in [16, 255] {
        task.memory.write_bytes(SOURCE_ADDRESS, &[0xCC; 8]).unwrap();
        let mut compatibility_load = registers(reason, PATH_ADDRESS);
        compatibility_load[2] = SOURCE_ADDRESS;
        compatibility_load[3] = 0;
        let (result, result_context) = call(
            &mut dispatcher,
            &mut task,
            reason,
            compatibility_load,
            false,
        );
        result.expect("supported direct-R1 load alias uses hosted path policy");
        assert_eq!(task.memory.read_bytes(SOURCE_ADDRESS, 5).unwrap(), b"hello");
        assert_eq!(result_context.registers[4], 5);
    }

    // Metadata updates preserve unrelated fields and reason 18 changes only
    // the 12-bit file type. Reason 9 applies the default FFD type only once.
    let mut update = registers(1, PATH_ADDRESS);
    update[2] = 0x1234_5678;
    update[3] = 0x8765_4321;
    update[5] = 0x12;
    let (updated, _) = call(&mut dispatcher, &mut task, 1, update, false);
    updated.expect("reason 1 updates load, exec and attributes");
    assert_owner(&dispatcher);
    let metadata = read_metadata(&metadata_path(&environment.root.join("Source"))).unwrap();
    assert_eq!(metadata.file_type, 0x123);
    assert_eq!(metadata.load_address, 0x45678);
    assert_eq!(metadata.execution_address, 0x8765_4321);
    assert_eq!(metadata.attributes, 0x12);
    let mut load_only = registers(2, PATH_ADDRESS);
    load_only[2] = 0xFFE5_4321;
    call(&mut dispatcher, &mut task, 2, load_only, false)
        .0
        .expect("reason 2 changes only load metadata");
    let mut exec_only = registers(3, PATH_ADDRESS);
    exec_only[3] = 0x7654_3210;
    call(&mut dispatcher, &mut task, 3, exec_only, false)
        .0
        .expect("reason 3 changes only execution metadata");
    let mut attributes_only = registers(4, PATH_ADDRESS);
    attributes_only[5] = 0x40;
    call(&mut dispatcher, &mut task, 4, attributes_only, false)
        .0
        .expect("reason 4 changes only attributes");
    let metadata = read_metadata(&metadata_path(&environment.root.join("Source"))).unwrap();
    assert_eq!(metadata.file_type, 0xFFE);
    assert_eq!(metadata.load_address, 0x54321);
    assert_eq!(metadata.execution_address, 0x7654_3210);
    assert_eq!(metadata.attributes, 0x40);
    let (default_type, _) = call(
        &mut dispatcher,
        &mut task,
        9,
        registers(9, PATH_ADDRESS),
        false,
    );
    default_type.expect("reason 9 sets the default file type");
    assert_eq!(
        read_metadata(&metadata_path(&environment.root.join("Source")))
            .unwrap()
            .file_type,
        0xFFE,
        "reason 9 must not replace an already assigned file type"
    );
    let mut set_type = registers(18, PATH_ADDRESS);
    set_type[2] = 0xFEDC_BA98;
    call(&mut dispatcher, &mut task, 18, set_type, false)
        .0
        .expect("reason 18 updates file type");
    assert_eq!(
        read_metadata(&metadata_path(&environment.root.join("Source")))
            .unwrap()
            .file_type,
        0xA98
    );
    fs::write(environment.root.join("NoType"), b"n").unwrap();
    write_metadata(
        &metadata_path(&environment.root.join("NoType")),
        &FileMetadata {
            guest_name: "NoType".into(),
            file_type: 0,
            load_address: 0,
            execution_address: 0,
            attributes: 0,
        },
    )
    .unwrap();
    set_path(&mut task, &environment.guest_path("NoType"));
    call(
        &mut dispatcher,
        &mut task,
        9,
        registers(9, PATH_ADDRESS),
        false,
    )
    .0
    .expect("reason 9 assigns FFD if file type is unset");
    assert_eq!(
        read_metadata(&metadata_path(&environment.root.join("NoType")))
            .unwrap()
            .file_type,
        0xFFD
    );

    // Save reason 0 validates and stages the full caller range before writing.
    task.memory.write_bytes(SOURCE_ADDRESS, b"fresh").unwrap();
    set_path(&mut task, &environment.guest_path("Created"));
    let mut save = registers(0, PATH_ADDRESS);
    save[2] = 0x3456_789A;
    save[3] = 0x1122_3344;
    save[4] = SOURCE_ADDRESS;
    save[5] = SOURCE_ADDRESS + 5;
    call(&mut dispatcher, &mut task, 0, save, false)
        .0
        .expect("reason 0 saves a bounded guest-memory block");
    assert_owner(&dispatcher);
    assert_eq!(
        fs::read(environment.root.join("Created")).unwrap(),
        b"fresh"
    );
    let saved_metadata = read_metadata(&metadata_path(&environment.root.join("Created"))).unwrap();
    assert_eq!(saved_metadata.file_type, 0x345);
    assert_eq!(saved_metadata.load_address, 0x6789A);
    assert_eq!(saved_metadata.execution_address, 0x1122_3344);

    set_path(&mut task, &environment.guest_path("TypedSave"));
    let mut save_type = registers(10, PATH_ADDRESS);
    save_type[2] = 0xBEE;
    save_type[4] = SOURCE_ADDRESS;
    save_type[5] = SOURCE_ADDRESS + 5;
    call(&mut dispatcher, &mut task, 10, save_type, false)
        .0
        .expect("reason 10 saves a block with an explicit file type");
    assert_eq!(
        fs::read(environment.root.join("TypedSave")).unwrap(),
        b"fresh"
    );
    assert_eq!(
        read_metadata(&metadata_path(&environment.root.join("TypedSave")))
            .unwrap()
            .file_type,
        0xBEE
    );

    // Named BASIC SYS uses the same service and caller's public rights as a
    // direct numeric SWI invocation.
    set_path(&mut other, &environment.guest_path("Created"));
    basic_compat::run_source(
        "10 SYS \"OS_File\",17,&2400 TO T%,L%,E%,N%,A%\n20 !&6000=T%\n30 END",
        &mut other,
        &mut dispatcher,
    )
    .expect("named BASIC SYS reaches OS_File");
    assert_owner(&dispatcher);
    assert_eq!(
        u32::from_le_bytes(
            other
                .memory
                .read_bytes(0x6000, 4)
                .unwrap()
                .try_into()
                .unwrap()
        ),
        1
    );
    let (numeric_swi, numeric_context) = call(
        &mut dispatcher,
        &mut other,
        17,
        registers(17, PATH_ADDRESS),
        false,
    );
    numeric_swi.expect("numeric OS_File remains publicly dispatchable");
    assert_owner(&dispatcher);
    assert_eq!(numeric_context.registers[0], 1);

    // Existing create-empty reasons preserve the hosted truncation behavior.
    set_path(&mut task, &environment.guest_path("Created"));
    let mut create = registers(7, PATH_ADDRESS);
    create[2] = 0xABC0_0000;
    create[3] = 0x4455_6677;
    call(&mut dispatcher, &mut task, 7, create, false)
        .0
        .expect("hosted reason 7 creates or truncates a regular file");
    assert!(
        fs::read(environment.root.join("Created"))
            .unwrap()
            .is_empty()
    );
    let created_metadata =
        read_metadata(&metadata_path(&environment.root.join("Created"))).unwrap();
    assert_eq!(created_metadata.file_type, 0xABC);
    assert_eq!(created_metadata.load_address, 0);
    assert_eq!(created_metadata.execution_address, 0x4455_6677);

    set_path(&mut task, &environment.guest_path("Create11"));
    let mut create11 = registers(11, PATH_ADDRESS);
    create11[2] = 0xCDE;
    call(&mut dispatcher, &mut task, 11, create11, false)
        .0
        .expect("reason 11 creates an empty typed file");
    assert!(
        fs::read(environment.root.join("Create11"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        read_metadata(&metadata_path(&environment.root.join("Create11")))
            .unwrap()
            .file_type,
        0xCDE
    );
    set_path(&mut task, &environment.guest_path("DeleteMe"));
    fs::write(environment.root.join("DeleteMe"), b"delete").unwrap();
    call(
        &mut dispatcher,
        &mut task,
        6,
        registers(6, PATH_ADDRESS),
        false,
    )
    .0
    .expect("reason 6 deletes a guest file");
    assert!(!environment.root.join("DeleteMe").exists());
    assert!(!metadata_path(&environment.root.join("DeleteMe")).exists());

    // A locked file and a same-Task open file cannot be mutated by OS_File's
    // destructive save/create/delete reasons.
    let mut locked_metadata = created_metadata.clone();
    locked_metadata.attributes |= 0x08;
    write_metadata(
        &metadata_path(&environment.root.join("Created")),
        &locked_metadata,
    )
    .unwrap();
    let locked_bytes = fs::read(environment.root.join("Created")).unwrap();
    for reason in [0, 6, 7] {
        set_path(&mut task, &environment.guest_path("Created"));
        let mut mutation = registers(reason, PATH_ADDRESS);
        if reason == 0 {
            mutation[4] = SOURCE_ADDRESS;
            mutation[5] = SOURCE_ADDRESS + 5;
        }
        assert!(
            call(&mut dispatcher, &mut task, reason, mutation, false)
                .0
                .is_err(),
            "locked-file mutation reason {reason} must fail"
        );
        assert_eq!(
            fs::read(environment.root.join("Created")).unwrap(),
            locked_bytes
        );
    }
    let mut unlocked = locked_metadata;
    unlocked.attributes &= !0x08;
    write_metadata(&metadata_path(&environment.root.join("Created")), &unlocked).unwrap();

    set_path(&mut task, &environment.guest_path("Created"));
    let mut open_existing = SwiContext::default();
    open_existing.registers[0] = 0x40;
    open_existing.registers[1] = PATH_ADDRESS;
    dispatcher
        .dispatch(OS_FIND, &mut task, &mut open_existing)
        .expect("open file before same-task OS_File mutation checks");
    let open_handle = open_existing.registers[0];
    let before_open_mutations = fs::read(environment.root.join("Created")).unwrap();
    for reason in [0, 6, 7] {
        let mut mutation = registers(reason, PATH_ADDRESS);
        if reason == 0 {
            mutation[4] = SOURCE_ADDRESS;
            mutation[5] = SOURCE_ADDRESS + 5;
        }
        assert!(
            call(&mut dispatcher, &mut task, reason, mutation, false)
                .0
                .is_err(),
            "same-Task open-file mutation reason {reason} must fail"
        );
        assert_eq!(
            fs::read(environment.root.join("Created")).unwrap(),
            before_open_mutations
        );
    }
    let mut close = SwiContext::default();
    close.registers[1] = open_handle;
    dispatcher
        .dispatch(OS_FIND, &mut task, &mut close)
        .expect("close the fixture handle after mutation checks");

    // Save/load are bounded and preflight guest spans before side effects.
    set_path(&mut task, &environment.guest_path("MustNotCreate"));
    let mut bad_save = registers(0, PATH_ADDRESS);
    bad_save[4] = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32 - 2;
    bad_save[5] = bad_save[4] + 5;
    let before_save = fs::read_dir(&environment.root).unwrap().count();
    assert!(
        call(&mut dispatcher, &mut task, 0, bad_save, false)
            .0
            .is_err()
    );
    assert_eq!(
        fs::read_dir(&environment.root).unwrap().count(),
        before_save
    );

    let protected_path = environment.root.join("Created");
    let protected_bytes = fs::read(&protected_path).unwrap();
    let protected_metadata = read_metadata(&metadata_path(&protected_path)).unwrap();
    set_path(&mut task, &environment.guest_path("Created"));
    let mut bad_existing_save = bad_save;
    bad_existing_save[1] = PATH_ADDRESS;
    assert!(
        call(&mut dispatcher, &mut task, 0, bad_existing_save, false)
            .0
            .is_err()
    );
    assert_eq!(fs::read(&protected_path).unwrap(), protected_bytes);
    assert_eq!(
        read_metadata(&metadata_path(&protected_path)).unwrap(),
        protected_metadata
    );

    // Oversized input is rejected without materializing the file in guest RAM.
    let large_path = environment.root.join("TooLarge");
    let file = fs::File::create(&large_path).unwrap();
    file.set_len((MAX_OS_FILE_TRANSFER + 1) as u64).unwrap();
    set_path(&mut task, &environment.guest_path("TooLarge"));
    let mut large_load = registers(16, PATH_ADDRESS);
    large_load[2] = SOURCE_ADDRESS;
    large_load[3] = 0;
    task.memory
        .write_bytes(SOURCE_ADDRESS, &[0xD4; 16])
        .unwrap();
    assert!(
        call(&mut dispatcher, &mut task, 16, large_load, false)
            .0
            .is_err()
    );
    assert_eq!(
        task.memory.read_bytes(SOURCE_ADDRESS, 16).unwrap(),
        [0xD4; 16]
    );

    task.memory
        .write_bytes(PATH_ADDRESS, &[b'B', b'a', 0xFF, 0])
        .unwrap();
    let (invalid_utf8, _) = call(
        &mut dispatcher,
        &mut task,
        17,
        registers(17, PATH_ADDRESS),
        false,
    );
    let invalid_utf8_message = format!("{invalid_utf8:?}");
    assert!(invalid_utf8.is_err());
    assert!(
        !invalid_utf8_message.contains(environment.root.to_string_lossy().as_ref()),
        "guest-facing errors must not leak host paths: {invalid_utf8_message}"
    );

    set_path(&mut task, "HostFS::DemoDisk.$./tmp/ricochet-escape");
    let (host_path_attempt, _) = call(
        &mut dispatcher,
        &mut task,
        7,
        registers(7, PATH_ADDRESS),
        false,
    );
    assert!(
        host_path_attempt.is_err(),
        "OS_File accepts only checked guest paths"
    );

    set_path(&mut task, &environment.guest_path("NewDir"));
    let create_dir_registers = registers(8, PATH_ADDRESS);
    let (created_dir, create_dir_context) =
        call(&mut dispatcher, &mut task, 8, create_dir_registers, false);
    created_dir.expect("reason 8 creates a guest directory");
    assert!(environment.root.join("NewDir").is_dir());
    assert_eq!(create_dir_context.registers[2..6], [0xA5A5_0000; 4]);
    let (catalogue_dir, catalogue_dir_context) = call(
        &mut dispatcher,
        &mut task,
        17,
        registers(17, PATH_ADDRESS),
        false,
    );
    catalogue_dir.expect("reason 17 catalogues a guest directory");
    assert_eq!(catalogue_dir_context.registers[0], 2);
    call(
        &mut dispatcher,
        &mut task,
        6,
        registers(6, PATH_ADDRESS),
        false,
    )
    .0
    .expect("reason 6 deletes a guest directory");
    assert!(!environment.root.join("NewDir").exists());

    // Wildcards are rejected rather than partially applying destructive or
    // metadata operations. The real file and metadata stay intact.
    set_path(&mut task, &environment.guest_path("Created.*"));
    let before = fs::read(environment.root.join("Created")).unwrap();
    assert!(
        call(
            &mut dispatcher,
            &mut task,
            6,
            registers(6, PATH_ADDRESS),
            false
        )
        .0
        .is_err()
    );
    assert_eq!(fs::read(environment.root.join("Created")).unwrap(), before);

    // Direct numeric calls require no elevated caller identity; the service
    // grant is not authority to borrow or mutate a different Task's memory.
    set_path(&mut other, &environment.guest_path("Created"));
    let (other_catalogue, other_context) = call(
        &mut dispatcher,
        &mut other,
        17,
        registers(17, PATH_ADDRESS),
        false,
    );
    other_catalogue.expect("an ordinary Task can use the bounded public service");
    assert_eq!(other.id, 0x0F02);
    assert_eq!(other_context.registers[0], 1);
    assert_eq!(
        task.memory.read_bytes(SOURCE_ADDRESS, 5).unwrap(),
        [0xD4; 5],
        "oversized load rejection leaves its caller destination untouched"
    );

    // Inactive module ownership is fail-closed rather than falling back to
    // the former numeric Rust handler.
    let mut manager = Task::trusted_mos_session(0x0FFF);
    dispatcher
        .basic64_module_manager()
        .quiesce("FileSwitch", &mut manager)
        .expect("trusted manager may quiesce the module");
    let (inactive, _) = call(
        &mut dispatcher,
        &mut task,
        17,
        registers(17, PATH_ADDRESS),
        false,
    );
    assert!(
        matches!(inactive, Err(RuntimeError::Program(ref message)) if message.contains("owning module")),
        "inactive OS_File must not fall through to a Rust numeric handler: {inactive:?}"
    );
}
