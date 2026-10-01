use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{DisplayEvent, OS_CLI, OS_FSCONTROL, OS_SET_VAR_VAL, SwiContext, SwiDispatcher},
};

const X_BIT: u32 = 1 << 17;
const CLI_ADDRESS: u32 = 0x2200;
const PATH_ADDRESS: u32 = 0x2400;
const VAR_NAME_ADDRESS: u32 = 0x2800;
const VAR_VALUE_ADDRESS: u32 = 0x3000;
const BUFFER_ADDRESS: u32 = 0x5000;

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
            "ricochet-fscontrol-compat-{}-{nonce}",
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

    fn guest_path(&self, relative: &str) -> String {
        if relative.is_empty() {
            "HostFS::DemoDisk.$".to_string()
        } else {
            format!("HostFS::DemoDisk.$.{relative}")
        }
    }

    fn prefix(&self, relative: &str) -> String {
        if relative.is_empty() {
            "HostFS::DemoDisk.$.".to_string()
        } else {
            format!("HostFS::DemoDisk.$.{relative}.")
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

fn put_string(task: &mut Task, address: u32, value: &[u8], terminator: u8) {
    task.memory.write_bytes(address, value).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, terminator)
        .unwrap();
}

fn registers(reason: u32) -> [u32; 16] {
    let mut registers = [0xA5A5_0000; 16];
    registers[0] = reason;
    registers
}

fn call_fs(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    registers: [u32; 16],
    x_form: bool,
) -> (Result<(), RuntimeError>, SwiContext) {
    let mut context = SwiContext::default();
    context.registers.copy_from_slice(&registers);
    let swi = OS_FSCONTROL | if x_form { X_BIT } else { 0 };
    let result = dispatcher.dispatch(swi, task, &mut context);
    (result, context)
}

fn set_variable(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    value: &[u8],
    variable_type: u32,
) -> Result<(), RuntimeError> {
    put_string(task, VAR_NAME_ADDRESS, name.as_bytes(), 0);
    task.memory.write_bytes(VAR_VALUE_ADDRESS, value).unwrap();
    if variable_type == 0 {
        task.memory
            .write_byte(VAR_VALUE_ADDRESS + value.len() as u32, 0)
            .unwrap();
    }
    let mut context = SwiContext::default();
    context.registers[0] = VAR_NAME_ADDRESS;
    context.registers[1] = VAR_VALUE_ADDRESS;
    context.registers[2] = value.len() as u32;
    context.registers[4] = variable_type;
    dispatcher.dispatch(OS_SET_VAR_VAL, task, &mut context)
}

fn take_display_text(receiver: &mpsc::Receiver<DisplayEvent>) -> String {
    receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .map(char::from)
        .collect()
}

fn catalogue(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    display: &mpsc::Receiver<DisplayEvent>,
    reason: u32,
    directory: Option<&str>,
) -> String {
    if let Some(directory) = directory {
        put_string(task, PATH_ADDRESS, directory.as_bytes(), 0);
    }
    let mut call = registers(reason);
    call[1] = if directory.is_some() { PATH_ADDRESS } else { 0 };
    call_fs(dispatcher, task, call, false)
        .0
        .unwrap_or_else(|error| panic!("OS_FSControl reason {reason}: {error}"));
    take_display_text(display)
}

fn call_cli(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    display: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> Result<String, RuntimeError> {
    put_string(task, CLI_ADDRESS, command.as_bytes(), 0);
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    dispatcher.dispatch(OS_CLI, task, &mut context)?;
    Ok(take_display_text(display))
}

fn canonical_name(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    pathname: &str,
    variable: Option<&str>,
    fallback: Option<&str>,
    capacity: u32,
    x_form: bool,
) -> (SwiContext, Result<(), RuntimeError>, Vec<u8>) {
    put_string(task, PATH_ADDRESS, pathname.as_bytes(), 0);
    let variable_address = 0x2C00;
    let fallback_address = 0x3400;
    if let Some(variable) = variable {
        put_string(task, variable_address, variable.as_bytes(), 0);
    }
    if let Some(fallback) = fallback {
        put_string(task, fallback_address, fallback.as_bytes(), b'\r');
    }
    let sentinel_len = 96usize;
    task.memory
        .write_bytes(BUFFER_ADDRESS, &vec![0xB6; sentinel_len])
        .unwrap();
    let mut call = registers(37);
    call[1] = PATH_ADDRESS;
    call[2] = BUFFER_ADDRESS;
    call[3] = if variable.is_some() {
        variable_address
    } else {
        0
    };
    call[4] = if fallback.is_some() {
        fallback_address
    } else {
        0
    };
    call[5] = capacity;
    let (result, context) = call_fs(dispatcher, task, call, x_form);
    let bytes = task
        .memory
        .read_bytes(BUFFER_ADDRESS, sentinel_len)
        .unwrap();
    (context, result, bytes)
}

#[test]
fn fscontrol_prefix_path_and_catalogue_compatibility() {
    let environment = Environment::new();
    for directory in [
        "Csd",
        "Csd/Sub",
        "Second",
        "Library",
        "Library/Sub",
        "UserRoot",
        "Ünicode",
    ] {
        fs::create_dir_all(environment.root.join(directory)).unwrap();
    }
    fs::write(environment.root.join("Csd/FromCsd"), b"csd").unwrap();
    fs::write(environment.root.join("Csd/Shared"), b"csd shared").unwrap();
    fs::write(environment.root.join("Second/Shared"), b"second shared").unwrap();
    fs::write(environment.root.join("Library/FromLibrary"), b"library").unwrap();
    fs::write(environment.root.join("Library/Shared"), b"library shared").unwrap();
    fs::write(
        environment.root.join("Library/Sub/FromSubLibrary"),
        b"sub library",
    )
    .unwrap();
    fs::write(environment.root.join("UserRoot/FromUserRoot"), b"urd").unwrap();
    fs::write(environment.root.join("Ünicode/Café"), b"utf8").unwrap();

    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0F51);
    let other = Task::new(0x0F52);

    // Distinct directory slots make catalogue headers and entries observable.
    let mut set_csd = registers(0);
    set_csd[1] = PATH_ADDRESS;
    put_string(&mut task, PATH_ADDRESS, b"HostFS::DemoDisk.$.Csd", 0);
    call_fs(&mut dispatcher, &mut task, set_csd, false)
        .0
        .expect("set CSD");
    let mut set_library = registers(1);
    set_library[1] = PATH_ADDRESS;
    put_string(&mut task, PATH_ADDRESS, b"HostFS::DemoDisk.$.Library", 0);
    call_fs(&mut dispatcher, &mut task, set_library, false)
        .0
        .expect("set library");
    let mut set_urd = registers(39);
    set_urd[1] = PATH_ADDRESS;
    put_string(&mut task, PATH_ADDRESS, b"HostFS::DemoDisk.$.UserRoot", 0);
    call_fs(&mut dispatcher, &mut task, set_urd, false)
        .0
        .expect("set URD");

    for (reason, title, entry) in [
        (5, "$.Csd", "FromCsd"),
        (6, "$.Csd", "FromCsd"),
        (7, "$.Library", "FromLibrary"),
        (8, "$.Library", "FromLibrary"),
    ] {
        let output = catalogue(&mut dispatcher, &mut task, &display, reason, None);
        let first_line = output.lines().next().unwrap_or_default();
        assert!(
            first_line.contains(title),
            "reason {reason} title: {output:?}"
        );
        assert!(
            output.contains(entry),
            "reason {reason} entries: {output:?}"
        );
        assert!(
            !output.contains("HostFS::DemoDisk.$.UserRoot"),
            "reason {reason} must not title the unrelated URD: {output:?}"
        );
    }

    let csd_subdirectory = catalogue(&mut dispatcher, &mut task, &display, 5, Some("Sub"));
    assert!(
        csd_subdirectory
            .lines()
            .next()
            .unwrap()
            .contains("$.Csd.Sub")
    );
    assert!(!csd_subdirectory.contains("FromSubLibrary"));
    let library_subdirectory = catalogue(&mut dispatcher, &mut task, &display, 7, Some("Sub"));
    assert!(
        library_subdirectory
            .lines()
            .next()
            .unwrap()
            .contains("$.Library.Sub")
    );
    assert!(library_subdirectory.contains("FromSubLibrary"));
    let library_subdirectory_info = catalogue(&mut dispatcher, &mut task, &display, 8, Some("Sub"));
    assert!(
        library_subdirectory_info
            .lines()
            .next()
            .unwrap()
            .contains("$.Library.Sub")
    );
    assert!(library_subdirectory_info.contains("FromSubLibrary"));

    let areas_before_bad_catalogue = task.memory.dynamic_area_count();
    assert!(
        call_cli(
            &mut dispatcher,
            &mut task,
            &display,
            "*CAT HostFS::DemoDisk.$.NoSuchCatalogue"
        )
        .is_err()
    );
    assert_eq!(
        task.memory.dynamic_area_count(),
        areas_before_bad_catalogue,
        "failed CLI catalogue must release both scratch areas"
    );
    let healthy_catalogue = call_cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*CAT HostFS::DemoDisk.$.Library",
    )
    .expect("healthy CLI catalogue follows a failed catalogue");
    assert!(healthy_catalogue.contains("FromLibrary"));

    // A recognized prefix returns the post-prefix logical address, the
    // previous temporary selector, and a null special-field pointer.
    let hostfs_path = environment.guest_path("Csd.Shared");
    put_string(&mut task, PATH_ADDRESS, hostfs_path.as_bytes(), 0);
    let before = task.file_system.temporary_file_system.clone();
    let mut select_temporary = registers(11);
    select_temporary[1] = PATH_ADDRESS;
    let (recognized_result, recognized) =
        call_fs(&mut dispatcher, &mut task, select_temporary, false);
    recognized_result.expect("recognize HostFS prefix");
    assert_eq!(recognized.registers[0], 11);
    assert_eq!(recognized.registers[1], PATH_ADDRESS + 7);
    assert_eq!(recognized.registers[2], 1);
    assert_eq!(recognized.registers[3], 0);
    assert_eq!(recognized.registers[4..], select_temporary[4..]);
    assert_eq!(task.file_system.temporary_file_system, "HOSTFS");

    // Unsupported filesystem names and special-field selectors fail without
    // altering either the active Task's selection or the register block.
    for rejected_prefix in ["OtherFS::Disc.$.Csd.Shared", "HostFS#Special:"] {
        put_string(&mut task, PATH_ADDRESS, rejected_prefix.as_bytes(), 0);
        let mut rejected_call = registers(11);
        rejected_call[1] = PATH_ADDRESS;
        let (result, context) = call_fs(&mut dispatcher, &mut task, rejected_call, false);
        assert!(result.is_err(), "must reject {rejected_prefix}");
        assert_eq!(context.registers, rejected_call);
        assert_eq!(task.file_system.temporary_file_system, "HOSTFS");
    }
    assert_eq!(before, "HostFS");
    assert_eq!(other.file_system.temporary_file_system, "HostFS");

    let no_prefix = b"Csd.Shared";
    put_string(&mut task, PATH_ADDRESS, no_prefix, 0);
    let mut no_prefix_call = registers(11);
    no_prefix_call[1] = PATH_ADDRESS;
    let (no_prefix_result, no_prefix_context) =
        call_fs(&mut dispatcher, &mut task, no_prefix_call, false);
    no_prefix_result.expect("no prefix is a no-op");
    assert_eq!(no_prefix_context.registers[0], 11);
    assert_eq!(no_prefix_context.registers[1], PATH_ADDRESS);
    assert_eq!(no_prefix_context.registers[2], u32::MAX);
    assert_eq!(no_prefix_context.registers[3], 0);
    assert_eq!(no_prefix_context.registers[4..], no_prefix_call[4..]);
    assert_eq!(task.file_system.temporary_file_system, "HOSTFS");

    put_string(&mut task, PATH_ADDRESS, b"HostFS:Dir#leaf", 0);
    let mut recognized_hash_path = registers(11);
    recognized_hash_path[1] = PATH_ADDRESS;
    let (recognized_hash_result, recognized_hash_context) =
        call_fs(&mut dispatcher, &mut task, recognized_hash_path, false);
    recognized_hash_result.expect("hash after a valid HostFS prefix is path data");
    assert_eq!(recognized_hash_context.registers[1], PATH_ADDRESS + 7);
    assert_eq!(recognized_hash_context.registers[2], 1);
    assert_eq!(recognized_hash_context.registers[3], 0);

    let mut restore = registers(19);
    restore[1] = 0x1234;
    call_fs(&mut dispatcher, &mut task, restore, false)
        .0
        .expect("restore temporary filing system");
    assert_eq!(task.file_system.temporary_file_system, "HOSTFS");

    // With no current selector, a recognized prefix reports zero as the
    // previous filing system. Restore (reason 19) returns to no selection.
    let mut clear_selection = registers(14);
    clear_selection[1] = 0;
    call_fs(&mut dispatcher, &mut task, clear_selection, false)
        .0
        .expect("clear selectors before no-prior-FS probe");
    put_string(&mut task, PATH_ADDRESS, hostfs_path.as_bytes(), 0);
    let mut select_from_none = registers(11);
    select_from_none[1] = PATH_ADDRESS;
    let (_, from_none) = call_fs(&mut dispatcher, &mut task, select_from_none, false);
    assert_eq!(from_none.registers[1], PATH_ADDRESS + 7);
    assert_eq!(from_none.registers[2], 0);
    assert_eq!(from_none.registers[3], 0);
    call_fs(&mut dispatcher, &mut task, registers(19), false)
        .0
        .expect("restore empty current selector");
    assert!(task.file_system.current_file_system.is_empty());
    assert!(task.file_system.temporary_file_system.is_empty());
    let mut select_hostfs = registers(14);
    select_hostfs[1] = 1;
    call_fs(&mut dispatcher, &mut task, select_hostfs, false)
        .0
        .expect("reselect HostFS after reason 19 test");

    // Reason 37 searches ordered prefixes and honors the String or
    // LiteralString path variable before falling back to R4 only when absent.
    let first_prefix = environment.prefix("First");
    let second_prefix = environment.prefix("Second");
    fs::create_dir_all(environment.root.join("First")).unwrap();
    fs::write(environment.root.join("First/Shared"), b"first shared").unwrap();
    let value = format!("{first_prefix},{second_prefix}");
    set_variable(
        &mut dispatcher,
        &mut task,
        "Compat$Path",
        value.as_bytes(),
        0,
    )
    .expect("store String path variable");
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        Some("Compat$Path"),
        Some(&second_prefix),
        96,
        true,
    );
    result.expect("String variable takes precedence over R4 fallback");
    let expected_first = environment.guest_path("First.Shared");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, expected_first.len() + 1)
            .unwrap(),
        [expected_first.as_bytes(), &[0]].concat()
    );

    let value = second_prefix.as_bytes();
    set_variable(&mut dispatcher, &mut task, "Compat$Path", value, 4)
        .expect("replace path variable live with LiteralString");
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        Some("Compat$Path"),
        Some(&first_prefix),
        96,
        false,
    );
    result.expect("live LiteralString path variable is read");
    let expected_second = environment.guest_path("Second.Shared");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, expected_second.len() + 1)
            .unwrap(),
        [expected_second.as_bytes(), &[0]].concat()
    );

    let intermediate_miss = format!("HostFS::DemoDisk.$.MissingParent.Sub.,{second_prefix}");
    set_variable(
        &mut dispatcher,
        &mut task,
        "Compat$Intermediate",
        intermediate_miss.as_bytes(),
        4,
    )
    .expect("store path list with missing intermediate parent");
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        Some("Compat$Intermediate"),
        None,
        96,
        false,
    );
    result.expect("a missing intermediate parent advances to the next candidate");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, expected_second.len() + 1)
            .unwrap(),
        [expected_second.as_bytes(), &[0]].concat()
    );

    // An absent variable selects R4. An existing empty variable selects CSD
    // only and does not consult R4.
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        Some("Compat$Missing"),
        Some(&first_prefix),
        96,
        false,
    );
    result.expect("absent variable falls back to R4 path string");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, expected_first.len() + 1)
            .unwrap(),
        [expected_first.as_bytes(), &[0]].concat()
    );
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "FromCsd",
        Some("Compat$AbsentDefault"),
        None,
        96,
        false,
    );
    result.expect("absent variable and R4 default to CSD");
    let expected_csd = environment.guest_path("Csd.FromCsd");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, expected_csd.len() + 1)
            .unwrap(),
        [expected_csd.as_bytes(), &[0]].concat()
    );
    set_variable(&mut dispatcher, &mut task, "Compat$Empty", b"", 4)
        .expect("store present empty path variable");
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "FromCsd",
        Some("Compat$Empty"),
        Some(&second_prefix),
        96,
        false,
    );
    result.expect("present empty variable means only the current directory");
    let expected_csd = environment.guest_path("Csd.FromCsd");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, expected_csd.len() + 1)
            .unwrap(),
        [expected_csd.as_bytes(), &[0]].concat()
    );

    // A fully qualified object bypasses the search list. If all prefixes
    // miss, the final attempted candidate is returned unchanged.
    let qualified = environment.guest_path("Second.Shared");
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        &qualified,
        Some("Compat$Path"),
        Some(&first_prefix),
        96,
        false,
    );
    result.expect("qualified path bypasses path sources");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, qualified.len() + 1)
            .unwrap(),
        [qualified.as_bytes(), &[0]].concat()
    );
    let invalid_source_pointer = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32 - 1;
    task.memory
        .write_byte(invalid_source_pointer, b'X')
        .unwrap();
    task.memory
        .write_bytes(BUFFER_ADDRESS, &vec![0xB6; 96])
        .unwrap();
    let areas_before_qualified = task.memory.dynamic_area_count();
    let mut qualified_bypass = registers(37);
    put_string(&mut task, PATH_ADDRESS, qualified.as_bytes(), 0);
    qualified_bypass[1] = PATH_ADDRESS;
    qualified_bypass[2] = BUFFER_ADDRESS;
    qualified_bypass[3] = invalid_source_pointer;
    qualified_bypass[4] = invalid_source_pointer;
    qualified_bypass[5] = 96;
    let (qualified_bypass_result, qualified_bypass_context) =
        call_fs(&mut dispatcher, &mut task, qualified_bypass, false);
    qualified_bypass_result.expect("qualified R1 bypasses unused malformed R3 and R4 sources");
    assert_eq!(
        qualified_bypass_context.registers[5],
        96 - qualified.len() as u32
    );
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, qualified.len() + 1)
            .unwrap(),
        [qualified.as_bytes(), &[0]].concat()
    );
    assert_eq!(
        task.memory.dynamic_area_count(),
        areas_before_qualified,
        "successful canonicalization must release command scratch"
    );
    let missing = "NoSuchLeaf";
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        missing,
        None,
        Some(&format!("{first_prefix},{second_prefix}")),
        96,
        false,
    );
    result.expect("missing leaf is canonicalized from final attempted prefix");
    let expected_missing = environment.guest_path("Second.NoSuchLeaf");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, expected_missing.len() + 1)
            .unwrap(),
        [expected_missing.as_bytes(), &[0]].concat()
    );
    let unresolved_wildcard = "NoMatch.*";
    let (_, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        unresolved_wildcard,
        None,
        Some(&second_prefix),
        96,
        false,
    );
    result.expect("an unresolved wildcard is returned as a canonical candidate");
    let expected_wildcard = environment.guest_path("Second.NoMatch.*");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, expected_wildcard.len() + 1)
            .unwrap(),
        [expected_wildcard.as_bytes(), &[0]].concat()
    );

    let unicode_prefix = environment.prefix("Ünicode");
    let unicode_candidate = environment.guest_path("Ünicode.Café");
    let (unicode_context, result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Café",
        None,
        Some(&unicode_prefix),
        unicode_candidate.len() as u32 + 1,
        false,
    );
    result.expect("multibyte path components survive candidate lookup");
    assert_eq!(unicode_context.registers[5], 1, "R5 uses UTF-8 byte length");
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, unicode_candidate.len() + 1)
            .unwrap(),
        [unicode_candidate.as_bytes(), &[0]].concat()
    );

    // Capacity reports PRM spare-byte semantics and never partially writes.
    let canonical = expected_first;
    let length = canonical.len() as u32;
    let (exact_context, exact_result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        None,
        Some(&first_prefix),
        length + 1,
        false,
    );
    exact_result.expect("exact name-plus-NUL capacity succeeds");
    assert_eq!(exact_context.registers[5], 1);
    let (x_context, x_result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        None,
        Some(&first_prefix),
        length + 1,
        true,
    );
    x_result.expect("X-form canonical-name exact-fit call");
    assert!(!x_context.overflow);
    assert_eq!(x_context.registers[5], 1);

    let (short_context, short_result, short_bytes) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        None,
        Some(&first_prefix),
        length,
        false,
    );
    short_result.expect("buffer without terminator space is a sizing result");
    assert_eq!(short_context.registers[5], 0);
    assert!(short_bytes.iter().all(|byte| *byte == 0xB6));

    let (first_pass_context, first_pass_result, first_pass_bytes) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        None,
        Some(&first_prefix),
        0,
        false,
    );
    first_pass_result.expect("zero-buffer first pass");
    assert_eq!(first_pass_context.registers[5], (-(length as i32)) as u32);
    assert!(first_pass_bytes.iter().all(|byte| *byte == 0xB6));

    let (max_context, max_result, _) = canonical_name(
        &mut dispatcher,
        &mut task,
        "Shared",
        None,
        Some(&first_prefix),
        u32::MAX,
        false,
    );
    max_result.expect("maximum U32 buffer capacity remains a valid bounded output");
    assert_eq!(max_context.registers[5], u32::MAX - length);
    assert_eq!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, canonical.len() + 1)
            .unwrap(),
        [canonical.as_bytes(), &[0]].concat()
    );

    let invalid_span = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32 - 1;
    task.memory.write_byte(invalid_span, 0xD2).unwrap();
    let mut bad_output = registers(37);
    put_string(&mut task, PATH_ADDRESS, b"Shared", 0);
    put_string(&mut task, 0x2C00, b"Compat$Empty", 0);
    bad_output[1] = PATH_ADDRESS;
    bad_output[2] = invalid_span;
    bad_output[3] = 0x2C00;
    bad_output[5] = 64;
    let areas_before_bad_output = task.memory.dynamic_area_count();
    assert!(
        call_fs(&mut dispatcher, &mut task, bad_output, false)
            .0
            .is_err()
    );
    assert_eq!(task.memory.read_byte(invalid_span).unwrap(), 0xD2);
    assert_eq!(
        task.memory.dynamic_area_count(),
        areas_before_bad_output,
        "failed output preflight must release its command scratch"
    );

    let areas_before_bad_prefix = task.memory.dynamic_area_count();
    let (invalid_variable_context, invalid_variable_result, invalid_variable_bytes) =
        canonical_name(
            &mut dispatcher,
            &mut task,
            "Shared",
            Some("Compat$Missing"),
            Some("bad-prefix"),
            96,
            true,
        );
    invalid_variable_result.expect("X-form malformed R4 returns with V set");
    assert!(invalid_variable_context.overflow);
    assert!(invalid_variable_bytes.iter().all(|byte| *byte == 0xB6));
    assert_eq!(
        task.memory.dynamic_area_count(),
        areas_before_bad_prefix,
        "malformed path sources must release command scratch"
    );

    let invalid_variable_pointer = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32 - 1;
    task.memory
        .write_byte(invalid_variable_pointer, b'X')
        .unwrap();
    let mut malformed_r3 = registers(37);
    put_string(&mut task, PATH_ADDRESS, b"Shared", 0);
    malformed_r3[1] = PATH_ADDRESS;
    malformed_r3[2] = BUFFER_ADDRESS;
    malformed_r3[3] = invalid_variable_pointer;
    malformed_r3[4] = 0x3400;
    malformed_r3[5] = 96;
    task.memory
        .write_bytes(BUFFER_ADDRESS, &vec![0xB6; 96])
        .unwrap();
    let areas_before_malformed_r3 = task.memory.dynamic_area_count();
    let (malformed_r3_result, _) = call_fs(&mut dispatcher, &mut task, malformed_r3, false);
    assert!(
        malformed_r3_result.is_err(),
        "invalid R3 must not fall back to R4"
    );
    assert!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, 96)
            .unwrap()
            .iter()
            .all(|byte| *byte == 0xB6)
    );
    assert_eq!(
        task.memory.dynamic_area_count(),
        areas_before_malformed_r3,
        "invalid variable-name pointers must release command scratch"
    );

    task.memory.write_bytes(0x3400, &[0xFF, b'\r']).unwrap();
    let mut invalid_utf8_source = registers(37);
    put_string(&mut task, PATH_ADDRESS, b"Shared", 0);
    invalid_utf8_source[1] = PATH_ADDRESS;
    invalid_utf8_source[2] = BUFFER_ADDRESS;
    invalid_utf8_source[4] = 0x3400;
    invalid_utf8_source[5] = 96;
    task.memory
        .write_bytes(BUFFER_ADDRESS, &vec![0xB6; 96])
        .unwrap();
    let areas_before_invalid_utf8 = task.memory.dynamic_area_count();
    let (invalid_utf8_result, _) = call_fs(&mut dispatcher, &mut task, invalid_utf8_source, false);
    assert!(
        invalid_utf8_result.is_err(),
        "invalid UTF-8 sources must fail"
    );
    assert_eq!(
        task.memory.dynamic_area_count(),
        areas_before_invalid_utf8,
        "invalid UTF-8 path sources must release command scratch"
    );
    assert!(
        task.memory
            .read_bytes(BUFFER_ADDRESS, 96)
            .unwrap()
            .iter()
            .all(|byte| *byte == 0xB6)
    );
}
