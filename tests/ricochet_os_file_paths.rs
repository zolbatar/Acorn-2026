use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    filesystem::{FileMetadata, metadata_path, write_metadata},
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, OS_FILE, OS_SET_VAR_VAL, SwiContext, SwiDispatcher},
};

const X_BIT: u32 = 1 << 17;
const OBJECT_ADDRESS: u32 = 0x2400;
const PATH_LIST_ADDRESS: u32 = 0x2800;
const PATH_NAME_ADDRESS: u32 = 0x2C00;
const VARIABLE_NAME_ADDRESS: u32 = 0x3000;
const VARIABLE_VALUE_ADDRESS: u32 = 0x3800;
const LOAD_ADDRESS: u32 = 0x5000;

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
            "ricochet-os-file-paths-{}-{nonce}",
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

    fn guest_prefix(&self, directory: &str) -> String {
        format!("HostFS::DemoDisk.$.{directory}.")
    }

    fn guest_path(&self, leaf: &str) -> String {
        format!("HostFS::DemoDisk.$.{leaf}")
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

fn c_string(task: &mut Task, address: u32, bytes: &[u8]) {
    task.memory.write_bytes(address, bytes).unwrap();
    task.memory
        .write_byte(address + bytes.len() as u32, 0)
        .unwrap();
}

fn control_string(task: &mut Task, address: u32, bytes: &[u8]) {
    task.memory.write_bytes(address, bytes).unwrap();
    task.memory
        .write_byte(address + bytes.len() as u32, b'\r')
        .unwrap();
}

fn set_variable(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    value: &[u8],
    variable_type: u32,
) -> Result<(), RuntimeError> {
    c_string(task, VARIABLE_NAME_ADDRESS, name.as_bytes());
    task.memory
        .write_bytes(VARIABLE_VALUE_ADDRESS, value)
        .unwrap();
    if variable_type == 0 {
        task.memory
            .write_byte(VARIABLE_VALUE_ADDRESS + value.len() as u32, 0)
            .unwrap();
    }
    let mut context = SwiContext::default();
    context.registers[0] = VARIABLE_NAME_ADDRESS;
    context.registers[1] = VARIABLE_VALUE_ADDRESS;
    context.registers[2] = value.len() as u32;
    context.registers[4] = variable_type;
    dispatcher.dispatch(OS_SET_VAR_VAL, task, &mut context)
}

fn dispatch_file(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    reason: u32,
    filename: &[u8],
    path_argument: Option<(u32, u32)>,
    load_buffer: Option<u32>,
    x_form: bool,
) -> (Result<(), RuntimeError>, SwiContext) {
    c_string(task, OBJECT_ADDRESS, filename);
    let mut context = SwiContext::default();
    context.registers[0] = reason;
    context.registers[1] = OBJECT_ADDRESS;
    if let Some((r4, address)) = path_argument {
        context.registers[r4 as usize] = address;
    }
    if let Some(buffer) = load_buffer {
        context.registers[2] = buffer;
        context.registers[3] = 0;
    }
    let number = OS_FILE | if x_form { X_BIT } else { 0 };
    let result = dispatcher.dispatch(number, task, &mut context);
    (result, context)
}

fn make_file(
    environment: &Environment,
    directory: &str,
    leaf: &str,
    bytes: &[u8],
    file_type: u32,
    execution_address: u32,
) {
    let directory_path = environment.root.join(directory);
    fs::create_dir_all(&directory_path).unwrap();
    let path = directory_path.join(leaf);
    fs::write(&path, bytes).unwrap();
    write_metadata(
        &metadata_path(&path),
        &FileMetadata {
            guest_name: leaf.to_string(),
            file_type,
            load_address: 0x12345,
            execution_address,
            attributes: (file_type & 0xFF) | 1,
        },
    )
    .unwrap();
}

fn assert_catalogue(context: &SwiContext, file_type: u32, execution: u32, length: u32) {
    assert_eq!(context.registers[0], 1);
    assert_eq!(context.registers[2], (file_type << 20) | 0x12345);
    assert_eq!(context.registers[3], execution);
    assert_eq!(context.registers[4], length);
    assert_eq!(context.registers[5], (file_type & 0xFF) | 1);
}

fn x_error_text(task: &Task, address: u32) -> String {
    let mut bytes = Vec::new();
    for offset in 0..256u32 {
        let byte = task.memory.read_byte(address + offset).unwrap();
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn os_file_search_reasons_honor_path_string_variable_and_files_path() {
    let environment = Environment::new();
    make_file(
        &environment,
        "First",
        "Shared",
        b"FIRST",
        0x111,
        0x1111_1111,
    );
    make_file(
        &environment,
        "Second",
        "Shared",
        b"SECOND",
        0x222,
        0x2222_2222,
    );
    make_file(
        &environment,
        "First",
        "Unreadable",
        b"NO-READ",
        0x101,
        0x1010_1010,
    );
    let unreadable_path = environment.root.join("First/Unreadable");
    let mut unreadable_metadata =
        ricochet::filesystem::read_metadata(&metadata_path(&unreadable_path)).unwrap();
    unreadable_metadata.attributes &= !1;
    write_metadata(&metadata_path(&unreadable_path), &unreadable_metadata).unwrap();
    make_file(
        &environment,
        "First",
        "PublicReadOnly",
        b"PUBLIC",
        0x110,
        0x1110_1010,
    );
    let public_read_path = environment.root.join("First/PublicReadOnly");
    let mut public_read_metadata =
        ricochet::filesystem::read_metadata(&metadata_path(&public_read_path)).unwrap();
    public_read_metadata.attributes = 1 << 4;
    write_metadata(&metadata_path(&public_read_path), &public_read_metadata).unwrap();
    make_file(
        &environment,
        "Second",
        "Unreadable",
        b"READABLE",
        0x102,
        0x1020_1020,
    );
    make_file(
        &environment,
        "Second",
        "SecondOnly",
        b"LATER",
        0x333,
        0x3333_3333,
    );
    make_file(
        &environment,
        "First",
        "DirectoryWins",
        b"not-a-dir",
        0x444,
        0x4444_4444,
    );
    fs::remove_file(environment.root.join("First/DirectoryWins")).unwrap();
    fs::remove_file(metadata_path(&environment.root.join("First/DirectoryWins"))).unwrap();
    fs::create_dir(environment.root.join("First/DirectoryWins")).unwrap();
    make_file(
        &environment,
        "Second",
        "DirectoryWins",
        b"SECOND-FILE",
        0x555,
        0x5555_5555,
    );
    make_file(
        &environment,
        "",
        "CurrentOnly",
        b"CURRENT",
        0x666,
        0x6666_6666,
    );

    let (mut dispatcher, _display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0F11);
    let baseline_dynamic_areas = task.memory.dynamic_area_count();

    let two_candidates = format!(
        "{},{}",
        environment.guest_prefix("First"),
        environment.guest_prefix("Second")
    );
    control_string(&mut task, PATH_LIST_ADDRESS, two_candidates.as_bytes());

    // Reason 13 uses the explicit R4 path string, honors candidate order,
    // and returns the matched object's metadata with standard R0/R2-R5.
    let (first_catalogue, first_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        13,
        b"Shared",
        Some((4, PATH_LIST_ADDRESS)),
        None,
        false,
    );
    first_catalogue.expect("reason 13 searches the first candidate");
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);
    assert_catalogue(&first_context, 0x111, 0x1111_1111, 5);

    // A NotFound candidate advances to the next prefix; a matching directory
    // does not. This catches both accidental R4 ignoring and skip-on-error.
    let fallback = format!(
        "{},{}",
        environment.guest_prefix("NoSuchDirectory"),
        environment.guest_prefix("Second")
    );
    control_string(&mut task, PATH_LIST_ADDRESS, fallback.as_bytes());
    let (later_catalogue, later_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        13,
        b"SecondOnly",
        Some((4, PATH_LIST_ADDRESS)),
        None,
        false,
    );
    later_catalogue.expect("NotFound advances to the next search candidate");
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);
    assert_catalogue(&later_context, 0x333, 0x3333_3333, 5);

    control_string(&mut task, PATH_LIST_ADDRESS, two_candidates.as_bytes());
    let (directory_catalogue, directory_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        13,
        b"DirectoryWins",
        Some((4, PATH_LIST_ADDRESS)),
        None,
        false,
    );
    directory_catalogue.expect("first matching directory is returned, not skipped");
    assert_eq!(directory_context.registers[0], 2);
    let destination = [0xA7; 24];
    task.memory.write_bytes(LOAD_ADDRESS, &destination).unwrap();
    let (directory_load, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        12,
        b"DirectoryWins",
        Some((4, PATH_LIST_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    assert!(
        directory_load.is_err(),
        "load does not skip a matched directory"
    );
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);
    assert_eq!(
        task.memory.read_bytes(LOAD_ADDRESS, 24).unwrap(),
        destination
    );

    // Reason 12 uses the same explicit path string and loads the first file's
    // exact bytes and metadata into the requested logical buffer.
    let (path_load, path_load_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        12,
        b"Shared",
        Some((4, PATH_LIST_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    path_load.expect("reason 12 loads through R4's path string");
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 5).unwrap(), b"FIRST");
    assert_eq!(path_load_context.registers[4], 5);
    assert_eq!(path_load_context.registers[3], 0x1111_1111);
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);

    // Fully qualified object names bypass even explicit path-string search
    // lists for both catalogue and load reasons.
    let (qualified_catalogue, qualified_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        13,
        environment.guest_path("Second.Shared").as_bytes(),
        Some((4, PATH_LIST_ADDRESS)),
        None,
        false,
    );
    qualified_catalogue.expect("qualified reason 13 bypasses its R4 search list");
    assert_catalogue(&qualified_context, 0x222, 0x2222_2222, 6);
    let (qualified_load, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        12,
        environment.guest_path("Second.Shared").as_bytes(),
        Some((4, PATH_LIST_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    qualified_load.expect("qualified reason 12 bypasses its R4 search list");
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 6).unwrap(), b"SECOND");

    // Path-variable reasons resolve the variable name at R4 on every call;
    // changes are live, and type 0 and LiteralString(4) are both raw values.
    c_string(&mut task, PATH_NAME_ADDRESS, b"Test$Path");
    set_variable(
        &mut dispatcher,
        &mut task,
        "Test$Path",
        format!("{}", environment.guest_prefix("First")).as_bytes(),
        0,
    )
    .expect("set String(0) path variable");
    let (string_catalogue, string_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        15,
        b"Shared",
        Some((4, PATH_NAME_ADDRESS)),
        None,
        false,
    );
    string_catalogue.expect("reason 15 resolves String(0) path variable");
    assert_catalogue(&string_context, 0x111, 0x1111_1111, 5);

    set_variable(
        &mut dispatcher,
        &mut task,
        "Test$Path",
        environment.guest_prefix("Second").as_bytes(),
        4,
    )
    .expect("replace path variable with LiteralString(4)");
    let (literal_catalogue, literal_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        15,
        b"Shared",
        Some((4, PATH_NAME_ADDRESS)),
        None,
        false,
    );
    literal_catalogue.expect("reason 15 resolves raw LiteralString(4) path variable");
    assert_catalogue(&literal_context, 0x222, 0x2222_2222, 6);

    let (variable_load, variable_load_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        14,
        b"Shared",
        Some((4, PATH_NAME_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    variable_load.expect("reason 14 loads using a named path variable");
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 6).unwrap(), b"SECOND");
    assert_eq!(variable_load_context.registers[2], (0x222 << 20) | 0x12345);

    // Missing variable names are errors, while an existing empty path value
    // searches only the current directory. Unset File$Path has the same CSD
    // default for reasons 5 and 255.
    c_string(&mut task, PATH_NAME_ADDRESS, b"Missing$Path");
    let (missing_path_variable, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        15,
        b"Shared",
        Some((4, PATH_NAME_ADDRESS)),
        None,
        false,
    );
    assert!(missing_path_variable.is_err());
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);
    c_string(&mut task, PATH_NAME_ADDRESS, b"Empty$Path");
    set_variable(&mut dispatcher, &mut task, "Empty$Path", b"", 0)
        .expect("set empty path variable");
    let (empty_path, empty_path_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        15,
        b"CurrentOnly",
        Some((4, PATH_NAME_ADDRESS)),
        None,
        false,
    );
    empty_path.expect("empty path variable means current directory only");
    assert_catalogue(&empty_path_context, 0x666, 0x6666_6666, 7);

    // No earlier operation has set File$Path, so this tests the unset case.
    let (unset_files_path, unset_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        5,
        b"CurrentOnly",
        None,
        None,
        false,
    );
    unset_files_path.expect("unset File$Path defaults to CSD");
    assert_catalogue(&unset_context, 0x666, 0x6666_6666, 7);

    let second_prefix = environment.guest_prefix("Second");
    set_variable(
        &mut dispatcher,
        &mut task,
        "File$Path",
        second_prefix.as_bytes(),
        0,
    )
    .expect("set File$Path for File$Path reason calls");
    let (file_path_catalogue, file_path_context) =
        dispatch_file(&mut dispatcher, &mut task, 5, b"Shared", None, None, false);
    file_path_catalogue.expect("reason 5 consults File$Path");
    assert_catalogue(&file_path_context, 0x222, 0x2222_2222, 6);
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);
    let (file_path_load, file_path_load_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        255,
        b"Shared",
        None,
        Some(LOAD_ADDRESS),
        false,
    );
    file_path_load.expect("reason 255 loads from File$Path");
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 6).unwrap(), b"SECOND");
    assert_eq!(file_path_load_context.registers[4], 6);
    let (qualified_255, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        255,
        environment.guest_path("First.Shared").as_bytes(),
        None,
        Some(LOAD_ADDRESS),
        false,
    );
    qualified_255.expect("qualified reason 255 bypasses File$Path");
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 5).unwrap(), b"FIRST");

    // An explicit filing-system-qualified object bypasses a conflicting
    // File$Path candidate; no path prefix may redirect it.
    let (explicit_catalogue, explicit_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        5,
        environment.guest_path("Second.Shared").as_bytes(),
        None,
        None,
        false,
    );
    explicit_catalogue.expect("explicit FS path bypasses File$Path");
    assert_catalogue(&explicit_context, 0x222, 0x2222_2222, 6);
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);

    // Reason 17/16 do not search. Simple leaf names are looked up only in
    // CSD; fully qualified names remain directly addressable.
    let (direct_miss, direct_miss_context) =
        dispatch_file(&mut dispatcher, &mut task, 17, b"Shared", None, None, false);
    direct_miss.expect("no-search catalogue reports not-found normally");
    assert_eq!(direct_miss_context.registers[0], 0);
    let (direct_load, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        16,
        environment.guest_path("Second.Shared").as_bytes(),
        None,
        Some(LOAD_ADDRESS),
        false,
    );
    direct_load.expect("reason 16 opens an explicit path without search");
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 6).unwrap(), b"SECOND");

    // Malformed candidates and bad R4 pointers are errors, not misses that
    // allow a later candidate to hide the bad path. The X form produces a
    // standard caller-scoped error block without touching caller output.
    let malformed = format!(
        "HostFS::DemoDisk.$.First/escape.,{}",
        environment.guest_prefix("Second")
    );
    control_string(&mut task, PATH_LIST_ADDRESS, malformed.as_bytes());
    let sentinels = [0xD9; 16];
    task.memory.write_bytes(LOAD_ADDRESS, &sentinels).unwrap();
    let (malformed_result, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        12,
        b"Shared",
        Some((4, PATH_LIST_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    assert!(malformed_result.is_err());
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 16).unwrap(), sentinels);
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);

    // Candidate prefix plus object name must remain within the combined
    // 4096-byte resolver bound; reject the whole request rather than
    // truncating it or touching the destination.
    control_string(
        &mut task,
        PATH_LIST_ADDRESS,
        environment.guest_prefix("First").as_bytes(),
    );
    let long_object = vec![b'q'; 4080];
    let (over_combined_limit, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        12,
        &long_object,
        Some((4, PATH_LIST_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    assert!(over_combined_limit.is_err());
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 16).unwrap(), sentinels);
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);

    // Invalid UTF-8 in an earlier candidate is not treated as NotFound and
    // must not fall through to the otherwise-valid second candidate.
    let mut invalid_utf8 = vec![0xFF, b','];
    invalid_utf8.extend_from_slice(environment.guest_prefix("Second").as_bytes());
    control_string(&mut task, PATH_LIST_ADDRESS, &invalid_utf8);
    let (invalid_encoding, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        12,
        b"Shared",
        Some((4, PATH_LIST_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    assert!(invalid_encoding.is_err());
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 16).unwrap(), sentinels);
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);

    let (bad_x_path, bad_x_context) = dispatch_file(
        &mut dispatcher,
        &mut task,
        13,
        b"Shared",
        Some((4, u32::MAX)),
        None,
        true,
    );
    bad_x_path.expect("X form returns through standard V/error-block state");
    assert!(bad_x_context.overflow);
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);
    let error_text = x_error_text(&task, bad_x_context.registers[0]);
    assert!(!error_text.contains(environment.root.to_string_lossy().as_ref()));

    // A path list without a control terminator within the 255-byte contract
    // is rejected atomically; it is never truncated into a plausible path.
    task.memory
        .write_bytes(PATH_LIST_ADDRESS, &[b'x'; 256])
        .unwrap();
    let (unterminated, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        13,
        b"Shared",
        Some((4, PATH_LIST_ADDRESS)),
        None,
        false,
    );
    assert!(unterminated.is_err());
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);

    // A missing File$Path candidate advances only on not-found; changing the
    // store value is observable by the next call without dispatcher restart.
    set_variable(
        &mut dispatcher,
        &mut task,
        "File$Path",
        environment.guest_prefix("NoSuchDirectory").as_bytes(),
        0,
    )
    .expect("set missing first File$Path candidate");
    let appended = format!(
        "{},{}",
        environment.guest_prefix("NoSuchDirectory"),
        environment.guest_prefix("First")
    );
    set_variable(
        &mut dispatcher,
        &mut task,
        "File$Path",
        appended.as_bytes(),
        0,
    )
    .expect("replace File$Path live");
    let (live_file_path, live_context) =
        dispatch_file(&mut dispatcher, &mut task, 5, b"Shared", None, None, false);
    live_file_path.expect("live File$Path updates affect the next operation");
    assert_catalogue(&live_context, 0x111, 0x1111_1111, 5);

    // PRM read permission is granted by either owner-read (bit 0) or
    // public-read (bit 4); this entry has only the latter.
    control_string(&mut task, PATH_LIST_ADDRESS, two_candidates.as_bytes());
    let (public_read_load, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        12,
        b"PublicReadOnly",
        Some((4, PATH_LIST_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    public_read_load.expect("public-read bit permits loading without owner-read");
    assert_eq!(task.memory.read_bytes(LOAD_ADDRESS, 6).unwrap(), b"PUBLIC");

    // An access-denied first match is an error, not a NotFound that allows
    // search to continue to the readable same-named file in the next prefix.
    let protected_output = [0xC3; 16];
    task.memory
        .write_bytes(LOAD_ADDRESS, &protected_output)
        .unwrap();
    control_string(&mut task, PATH_LIST_ADDRESS, two_candidates.as_bytes());
    let (unreadable_load, _) = dispatch_file(
        &mut dispatcher,
        &mut task,
        12,
        b"Unreadable",
        Some((4, PATH_LIST_ADDRESS)),
        Some(LOAD_ADDRESS),
        false,
    );
    assert!(unreadable_load.is_err());
    assert_eq!(
        task.memory.read_bytes(LOAD_ADDRESS, 16).unwrap(),
        protected_output
    );
    assert_eq!(task.memory.dynamic_area_count(), baseline_dynamic_areas);
}
