use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Mutex, mpsc},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, OS_CLI, SwiContext, SwiDispatcher},
};

const OS_READ_VAR_VAL: u32 = 0x23;
const OS_SET_VAR_VAL: u32 = 0x24;
const CLI_ADDRESS: u32 = 0x2100;
const NAME_ADDRESS: u32 = 0x3000;
const VALUE_ADDRESS: u32 = 0x4000;
const OUTPUT_ADDRESS: u32 = 0x8000;
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

struct Environment {
    root: PathBuf,
    old_volume: Option<std::ffi::OsString>,
    old_config: Option<std::ffi::OsString>,
}

impl Environment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ricochet-alias-boundaries-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        let old_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        let old_config = std::env::var_os("RICOCHET_CONFIG_PATH");
        unsafe {
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
            std::env::set_var("RICOCHET_CONFIG_PATH", root.join("configure"));
        }
        Self {
            root,
            old_volume,
            old_config,
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
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug, Eq, PartialEq)]
struct Variable {
    value: Vec<u8>,
    kind: u32,
}

fn dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_tx, input_rx) = mpsc::channel();
    let (display_tx, display_rx) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_rx), display_tx),
        display_rx,
    )
}

fn put_c_string(task: &mut Task, address: u32, value: &str) {
    task.memory.write_bytes(address, value.as_bytes()).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn cli(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    display: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> (Result<(), RuntimeError>, String) {
    let _ = display.try_iter().count();
    let scratch_before = task.memory.dynamic_area_count();
    put_c_string(task, CLI_ADDRESS, command);
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    if result.is_ok() {
        assert_eq!(context.registers[0], CLI_ADDRESS, "OS_CLI preserves R0");
    }
    let output = display
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    let output = String::from_utf8_lossy(&output).into_owned();
    assert_eq!(
        task.memory.dynamic_area_count(),
        scratch_before,
        "alias command scratch is released for {command:?}; output={output:?}"
    );
    (result, output)
}

fn bootstrap(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    display: &mpsc::Receiver<DisplayEvent>,
) {
    cli(dispatcher, task, display, "*HELP S.")
        .0
        .expect("CLI startup publishes System SWIs before direct register checks");
}

fn set_variable(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    value: &[u8],
    kind: u32,
) -> Result<(), RuntimeError> {
    put_c_string(task, NAME_ADDRESS, name);
    task.memory.write_bytes(VALUE_ADDRESS, value).unwrap();
    if kind == 0 {
        task.memory
            .write_byte(VALUE_ADDRESS + value.len() as u32, 0)
            .unwrap();
    }
    let mut context = SwiContext::default();
    context.registers[0] = NAME_ADDRESS;
    context.registers[1] = VALUE_ADDRESS;
    context.registers[2] = value.len() as u32;
    context.registers[4] = kind;
    dispatcher.dispatch(OS_SET_VAR_VAL, task, &mut context)
}

fn read_variable(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
) -> Result<Option<Variable>, RuntimeError> {
    put_c_string(task, NAME_ADDRESS, name);
    task.memory
        .write_bytes(OUTPUT_ADDRESS, &[0xA5; 512])
        .unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = NAME_ADDRESS;
    context.registers[1] = OUTPUT_ADDRESS;
    context.registers[2] = 512;
    match dispatcher.dispatch(OS_READ_VAR_VAL, task, &mut context) {
        Ok(()) => {}
        Err(RuntimeError::Structured {
            type_name, code, ..
        }) if type_name == "SystemVariableNotFound" && code == 2 => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = context.registers[2] as usize;
    Ok(Some(Variable {
        value: task.memory.read_bytes(OUTPUT_ADDRESS, len).unwrap(),
        kind: context.registers[4],
    }))
}

fn run_stdio(
    config: &std::path::Path,
    volume: &std::path::Path,
    input: &[u8],
) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ricochet"))
        .arg("--stdio")
        .env("RICOCHET_CONFIG_PATH", config)
        .env("RICOCHET_DEMO_VOLUME", volume)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start isolated stdio runtime");
    child.stdin.take().unwrap().write_all(input).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().expect("poll stdio child").is_some() {
            break child.wait_with_output().expect("collect stdio output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("isolated stdio runtime exceeded its 15-second test deadline");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn maximum_alias_name_exact_prefix_ambiguity_fallback_and_recovery() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0A_B001);
    bootstrap(&mut dispatcher, &mut task, &display);

    let max_name = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let prefix = &max_name[..25];
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*SET Alias${max_name} SET ALIAS_BOUNDARY %0"),
    )
    .0
    .expect("Alias$ plus 26-byte suffix fits the 32-byte variable-name bound");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, &format!("Alias${max_name}")).unwrap(),
        Some(Variable {
            value: b"SET ALIAS_BOUNDARY %0".to_vec(),
            kind: 0
        })
    );
    let mut ordinary = Task::new(0x0A_B101);
    let (result, _) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display,
        &format!("*{max_name} unauthorized"),
    );
    assert!(
        result.is_err(),
        "a maximum-length alias cannot borrow the trusted creator's variable-write right"
    );
    assert!(
        read_variable(&mut dispatcher, &mut ordinary, "ALIAS_BOUNDARY")
            .unwrap()
            .is_none(),
        "failed alias target does not mutate through the original ordinary caller"
    );

    for (invocation, expected) in [
        (format!("*{max_name} exact-value"), "exact-value"),
        (
            format!("*{max_name}. dotted-full-value"),
            "dotted-full-value",
        ),
        (
            format!("*{prefix}. dotted-prefix-value"),
            "dotted-prefix-value",
        ),
    ] {
        cli(&mut dispatcher, &mut task, &display, &invocation)
            .0
            .expect("maximum-length exact and final-dot alias forms resolve");
        assert_eq!(
            read_variable(&mut dispatcher, &mut task, "ALIAS_BOUNDARY").unwrap(),
            Some(Variable {
                value: expected.as_bytes().to_vec(),
                kind: 0
            }),
            "lookup result for {invocation:?}"
        );
    }

    // The 32-byte variable name boundary is retained: 27 command bytes after
    // Alias$ cannot be stored, even though the CLI itself accepts the token.
    let overlong_name = format!("Alias${}", "Z".repeat(27));
    assert!(
        set_variable(&mut dispatcher, &mut task, &overlong_name, b"SET A B", 4).is_err(),
        "do not increase the existing variable-store name bound to accommodate aliases"
    );

    let second_max_name = format!("{prefix}1");
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*SET Alias${second_max_name} SET ALIAS_SECOND %0"),
    )
    .0
    .expect("a second maximum-length name with the same 25-byte prefix is storable");
    let (result, error_output) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*{prefix}. ambiguous"),
    );
    let error = result.expect_err("a non-unique maximum-length alias prefix is rejected");
    assert!(
        error.to_string().to_ascii_lowercase().contains("ambiguous"),
        "the failure must be an alias ambiguity, not an overlong selector/name error: {error:?}, {error_output:?}"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_BOUNDARY").unwrap(),
        Some(Variable {
            value: b"dotted-prefix-value".to_vec(),
            kind: 0
        }),
        "an ambiguous lookup does not dispatch either alias target"
    );

    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*UNSET Alias${second_max_name}"),
    )
    .0
    .expect("removing one colliding alias restores unique-prefix lookup");
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*{prefix}. recovered-prefix"),
    )
    .0
    .expect("unique 25-byte prefix resolves after collision removal");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_BOUNDARY").unwrap(),
        Some(Variable {
            value: b"recovered-prefix".to_vec(),
            kind: 0
        })
    );

    let (result, no_match_output) = cli(&mut dispatcher, &mut task, &display, "*NOBOUNDARYMATCH.");
    let error = result.expect_err("no alias prefix falls through to the normal registry lookup");
    assert!(
        error.to_string().contains("Bad command"),
        "no-match uses normal command lookup rather than variable-buffer error: {error:?}, {no_match_output:?}"
    );
    assert!(
        !error.to_string().contains("selector") && !error.to_string().contains("VariableName"),
        "no-match final-dot path did not build an overlong Alias$ selector: {error:?}"
    );

    let (rom_dot_result, rom_dot_output) =
        cli(&mut dispatcher, &mut task, &display, "*ROMMODULES.");
    let (rom_exact_result, rom_exact_output) =
        cli(&mut dispatcher, &mut task, &display, "*ROMMODULES");
    assert_eq!(
        rom_dot_result.map_err(|error| error.to_string()),
        rom_exact_result.map_err(|error| error.to_string()),
        "a longest non-aliased registry command retains its dotted fallback route"
    );
    assert_eq!(rom_dot_output, rom_exact_output);

    cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET ALIAS_AFTER_BOUNDARY_ERROR healthy",
    )
    .0
    .expect("scratch and lookup contexts are released after alias errors");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_AFTER_BOUNDARY_ERROR").unwrap(),
        Some(Variable {
            value: b"healthy".to_vec(),
            kind: 0
        })
    );
}

#[test]
fn invalid_alias_suffixes_do_not_capture_prefixes_and_stdio_accepts_maximum_aliases() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0A_B002);
    bootstrap(&mut dispatcher, &mut task, &display);

    let (status_result, status_output) = cli(&mut dispatcher, &mut task, &display, "*STATUS");
    let (s_result, s_output) = cli(&mut dispatcher, &mut task, &display, "*S.");
    status_result.expect("STATUS remains available before aliases");
    s_result.expect("S. retains the registry winner without alias variables");
    assert_eq!(status_output, s_output);

    set_variable(
        &mut dispatcher,
        &mut task,
        "Alias$S%",
        b"SET ALIAS_INVALID_PERCENT forged",
        4,
    )
    .expect("the underlying visible-ASCII variable store may hold %");
    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*S.");
    result.expect("invalid alias suffix bytes are filtered before prefix matching");
    assert_eq!(status_output, output, "invalid names do not shadow STATUS");
    let (_, output) = cli(&mut dispatcher, &mut task, &display, "*SHOW Alias$S%");
    assert!(
        output.contains("Alias$S%"),
        "stored non-command suffix remains a normal variable: {output:?}"
    );

    cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET Alias$SystemHelper SET ALIAS_VALID_SYSTEM reached",
    )
    .0
    .expect("a valid matching alias can be added beside invalid stored selectors");
    cli(&mut dispatcher, &mut task, &display, "*S.")
        .0
        .expect("only the valid alias matches S. after suffix validation");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_VALID_SYSTEM").unwrap(),
        Some(Variable {
            value: b"reached".to_vec(),
            kind: 0
        })
    );
    let (result, bypassed_status) = cli(&mut dispatcher, &mut task, &display, "*%S.");
    result.expect("leading percent bypasses the valid matching alias");
    assert_eq!(status_output, bypassed_status);

    cli(&mut dispatcher, &mut task, &display, "*UNSET Alias$S%")
        .0
        .expect("second invalid-name variable can be deleted");

    let stdio_volume = environment.root.join("stdio-volume");
    fs::create_dir_all(&stdio_volume).unwrap();
    let max_name = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    let input = format!(
        "*SET Alias${max_name} SET ALIAS_STDIO %0\n*{max_name} exact-stdio\n*SHOW ALIAS_STDIO\n*{max_name}. dotted-stdio\n*SHOW ALIAS_STDIO\n*QUIT\n"
    );
    let output = run_stdio(
        &environment.root.join("stdio.configure"),
        &stdio_volume,
        input.as_bytes(),
    );
    assert!(
        output.status.success(),
        "maximum-name stdio sequence failed: {output:?}"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("exact-stdio"),
        "exact max-name alias ran: {stdout}"
    );
    assert!(
        stdout.contains("dotted-stdio"),
        "dotted max-name alias ran: {stdout}"
    );
    assert!(
        !stdout.contains("VariableName"),
        "no selector overflow leaked to stdio: {stdout}"
    );
}
