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
const RICOCHET_DEFINITION_SOURCE: u32 = 0x4FF15;
const CLI_ADDRESS: u32 = 0x2100;
const SOURCE_SELECTOR_ADDRESS: u32 = 0x3100;
const SOURCE_BUFFER_ADDRESS: u32 = 0x3200;
const FILE_BRIDGE_SENTINEL: u32 = 0x4000;
const OLD_SCRATCH_SENTINEL_A: u32 = 0x5000;
const OLD_SCRATCH_SENTINEL_B: u32 = 0x5800;
const RICOCHET_MODULE_LOOKUP: u32 = 0x4FF12;

// These fixtures intentionally publish colliding prefixes from two different
// modules. PRM command selection is first-match in module order, while *Help
// on the abbreviation reports every matching command.
const REGISTRY_ALPHA_V1: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE AlphaRegistry 1.0.0
REM @SWI RegistryAlpha_Probe &4FF70 Probe REGISTERS=R0:U32:OUT
REM @COMMAND ZedAlpha Commands PROC RunAlpha "ZedAlpha [tail]" "alpha descriptor v1"
REM @COMMAND ZedAlpine Commands PROC RunAlpine "ZedAlpine [tail]" "alpha second descriptor"
REM @COMMAND ZedFiles FileCommands PROC RunFile "ZedFiles <path>" "alpha file descriptor"
REM @PRIVATE PROC Hidden
DEF PROC Probe
    R0% = 71
ENDPROC
DEF PROC RunAlpha(tail$ AS STRING)
    PRINT "alpha-v1:"; tail$
ENDPROC
DEF PROC RunAlpine(tail$ AS STRING)
    PRINT "alpha-alpine:"; tail$
ENDPROC
DEF PROC RunFile(tail$ AS STRING)
    PRINT "alpha-file:"; tail$
ENDPROC
DEF PROC Hidden
    REM help-private-source-marker
ENDPROC
"#;

const REGISTRY_ALPHA_V2: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE AlphaRegistry 1.0.0
REM @SWI RegistryAlpha_Probe &4FF70 Probe REGISTERS=R0:U32:OUT
REM @COMMAND ZedAlpha Commands PROC RunAlpha "ZedAlpha [tail]" "alpha descriptor v2"
REM @COMMAND ZedAlpine Commands PROC RunAlpine "ZedAlpine [tail]" "alpha second descriptor"
REM @COMMAND ZedFiles FileCommands PROC RunFile "ZedFiles <path>" "alpha file descriptor"
REM @PRIVATE PROC Hidden
DEF PROC Probe
    R0% = 72
ENDPROC
DEF PROC RunAlpha(tail$ AS STRING)
    PRINT "alpha-v2:"; tail$
ENDPROC
DEF PROC RunAlpine(tail$ AS STRING)
    PRINT "alpha-alpine-v2:"; tail$
ENDPROC
DEF PROC RunFile(tail$ AS STRING)
    PRINT "alpha-file-v2:"; tail$
ENDPROC
DEF PROC Hidden
    REM help-private-source-marker-v2
ENDPROC
"#;

const REGISTRY_ALPHA_INCOMPATIBLE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE AlphaRegistry 1.0.0
REM @STATE NEWSTATE% UINT32
REM @SWI RegistryAlpha_Probe &4FF70 Probe REGISTERS=R0:U32:OUT
REM @COMMAND ZedAlpha Commands PROC RunAlpha "ZedAlpha [tail]" "alpha rejected descriptor"
REM @COMMAND ZedAlpine Commands PROC RunAlpine "ZedAlpine [tail]" "alpha second descriptor"
REM @COMMAND ZedFiles FileCommands PROC RunFile "ZedFiles <path>" "alpha file descriptor"
REM @PRIVATE PROC Hidden
DEF PROC Probe
    R0% = 99
ENDPROC
DEF PROC RunAlpha(tail$ AS STRING)
    PRINT "alpha-rejected:"; tail$
ENDPROC
DEF PROC RunAlpine(tail$ AS STRING)
    PRINT "alpha-alpine-rejected:"; tail$
ENDPROC
DEF PROC RunFile(tail$ AS STRING)
    PRINT "alpha-file-rejected:"; tail$
ENDPROC
DEF PROC Hidden
    REM help-private-source-marker-rejected
ENDPROC
"#;

const REGISTRY_BETA: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ZetaRegistry 1.0.0
REM @SWI RegistryBeta_Probe &4FF72 Probe REGISTERS=R0:U32:OUT
REM @COMMAND ZedAlpha Commands PROC RunShadow "ZedAlpha [tail]" "beta shadow descriptor"
REM @COMMAND ZedAlbatross Commands PROC RunBeta "ZedAlbatross [tail]" "beta descriptor"
DEF PROC Probe
    R0% = 73
ENDPROC
DEF PROC RunShadow(tail$ AS STRING)
    PRINT "beta-shadow:"; tail$
ENDPROC
DEF PROC RunBeta(tail$ AS STRING)
    PRINT "beta:"; tail$
ENDPROC
"#;

// A command is itself a valid module-owned manifest export; this fixture has
// no dummy SWI, symbol export, or Start hook.
const COMMAND_ONLY_MODULE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE CommandOnly 1.0.0
REM @COMMAND OnlyCommand Commands PROC RunOnly "OnlyCommand [tail]" "command-only guest descriptor"
DEF PROC RunOnly(tail$ AS STRING)
    PRINT "command-only:"; tail$
ENDPROC
"#;

const FORGED_COMMAND_BRIDGE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ForgedCommandBridge 1.0.0
REM @COMMAND ZedForged Commands BRIDGE ZedForged "ZedForged" "forged bridge"
"#;

const DUPLICATE_COMMANDS_IN_MODULE: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE DuplicateCommands 1.0.0
REM @SWI Duplicate_Probe &4FF73 Probe REGISTERS=R0:U32:OUT
REM @COMMAND ZedDuplicate Commands PROC First "ZedDuplicate" "first duplicate"
REM @COMMAND ZedDuplicate Commands PROC Second "ZedDuplicate" "second duplicate"
DEF PROC Probe
    R0% = 1
ENDPROC
DEF PROC First(tail$ AS STRING)
    PRINT "first duplicate"
ENDPROC
DEF PROC Second(tail$ AS STRING)
    PRINT "second duplicate"
ENDPROC
"#;

const MALFORMED_COMMAND_METADATA: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE MalformedCommandMetadata 1.0.0
REM @SWI Malformed_Probe &4FF74 Probe REGISTERS=R0:U32:OUT
REM @COMMAND ZedMalformed UnsupportedCategory PROC Run "ZedMalformed" "bad category"
DEF PROC Probe
    R0% = 1
ENDPROC
DEF PROC Run(tail$ AS STRING)
    PRINT "should never publish"
ENDPROC
"#;

struct IsolatedEnvironment {
    root: PathBuf,
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
            "ricochet-command-help-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let old_config_path = std::env::var_os("RICOCHET_CONFIG_PATH");
        let old_demo_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        // This integration binary has one test. Isolate both inputs before
        // constructing the dispatcher or any runtime-backed helper.
        unsafe {
            std::env::set_var("RICOCHET_CONFIG_PATH", root.join("isolated-configure"));
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
        }
        Self {
            root,
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

fn put_string(task: &mut Task, address: u32, value: &str) {
    task.memory.write_bytes(address, value.as_bytes()).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn read_string(task: &Task, address: u32, capacity: usize) -> String {
    String::from_utf8(task.memory.read_c_string(address, capacity).unwrap()).unwrap()
}

fn module_info(dispatcher: &mut SwiDispatcher, task: &mut Task, title: &str) -> bool {
    put_string(task, SOURCE_SELECTOR_ADDRESS, title);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = SOURCE_SELECTOR_ADDRESS;
    dispatcher
        .dispatch(RICOCHET_MODULE_LOOKUP, task, &mut context)
        .unwrap();
    context.registers[1] != 0
}

fn normalize_output(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace("\n\r", "\n")
        .replace('\r', "\n")
        .trim_end_matches('\n')
        .to_owned()
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
    let mut child = child.spawn().expect("spawn the public --stdio runtime");
    child
        .stdin
        .take()
        .expect("child stdin is piped")
        .write_all(input)
        .expect("write the bounded command sequence");
    child
        .wait_with_output()
        .expect("wait for public stdio runtime")
}

fn process_text(output: &Output) -> String {
    let mut bytes = output.stdout.clone();
    bytes.extend_from_slice(&output.stderr);
    String::from_utf8_lossy(&bytes).into_owned()
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
        "OS_CLI leaked task-local command scratch for {command:?}"
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

fn query_source(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    selector: &str,
) -> Result<String, RuntimeError> {
    put_string(task, SOURCE_SELECTOR_ADDRESS, selector);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = SOURCE_SELECTOR_ADDRESS;
    context.registers[3] = SOURCE_BUFFER_ADDRESS;
    context.registers[4] = 1024;
    context.registers[7] = SOURCE_BUFFER_ADDRESS + 0x500;
    context.registers[8] = 128;
    dispatcher.dispatch(RICOCHET_DEFINITION_SOURCE, task, &mut context)?;
    Ok(read_string(task, SOURCE_BUFFER_ADDRESS, 1024))
}

fn assert_success(result: Result<(), RuntimeError>, output: &str, command: &str) {
    assert!(
        result.is_ok(),
        "{command:?} returned {result:?}; output was {output:?}"
    );
    let lower = output.to_ascii_lowercase();
    assert!(
        !["bad command", "unsupported", "unknown command", "rom/rma"]
            .iter()
            .any(|fragment| lower.contains(fragment)),
        "{command:?} fell through into a legacy/error route: {output:?}"
    );
}

fn assert_help_has(output: &str, fragment: &str, command: &str) {
    assert!(
        output
            .to_ascii_lowercase()
            .contains(&fragment.to_ascii_lowercase()),
        "{command:?} output {output:?} did not contain {fragment:?}"
    );
}

fn assert_no_private_source(output: &str, command: &str) {
    assert!(
        !output.contains("help-private-source-marker"),
        "{command:?} leaked a definition body through Help: {output:?}"
    );
}

fn normalized_snapshot(header: &str, rows: &[&str]) -> String {
    std::iter::once(header)
        .chain(rows.iter().copied())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn help_and_command_registry_are_live_module_owned_and_read_only() {
    let environment = IsolatedEnvironment::new();
    write_guest_module(&environment.root, "RegistryAlphaV1", REGISTRY_ALPHA_V1);
    write_guest_module(&environment.root, "RegistryAlphaV2", REGISTRY_ALPHA_V2);
    write_guest_module(
        &environment.root,
        "RegistryAlphaIncompatible",
        REGISTRY_ALPHA_INCOMPATIBLE,
    );
    write_guest_module(&environment.root, "RegistryBeta", REGISTRY_BETA);
    write_guest_module(&environment.root, "CommandOnlyGuest", COMMAND_ONLY_MODULE);
    write_guest_module(
        &environment.root,
        "ForgedCommandBridge",
        FORGED_COMMAND_BRIDGE,
    );
    write_guest_module(
        &environment.root,
        "DuplicateCommands",
        DUPLICATE_COMMANDS_IN_MODULE,
    );
    write_guest_module(
        &environment.root,
        "MalformedCommandMetadata",
        MALFORMED_COMMAND_METADATA,
    );
    // Startup recovery is orthogonal to Help. This deliberately damaged file
    // belongs only to this unique test environment and is repaired explicitly.
    let config_path = environment.root.join("isolated-configure");
    let damaged_config = b"RICOCHET-CONFIG 3\nLanguage=0\ntruncated-row";
    fs::write(&config_path, damaged_config).unwrap();
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel::<DisplayEvent>();
    let mut dispatcher =
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    let mut mos = Task::trusted_mos_session(0xCA_0101);
    let mut ordinary = Task::new(0xCA_0102);
    let mut source_inspector = Task::trusted_source_inspector(0xCA_0103);

    assert_eq!(mos.memory.dynamic_area_count(), 0);
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);

    for task in [&mut mos, &mut ordinary, &mut source_inspector] {
        task.memory
            .write_bytes(OLD_SCRATCH_SENTINEL_A, &[0xA1; 32])
            .unwrap();
        task.memory
            .write_bytes(OLD_SCRATCH_SENTINEL_B, &[0xB2; 32])
            .unwrap();
        task.memory
            .write_bytes(FILE_BRIDGE_SENTINEL, &[0xD4; 32])
            .unwrap();
    }

    // RISC OS 3 User Guide: *Help without a topic lists command information;
    // a topic gives syntax/details; *Help on an abbreviation lists all
    // matches, while executing that abbreviation uses the first registered
    // command. PRM Module help tables own both command keywords and handlers.
    // See https://www.riscos.com/support/users/userguide3/book3b/book3_2.html
    // and https://www.riscos.com/support/developers/prm/modules.html.
    let (bare_result, bare_help) = cli(&mut dispatcher, &mut ordinary, &display_receiver, "*HELP");
    assert_success(bare_result, &bare_help, "*HELP");
    assert!(
        !bare_help.is_empty(),
        "bare Help returned no command inventory"
    );
    assert_no_private_source(&bare_help, "*HELP");
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_A, 32)
            .unwrap(),
        vec![0xA1; 32]
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_B, 32)
            .unwrap(),
        vec![0xB2; 32]
    );
    for task in [&mos, &ordinary, &source_inspector] {
        assert_eq!(
            task.memory.read_bytes(FILE_BRIDGE_SENTINEL, 32).unwrap(),
            vec![0xD4; 32],
            "a command/help route must not clobber caller memory at {FILE_BRIDGE_SENTINEL:#x}"
        );
    }

    let (commands_result, commands_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP Commands",
    );
    assert_success(commands_result, &commands_help, "*HELP Commands");
    let (files_result, files_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP FileCommands",
    );
    assert_success(files_result, &files_help, "*HELP FileCommands");
    let (modules_result, modules_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP Modules",
    );
    assert_success(modules_result, &modules_help, "*HELP Modules");
    let (syntax_result, syntax_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP Syntax",
    );
    assert_success(syntax_result, &syntax_help, "*HELP Syntax");
    let command_rows = [
        "  *HELP - Show commands, module versions, syntax, or matching command details",
        "  *CONFIGURE - Set supported Ricochet configuration",
        "  *STATUS - Show effective configuration values and recovery status",
        "  *SET - Create or update a guest-owned string system variable",
        "  *SHOW - Read guest-owned string system variables",
        "  *INSPECT - Read-only module, SWI, and retained-source inspection",
        "  *MODULES - List active BASIC64 modules and versions",
        "  *BASIC - Run BASIC with saved compatibility preferences",
        "  *BASIC64 - Run with native BASIC64 launch defaults",
        "  *RUN - Run BASIC source or tokenized BASIC",
        "  *RMLOAD - Load a BASIC64 module from a guest path",
        "  *RMRUN - Load a module; no separate application entry is hosted",
        "  *RMKILL - Unload an active BASIC64 module",
        "  *RMENSURE - Run a bounded command only when a module version is absent or too old",
        "  *RMREINIT - Requires native ROM and RMA module lifecycle support",
        "  *RMINSERT - Requires ROM module support",
        "  *RMTIDY - Requires native RMA compaction and module lifecycle support",
        "  *RMCLEAR - Requires native ROM module support",
        "  *RMFASTER - Requires native ROM module support",
        "  *ROMMODULES - Requires a ROM module inventory",
        "  *UNPLUG - Requires ROM unplug state",
        "  *UNSET - Delete matching guest-owned string system variables",
        "  *QUIT - Exit Ricochet",
        "  *OBEY - Execute bounded guest command text through OS_CLI",
        "  *EXEC - Select a bounded guest file as this task's input stream, or close it",
        "  *DESKTOP - Start Ricochet Desktop",
        "  *FX - Run a supported hosted OS_Byte command",
    ];
    let all_rows = [
        "  *HELP - Show commands, module versions, syntax, or matching command details",
        "  *CONFIGURE - Set supported Ricochet configuration",
        "  *STATUS - Show effective configuration values and recovery status",
        "  *SET - Create or update a guest-owned string system variable",
        "  *SHOW - Read guest-owned string system variables",
        "  *INSPECT - Read-only module, SWI, and retained-source inspection",
        "  *MODULES - List active BASIC64 modules and versions",
        "  *BASIC - Run BASIC with saved compatibility preferences",
        "  *BASIC64 - Run with native BASIC64 launch defaults",
        "  *RUN - Run BASIC source or tokenized BASIC",
        "  *RMLOAD - Load a BASIC64 module from a guest path",
        "  *RMRUN - Load a module; no separate application entry is hosted",
        "  *RMKILL - Unload an active BASIC64 module",
        "  *RMENSURE - Run a bounded command only when a module version is absent or too old",
        "  *RMREINIT - Requires native ROM and RMA module lifecycle support",
        "  *RMINSERT - Requires ROM module support",
        "  *RMTIDY - Requires native RMA compaction and module lifecycle support",
        "  *RMCLEAR - Requires native ROM module support",
        "  *RMFASTER - Requires native ROM module support",
        "  *ROMMODULES - Requires a ROM module inventory",
        "  *UNPLUG - Requires ROM unplug state",
        "  *UNSET - Delete matching guest-owned string system variables",
        "  *QUIT - Exit Ricochet",
        "  *OBEY - Execute bounded guest command text through OS_CLI",
        "  *EXEC - Select a bounded guest file as this task's input stream, or close it",
        "  *DIR - Select or report the current directory",
        "  *DELETE - Delete a file",
        "  *DISC - Read or set the volume name",
        "  *DESKTOP - Start Ricochet Desktop",
        "  *FILETYPE - Set a RISC OS file type",
        "  *FX - Run a supported hosted OS_Byte command",
        "  *CAT - Catalogue a directory; * is the CAT shortcut",
        "  *CDIR - Create a directory",
        "  *RENAME - Rename a file or directory",
        "  *TYPE - Display a text file",
        "  *HOSTFS - Select the hosted filing system",
    ];
    let file_rows = [
        "  *DIR - Select or report the current directory",
        "  *DELETE - Delete a file",
        "  *DISC - Read or set the volume name",
        "  *FILETYPE - Set a RISC OS file type",
        "  *CAT - Catalogue a directory; * is the CAT shortcut",
        "  *CDIR - Create a directory",
        "  *RENAME - Rename a file or directory",
        "  *TYPE - Display a text file",
        "  *HOSTFS - Select the hosted filing system",
    ];
    assert_eq!(
        normalize_output(&bare_help),
        normalized_snapshot(
            "Commands (use *Help <command>, *Help <module>, or *Help Syntax):",
            &all_rows,
        ),
        "bare *HELP must be exactly the module-owned registry output, with no appended fallback"
    );
    assert_eq!(
        normalize_output(&commands_help),
        normalized_snapshot("Commands:", &command_rows),
        "*HELP Commands must print the registered Commands category exactly once"
    );
    assert_eq!(
        normalize_output(&files_help),
        normalized_snapshot("File commands:", &file_rows),
        "*HELP FileCommands must print the registered FileCommands category exactly once"
    );
    assert_eq!(
        normalize_output(&modules_help),
        "MODULES\nBoot 1.0.0 Active\nColourTrans 1.0.0 Active\nConsole 1.0.0 Active\nDesktopServices 1.0.0 Active\nDisplayManager 1.0.0 Active\nError 1.0.0 Active\nFileSwitch 1.0.0 Active\nGraphics 1.0.0 Active\nMemory 1.0.0 Active\nModuleManager 1.0.0 Active\nMos 1.0.0 Active\nRicochetCommands 1.0.0 Active\nSystem 1.0.0 Active\nTaskManager 1.0.0 Active\nWimp 1.0.0 Active",
        "*HELP Modules must show real active hosted modules and no fabricated ROM/RMA rows"
    );
    let syntax_rows = [
        "  <value> is a required value to substitute; [value] is optional.",
        "  A vertical bar separates alternatives.",
        "  *Help [topic, command or module] - Show commands, module versions, syntax, or matching command details",
        "  *Configure <option> <value> - Set supported Ricochet configuration",
        "  *Status [option] - Show effective configuration values and recovery status",
        "  *Set <name> [value] - Create or update a guest-owned string system variable",
        "  *Show [pattern] - Read guest-owned string system variables",
        "  *Inspect MODULES / MODULE / SWI / DEFINITION ... - Read-only module, SWI, and retained-source inspection",
        "  *Modules - List active BASIC64 modules and versions",
        "  *BASIC <file> - Run BASIC with saved compatibility preferences",
        "  *BASIC64 [options] <file> - Run with native BASIC64 launch defaults",
        "  *RUN <file> - Run BASIC source or tokenized BASIC",
        "  *RMLoad <path> - Load a BASIC64 module from a guest path",
        "  *RMRun <path> - Load a module; no separate application entry is hosted",
        "  *RMKill <title> - Unload an active BASIC64 module",
        "  *RMEnsure <title> <version> [command] - Run a bounded command only when a module version is absent or too old",
        "  *RMReInit <title> [init-string] - Requires native ROM and RMA module lifecycle support",
        "  *RMInsert <title> [ROM-section] - Requires ROM module support",
        "  *RMTidy - Requires native RMA compaction and module lifecycle support",
        "  *RMClear <title> - Requires native ROM module support",
        "  *RMFaster - Requires native ROM module support",
        "  *ROMModules - Requires a ROM module inventory",
        "  *Unplug <module> - Requires ROM unplug state",
        "  *Unset <pattern> - Delete matching guest-owned string system variables",
        "  *QUIT - Exit Ricochet",
        "  *Obey <guest-path> - Execute bounded guest command text through OS_CLI",
        "  *Exec [guest-path] - Select a bounded guest file as this task's input stream, or close it",
        "  *DIR [directory] - Select or report the current directory",
        "  *DELETE <file> - Delete a file",
        "  *DISC [name] - Read or set the volume name",
        "  *DESKTOP - Start Ricochet Desktop",
        "  *FILETYPE <file> <type> - Set a RISC OS file type",
        "  *FX <parameters> - Run a supported hosted OS_Byte command",
        "  *CAT [directory] - Catalogue a directory; * is the CAT shortcut",
        "  *CDIR <directory> - Create a directory",
        "  *RENAME <old> <new> - Rename a file or directory",
        "  *TYPE <file> - Display a text file",
        "  *HOSTFS - Select the hosted filing system",
    ];
    assert_eq!(
        normalize_output(&syntax_help),
        normalized_snapshot(
            "Command syntax (final-dot abbreviations select the first matching command):",
            &syntax_rows,
        ),
        "*HELP Syntax must enumerate the full registered syntax set exactly once"
    );
    let (case_result, case_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*hElP fIlEcOmMaNdS",
    );
    assert_success(case_result, &case_help, "mixed-case *HELP");
    assert_eq!(normalize_output(&case_help), normalize_output(&files_help));
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);

    let (unknown_result, unknown_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP NoSuchHelpTopic",
    );
    assert!(
        unknown_result.is_err()
            || ["unknown", "not found", "no help", "no match"]
                .iter()
                .any(|marker| unknown_help.to_ascii_lowercase().contains(marker)),
        "unknown Help topic was silently accepted: {unknown_result:?}, {unknown_help:?}"
    );
    let (unknown_command_result, unknown_command_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*NoSuchRicochetCommand",
    );
    assert!(
        unknown_command_result.is_err()
            || unknown_command_output
                .to_ascii_lowercase()
                .contains("bad command"),
        "unknown command fell through into a filing-system/file-execution route: {unknown_command_result:?}, {unknown_command_output:?}"
    );
    let (obey_help_result, obey_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP OBEY",
    );
    assert_success(obey_help_result, &obey_help, "*HELP OBEY");
    assert_help_has(
        &obey_help,
        "*Obey <guest-path>",
        "bounded Obey subset syntax",
    );
    let (exec_help_result, exec_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP EXEC",
    );
    assert_success(exec_help_result, &exec_help, "*HELP EXEC");
    assert_help_has(
        &exec_help,
        "*Exec [guest-path]",
        "bounded Exec input-source syntax",
    );

    // Ordinary tasks may inspect public Help metadata but neither read retained
    // source nor gain authority to load modules/configuration from that view.
    put_string(
        &mut ordinary,
        SOURCE_SELECTOR_ADDRESS,
        "AlphaRegistry/Hidden",
    );
    ordinary
        .memory
        .write_bytes(SOURCE_BUFFER_ADDRESS, &[0xC3; 32])
        .unwrap();
    let mut denied_source = SwiContext::default();
    denied_source.registers[0] = 1;
    denied_source.registers[1] = SOURCE_SELECTOR_ADDRESS;
    denied_source.registers[3] = SOURCE_BUFFER_ADDRESS;
    denied_source.registers[4] = 1024;
    denied_source.registers[7] = SOURCE_BUFFER_ADDRESS + 0x500;
    denied_source.registers[8] = 128;
    let denied_source_result = dispatcher.dispatch(
        RICOCHET_DEFINITION_SOURCE,
        &mut ordinary,
        &mut denied_source,
    );
    assert!(matches!(
        denied_source_result,
        Err(RuntimeError::Structured {
            type_name,
            code: 1,
            ..
        }) if type_name == "TaskAuthorizationDenied"
    ));
    assert_eq!(
        ordinary
            .memory
            .read_bytes(SOURCE_BUFFER_ADDRESS, 32)
            .unwrap(),
        vec![0xC3; 32],
        "denied source access must not modify its protected output"
    );
    let (denied_load, denied_load_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RMLoad RegistryAlphaV1",
    );
    assert!(
        denied_load.is_err()
            || denied_load_output
                .to_ascii_lowercase()
                .contains("authority"),
        "Help must not confer module-management authority: {denied_load:?}, {denied_load_output:?}"
    );
    let (denied_config, denied_config_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*CONFIGURE Language 3",
    );
    assert!(
        denied_config.is_err()
            || denied_config_output
                .to_ascii_lowercase()
                .contains("authority"),
        "Help must not confer configuration-write authority: {denied_config:?}, {denied_config_output:?}"
    );
    assert!(!module_info(
        &mut dispatcher,
        &mut ordinary,
        "AlphaRegistry"
    ));

    // The public Help command uses bounded caller memory and short-lived
    // task-local scratch, even for rejected command lines.
    ordinary
        .memory
        .write_bytes(CLI_ADDRESS, &[b'X'; 256])
        .unwrap();
    let mut unterminated = SwiContext::default();
    unterminated.registers[0] = CLI_ADDRESS;
    assert!(matches!(
        dispatcher.dispatch(OS_CLI, &mut ordinary, &mut unterminated),
        Err(RuntimeError::Memory(_))
    ));
    assert_eq!(
        unterminated.registers[0], CLI_ADDRESS,
        "OS_CLI must preserve R0 even when its bounded command reader rejects input"
    );
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    let mut invalid_pointer = SwiContext::default();
    invalid_pointer.registers[0] = u32::MAX;
    assert!(matches!(
        dispatcher.dispatch(OS_CLI, &mut ordinary, &mut invalid_pointer),
        Err(RuntimeError::Memory(_))
    ));
    assert_eq!(
        invalid_pointer.registers[0],
        u32::MAX,
        "a failed command must not rewrite the caller's invalid R0"
    );
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);

    // A corrupted config file remains untouched while Help and public STATUS
    // are read-only; the explicit trusted DEFAULTS repair preserves a copy.
    let original_config = fs::read(&config_path).unwrap();
    let (status_result, status_output) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*STATUS");
    assert_success(status_result, &status_output, "*STATUS");
    assert_help_has(
        &status_output,
        "ConfigurationRecovery=MALFORMED_OR_TRUNCATED",
        "*STATUS recovery",
    );
    assert_eq!(fs::read(&config_path).unwrap(), original_config);
    let (repair_result, repair_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*CONFIGURE DEFAULTS",
    );
    assert_success(repair_result, &repair_output, "*CONFIGURE DEFAULTS");
    assert!(
        fs::read_dir(&environment.root)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains(".recovery-")),
        "authorized defaults should preserve the corrupt input before rewriting"
    );
    assert_eq!(mos.memory.dynamic_area_count(), 0);

    // Built-in command routes remain reachable through OS_CLI beside Help.
    for command in ["*INSPECT MODULES", "*Modules", "*STATUS Language"] {
        let (result, output) = cli(&mut dispatcher, &mut ordinary, &display_receiver, command);
        assert_success(result, &output, command);
        assert!(!output.is_empty(), "{command:?} produced no visible result");
        assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    }
    let (retired_trellis_result, retired_trellis_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*TRELLIS MODULES",
    );
    assert!(
        retired_trellis_result.is_err()
            || retired_trellis_output
                .to_ascii_lowercase()
                .contains("bad command"),
        "retired *TRELLIS alias unexpectedly remained public: {retired_trellis_result:?}, {retired_trellis_output:?}"
    );
    let (catalogue_result, catalogue_output) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*CAT");
    assert_success(catalogue_result, &catalogue_output, "*CAT");
    let (catalogue_alias_result, catalogue_alias_output) =
        cli(&mut dispatcher, &mut ordinary, &display_receiver, "*.");
    assert_success(catalogue_alias_result, &catalogue_alias_output, "*.");
    assert_eq!(
        normalize_output(&catalogue_alias_output),
        normalize_output(&catalogue_output),
        "the FileSwitch * shortcut must resolve to the registered CAT command"
    );
    let rename_source = environment.root.join("BridgeRenameSource");
    let rename_target = environment.root.join("BridgeRenameTarget");
    fs::write(&rename_source, b"rename fixture").unwrap();
    let (rename_result, rename_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*RENAME BridgeRenameSource BridgeRenameTarget",
    );
    assert_success(rename_result, &rename_output, "two-operand *RENAME bridge");
    assert!(!rename_source.exists());
    assert_eq!(fs::read(&rename_target).unwrap(), b"rename fixture");
    for (address, sentinel) in [(FILE_BRIDGE_SENTINEL, 0xD4), (OLD_SCRATCH_SENTINEL_A, 0xA1)] {
        assert_eq!(
            ordinary.memory.read_bytes(address, 32).unwrap(),
            vec![sentinel; 32],
            "two-operand file bridge clobbered caller memory at {address:#x}"
        );
    }
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);

    // Load modules with the trusted MOS principal; public Help must see their
    // descriptors at the same time OS_CLI can invoke the associated handlers.
    for path in ["RegistryAlphaV1", "RegistryBeta", "CommandOnlyGuest"] {
        let (result, output) = cli(
            &mut dispatcher,
            &mut mos,
            &display_receiver,
            &format!("*RMLoad {path}"),
        );
        assert_success(result, &output, &format!("*RMLoad {path}"));
        assert_eq!(mos.memory.dynamic_area_count(), 0);
    }
    assert!(module_info(&mut dispatcher, &mut ordinary, "AlphaRegistry"));
    assert!(module_info(&mut dispatcher, &mut ordinary, "ZetaRegistry"));
    assert!(
        module_info(&mut dispatcher, &mut ordinary, "CommandOnly"),
        "a valid @COMMAND-only module must publish without a dummy SWI"
    );
    let authorized_source = query_source(
        &mut dispatcher,
        &mut source_inspector,
        "AlphaRegistry/Hidden",
    )
    .expect("the owner source-inspector profile can read retained private source");
    assert!(authorized_source.contains("help-private-source-marker"));
    assert_eq!(source_inspector.memory.dynamic_area_count(), 0);

    let (command_only_help_result, command_only_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP OnlyCommand",
    );
    assert_success(
        command_only_help_result,
        &command_only_help,
        "Help for command-only module",
    );
    assert_eq!(
        normalize_output(&command_only_help),
        "  OnlyCommand [tail] - command-only guest descriptor",
        "a command-only module must publish a complete Help row"
    );
    let (command_only_exec_result, command_only_exec) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*OnlyCommand works",
    );
    assert_success(
        command_only_exec_result,
        &command_only_exec,
        "execution from command-only module",
    );
    assert_help_has(
        &command_only_exec,
        "command-only:works",
        "execution from command-only module",
    );

    let (prefix_help_result, prefix_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP ZedA.",
    );
    assert_success(prefix_help_result, &prefix_help, "*HELP ZedA.");
    assert_eq!(
        normalize_output(&prefix_help),
        "  ZedAlpha [tail] - alpha descriptor v1\n  ZedAlpine [tail] - alpha second descriptor\n  ZedAlpha [tail] - beta shadow descriptor\n  ZedAlbatross [tail] - beta descriptor",
        "Help abbreviation should enumerate all active prefix matches in registry order"
    );
    let (mixed_case_prefix_result, mixed_case_prefix_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP zEdA.",
    );
    assert_success(
        mixed_case_prefix_result,
        &mixed_case_prefix_help,
        "case-insensitive Help abbreviation",
    );
    assert_eq!(
        normalize_output(&mixed_case_prefix_help),
        normalize_output(&prefix_help),
        "Help prefix matching must be case-insensitive"
    );
    assert_no_private_source(&prefix_help, "*HELP ZedA.");

    let (all_result, all_help) = cli(&mut dispatcher, &mut ordinary, &display_receiver, "*HELP .");
    assert_success(all_result, &all_help, "*HELP .");
    let mut expected_all_help = vec![
        "  *ZedAlpha - alpha descriptor v1".to_owned(),
        "  *ZedAlpine - alpha second descriptor".to_owned(),
        "  *ZedFiles - alpha file descriptor".to_owned(),
        "  *OnlyCommand - command-only guest descriptor".to_owned(),
    ];
    expected_all_help.extend(
        normalize_output(&bare_help)
            .lines()
            .skip(1)
            .map(str::to_owned),
    );
    expected_all_help.extend([
        "  *ZedAlpha - beta shadow descriptor".to_owned(),
        "  *ZedAlbatross - beta descriptor".to_owned(),
    ]);
    assert_eq!(
        normalize_output(&all_help),
        expected_all_help.join("\n"),
        "*HELP . must enumerate every active module-owned command in registry order exactly once"
    );
    assert_no_private_source(&all_help, "*HELP .");

    let (all_modules_result, all_modules_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP Modules",
    );
    assert_success(
        all_modules_result,
        &all_modules_help,
        "*HELP Modules with guests",
    );
    assert_eq!(
        normalize_output(&all_modules_help),
        "MODULES\nAlphaRegistry 1.0.0 Active\nBoot 1.0.0 Active\nColourTrans 1.0.0 Active\nCommandOnly 1.0.0 Active\nConsole 1.0.0 Active\nDesktopServices 1.0.0 Active\nDisplayManager 1.0.0 Active\nError 1.0.0 Active\nFileSwitch 1.0.0 Active\nGraphics 1.0.0 Active\nMemory 1.0.0 Active\nModuleManager 1.0.0 Active\nMos 1.0.0 Active\nRicochetCommands 1.0.0 Active\nSystem 1.0.0 Active\nTaskManager 1.0.0 Active\nWimp 1.0.0 Active\nZetaRegistry 1.0.0 Active",
        "Help Modules must contain the exact current module identities, versions, and states"
    );
    assert!(
        ![
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
        ]
        .iter()
        .any(|month| all_modules_help.contains(month)),
        "the Modules topic must not invent creation dates: {all_modules_help:?}"
    );

    let (file_help_result, file_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP FileCommands",
    );
    assert_success(
        file_help_result,
        &file_help,
        "*HELP FileCommands with guest entry",
    );
    assert_eq!(
        normalize_output(&file_help),
        format!(
            "File commands:\n  *ZedFiles - alpha file descriptor\n{}",
            file_rows.join("\n")
        ),
        "a live guest FileCommands row must appear exactly once in active-module order"
    );

    let (module_help_result, module_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP AlphaRegistry",
    );
    assert_success(module_help_result, &module_help, "*HELP AlphaRegistry");
    assert_help_has(&module_help, "AlphaRegistry", "*HELP AlphaRegistry");
    assert_help_has(&module_help, "1.0.0", "*HELP AlphaRegistry");
    assert!(
        ![
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
        ]
        .iter()
        .any(|month| module_help.contains(month)),
        "Help must not invent a module creation date: {module_help:?}"
    );
    let (mixed_case_module_result, mixed_case_module_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP alpharegistry",
    );
    assert_success(
        mixed_case_module_result,
        &mixed_case_module_help,
        "case-insensitive module Help",
    );
    for expected in [
        "AlphaRegistry",
        "1.0.0",
        "ZedAlpha [tail]",
        "alpha descriptor v1",
    ] {
        assert_help_has(
            &mixed_case_module_help,
            expected,
            "case-insensitive module Help",
        );
    }

    let (specific_result, specific_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP ZedAlpha",
    );
    assert_success(specific_result, &specific_help, "*HELP ZedAlpha");
    assert_eq!(
        normalize_output(&specific_help),
        "  ZedAlpha [tail] - alpha descriptor v1\n  ZedAlpha [tail] - beta shadow descriptor",
        "exact command Help should show each matching owner exactly once"
    );
    assert_no_private_source(&specific_help, "*HELP ZedAlpha");
    let (mixed_case_specific_result, mixed_case_specific_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP zEdAlPhA",
    );
    assert_success(
        mixed_case_specific_result,
        &mixed_case_specific_help,
        "case-insensitive exact command Help",
    );
    assert_eq!(
        normalize_output(&mixed_case_specific_help),
        normalize_output(&specific_help),
        "exact command Help matching must be case-insensitive"
    );

    // Help prefix matching is many-to-one; command execution is first-match.
    // Module ordering is AlphaRegistry before ZetaRegistry, then descriptor
    // declaration order inside AlphaRegistry.
    let (first_result, first_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*ZedA. payload",
    );
    assert_success(first_result, &first_output, "*ZedA. payload");
    assert_help_has(&first_output, "alpha-v1:payload", "first-match dispatch");
    let (raw_tail_result, raw_tail_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "** ZedAlpha  two  words",
    );
    assert_success(
        raw_tail_result,
        &raw_tail_output,
        "raw command argument tail",
    );
    assert_help_has(
        &raw_tail_output,
        "alpha-v1: two  words",
        "raw command argument tail",
    );
    let (exact_result, exact_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*ZedAlbatross exact",
    );
    assert_success(
        exact_result,
        &exact_output,
        "exact command should win over its prefix",
    );
    assert_help_has(&exact_output, "beta:exact", "exact command dispatch");
    let (case_result, case_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*zEdAlPhA mixed",
    );
    assert_success(
        case_result,
        &case_output,
        "case-insensitive command dispatch",
    );
    assert_help_has(&case_output, "alpha-v1:mixed", "case-insensitive dispatch");

    // A compatible same-title replacement swaps handler and display metadata
    // together. An incompatible candidate must leave both intact.
    let (replacement_result, replacement_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMLoad RegistryAlphaV2",
    );
    assert_success(
        replacement_result,
        &replacement_output,
        "same-title command replacement",
    );
    let (replaced_help_result, replaced_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP ZedAlpha",
    );
    assert_success(
        replaced_help_result,
        &replaced_help,
        "Help after replacement",
    );
    assert_eq!(
        normalize_output(&replaced_help),
        "  ZedAlpha [tail] - alpha descriptor v2\n  ZedAlpha [tail] - beta shadow descriptor",
        "compatible replacement must atomically swap its Help descriptor and retain the shadow row"
    );
    let (replaced_exec_result, replaced_exec) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*ZedA. payload",
    );
    assert_success(
        replaced_exec_result,
        &replaced_exec,
        "execution after replacement",
    );
    assert_help_has(
        &replaced_exec,
        "alpha-v2:payload",
        "execution after replacement",
    );

    let (rejected_result, rejected_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMLoad RegistryAlphaIncompatible",
    );
    assert!(
        rejected_result.is_err()
            || ["incompatible", "workspace", "state", "replacement"]
                .iter()
                .any(|part| rejected_output.to_ascii_lowercase().contains(part)),
        "incompatible candidate was not rejected: {rejected_result:?}, {rejected_output:?}"
    );
    let (after_reject_help_result, after_reject_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP ZedAlpha",
    );
    assert_success(
        after_reject_help_result,
        &after_reject_help,
        "Help after rejected replacement",
    );
    assert_eq!(
        normalize_output(&after_reject_help),
        "  ZedAlpha [tail] - alpha descriptor v2\n  ZedAlpha [tail] - beta shadow descriptor",
        "rejected replacement must leave the old descriptor and other owner intact"
    );
    let (after_reject_exec_result, after_reject_exec) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*ZedA. payload",
    );
    assert_success(
        after_reject_exec_result,
        &after_reject_exec,
        "execution after rejected replacement",
    );
    assert_help_has(
        &after_reject_exec,
        "alpha-v2:payload",
        "execution after rejected replacement",
    );

    // A guest cannot publish a Rust bridge entry; failed loads must not leak
    // forged registry metadata.
    let (forged_result, forged_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMLoad ForgedCommandBridge",
    );
    assert!(
        forged_result.is_err()
            || ["bridge", "capability", "manifest", "not allowed"]
                .iter()
                .any(|part| forged_output.to_ascii_lowercase().contains(part)),
        "guest bridge declaration was not rejected: {forged_result:?}, {forged_output:?}"
    );
    let (forged_help_result, forged_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP ZedForged",
    );
    assert!(
        forged_help_result.is_err()
            || ["unknown", "not found", "no help", "no match"]
                .iter()
                .any(|marker| forged_help.to_ascii_lowercase().contains(marker)),
        "rejected bridge metadata became visible: {forged_help_result:?}, {forged_help:?}"
    );
    for (path, command_name) in [
        ("DuplicateCommands", "ZedDuplicate"),
        ("MalformedCommandMetadata", "ZedMalformed"),
    ] {
        let (rejected_load_result, rejected_load_output) = cli(
            &mut dispatcher,
            &mut mos,
            &display_receiver,
            &format!("*RMLoad {path}"),
        );
        assert!(
            rejected_load_result.is_err()
                || ["command", "duplicate", "category", "metadata", "manifest"]
                    .iter()
                    .any(|part| rejected_load_output.to_ascii_lowercase().contains(part)),
            "invalid command registry module {path:?} was not rejected: {rejected_load_result:?}, {rejected_load_output:?}"
        );
        let (rejected_help_result, rejected_help_output) = cli(
            &mut dispatcher,
            &mut ordinary,
            &display_receiver,
            &format!("*HELP {command_name}"),
        );
        assert!(
            rejected_help_result.is_err()
                || ["unknown", "not found", "no help", "no match"]
                    .iter()
                    .any(|marker| rejected_help_output.to_ascii_lowercase().contains(marker)),
            "invalid module entry {command_name:?} became visible: {rejected_help_result:?}, {rejected_help_output:?}"
        );
    }

    let (kill_result, kill_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMKill AlphaRegistry",
    );
    assert_success(kill_result, &kill_output, "unload command owner");
    let (remaining_result, remaining_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP ZedA.",
    );
    assert_success(remaining_result, &remaining_help, "Help after owner unload");
    assert_eq!(
        normalize_output(&remaining_help),
        "  ZedAlpha [tail] - beta shadow descriptor\n  ZedAlbatross [tail] - beta descriptor",
        "unloading AlphaRegistry must remove only Alpha-owned rows and expose ZetaRegistry's matches"
    );
    let (remaining_exec_result, remaining_exec) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*ZedA. after-unload",
    );
    assert_success(
        remaining_exec_result,
        &remaining_exec,
        "first remaining command after unload",
    );
    assert_help_has(
        &remaining_exec,
        "beta-shadow:after-unload",
        "dispatch after owner unload",
    );
    let (shadow_help_result, shadow_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP ZedAlpha",
    );
    assert_success(
        shadow_help_result,
        &shadow_help,
        "shadow command Help after owner unload",
    );
    assert_eq!(
        normalize_output(&shadow_help),
        "  ZedAlpha [tail] - beta shadow descriptor",
        "Help for the surviving duplicate should contain only its registered descriptor"
    );
    assert!(!module_info(
        &mut dispatcher,
        &mut ordinary,
        "AlphaRegistry"
    ));

    let (last_kill_result, last_kill_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMKill ZetaRegistry",
    );
    assert_success(
        last_kill_result,
        &last_kill_output,
        "unload final command owner",
    );
    let (command_only_kill_result, command_only_kill_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*RMKill CommandOnly",
    );
    assert_success(
        command_only_kill_result,
        &command_only_kill_output,
        "unload command-only module",
    );
    assert!(!module_info(&mut dispatcher, &mut ordinary, "CommandOnly"));
    let (command_only_unloaded_result, command_only_unloaded_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP OnlyCommand",
    );
    assert!(
        command_only_unloaded_result.is_err()
            || ["unknown", "not found", "no help", "no match"]
                .iter()
                .any(|marker| command_only_unloaded_help
                    .to_ascii_lowercase()
                    .contains(marker)),
        "command-only module's command remained in Help after unload: {command_only_unloaded_result:?}, {command_only_unloaded_help:?}"
    );
    let (empty_result, empty_help) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP ZedA.",
    );
    assert!(
        empty_result.is_err()
            || ["unknown", "not found", "no help", "no match"]
                .iter()
                .any(|marker| empty_help.to_ascii_lowercase().contains(marker)),
        "Help retained an unloaded module command: {empty_result:?}, {empty_help:?}"
    );
    let (unloaded_exec_result, unloaded_exec) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*ZedA. after-unload",
    );
    assert!(
        unloaded_exec_result.is_err() || unloaded_exec.to_ascii_lowercase().contains("bad command"),
        "OS_CLI invoked a command after its owner was unloaded: {unloaded_exec_result:?}, {unloaded_exec:?}"
    );

    // The bounded Obey subset is registered, but Exec remains unsupported and
    // Help must not imply broader PRM script compatibility. No generated
    // module date/RMA fields exist.
    let (_, final_modules) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*HELP Modules",
    );
    assert!(!final_modules.to_ascii_lowercase().contains("rma"));
    assert!(!final_modules.contains("0x"));

    assert_eq!(mos.memory.dynamic_area_count(), 0);
    assert_eq!(ordinary.memory.dynamic_area_count(), 0);
    assert_eq!(source_inspector.memory.dynamic_area_count(), 0);
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_A, 32)
            .unwrap(),
        vec![0xA1; 32]
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OLD_SCRATCH_SENTINEL_B, 32)
            .unwrap(),
        vec![0xB2; 32]
    );

    // Also cross the real terminal frontend: the command registry's Help must
    // be the same OS_CLI path used after the stdio prompt, with no Rust legacy
    // help appended after the BASIC64-owned output.
    let stdio_config = environment.root.join("stdio-configure");
    let stdio = run_stdio(
        &environment.root,
        &stdio_config,
        b"*HELP\n*HELP Syntax\n*STATUS Language\n*INSPECT MODULES\n*Modules\nQUIT\n",
    );
    let stdio_text = process_text(&stdio);
    assert!(
        stdio.status.success(),
        "stdio sequence failed: {stdio_text:?}"
    );
    let normalized_stdio = normalize_output(&stdio_text);
    assert!(
        normalized_stdio.contains(&normalize_output(&bare_help)),
        "stdio HELP did not return the same inventory as OS_CLI: {normalized_stdio:?}"
    );
    for expected in ["Syntax", "Language=0", "RicochetCommands"] {
        assert_help_has(
            &normalized_stdio,
            expected,
            "public --stdio command sequence",
        );
    }
    assert!(
        !normalized_stdio
            .to_ascii_lowercase()
            .contains("unsupported rom/rma")
            && !normalized_stdio
                .to_ascii_lowercase()
                .contains("bad command"),
        "stdio command results fell through into a legacy diagnostic: {normalized_stdio:?}"
    );
}
