use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiContext, SwiDispatcher},
};

const OS_CLI: u32 = 0x05;
const MODULE_LOOKUP: u32 = 0x4FF12;
const CLI_ADDRESS: u32 = 0x2100;
const MODULE_SELECTOR_ADDRESS: u32 = 0x3100;
const OLD_FILE_SCRATCH: u32 = 0x4000;
const OLD_CLI_SCRATCH_A: u32 = 0x5000;
const OLD_CLI_SCRATCH_B: u32 = 0x5800;

const PROBE_MODULE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE CompatibilityProbe 1.0.0
REM @SWI CompatibilityProbe_Ping &4FF78 Ping REGISTERS=R0:U32:OUT
DEF PROC Ping
    R0% = 78
ENDPROC
"#;

struct IsolatedEnvironment {
    root: PathBuf,
    config_path: PathBuf,
    old_config_path: Option<std::ffi::OsString>,
    old_demo_volume: Option<std::ffi::OsString>,
}

impl IsolatedEnvironment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ricochet-command-compat-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("configure");
        let old_config_path = std::env::var_os("RICOCHET_CONFIG_PATH");
        let old_demo_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        // This integration binary contains one test, and every persistent input
        // is isolated from the user's normal settings and demo volume.
        unsafe {
            std::env::set_var("RICOCHET_CONFIG_PATH", &config_path);
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
        }
        Self {
            root,
            config_path,
            old_config_path,
            old_demo_volume,
        }
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.old_config_path {
                std::env::set_var("RICOCHET_CONFIG_PATH", value);
            } else {
                std::env::remove_var("RICOCHET_CONFIG_PATH");
            }
            if let Some(value) = &self.old_demo_volume {
                std::env::set_var("RICOCHET_DEMO_VOLUME", value);
            } else {
                std::env::remove_var("RICOCHET_DEMO_VOLUME");
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_guest_module(root: &Path, file_stem: &str, source: &str) {
    fs::write(root.join(format!("{file_stem}.bas64")), source).unwrap();
    fs::write(
        root.join(format!("{file_stem}.bas64.ricochetmeta")),
        format!(
            "Ricochet file metadata v1\nformat-version=1\nguest-name={file_stem}\nfile-type=0x00000064\nload-address=0x00000000\nexecution-address=0x00000000\nattributes=0x00000000\n"
        ),
    )
    .unwrap();
}

fn new_dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender),
        display_receiver,
    )
}

fn put_string(task: &mut Task, address: u32, value: &str) {
    task.memory.write_bytes(address, value.as_bytes()).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn normalize_output(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace("\n\r", "\n")
        .replace('\r', "\n")
        .trim_end_matches('\n')
        .to_owned()
}

fn command_names_from_help(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let command = line.trim_start().strip_prefix('*')?;
            Some(
                command
                    .split_whitespace()
                    .next()
                    .expect("a Help command row has a name")
                    .to_ascii_uppercase(),
            )
        })
        .collect()
}

fn cli(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> (Result<(), RuntimeError>, String) {
    let _ = receiver.try_iter().count();
    let dynamic_areas_before = task.memory.dynamic_area_count();
    put_string(task, CLI_ADDRESS, command);
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    assert_eq!(
        context.registers[0], CLI_ADDRESS,
        "OS_CLI must preserve R0 for {command:?}"
    );
    assert_eq!(
        task.memory.dynamic_area_count(),
        dynamic_areas_before,
        "OS_CLI leaked command scratch for {command:?}"
    );
    let output = receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    (result, String::from_utf8_lossy(&output).into_owned())
}

fn module_is_published(dispatcher: &mut SwiDispatcher, task: &mut Task, title: &str) -> bool {
    put_string(task, MODULE_SELECTOR_ADDRESS, title);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = MODULE_SELECTOR_ADDRESS;
    dispatcher
        .dispatch(MODULE_LOOKUP, task, &mut context)
        .expect("read-only module lookup is public");
    context.registers[1] != 0
}

fn run_stdio(root: &Path, config_path: &Path, input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ricochet"));
    child
        .arg("--stdio")
        .env("RICOCHET_CONFIG_PATH", config_path)
        .env("RICOCHET_DEMO_VOLUME", root)
        .env_remove("RICOCHET_BOOT_CAPSULE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child.spawn().expect("spawn the public stdio runtime");
    child
        .stdin
        .take()
        .expect("child stdin is piped")
        .write_all(input)
        .expect("write a bounded command sequence");
    child
        .wait_with_output()
        .expect("wait for the public stdio runtime")
}

fn help_prefix(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    prefix: &str,
    expected_names: &[&str],
) {
    let command = format!("*HELP {prefix}");
    let (result, output) = cli(dispatcher, task, receiver, &command);
    result.expect("Help prefix lookup is read-only");
    let names = command_names_from_help(&output);
    assert_eq!(
        names,
        expected_names
            .iter()
            .map(|name| name.to_ascii_uppercase())
            .collect::<Vec<_>>(),
        "Help must list every matching command in the active execution order for {prefix:?}: {output:?}"
    );
}

#[test]
fn public_cli_preserves_legacy_prefix_priority_and_module_command_identity() {
    let environment = IsolatedEnvironment::new();
    write_guest_module(&environment.root, "CompatibilityProbe", PROBE_MODULE);

    let (mut dispatcher, display_receiver) = new_dispatcher();
    let mut ordinary = Task::new(0xCC_0101);
    for (address, byte) in [
        (OLD_FILE_SCRATCH, 0xA1),
        (OLD_CLI_SCRATCH_A, 0xA2),
        (OLD_CLI_SCRATCH_B, 0xA3),
    ] {
        ordinary.memory.write_bytes(address, &[byte; 32]).unwrap();
    }
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);

    // The public Help query lists every prefix match. Execution uses its first
    // row, preserving the established MOS command-routing order.
    help_prefix(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "BA.",
        &["BASIC", "BASIC64"],
    );
    help_prefix(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "D.",
        &["DIR", "DELETE", "DISC", "DESKTOP"],
    );
    help_prefix(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "F.",
        &["FILETYPE", "FX"],
    );
    help_prefix(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "R.",
        &[
            "RUN",
            "RMLOAD",
            "RMRUN",
            "RMKILL",
            "RMENSURE",
            "RMREINIT",
            "RMINSERT",
            "RMTIDY",
            "RMCLEAR",
            "RMFASTER",
            "ROMMODULES",
            "RENAME",
        ],
    );

    for (command, expected) in [
        ("*BA.", "Syntax: *BASIC <file>"),
        ("*BASIC.", "Syntax: *BASIC <file>"),
        ("*BASIC", "Syntax: *BASIC <file>"),
        (
            "*BASIC64",
            "Syntax: *BASIC64 [--mode CLASSIC|BASIC64|HYBRID] [--text CLASSIC|MODERN] [--override] <file>",
        ),
        ("*F.", "Syntax: *FILETYPE <file> <type>"),
        ("*R.", "Syntax: RUN <file.bas64|bas|txt|asc|bbc>"),
    ] {
        let (result, output) = cli(&mut dispatcher, &mut ordinary, &display_receiver, command);
        result.expect("a syntax-only command route is a normal CLI completion");
        assert_eq!(
            normalize_output(&output),
            expected,
            "{command:?} must execute the first Help-listed command, not a later prefix collision"
        );
    }

    let (dir_result, dir_abbrev) = cli(&mut dispatcher, &mut ordinary, &display_receiver, "*D.");
    let (dir_exact_result, dir_exact) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*DIR");
    dir_result.expect("*D. must route to the existing DIR command");
    dir_exact_result.expect("*DIR must remain callable");
    assert_eq!(normalize_output(&dir_abbrev), normalize_output(&dir_exact));
    assert!(!dispatcher.desktop_requested());

    let (desktop_result, desktop_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*DESKTOP",
    );
    assert!(
        matches!(desktop_result, Err(RuntimeError::Program(ref message)) if message.contains("DESKTOP requires the windowed host")),
        "exact *DESKTOP must select Desktop (without starting a UI in this headless test): {desktop_result:?}, {desktop_output:?}"
    );
    assert!(!dispatcher.desktop_requested());

    // Each unsupported module command uses its selected descriptor's canonical
    // spelling, not a stale name from a previous or nested interpreter frame.
    let unsupported = [
        ("*ROMModules", "ROMMODULES"),
        ("*romm.", "ROMMODULES"),
        ("*RMRE.", "RMREINIT"),
        ("*RMIN.", "RMINSERT"),
        ("*RMT.", "RMTIDY"),
        ("*RMC.", "RMCLEAR"),
        ("*RMF.", "RMFASTER"),
        ("*UNP.", "UNPLUG"),
    ];
    for (command, canonical_name) in unsupported {
        let (result, output) = cli(&mut dispatcher, &mut ordinary, &display_receiver, command);
        result.expect("unsupported native module operations return explicit CLI feedback");
        assert_eq!(
            normalize_output(&output),
            format!(
                "Unsupported *{canonical_name}: the hosted module manager has no native ROM/RMA implementation; no state changed."
            ),
            "{command:?} lost or misreported its registry-selected identity"
        );
    }

    let (nested_result, nested_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RMEnsure MissingCompatibilityModule 1.0.0 *ROMModules",
    );
    nested_result.expect("RMEnsure dispatches its fallback command in the current CLI context");
    assert_eq!(
        normalize_output(&nested_output),
        "Unsupported *ROMMODULES: the hosted module manager has no native ROM/RMA implementation; no state changed."
    );
    let (after_nested_result, after_nested_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RMFaster",
    );
    after_nested_result.expect("the next top-level handler still has its own identity");
    assert_eq!(
        normalize_output(&after_nested_output),
        "Unsupported *RMFASTER: the hosted module manager has no native ROM/RMA implementation; no state changed."
    );

    // A help/inspection caller is still an ordinary task. Direct and nested
    // management attempts are denied and cannot publish the valid guest module.
    let (modules_before_result, modules_before) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT MODULES",
    );
    modules_before_result.expect("public module inventory is readable");
    let (denied_load, denied_load_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RMLoad CompatibilityProbe",
    );
    assert!(
        matches!(
            denied_load,
            Err(RuntimeError::Structured {
                ref type_name,
                code: 2,
                ..
            }) if type_name == "TaskAuthorizationDenied"
        ) || denied_load_output
            .to_ascii_lowercase()
            .contains("authority"),
        "ordinary task unexpectedly loaded a module: {denied_load:?}, {denied_load_output:?}"
    );
    let (nested_denied_load, nested_denied_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RMEnsure MissingCompatibilityModule 1.0.0 *RMLoad CompatibilityProbe",
    );
    assert!(
        matches!(
            nested_denied_load,
            Err(RuntimeError::Structured {
                ref type_name,
                code: 2,
                ..
            }) if type_name == "TaskAuthorizationDenied"
        ) || nested_denied_output
            .to_ascii_lowercase()
            .contains("authority"),
        "nested command laundered module-management authority: {nested_denied_load:?}, {nested_denied_output:?}"
    );
    assert!(
        !module_is_published(&mut dispatcher, &mut ordinary, "CompatibilityProbe"),
        "denied direct/nested loads must not publish the candidate"
    );
    let (modules_after_result, modules_after) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT MODULES",
    );
    modules_after_result.expect("public module inventory remains readable after denial");
    assert_eq!(
        normalize_output(&modules_after),
        normalize_output(&modules_before)
    );

    for (address, byte) in [
        (OLD_FILE_SCRATCH, 0xA1),
        (OLD_CLI_SCRATCH_A, 0xA2),
        (OLD_CLI_SCRATCH_B, 0xA3),
    ] {
        assert_eq!(
            ordinary.memory.read_bytes(address, 32).unwrap(),
            vec![byte; 32],
            "CLI routing clobbered old bridge/scratch memory at {address:#x}"
        );
    }
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert!(!environment.config_path.exists());

    // Repeat the priority and identity-sensitive routes through the actual
    // interactive OS_CLI/stdio path, not only direct dispatcher calls.
    let stdio = run_stdio(
        &environment.root,
        &environment.config_path,
        b"*HELP BA.\n*BA.\n*D.\n*F.\n*R.\n*ROMModules\n*RMEnsure MissingCompatibilityModule 1.0.0 *ROMModules\n*RMFaster\n*QUIT\n",
    );
    let stdio_text = String::from_utf8_lossy(&stdio.stdout).to_string()
        + &String::from_utf8_lossy(&stdio.stderr);
    assert!(
        stdio.status.success(),
        "stdio runtime failed: {stdio_text:?}"
    );
    for expected in [
        "*BASIC <file>",
        "*BASIC64 [options] <file>",
        "Syntax: *BASIC <file>",
        "Syntax: *FILETYPE <file> <type>",
        "Syntax: RUN <file.bas64|bas|txt|asc|bbc>",
        "Unsupported *ROMMODULES:",
        "Unsupported *RMFASTER:",
    ] {
        assert!(
            stdio_text.contains(expected),
            "public stdio route omitted {expected:?}: {stdio_text:?}"
        );
    }
    assert!(
        !stdio_text.contains("Unsupported *:"),
        "stdio invocation lost the selected command identity: {stdio_text:?}"
    );
    assert!(
        !stdio_text.contains("Bad command"),
        "a supported abbreviation fell through in stdio: {stdio_text:?}"
    );
}
