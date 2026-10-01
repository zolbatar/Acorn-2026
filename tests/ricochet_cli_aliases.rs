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
        let root = std::env::temp_dir().join(format!("ricochet-cli-aliases-{nonce}"));
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

    fn guest_path(&self, name: &str) -> String {
        format!("HostFS::DemoDisk.$.{}", name.replace('/', "."))
    }

    fn write_script(&self, name: &str, contents: &[u8]) {
        fs::write(self.root.join(name), contents).unwrap();
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

fn set_alias(dispatcher: &mut SwiDispatcher, task: &mut Task, command: &str, value: &str) {
    set_variable(
        dispatcher,
        task,
        &format!("Alias${command}"),
        value.as_bytes(),
        4,
    )
    .expect("trusted task can store a literal alias target");
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
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .expect("write bounded command input");
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
fn aliases_are_live_variables_shadow_registry_and_percent_bypasses_once() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0A_1101);
    bootstrap(&mut dispatcher, &mut task, &display);
    for (abbreviation, exact) in [("*S.", "*STATUS"), ("*D.", "*DIR"), ("*F.", "*FILETYPE")] {
        let (abbrev_result, abbrev_output) =
            cli(&mut dispatcher, &mut task, &display, abbreviation);
        let (exact_result, exact_output) = cli(&mut dispatcher, &mut task, &display, exact);
        abbrev_result.expect("existing no-alias abbreviation remains available");
        exact_result.expect("corresponding exact command remains available");
        assert_eq!(
            abbrev_output, exact_output,
            "{abbreviation} still routes to the existing first registry command"
        );
    }
    assert!(
        set_variable(
            &mut dispatcher,
            &mut task,
            "Alias$Macro",
            b"ECHO no-macro",
            2
        )
        .is_err(),
        "macro aliases stay explicitly unsupported by the bounded store"
    );
    assert!(
        read_variable(&mut dispatcher, &mut task, "Alias$Macro")
            .unwrap()
            .is_none()
    );
    let longest_alias = "A".repeat(26);
    set_alias(
        &mut dispatcher,
        &mut task,
        &longest_alias,
        "SET ALIAS_LONGEST_NAME accepted",
    );
    let (result, _) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*{longest_alias}"),
    );
    result.expect("26-byte alias suffix fits Alias$ plus the store name bound");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_LONGEST_NAME").unwrap(),
        Some(Variable {
            value: b"accepted".to_vec(),
            kind: 0
        })
    );
    let too_long_alias = format!("Alias${}", "B".repeat(27));
    assert!(set_variable(&mut dispatcher, &mut task, &too_long_alias, b"SET X Y", 4).is_err());

    set_alias(
        &mut dispatcher,
        &mut task,
        "DirTrap",
        "SET ALIAS_DIR_SHADOW reached",
    );
    cli(&mut dispatcher, &mut task, &display, "*D.")
        .0
        .expect("a unique alias prefix precedes built-in abbreviation matching");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_DIR_SHADOW").unwrap(),
        Some(Variable {
            value: b"reached".to_vec(),
            kind: 0
        })
    );
    let (_, alias_bypassed_dir) = cli(&mut dispatcher, &mut task, &display, "*%D.");
    let (_, exact_dir) = cli(&mut dispatcher, &mut task, &display, "*DIR");
    assert_eq!(
        alias_bypassed_dir, exact_dir,
        "% bypass restores DIR abbreviation"
    );

    cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET Alias$Live SET ALIAS_LIVE first-target",
    )
    .0
    .expect("*SET creates a live Alias$ variable");
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*lIvE");
    result.expect("alias lookup ignores case");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_LIVE").unwrap(),
        Some(Variable {
            value: b"first-target".to_vec(),
            kind: 0
        })
    );

    cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET Alias$Live SET ALIAS_LIVE changed-target",
    )
    .0
    .expect("*SET changes the live alias");
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*LIVE");
    result.expect("updated alias dispatch succeeds");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_LIVE").unwrap(),
        Some(Variable {
            value: b"changed-target".to_vec(),
            kind: 0
        })
    );
    let (_, output) = cli(&mut dispatcher, &mut task, &display, "*SHOW Alias$*");
    assert!(
        output.contains("Alias$Live"),
        "live aliases appear in SHOW: {output:?}"
    );
    let (result, help_commands) = cli(&mut dispatcher, &mut task, &display, "*HELP Commands");
    result.expect("command Help remains available with aliases present");
    assert!(
        !help_commands.contains("*LIVE"),
        "aliases are not registry command entries: {help_commands:?}"
    );
    let (_, output) = cli(&mut dispatcher, &mut task, &display, "*HELP ALIASES");
    assert!(output.to_ascii_lowercase().contains("alias"), "{output:?}");

    cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET Alias$Help SET ALIAS_HELP_SHADOW reached",
    )
    .0
    .expect("a variable can deliberately shadow a registered command");
    cli(&mut dispatcher, &mut task, &display, "*hElP")
        .0
        .expect("alias shadows the registry command");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_HELP_SHADOW").unwrap(),
        Some(Variable {
            value: b"reached".to_vec(),
            kind: 0
        })
    );

    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*%HELP");
    result.expect("leading percent bypasses alias and dispatches built-in Help");
    assert!(
        output.to_ascii_lowercase().contains("commands"),
        "{output:?}"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_HELP_SHADOW").unwrap(),
        Some(Variable {
            value: b"reached".to_vec(),
            kind: 0
        }),
        "bypass does not execute the shadow alias"
    );
    cli(&mut dispatcher, &mut task, &display, "*UNSET Alias$Help")
        .0
        .expect("the alias is removed through the public variable command");
    assert!(
        read_variable(&mut dispatcher, &mut task, "Alias$Help")
            .unwrap()
            .is_none()
    );

    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*UNSET Alias$Live");
    result.expect("the remaining alias can be deleted");
    assert!(
        read_variable(&mut dispatcher, &mut task, "Alias$Live")
            .unwrap()
            .is_none()
    );
    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*LIVE");
    assert!(
        result.is_err() || output.contains("Bad command"),
        "{output:?}"
    );
}

#[test]
fn aliases_expand_parameters_once_append_unused_arguments_and_chain() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0A_1102);
    bootstrap(&mut dispatcher, &mut task, &display);

    set_alias(&mut dispatcher, &mut task, "Raw", "SET ALIAS_RAW %*0");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "Alias$Raw").unwrap(),
        Some(Variable {
            value: b"SET ALIAS_RAW %*0".to_vec(),
            kind: 4
        }),
        "literal aliases preserve template percent bytes until dispatch"
    );
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*RAW alpha  beta");
    result.expect("raw argument suffix is substituted");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_RAW").unwrap(),
        Some(Variable {
            value: b"alpha  beta".to_vec(),
            kind: 0
        })
    );

    set_alias(&mut dispatcher, &mut task, "One", "SET ALIAS_ONE %0");
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*ONE \"two words\"");
    result.expect("quote-aware positional parameter runs through receiving command");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_ONE").unwrap(),
        Some(Variable {
            value: b"two words".to_vec(),
            kind: 0
        })
    );

    set_alias(
        &mut dispatcher,
        &mut task,
        "Missing",
        "SET ALIAS_MISSING %9",
    );
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*MISSING");
    result.expect("missing positional arguments substitute as empty strings");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_MISSING").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );

    set_alias(
        &mut dispatcher,
        &mut task,
        "MissingTail",
        "SET ALIAS_MISSING_TAIL %*9",
    );
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*MISSINGTAIL");
    result.expect("missing raw suffix substitutes as an empty string");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_MISSING_TAIL").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );

    set_alias(
        &mut dispatcher,
        &mut task,
        "Quoted",
        "SET ALIAS_UNMATCHED_QUOTE %0",
    );
    let (result, output) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*QUOTED \"unterminated",
    );
    assert!(
        result.is_err(),
        "unmatched argument quote rejects the alias call: {output:?}"
    );
    assert!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_UNMATCHED_QUOTE")
            .unwrap()
            .is_none()
    );

    set_alias(&mut dispatcher, &mut task, "Append", "SET ALIAS_APPEND %0");
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*APPEND first second");
    result.expect("unused arguments append after the highest referenced slot");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_APPEND").unwrap(),
        Some(Variable {
            value: b"first second".to_vec(),
            kind: 0
        })
    );

    set_alias(&mut dispatcher, &mut task, "Escape", "SET ALIAS_ESCAPE %%0");
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*ESCAPE actual");
    result.expect("escaped percent is literal in this substitution pass");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_ESCAPE").unwrap(),
        Some(Variable {
            value: b"%0 actual".to_vec(),
            kind: 0
        }),
        "unreferenced parameters append after the escaped-percent target"
    );

    set_alias(&mut dispatcher, &mut task, "Ten", "SET ALIAS_TEN %10");
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*TEN first second");
    result.expect("parameter numbers remain one digit");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_TEN").unwrap(),
        Some(Variable {
            value: b"second0".to_vec(),
            kind: 0
        }),
        "%10 substitutes %1 then copies the literal zero"
    );

    set_alias(
        &mut dispatcher,
        &mut task,
        "Unknown",
        "SET ALIAS_UNKNOWN %q",
    );
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*UNKNOWN");
    result.expect("unrecognized percent pair is copied literally");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_UNKNOWN").unwrap(),
        Some(Variable {
            value: b"%q".to_vec(),
            kind: 0
        })
    );

    set_alias(
        &mut dispatcher,
        &mut task,
        "EmptyArg",
        "SET ALIAS_EMPTY_ARG %0",
    );
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*EMPTYARG \"\"");
    result.expect("an empty quoted positional argument is present");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_EMPTY_ARG").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );

    set_alias(&mut dispatcher, &mut task, "ChainA", "ChainB %0");
    set_alias(&mut dispatcher, &mut task, "ChainB", "SET ALIAS_CHAIN %0");
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*CHAINA %3");
    result.expect("alias-to-alias dispatch retains arguments");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_CHAIN").unwrap(),
        Some(Variable {
            value: b"%3".to_vec(),
            kind: 0
        })
    );

    set_alias(
        &mut dispatcher,
        &mut task,
        "NoRescan",
        "SET ALIAS_NO_RESCAN %0",
    );
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*NORESCAN %8");
    result.expect("substitution output is not scanned again as an alias template");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_NO_RESCAN").unwrap(),
        Some(Variable {
            value: b"%8".to_vec(),
            kind: 0
        })
    );

    cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET ALIAS_SOURCE captured-before-dispatch",
    )
    .0
    .expect("type-0 expansion source is created");
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET Alias$Eager SET ALIAS_EAGER <ALIAS_SOURCE>",
    )
    .0
    .expect("type-0 alias target uses the existing immediate variable expansion");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "Alias$Eager").unwrap(),
        Some(Variable {
            value: b"SET ALIAS_EAGER captured-before-dispatch".to_vec(),
            kind: 0
        })
    );
    cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET ALIAS_SOURCE changed-after-definition",
    )
    .0
    .expect("type-0 alias expansion source can change later");
    cli(&mut dispatcher, &mut task, &display, "*EAGER")
        .0
        .expect("eagerly expanded alias dispatches its stored String value");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_EAGER").unwrap(),
        Some(Variable {
            value: b"captured-before-dispatch".to_vec(),
            kind: 0
        }),
        "aliases do not acquire general late CLI-variable expansion"
    );

    set_alias(
        &mut dispatcher,
        &mut task,
        "Plus+Mode",
        "SET ALIAS_PUNCTUATION accepted",
    );
    cli(&mut dispatcher, &mut task, &display, "*PLUS+MODE")
        .0
        .expect("documented punctuation is accepted in an alias command name");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_PUNCTUATION").unwrap(),
        Some(Variable {
            value: b"accepted".to_vec(),
            kind: 0
        })
    );

    set_alias(
        &mut dispatcher,
        &mut task,
        "UniqueAlpha",
        "SET ALIAS_UNIQUE unique-prefix",
    );
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*UNIQUEA.");
    result.expect("a unique final-dot alias abbreviation resolves");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_UNIQUE").unwrap(),
        Some(Variable {
            value: b"unique-prefix".to_vec(),
            kind: 0
        })
    );
}

#[test]
fn aliases_reject_ambiguous_empty_cyclic_deep_and_oversized_expansions_atomically() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0A_1103);
    bootstrap(&mut dispatcher, &mut task, &display);

    set_alias(&mut dispatcher, &mut task, "Abacus", "ECHO first");
    set_alias(&mut dispatcher, &mut task, "About", "ECHO second");
    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*AB.");
    assert!(
        result.is_err(),
        "ambiguous alias abbreviation must error: {output:?}"
    );

    set_alias(&mut dispatcher, &mut task, "Empty", "");
    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*EMPTY");
    assert!(
        result.is_err(),
        "empty aliases must fail clearly: {output:?}"
    );

    set_alias(&mut dispatcher, &mut task, "CycleA", "CycleB");
    set_alias(&mut dispatcher, &mut task, "CycleB", "CycleA");
    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*CYCLEA");
    assert!(result.is_err(), "alias cycle must fail: {output:?}");

    for (name, target) in [
        ("BadStar", "SET ALIAS_BAD_STAR %*"),
        ("BadStarX", "SET ALIAS_BAD_STAR_X %*x"),
    ] {
        set_alias(&mut dispatcher, &mut task, name, target);
        let (result, output) = cli(
            &mut dispatcher,
            &mut task,
            &display,
            &format!("*{name} value"),
        );
        assert!(
            result.is_err(),
            "malformed %* placeholder must reject: {output:?}"
        );
    }
    assert!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_BAD_STAR")
            .unwrap()
            .is_none()
    );
    assert!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_BAD_STAR_X")
            .unwrap()
            .is_none()
    );

    let chain = [
        "DepthA", "DepthB", "DepthC", "DepthD", "DepthE", "DepthF", "DepthG", "DepthH", "DepthI",
    ];
    for pair in chain.windows(2) {
        set_alias(&mut dispatcher, &mut task, pair[0], pair[1]);
    }
    set_alias(
        &mut dispatcher,
        &mut task,
        "DepthI",
        "SET ALIAS_DEPTH bounded-depth",
    );
    let (result, _) = cli(&mut dispatcher, &mut task, &display, "*DEPTHB");
    result.expect("eight active alias expansions are within the hosted bound");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_DEPTH").unwrap(),
        Some(Variable {
            value: b"bounded-depth".to_vec(),
            kind: 0
        })
    );
    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*DEPTHA");
    assert!(
        result.is_err(),
        "ninth active alias expansion must reject: {output:?}"
    );

    let long_target = format!("SET ALIAS_LIMIT {}", "x".repeat(239));
    set_variable(
        &mut dispatcher,
        &mut task,
        "Alias$Long",
        long_target.as_bytes(),
        4,
    )
    .expect("literal alias target fits the variable store");
    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*LONG");
    result.expect("a 255-byte ASCII expanded command is accepted");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_LIMIT").unwrap(),
        Some(Variable {
            value: b"x".repeat(239),
            kind: 0
        }),
        "the complete 255-byte command executes without truncation: {output:?}"
    );

    let multibyte_target = format!("SET ALIAS_UNICODE {}%0", "x".repeat(220));
    set_variable(
        &mut dispatcher,
        &mut task,
        "Alias$Unicode",
        multibyte_target.as_bytes(),
        4,
    )
    .expect("multibyte literal target fits the variable store");
    let (result, output) = cli(&mut dispatcher, &mut task, &display, "*UNICODE ééééééééé");
    assert!(
        result.is_err(),
        "255-byte limit counts UTF-8 bytes: {output:?}"
    );

    let (healthy, output) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET ALIAS_AFTER_FAILURE healthy",
    );
    healthy.expect("alias errors release command scratch and restore dispatch context");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "ALIAS_AFTER_FAILURE").unwrap(),
        Some(Variable {
            value: b"healthy".to_vec(),
            kind: 0
        }),
        "healthy command succeeds after alias errors; output={output:?}"
    );
}

#[test]
fn aliases_keep_caller_authority_obey_provenance_and_stdio_input() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut trusted = Task::trusted_mos_session(0x0A_1104);
    bootstrap(&mut dispatcher, &mut trusted, &display);
    set_alias(
        &mut dispatcher,
        &mut trusted,
        "Guarded",
        "SET ALIAS_GUARDED forged",
    );

    let mut ordinary = Task::new(0x0A_1204);
    let (result, _) = cli(&mut dispatcher, &mut ordinary, &display, "*GUARDED");
    assert!(
        result.is_err(),
        "an alias cannot lend the trusted writer's authority"
    );
    assert!(
        read_variable(&mut dispatcher, &mut ordinary, "ALIAS_GUARDED")
            .unwrap()
            .is_none()
    );

    environment.write_script(
        "alias-denied",
        b"*SHOW Alias$Guarded\n*RMEnsure AliasNeverInstalled 1.00 *GUARDED\n*SHOW Alias$Guarded\n",
    );
    let guest_path = environment.guest_path("alias-denied");
    let (result, output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display,
        &format!("*OBEY {guest_path}"),
    );
    let error = result
        .expect_err("nested alias retains ordinary caller authority and Obey context")
        .to_string();
    assert!(
        error.contains("HostFS::DemoDisk.$.alias-denied:2"),
        "source path/line survives alias dispatch: {error}"
    );
    assert!(
        !error.contains(&environment.root.display().to_string()),
        "alias source errors do not expose host paths: {error}"
    );
    assert_eq!(
        output.matches("Alias$Guarded").count(),
        1,
        "prior effect remains and later command stops after nested alias error: {output:?}"
    );
    let (healthy, _) = cli(
        &mut dispatcher,
        &mut trusted,
        &display,
        "*SET ALIAS_AFTER_OBEY_ERROR recovered",
    );
    healthy.expect("source and alias contexts unwind after nested error");

    let stdio_root = environment.root.join("stdio-volume");
    fs::create_dir_all(&stdio_root).unwrap();
    let input = b"*SET Alias$QUIT SET ALIAS_QUIT_SHADOW still-running\n*QUIT\n*SHOW ALIAS_QUIT_SHADOW\n*%QUIT\n*SET ALIAS_AFTER_QUIT wrong\n";
    let output = run_stdio(
        &environment.root.join("stdio.configure"),
        &stdio_root,
        input,
    );
    assert!(output.status.success(), "stdio failed: {output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ALIAS_QUIT_SHADOW"),
        "alias shadows QUIT in stdio: {stdout}"
    );
    assert!(
        stdout.contains("still-running"),
        "shadow target ran: {stdout}"
    );
    assert!(
        !stdout.contains("ALIAS_AFTER_QUIT"),
        "%QUIT bypassed alias and stopped stdio: {stdout}"
    );
}
