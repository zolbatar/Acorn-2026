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
    swi::{
        DisplayEvent, OS_CLI, OS_FIND, OS_FSCONTROL, SwiContext, SwiDispatchRoute, SwiDispatcher,
    },
};

const X_BIT: u32 = 1 << 17;
const CLI_ADDRESS: u32 = 0x2200;
const PATH_A: u32 = 0x2400;
const PATH_B: u32 = 0x2800;
const BUFFER: u32 = 0x3000;
const CONTROL_BLOCK: u32 = GUEST_MEMORY_BASE;

struct Environment {
    root: PathBuf,
    outside_target: PathBuf,
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
        let root =
            std::env::temp_dir().join(format!("ricochet-fscontrol-{}-{nonce}", std::process::id()));
        let outside_target = root.with_extension("outside-target");
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
            outside_target,
            old_volume,
            old_config,
            old_capsule,
        }
    }

    fn guest_path(&self, relative: &str) -> String {
        if relative.is_empty() {
            "HostFS::DemoDisk.$".to_string()
        } else {
            format!("HostFS::DemoDisk.$.{}", relative.replace('/', "."))
        }
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
        let _ = fs::remove_file(&self.outside_target);
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

fn put_c_string(task: &mut Task, address: u32, value: &str) {
    task.memory.write_bytes(address, value.as_bytes()).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn call_fs(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    registers: [u32; 16],
    x_form: bool,
) -> (Result<(), RuntimeError>, SwiContext) {
    let mut context = SwiContext::default();
    context.registers.copy_from_slice(&registers);
    let number = OS_FSCONTROL | if x_form { X_BIT } else { 0 };
    let result = dispatcher.dispatch(number, task, &mut context);
    (result, context)
}

fn registers(reason: u32) -> [u32; 16] {
    let mut registers = [0xA5A5_0000; 16];
    registers[0] = reason;
    registers
}

fn set_fs_string(task: &mut Task, address: u32, value: &str) {
    put_c_string(task, address, value);
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
            }) if *number == OS_FSCONTROL
                && name.eq_ignore_ascii_case("OS_FSControl")
                && module.eq_ignore_ascii_case("FileSwitch")
                && definition.eq_ignore_ascii_case("FileSystemControl")
        ),
        "OS_FSControl did not route through FileSwitch::FileSystemControl: {:?}",
        dispatcher.last_dispatch_route()
    );
}

fn take_display_text(receiver: &mpsc::Receiver<DisplayEvent>) -> String {
    receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(char::from)
        .collect()
}

fn call_cli(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> Result<String, RuntimeError> {
    put_c_string(task, CLI_ADDRESS, command);
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    dispatcher.dispatch(OS_CLI, task, &mut context)?;
    Ok(take_display_text(receiver))
}

fn write_metadata_fixture(path: &std::path::Path, guest_name: &str, file_type: u32) {
    fs::write(path, b"payload").unwrap();
    write_metadata(
        &metadata_path(path),
        &FileMetadata {
            guest_name: guest_name.to_string(),
            file_type,
            load_address: 0x12345,
            execution_address: 0x0102_0304,
            attributes: 0x11,
        },
    )
    .unwrap();
}

#[test]
fn fscontrol_is_module_owned_checked_and_preserves_hosted_contracts() {
    let environment = Environment::new();
    fs::create_dir_all(environment.root.join("A/Sub")).unwrap();
    fs::create_dir_all(environment.root.join("B")).unwrap();
    fs::create_dir_all(environment.root.join("Library")).unwrap();
    fs::write(environment.root.join("Library/FromLib"), b"library").unwrap();
    fs::create_dir_all(environment.root.join("UserRoot")).unwrap();
    write_metadata_fixture(&environment.root.join("A/Original"), "Original", 0xFFF);
    fs::write(environment.root.join("A/Collision"), b"one").unwrap();
    fs::write(environment.root.join("B/Collision"), b"two").unwrap();

    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::new(0x0F31);
    let mut other = Task::new(0x0F32);
    let active_modules = dispatcher
        .module_registry()
        .active_modules_sorted()
        .iter()
        .map(|module| module.manifest.name.clone())
        .collect::<Vec<_>>();
    assert!(
        active_modules
            .iter()
            .any(|name| name.eq_ignore_ascii_case("FileSwitch")),
        "FileSwitch bootstrap is absent; active modules: {active_modules:?}"
    );

    // Directory setters affect only the caller Task and preserve unrelated
    // registers. Reason 0 also updates the previous-directory slot.
    set_fs_string(&mut task, PATH_A, &environment.guest_path("A"));
    let mut set_dir = registers(0);
    set_dir[1] = PATH_A;
    let (set_result, set_context) = call_fs(&mut dispatcher, &mut task, set_dir, false);
    set_result.expect("set CSD");
    assert_owner(&dispatcher);
    assert_eq!(task.file_system.current_directory, ["A"]);
    assert_eq!(task.file_system.previous_directory, Vec::<String>::new());
    assert_eq!(other.file_system.current_directory, Vec::<String>::new());
    assert_eq!(set_context.registers[1], PATH_A);
    assert_eq!(set_context.registers[2], 0xA5A5_0000);

    // Catalogue/examine reasons 5–9 keep directory and wildcard inputs
    // distinct; null library paths resolve through the library state.
    set_fs_string(&mut task, PATH_A, &environment.guest_path("A"));
    let mut catalogue = registers(5);
    catalogue[1] = PATH_A;
    call_fs(&mut dispatcher, &mut task, catalogue, false)
        .0
        .expect("reason 5 catalogue");
    let catalogue_output = take_display_text(&display);
    assert!(catalogue_output.contains("Original"));
    assert!(catalogue_output.contains("Collision"));
    let mut examine = registers(6);
    examine[1] = PATH_A;
    call_fs(&mut dispatcher, &mut task, examine, false)
        .0
        .expect("reason 6 examine");
    assert!(take_display_text(&display).contains("<DIR>"));

    set_fs_string(&mut task, PATH_A, &environment.guest_path("Library"));
    let mut set_library = registers(1);
    set_library[1] = PATH_A;
    let (library_result, library_context) = call_fs(&mut dispatcher, &mut task, set_library, false);
    library_result.expect("set library directory");
    assert_eq!(task.file_system.library_directory, ["Library"]);
    assert_eq!(library_context.registers[1], PATH_A);

    let mut default_library = registers(1);
    default_library[1] = 0;
    call_fs(&mut dispatcher, &mut task, default_library, false)
        .0
        .expect("null library path selects the default Library directory");
    assert_eq!(task.file_system.library_directory, ["Library"]);

    let mut library_catalogue = registers(7);
    library_catalogue[1] = 0;
    call_fs(&mut dispatcher, &mut task, library_catalogue, false)
        .0
        .expect("reason 7 defaults to library directory");
    assert!(take_display_text(&display).contains("FromLib"));
    let mut library_examine = registers(8);
    library_examine[1] = 0;
    call_fs(&mut dispatcher, &mut task, library_examine, false)
        .0
        .expect("reason 8 examines library directory");
    assert!(take_display_text(&display).contains("FromLib"));

    set_fs_string(&mut task, PATH_A, &environment.guest_path("A.Collision"));
    let mut object_examine = registers(9);
    object_examine[1] = PATH_A;
    call_fs(&mut dispatcher, &mut task, object_examine, false)
        .0
        .expect("reason 9 examines wildcarded objects");
    assert!(take_display_text(&display).contains("Collision"));

    set_fs_string(&mut task, PATH_A, &environment.guest_path("UserRoot"));
    let mut set_urd = registers(39);
    set_urd[1] = PATH_A;
    call_fs(&mut dispatcher, &mut task, set_urd, false)
        .0
        .expect("set user root");
    assert_eq!(task.file_system.user_root, ["UserRoot"]);

    let swap = registers(40);
    call_fs(&mut dispatcher, &mut task, swap, false)
        .0
        .expect("swap current and previous directories");
    assert_eq!(task.file_system.current_directory, Vec::<String>::new());
    assert_eq!(task.file_system.previous_directory, ["A"]);
    assert_eq!(other.file_system.current_directory, Vec::<String>::new());

    for (reason, expected_field) in [(43, 0), (44, 1), (45, 2)] {
        let clear = registers(reason);
        call_fs(&mut dispatcher, &mut task, clear, false)
            .0
            .unwrap_or_else(|error| panic!("reason {reason}: {error}"));
        match expected_field {
            0 => assert!(task.file_system.current_directory.is_empty()),
            1 => assert!(task.file_system.user_root.is_empty()),
            _ => assert!(task.file_system.library_directory.is_empty()),
        }
    }
    let mut null_current = registers(0);
    null_current[1] = 0;
    call_fs(&mut dispatcher, &mut task, null_current, false)
        .0
        .expect("null current-directory path selects root");
    assert!(task.file_system.current_directory.is_empty());

    // Filing-system lookup returns a bounded logical control-block identity,
    // never a native host pointer. Numeric and textual selectors are both
    // supported; unknown identities return zero without changing the store.
    let mut lookup_numeric = registers(13);
    lookup_numeric[1] = 1;
    let (numeric_result, numeric_context) =
        call_fs(&mut dispatcher, &mut task, lookup_numeric, false);
    numeric_result.expect("numeric HostFS lookup");
    assert_eq!(numeric_context.registers[0], 13);
    assert_eq!(numeric_context.registers[1], 1);
    assert_eq!(numeric_context.registers[2], CONTROL_BLOCK);
    assert!(numeric_context.registers[2] < GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32);

    put_c_string(&mut task, PATH_A, "HostFS#suffix");
    let mut lookup_name = registers(13);
    lookup_name[1] = PATH_A;
    lookup_name[2] = 0;
    let (named_result, named_context) = call_fs(&mut dispatcher, &mut task, lookup_name, true);
    named_result.expect("X-form textual HostFS lookup");
    assert!(!named_context.overflow);
    assert_eq!(named_context.registers[1], 1);
    assert_eq!(named_context.registers[2], CONTROL_BLOCK);

    put_c_string(&mut task, PATH_A, "HostFS#suffix");
    let mut control_terminator_only = registers(13);
    control_terminator_only[1] = PATH_A;
    control_terminator_only[2] = 1;
    assert!(
        call_fs(&mut dispatcher, &mut task, control_terminator_only, false)
            .0
            .is_err()
    );

    let mut unknown = registers(13);
    unknown[1] = 2;
    let (unknown_result, unknown_context) = call_fs(&mut dispatcher, &mut task, unknown, false);
    unknown_result.expect("unknown numeric filesystem is a not-found result");
    assert_eq!(unknown_context.registers[1], 2);
    assert_eq!(unknown_context.registers[2], 0);

    put_c_string(&mut task, PATH_A, "HostFS#");
    let mut control_only = registers(13);
    control_only[1] = PATH_A;
    control_only[2] = 1;
    assert!(
        call_fs(&mut dispatcher, &mut task, control_only, false)
            .0
            .is_err()
    );

    // R14 selects by number/name and clears both selectors when R1 is zero.
    let mut select_number = registers(14);
    select_number[1] = 1;
    let (select_result, select_context) = call_fs(&mut dispatcher, &mut task, select_number, false);
    select_result.expect("select HostFS by number");
    assert_eq!(select_context.registers, select_number);
    assert!(
        task.file_system
            .current_file_system
            .eq_ignore_ascii_case("HostFS")
    );
    assert!(
        task.file_system
            .temporary_file_system
            .eq_ignore_ascii_case("HostFS")
    );

    put_c_string(&mut task, PATH_A, "HostFS");
    let mut select_by_name = registers(14);
    select_by_name[1] = PATH_A;
    let (select_name_result, select_name_context) =
        call_fs(&mut dispatcher, &mut task, select_by_name, false);
    select_name_result.expect("select HostFS by name");
    assert_eq!(select_name_context.registers, select_by_name);
    let mut missing_fs = registers(14);
    missing_fs[1] = 2;
    assert!(
        call_fs(&mut dispatcher, &mut task, missing_fs, false)
            .0
            .is_err()
    );
    assert!(
        task.file_system
            .current_file_system
            .eq_ignore_ascii_case("HostFS")
    );
    assert!(
        task.file_system
            .temporary_file_system
            .eq_ignore_ascii_case("HostFS")
    );

    task.file_system.current_file_system = "HostFS".to_string();
    task.file_system.temporary_file_system = "HostFS".to_string();
    let mut clear_fs = registers(14);
    clear_fs[1] = 0;
    call_fs(&mut dispatcher, &mut task, clear_fs, false)
        .0
        .expect("clear current and temporary filing systems");
    assert!(task.file_system.current_file_system.is_empty());
    assert!(task.file_system.temporary_file_system.is_empty());
    assert_eq!(other.file_system.current_file_system, "HostFS");

    let mut reselect = registers(14);
    reselect[1] = 1;
    call_fs(&mut dispatcher, &mut task, reselect, false)
        .0
        .expect("reselect HostFS after testing clear");

    // Temporary selection via a filename prefix is independently reversible
    // and remains Task-local.
    set_fs_string(&mut task, PATH_A, &environment.guest_path("A/Original"));
    let mut temp_select = registers(11);
    temp_select[1] = PATH_A;
    let (temp_result, temp_context) = call_fs(&mut dispatcher, &mut task, temp_select, false);
    temp_result.expect("select temporary filing system from prefix");
    assert_eq!(temp_context.registers[0], temp_select[0]);
    assert_eq!(temp_context.registers[1], PATH_A + 7);
    assert_eq!(temp_context.registers[2], 1);
    assert_eq!(temp_context.registers[3], 0);
    assert_eq!(temp_context.registers[4..6], temp_select[4..6]);
    assert!(
        task.file_system
            .temporary_file_system
            .eq_ignore_ascii_case("HostFS")
    );
    assert_eq!(other.file_system.temporary_file_system, "HostFS");
    call_fs(&mut dispatcher, &mut task, registers(19), false)
        .0
        .expect("restore temporary filesystem");
    assert_eq!(
        task.file_system.temporary_file_system,
        task.file_system.current_file_system
    );

    // Reason 18 and 31 round-trip the supported named/numeric type spellings
    // while preserving unrelated registers.
    let mut file_type_name = registers(18);
    file_type_name[2] = 0xFFF;
    let (_, type_name_context) = call_fs(&mut dispatcher, &mut task, file_type_name, false);
    assert_eq!(&type_name_context.registers[2].to_le_bytes(), b"Text");
    assert_eq!(&type_name_context.registers[3].to_le_bytes(), b"    ");
    assert_eq!(type_name_context.registers[0], 18);
    put_c_string(&mut task, PATH_A, "BASIC64");
    let mut parse_type = registers(31);
    parse_type[1] = PATH_A;
    let (parse_result, parse_context) = call_fs(&mut dispatcher, &mut task, parse_type, false);
    parse_result.expect("parse named file type");
    assert_eq!(parse_context.registers[0], 31);
    assert_eq!(parse_context.registers[1], PATH_A);
    assert_eq!(parse_context.registers[2], 0x064);

    // Reason 33 returns the filesystem name atomically, with a NUL and
    // preserved inputs. Unknown selectors return the empty string.
    let mut fs_name_buffer = [0xCC; 16];
    task.memory.write_bytes(BUFFER, &fs_name_buffer).unwrap();
    let mut fs_name = registers(33);
    fs_name[1] = 1;
    fs_name[2] = BUFFER;
    fs_name[3] = 7;
    let (fs_name_result, fs_name_context) = call_fs(&mut dispatcher, &mut task, fs_name, false);
    fs_name_result.expect("read filesystem name");
    assert_eq!(task.memory.read_bytes(BUFFER, 7).unwrap(), b"HOSTFS\0");
    assert_eq!(fs_name_context.registers[..4], fs_name[..4]);

    fs_name[3] = u32::MAX;
    call_fs(&mut dispatcher, &mut task, fs_name, false)
        .0
        .expect("U32 capacity validates/writes only the actual filesystem name span");
    assert_eq!(task.memory.read_bytes(BUFFER, 7).unwrap(), b"HOSTFS\0");

    fs_name_buffer.fill(0xCC);
    task.memory.write_bytes(BUFFER, &fs_name_buffer).unwrap();
    fs_name[3] = 6;
    assert!(
        call_fs(&mut dispatcher, &mut task, fs_name, false)
            .0
            .is_err()
    );
    assert_eq!(task.memory.read_bytes(BUFFER, 16).unwrap(), fs_name_buffer);

    task.memory.write_bytes(BUFFER, &fs_name_buffer).unwrap();
    let mut unknown_fs_name = registers(33);
    unknown_fs_name[1] = 2;
    unknown_fs_name[2] = BUFFER;
    unknown_fs_name[3] = 1;
    let (unknown_name_result, unknown_name_context) =
        call_fs(&mut dispatcher, &mut task, unknown_fs_name, false);
    unknown_name_result.expect("unknown filesystem name produces empty string");
    assert_eq!(unknown_name_context.registers[..4], unknown_fs_name[..4]);
    assert_eq!(task.memory.read_byte(BUFFER).unwrap(), 0);
    assert_eq!(task.memory.read_bytes(BUFFER + 1, 15).unwrap(), [0xCC; 15]);

    // Reason 37's first pass reports the signed-in-U32 deficit without
    // partial output; exact-fit and spare-buffer passes include the NUL.
    set_fs_string(&mut task, PATH_A, &environment.guest_path("A/Original"));
    let canonical = environment.guest_path("A/Original");
    let mut output = [0xB6; 96];
    task.memory.write_bytes(BUFFER, &output).unwrap();
    let canonical_len = canonical.len() as u32;
    let mut canonical_call = registers(37);
    canonical_call[1] = PATH_A;
    canonical_call[2] = BUFFER;
    canonical_call[3] = 0;
    canonical_call[4] = 0;
    canonical_call[5] = canonical_len;
    let (size_result, size_context) = call_fs(&mut dispatcher, &mut task, canonical_call, false);
    size_result.expect("canonical-name sizing pass");
    assert_eq!(size_context.registers[5], 0);
    assert_eq!(
        task.memory.read_bytes(BUFFER, output.len()).unwrap(),
        output
    );

    canonical_call[5] = 0;
    let (zero_capacity_result, zero_capacity_context) =
        call_fs(&mut dispatcher, &mut task, canonical_call, false);
    zero_capacity_result.expect("zero-capacity canonical probe");
    assert_eq!(
        zero_capacity_context.registers[5],
        (-(canonical_len as i32)) as u32
    );
    assert_eq!(
        task.memory.read_bytes(BUFFER, output.len()).unwrap(),
        output
    );

    let end_buffer = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32 - 2;
    task.memory.write_bytes(end_buffer, &[0xE1, 0xE2]).unwrap();
    canonical_call[2] = end_buffer;
    canonical_call[5] = canonical_len + 1;
    assert!(
        call_fs(&mut dispatcher, &mut task, canonical_call, false)
            .0
            .is_err()
    );
    assert_eq!(task.memory.read_bytes(end_buffer, 2).unwrap(), [0xE1, 0xE2]);
    canonical_call[2] = BUFFER;

    canonical_call[5] = canonical_len + 1;
    let (exact_result, exact_context) = call_fs(&mut dispatcher, &mut task, canonical_call, false);
    exact_result.expect("canonical-name exact-fit pass");
    assert_eq!(exact_context.registers[5], 1);
    assert_eq!(
        task.memory
            .read_bytes(BUFFER, canonical_len as usize + 1)
            .unwrap(),
        [canonical.as_bytes(), &[0]].concat()
    );

    canonical_call[5] = u32::MAX;
    let (large_capacity_result, large_capacity_context) =
        call_fs(&mut dispatcher, &mut task, canonical_call, false);
    large_capacity_result.expect("U32 canonical capacity writes only the actual string span");
    assert_eq!(
        large_capacity_context.registers[5],
        u32::MAX - canonical_len
    );
    assert_eq!(
        task.memory
            .read_bytes(BUFFER, canonical_len as usize + 1)
            .unwrap(),
        [canonical.as_bytes(), &[0]].concat()
    );

    output.fill(0xB6);
    task.memory.write_bytes(BUFFER, &output).unwrap();
    canonical_call[5] = canonical_len - 2;
    let (short_result, short_context) = call_fs(&mut dispatcher, &mut task, canonical_call, false);
    short_result.expect("short canonical-name probe");
    assert_eq!(short_context.registers[5], (-2i32) as u32);
    assert_eq!(
        task.memory.read_bytes(BUFFER, output.len()).unwrap(),
        output
    );

    // Rename preserves payload and updates the existing metadata sidecar;
    // destination conflicts and sandbox escapes are mutation-atomic.
    set_fs_string(&mut task, PATH_A, &environment.guest_path("A/Original"));
    set_fs_string(&mut task, PATH_B, &environment.guest_path("A/Renamed"));
    let mut rename = registers(25);
    rename[1] = PATH_A;
    rename[2] = PATH_B;
    let (rename_result, rename_context) = call_fs(&mut dispatcher, &mut task, rename, false);
    rename_result.expect("rename payload and metadata");
    assert_eq!(rename_context.registers, rename);
    assert!(!environment.root.join("A/Original").exists());
    assert_eq!(
        fs::read(environment.root.join("A/Renamed")).unwrap(),
        b"payload"
    );
    let renamed_metadata =
        read_metadata(&metadata_path(&environment.root.join("A/Renamed"))).unwrap();
    assert_eq!(renamed_metadata.guest_name, "Renamed");
    assert_eq!(renamed_metadata.file_type, 0xFFF);

    fs::write(environment.root.join("A/Exists"), b"kept").unwrap();
    set_fs_string(&mut task, PATH_A, &environment.guest_path("A/Renamed"));
    set_fs_string(&mut task, PATH_B, &environment.guest_path("A/Exists"));
    rename[1] = PATH_A;
    rename[2] = PATH_B;
    assert!(
        call_fs(&mut dispatcher, &mut task, rename, false)
            .0
            .is_err()
    );
    assert_eq!(
        fs::read(environment.root.join("A/Renamed")).unwrap(),
        b"payload"
    );
    assert_eq!(
        fs::read(environment.root.join("A/Exists")).unwrap(),
        b"kept"
    );

    set_fs_string(&mut task, PATH_A, "HostFS::DemoDisk.$.A/../../escape");
    set_fs_string(&mut task, PATH_B, &environment.guest_path("A/Escaped"));
    rename[1] = PATH_A;
    rename[2] = PATH_B;
    assert!(
        call_fs(&mut dispatcher, &mut task, rename, false)
            .0
            .is_err()
    );
    assert!(!environment.root.join("A/Escaped").exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let outside = &environment.outside_target;
        fs::write(outside, b"outside").unwrap();
        symlink(outside, environment.root.join("A/ExternalLink")).unwrap();
        set_fs_string(&mut task, PATH_A, &environment.guest_path("A/ExternalLink"));
        set_fs_string(
            &mut task,
            PATH_B,
            &environment.guest_path("A/ExternalMoved"),
        );
        rename[1] = PATH_A;
        rename[2] = PATH_B;
        assert!(
            call_fs(&mut dispatcher, &mut task, rename, false)
                .0
                .is_err()
        );
        assert_eq!(fs::read(outside).unwrap(), b"outside");
        assert!(!environment.root.join("A/ExternalMoved").exists());
        fs::remove_file(outside).unwrap();
    }

    // A volume rename is a controlled mutation of this temporary fixture;
    // it does not rewrite the Task's directory state or touch outside data.
    set_fs_string(&mut task, PATH_A, &environment.guest_path("MissingObject"));
    set_fs_string(&mut task, PATH_B, "MustNotApply");
    let mut missing_volume_object = registers(50);
    missing_volume_object[1] = PATH_A;
    missing_volume_object[2] = PATH_B;
    assert!(
        call_fs(&mut dispatcher, &mut task, missing_volume_object, false)
            .0
            .is_err()
    );
    let renamed_object_canonical = canonical.replace("A.Original", "A.Renamed");
    set_fs_string(&mut task, PATH_A, &renamed_object_canonical);
    canonical_call[1] = PATH_A;
    canonical_call[5] = canonical_len + 1;
    call_fs(&mut dispatcher, &mut task, canonical_call, false)
        .0
        .expect("missing reason50 object leaves volume name unchanged");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER, renamed_object_canonical.len() + 1)
            .unwrap(),
        [renamed_object_canonical.as_bytes(), &[0]].concat()
    );
    assert_eq!(
        task.memory
            .read_byte(BUFFER + renamed_object_canonical.len() as u32 + 1)
            .unwrap(),
        0xB6
    );

    set_fs_string(&mut task, PATH_A, &renamed_object_canonical);
    set_fs_string(&mut task, PATH_B, "RenamedDisk");
    let mut rename_volume = registers(50);
    rename_volume[1] = PATH_A;
    rename_volume[2] = PATH_B;
    let current_dir_before = task.file_system.current_directory.clone();
    call_fs(&mut dispatcher, &mut task, rename_volume, false)
        .0
        .expect("rename test volume through an existing guest object");
    assert_eq!(task.file_system.current_directory, current_dir_before);
    let renamed_volume_path = canonical
        .replace("DemoDisk", "RenamedDisk")
        .replace("A.Original", "A.Renamed");
    set_fs_string(&mut task, PATH_A, &renamed_volume_path);
    canonical_call[1] = PATH_A;
    canonical_call[5] = canonical_len + 1;
    let (renamed_canonical_result, renamed_canonical) =
        call_fs(&mut dispatcher, &mut task, canonical_call, false);
    renamed_canonical_result.expect("canonicalize path after volume rename");
    let renamed_name = renamed_volume_path.clone();
    assert_eq!(
        renamed_canonical.registers[5],
        (canonical_len as i64 + 1 - renamed_name.len() as i64) as i32 as u32
    );
    canonical_call[5] = renamed_name.len() as u32 + 1;
    let (_, renamed_exact) = call_fs(&mut dispatcher, &mut task, canonical_call, false);
    assert_eq!(renamed_exact.registers[5], 1);
    assert_eq!(
        task.memory
            .read_bytes(BUFFER, renamed_name.len() + 1)
            .unwrap(),
        [renamed_name.as_bytes(), &[0]].concat()
    );
    let other_canonical_name = renamed_name.replace("A.Renamed", "B.Collision");
    put_c_string(&mut other, PATH_A, &other_canonical_name);
    let mut other_canonical_call = registers(37);
    other_canonical_call[1] = PATH_A;
    other_canonical_call[2] = BUFFER;
    other_canonical_call[3] = 0;
    other_canonical_call[4] = 0;
    other_canonical_call[5] = other_canonical_name.len() as u32 + 1;
    let (other_canonical_result, other_canonical_context) =
        call_fs(&mut dispatcher, &mut other, other_canonical_call, false);
    other_canonical_result.unwrap_or_else(|error| {
        panic!(
            "renamed volume name is visible across tasks (path bytes={}, input R5={}, registers={:?}): {error}",
            other_canonical_name.len(),
            other_canonical_call[5],
            other_canonical_context.registers
        )
    });
    assert_eq!(other.file_system.current_directory, Vec::<String>::new());
    assert!(
        String::from_utf8(
            other
                .memory
                .read_bytes(BUFFER, other_canonical_name.len())
                .unwrap()
        )
        .unwrap()
        .contains("RenamedDisk")
    );

    set_fs_string(&mut task, PATH_A, &renamed_volume_path);
    set_fs_string(&mut task, PATH_B, "invalid/name");
    rename_volume[2] = PATH_B;
    let (invalid_rename_result, invalid_rename_context) =
        call_fs(&mut dispatcher, &mut task, rename_volume, false);
    assert!(
        invalid_rename_result.is_err(),
        "invalid volume name should be rejected (R0={}, R1={}, R2={}, registers={:?}, object={:?}, name={:?})",
        rename_volume[0],
        rename_volume[1],
        rename_volume[2],
        invalid_rename_context.registers,
        task.memory
            .read_bytes(PATH_A, renamed_volume_path.len() + 1),
        task.memory.read_bytes(PATH_B, 12),
    );
    set_fs_string(&mut task, PATH_A, &renamed_volume_path);
    canonical_call[1] = PATH_A;
    let (_, unchanged_volume) = call_fs(&mut dispatcher, &mut task, canonical_call, false);
    assert_eq!(unchanged_volume.registers[5], 1);

    // CLI bridges use the same public FileSwitch service routes.
    call_cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*DIR HostFS::RenamedDisk.$.A",
    )
    .expect("CLI DIR bridge");
    assert_eq!(task.file_system.current_directory, ["A"]);
    let catalog_output = call_cli(&mut dispatcher, &mut task, &display, "*CAT").unwrap();
    assert!(catalog_output.contains("Renamed"));
    assert!(matches!(
        dispatcher.last_dispatch_route(),
        Some(SwiDispatchRoute::ModuleOwned { number, name, .. })
            if *number == OS_CLI && name.eq_ignore_ascii_case("OS_CLI")
    ));
    call_cli(&mut dispatcher, &mut task, &display, "*HOSTFS").expect("CLI HOSTFS bridge");
    assert_eq!(task.file_system.current_file_system, "HOSTFS");
    call_cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*RENAME HostFS::RenamedDisk.$.A.Renamed HostFS::RenamedDisk.$.A.CliRenamed",
    )
    .expect("CLI RENAME bridge");
    assert_eq!(
        fs::read(environment.root.join("A/CliRenamed")).unwrap(),
        b"payload"
    );
    assert_eq!(
        read_metadata(&metadata_path(&environment.root.join("A/CliRenamed")))
            .unwrap()
            .guest_name,
        "CliRenamed"
    );

    call_cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*FILETYPE HostFS::RenamedDisk.$.A.CliRenamed Text",
    )
    .expect("CLI FILETYPE bridge");
    assert_eq!(
        read_metadata(&metadata_path(&environment.root.join("A/CliRenamed")))
            .unwrap()
            .file_type,
        0xFFF
    );

    // OS_FSControl 22 closes every channel owned by the caller but cannot
    // close a different Task's handles.
    let cli_renamed_volume_path = renamed_volume_path.replace("A.Renamed", "A.CliRenamed");
    set_fs_string(&mut task, PATH_A, &cli_renamed_volume_path);
    set_fs_string(&mut other, PATH_A, &cli_renamed_volume_path);
    let mut task_open = SwiContext::default();
    task_open.registers[0] = 0x40;
    task_open.registers[1] = PATH_A;
    dispatcher
        .dispatch(OS_FIND, &mut task, &mut task_open)
        .unwrap();
    let mut other_open = SwiContext::default();
    other_open.registers[0] = 0x40;
    other_open.registers[1] = PATH_A;
    dispatcher
        .dispatch(OS_FIND, &mut other, &mut other_open)
        .unwrap();
    assert!(!task.file_system.open_files.is_empty());
    assert!(!other.file_system.open_files.is_empty());
    let (close_result, close_context) = call_fs(&mut dispatcher, &mut task, registers(22), false);
    close_result.expect("reason 22 close-all");
    assert_eq!(close_context.registers[0], 22);
    assert!(task.file_system.open_files.is_empty());
    assert!(!other.file_system.open_files.is_empty());

    // Named BASIC SYS uses the same module-owned endpoint. Numeric SYS is not
    // part of the current BASIC syntax; numeric direct SWI and X dispatch are.
    basic_compat::run_source(
        "10 SYS \"OS_FSControl\",18,&FFF TO A%,B%\n20 END",
        &mut task,
        &mut dispatcher,
    )
    .expect("named BASIC SYS reaches FileSystemControl");
    assert_owner(&dispatcher);

    // X errors use the standard error block. Inactive ownership fails closed
    // instead of reviving the previous numeric Rust implementation.
    let mut bad_pointer = registers(37);
    bad_pointer[1] = u32::MAX;
    bad_pointer[2] = BUFFER;
    bad_pointer[5] = 10;
    let (_, x_error) = call_fs(&mut dispatcher, &mut task, bad_pointer, true);
    assert!(x_error.overflow);
    assert!(
        !String::from_utf8_lossy(&task.memory.read_bytes(x_error.registers[0], 64).unwrap())
            .contains(environment.root.to_string_lossy().as_ref())
    );

    let mut manager = Task::trusted_mos_session(0x0F3F);
    dispatcher
        .basic64_module_manager()
        .quiesce("FileSwitch", &mut manager)
        .expect("trusted manager can quiesce FileSwitch");
    let (inactive, _) = call_fs(&mut dispatcher, &mut task, registers(14), false);
    assert!(matches!(
        inactive,
        Err(RuntimeError::Program(ref message)) if message.contains("owning module")
    ));
}
