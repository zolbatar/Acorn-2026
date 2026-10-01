use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
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
        let root = std::env::temp_dir().join(format!("ricochet-obey-parameters-{nonce}"));
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

    fn host_path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn write_script(&self, name: &str, contents: &[u8]) {
        let path = self.host_path(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn guest_path(&self, name: &str) -> String {
        let guest_name = name.replace('/', ".");
        format!("HostFS::DemoDisk.$.{guest_name}")
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
        "command scratch released after {command:?}; output={output:?}"
    );
    (result, output)
}

fn obey(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    display: &mpsc::Receiver<DisplayEvent>,
    guest_path: &str,
    arguments: &str,
) -> (Result<(), RuntimeError>, String) {
    let path = if guest_path.contains(' ') {
        format!("\"{guest_path}\"")
    } else {
        guest_path.to_owned()
    };
    let tail = if arguments.is_empty() {
        String::new()
    } else {
        format!(" {arguments}")
    };
    cli(dispatcher, task, display, &format!("*OBEY {path}{tail}"))
}

#[derive(Debug, Eq, PartialEq)]
struct Variable {
    value: Vec<u8>,
    kind: u32,
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

fn run_stdio(config: &Path, volume: &Path, input: &[u8]) -> std::process::Output {
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
fn obey_substitutes_bounded_argument_forms_once_and_preserves_quotes_and_raw_tail() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0B_A001);
    cli(&mut dispatcher, &mut task, &display, "*HELP S.")
        .0
        .expect("CLI startup publishes System SWIs before direct register checks");

    environment.write_script(
        "parameters",
        b"*SET OBEY_PARAM_0 %0\n*SET OBEY_PARAM_1 %1\n*SET OBEY_PARAM_2 %2\n*SET OBEY_PARAM_4 %4\n*SET OBEY_PARAM_9 %9\n*SET OBEY_RAW_0 \"%*0\"\n*SET OBEY_RAW_1 \"%*1\"\n*SET OBEY_RAW_4 \"%*4\"\n*SET OBEY_RAW_9 \"%*9\"\n*SET OBEY_MISSING %7\n*SET OBEY_TEN %10\n*SET OBEY_ESCAPED %%0\n*SET OBEY_UNRESCANNED %2\n*SET OBEY_PIPE left||right\n",
    );
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("parameters"),
        "alpha  beta %3 tail",
    );
    result.expect("space-delimited arguments and raw suffix substitution run");

    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_PARAM_0").unwrap(),
        Some(Variable {
            value: b"alpha".to_vec(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_PARAM_1").unwrap(),
        Some(Variable {
            value: b"beta".to_vec(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_PARAM_2").unwrap(),
        Some(Variable {
            value: b"%3".to_vec(),
            kind: 0
        }),
        "argument contents are not rescanned as a template"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_PARAM_4").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        }),
        "missing arguments substitute as an empty string"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_PARAM_9").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_RAW_0").unwrap(),
        Some(Variable {
            value: b"alpha  beta %3 tail".to_vec(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_RAW_1").unwrap(),
        Some(Variable {
            value: b"beta %3 tail".to_vec(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_RAW_4").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_RAW_9").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_MISSING").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_TEN").unwrap(),
        Some(Variable {
            value: b"beta0".to_vec(),
            kind: 0
        }),
        "%10 is parsed as %1 followed by literal 0"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_ESCAPED").unwrap(),
        Some(Variable {
            value: b"%0".to_vec(),
            kind: 0
        }),
        "%% escapes the percent and the result is not rescanned"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_UNRESCANNED").unwrap(),
        Some(Variable {
            value: b"%3".to_vec(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_PIPE").unwrap(),
        Some(Variable {
            value: b"left|right".to_vec(),
            kind: 0
        }),
        "existing CLI pipe escape behavior remains intact"
    );

    environment.write_script(
        "quoted-parameters",
        b"*SET OBEY_QUOTED_ARG %1\n*SET OBEY_QUOTED_EMPTY %2\n",
    );
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("quoted-parameters"),
        "first \"two words\" \"\"",
    );
    result.expect("quoted and empty quoted parameters are grouped");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_QUOTED_ARG").unwrap(),
        Some(Variable {
            value: b"two words".to_vec(),
            kind: 0
        }),
        "downstream SET applies its documented quotes to the substituted text"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_QUOTED_EMPTY").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        }),
        "an empty quoted token is present and becomes an empty SET value"
    );

    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("parameters"),
        "zero one two three four five six seven eight ten",
    );
    result.expect("the tenth positional argument is addressable as %9");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_PARAM_9").unwrap(),
        Some(Variable {
            value: b"ten".to_vec(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_RAW_9").unwrap(),
        Some(Variable {
            value: b"ten".to_vec(),
            kind: 0
        }),
        "%*9 retains the raw suffix beginning at the tenth token"
    );

    environment.write_script("no-args", b"*SET OBEY_NO_ARGS %0\n*SET OBEY_NO_TAIL %*0\n");
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("no-args"),
        "",
    );
    result.expect("scripts with no argument tail run");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_NO_ARGS").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_NO_TAIL").unwrap(),
        Some(Variable {
            value: Vec::new(),
            kind: 0
        })
    );
}

#[test]
fn obey_rejects_ambiguous_or_oversized_parameter_expansion_before_dispatch() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0B_A002);
    cli(&mut dispatcher, &mut task, &display, "*HELP S.")
        .0
        .expect("CLI startup publishes System SWIs before direct register checks");

    environment.write_script(
        "malformed",
        b"*SET OBEY_MALFORMED_PREFIX kept\n*SET OBEY_MALFORMED_TARGET %*\n*SET OBEY_MALFORMED_LATER wrong\n",
    );
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("malformed"),
        "one",
    );
    let error = result.expect_err("incomplete %* is rejected").to_string();
    assert!(error.contains("HostFS::DemoDisk.$.malformed:2"), "{error}");
    assert!(
        error.contains("parameter"),
        "missing parameter cause: {error}"
    );
    let prefix_value = read_variable(&mut dispatcher, &mut task, "OBEY_MALFORMED_PREFIX").unwrap();
    assert!(
        prefix_value.is_some(),
        "prior complete lines retain their side effects; prefix={prefix_value:?}; error={error}"
    );
    assert!(
        read_variable(&mut dispatcher, &mut task, "OBEY_MALFORMED_TARGET")
            .unwrap()
            .is_none()
    );
    assert!(
        read_variable(&mut dispatcher, &mut task, "OBEY_MALFORMED_LATER")
            .unwrap()
            .is_none()
    );

    environment.write_script(
        "unmatched-quote",
        b"*SET OBEY_QUOTE_PREFIX kept\n*SET OBEY_QUOTE_TARGET %0\n",
    );
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("unmatched-quote"),
        "\"unterminated",
    );
    assert!(result.is_err(), "unmatched argument quotes are rejected");
    assert!(
        read_variable(&mut dispatcher, &mut task, "OBEY_QUOTE_PREFIX")
            .unwrap()
            .is_none(),
        "invocation is rejected before any line executes"
    );

    environment.write_script(
        "e",
        b"*SET OBEY_EXPANSION_TARGET prefix-prefix-%0-suffix\n*SET OBEY_EXPANSION_LATER wrong\n",
    );
    let too_long = "x".repeat(220);
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("e"),
        &too_long,
    );
    let error = result
        .expect_err("expanded command is bounded by UTF-8 bytes")
        .to_string();
    assert!(error.contains("HostFS::DemoDisk.$.e:1"), "{error}");
    assert!(error.contains("255") || error.contains("limit"), "{error}");
    assert!(
        read_variable(&mut dispatcher, &mut task, "OBEY_EXPANSION_TARGET")
            .unwrap()
            .is_none(),
        "oversized line is rejected without truncation or dispatch"
    );
    assert!(
        read_variable(&mut dispatcher, &mut task, "OBEY_EXPANSION_LATER")
            .unwrap()
            .is_none()
    );

    let (healthy, _) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*SET OBEY_AFTER_BAD yes",
    );
    healthy.expect("source and command state are clean after parameter failures");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_AFTER_BAD").unwrap(),
        Some(Variable {
            value: b"yes".to_vec(),
            kind: 0
        })
    );
}

#[test]
fn obey_directory_is_nested_read_only_guest_scope_restored_after_success_error_and_quit() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0B_A003);
    cli(&mut dispatcher, &mut task, &display, "*HELP S.")
        .0
        .expect("CLI startup publishes System SWIs before direct register checks");

    set_variable(&mut dispatcher, &mut task, "Obey$Dir", b"prior-string", 0).unwrap();
    environment.write_script(
        "parent/outer",
        b"*SET OBEY_PARENT_DIR <Obey$Dir>\n*OBEY HostFS::DemoDisk.$.parent.child.inner %1\n*SET OBEY_PARENT_RESUMED <Obey$Dir>\n*SET OBEY_PARENT_ARG %0\n",
    );
    environment.write_script(
        "parent/child/inner",
        b"*SET OBEY_CHILD_DIR <Obey$Dir>\n*SET OBEY_CHILD_ARG %0\n",
    );
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("parent/outer"),
        "parent-sentinel child-sentinel",
    );
    result.expect("nested scripts complete");
    let parent_dir = b"HostFS::DemoDisk.$.parent".to_vec();
    let child_dir = b"HostFS::DemoDisk.$.parent.child".to_vec();
    for name in ["OBEY_PARENT_DIR", "OBEY_PARENT_RESUMED"] {
        assert_eq!(
            read_variable(&mut dispatcher, &mut task, name).unwrap(),
            Some(Variable {
                value: parent_dir.clone(),
                kind: 0
            }),
            "the nested frame's directory is popped before parent resumes"
        );
    }
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_CHILD_DIR").unwrap(),
        Some(Variable {
            value: child_dir,
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_PARENT_ARG").unwrap(),
        Some(Variable {
            value: b"parent-sentinel".to_vec(),
            kind: 0
        })
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_CHILD_ARG").unwrap(),
        Some(Variable {
            value: b"child-sentinel".to_vec(),
            kind: 0
        }),
        "nested invocation gets its own args while the outer args are restored"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "Obey$Dir").unwrap(),
        Some(Variable {
            value: b"prior-string".to_vec(),
            kind: 0
        }),
        "preexisting variable is restored after success"
    );

    set_variable(&mut dispatcher, &mut task, "Obey$Dir", b"prior-type4", 4).unwrap();
    environment.write_script(
        "failure",
        b"*SET OBEY_FAILURE_DIR <Obey$Dir>\n*NO_SUCH_COMMAND\n",
    );
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("failure"),
        "",
    );
    let error = result
        .expect_err("script error unwinds the directory scope")
        .to_string();
    assert!(error.contains("HostFS::DemoDisk.$.failure:2"), "{error}");
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "Obey$Dir").unwrap(),
        Some(Variable {
            value: b"prior-type4".to_vec(),
            kind: 4
        }),
        "preexisting typed variable is restored after failure"
    );

    // A fresh runtime has no backing Obey$Dir value, so this also checks that
    // the frame overlay disappears back to true absence rather than leaving
    // the last guest directory in the shared variable store.
    let (mut fresh_dispatcher, fresh_display) = self::dispatcher();
    let mut no_prior_task = Task::new(0x0B_A203);
    cli(
        &mut fresh_dispatcher,
        &mut no_prior_task,
        &fresh_display,
        "*HELP S.",
    )
    .0
    .expect("fresh runtime publishes System SWIs");
    environment.write_script("no-prior", b"*SHOW Obey$Dir\n*QUIT\n");
    let (result, directory_output) = obey(
        &mut fresh_dispatcher,
        &mut no_prior_task,
        &fresh_display,
        &environment.guest_path("no-prior"),
        "",
    );
    result.expect("ordinary caller can execute a read-only script and QUIT");
    assert!(
        directory_output.contains("Obey$Dir (String): HostFS::DemoDisk.$"),
        "active directory is visible as a string to *SHOW: {directory_output:?}"
    );
    assert_eq!(
        read_variable(&mut fresh_dispatcher, &mut no_prior_task, "Obey$Dir").unwrap(),
        None,
        "the virtual directory is restored to absence after QUIT"
    );
    // QUIT is latched for a runtime; error unwinding therefore gets its own
    // fresh runtime and trusted caller (so line 1 can leave a visible effect).
    let (mut error_dispatcher, error_display) = self::dispatcher();
    let mut no_prior_error_task = Task::trusted_mos_session(0x0B_A204);
    cli(
        &mut error_dispatcher,
        &mut no_prior_error_task,
        &error_display,
        "*HELP S.",
    )
    .0
    .expect("second fresh runtime publishes System SWIs");
    environment.write_script(
        "no-prior-error",
        b"*SET OBEY_NO_PRIOR_ERROR_PREFIX kept\n*SET Obey$Dir forged\n*SET OBEY_NO_PRIOR_ERROR_LATER wrong\n",
    );
    let (result, _) = obey(
        &mut error_dispatcher,
        &mut no_prior_error_task,
        &error_display,
        &environment.guest_path("no-prior-error"),
        "",
    );
    let error = result
        .expect_err("a denied reserved-variable write unwinds the directory frame")
        .to_string();
    assert!(
        error.contains("HostFS::DemoDisk.$.no-prior-error:2"),
        "{error}"
    );
    assert!(
        read_variable(
            &mut error_dispatcher,
            &mut no_prior_error_task,
            "OBEY_NO_PRIOR_ERROR_PREFIX"
        )
        .unwrap()
        .is_some()
    );
    assert!(
        read_variable(
            &mut error_dispatcher,
            &mut no_prior_error_task,
            "OBEY_NO_PRIOR_ERROR_LATER"
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        read_variable(&mut error_dispatcher, &mut no_prior_error_task, "Obey$Dir").unwrap(),
        None,
        "an error also restores a previously absent virtual directory"
    );

    set_variable(&mut dispatcher, &mut task, "Obey$Dir", b"quit-prior", 0).unwrap();
    environment.write_script(
        "quit-scope",
        b"*SET OBEY_QUIT_DIR <Obey$Dir>\n*QUIT\n*SET OBEY_QUIT_LATER wrong\n",
    );
    let (result, _) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("quit-scope"),
        "",
    );
    result.expect("QUIT unwinds script and directory frame");
    assert!(
        read_variable(&mut dispatcher, &mut task, "OBEY_QUIT_LATER")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "Obey$Dir").unwrap(),
        Some(Variable {
            value: b"quit-prior".to_vec(),
            kind: 0
        })
    );

    let mut other_task = Task::new(0x0B_A103);
    assert_eq!(
        read_variable(&mut dispatcher, &mut other_task, "Obey$Dir").unwrap(),
        Some(Variable {
            value: b"quit-prior".to_vec(),
            kind: 0
        }),
        "no transient Obey directory leaks into a different task"
    );
}

#[test]
fn obey_directory_cannot_be_forged_by_script_and_stdio_keeps_queued_input() {
    let _lock = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0B_A004);
    cli(&mut dispatcher, &mut task, &display, "*HELP S.")
        .0
        .expect("CLI startup publishes System SWIs before direct register checks");
    set_variable(&mut dispatcher, &mut task, "Obey$Dir", b"stored-value", 0).unwrap();
    environment.write_script(
        "forged",
        b"*SET OBEY_FORGE_DIR <Obey$Dir>\n*SET Obey$Dir forged-value\n*SET OBEY_FORGE_AFTER <Obey$Dir>\n",
    );
    let (result, forge_output) = obey(
        &mut dispatcher,
        &mut task,
        &display,
        &environment.guest_path("forged"),
        "",
    );
    let error = result
        .expect_err("reserved virtual variable is not writable inside a script")
        .to_string();
    assert!(error.contains("HostFS::DemoDisk.$.forged:2"), "{error}");
    let expected = b"HostFS::DemoDisk.$".to_vec();
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "OBEY_FORGE_DIR").unwrap(),
        Some(Variable {
            value: expected,
            kind: 0
        }),
        "the source sees the protected virtual directory before the write attempt; output={forge_output:?}"
    );
    assert!(
        read_variable(&mut dispatcher, &mut task, "OBEY_FORGE_AFTER")
            .unwrap()
            .is_none(),
        "a rejected write stops later script lines"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut task, "Obey$Dir").unwrap(),
        Some(Variable {
            value: b"stored-value".to_vec(),
            kind: 0
        }),
        "script assignment cannot mutate the underlying stored value"
    );

    let mut ordinary = Task::new(0x0B_A104);
    environment.write_script(
        "ordinary-denied",
        b"*SET Obey$Dir forged\n*SET OBEY_ORDINARY_LATER wrong\n",
    );
    let (denied, _) = obey(
        &mut dispatcher,
        &mut ordinary,
        &display,
        &environment.guest_path("ordinary-denied"),
        "",
    );
    let error = denied
        .expect_err("ordinary Task retains its missing write authority")
        .to_string();
    assert!(
        error.contains("HostFS::DemoDisk.$.ordinary-denied:1"),
        "{error}"
    );
    assert_eq!(
        read_variable(&mut dispatcher, &mut ordinary, "Obey$Dir").unwrap(),
        Some(Variable {
            value: b"stored-value".to_vec(),
            kind: 0
        }),
        "ordinary caller sees only the shared stored value after unwind"
    );

    environment.write_script(
        "stdio-parameters",
        b"*SET OBEY_STDIO_ARG %0\n*SET OBEY_STDIO_DIR <Obey$Dir>\n",
    );
    let input = format!(
        "*OBEY {} queued-argument\r*SHOW OBEY_STDIO_ARG\r*SHOW OBEY_STDIO_DIR\r*SET OBEY_STDIO_QUEUED after-script\r*SHOW OBEY_STDIO_QUEUED\r*QUIT\r",
        environment.guest_path("stdio-parameters")
    );
    let output = run_stdio(
        &environment.host_path("stdio-configure"),
        &environment.root,
        input.as_bytes(),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("queued-argument"),
        "script arguments lost: {stdout}"
    );
    assert!(
        stdout.contains("after-script"),
        "queued shell input was consumed: {stdout}"
    );
    assert!(
        stdout.contains("HostFS::DemoDisk.$"),
        "stdio Obey$Dir missing: {stdout}"
    );
}
