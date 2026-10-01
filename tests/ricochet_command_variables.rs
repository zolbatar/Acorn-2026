use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{Mutex, mpsc},
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiContext, SwiDispatcher},
};

const X_BIT: u32 = 1 << 17;
const OS_CLI: u32 = 0x05;
const OS_MODULE: u32 = 0x1E;
const OS_READ_VAR_VAL: u32 = 0x23;
const OS_SET_VAR_VAL: u32 = 0x24;
const RICOCHET_MODULE_LOOKUP: u32 = 0x4FF12;

const CLI_ADDRESS: u32 = 0x2100;
const MODULE_PATH_ADDRESS: u32 = 0x2200;
const NAME_ADDRESS: u32 = 0x3000;
const VALUE_ADDRESS: u32 = 0x4000;
const OUTPUT_ADDRESS: u32 = 0x8000;
const OLD_BRIDGE_SCRATCH_ADDRESSES: [u32; 3] = [0x5000, 0x5800, 0x6000];
const OUTPUT_SENTINEL: u8 = 0xA5;
const MAX_VALUE_BYTES: usize = 256;
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

const FORGED_VARIABLE_WRITER: &str = r#"REM @BASIC64 MODE=BASIC64
REM @SYSTEM_PROFILE 0.1
REM @MODULE ForgedVariableWriter 1.0.0
REM @CAPABILITY SystemVariableStore
REM @IMPORT Host.SystemVariables.Write SystemVariableStore
REM @SWI ForgedVariable_Probe &4FF91 Probe REGISTERS=R0:U32:OUT
DEF PROC Probe
    R0% = 0
ENDPROC
"#;

struct IsolatedEnvironment {
    root: PathBuf,
    config_path: PathBuf,
    sentinel_name: String,
    sentinel_value: String,
    previous_config: Option<std::ffi::OsString>,
    previous_volume: Option<std::ffi::OsString>,
    previous_sentinel: Option<std::ffi::OsString>,
}

impl IsolatedEnvironment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ricochet-command-variables-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let config_path = root.join("configure");
        let sentinel_name = format!("RICOCHET_TEST_{nonce:X}");
        let sentinel_value = format!("host-only-value-{nonce:X}");
        let previous_config = std::env::var_os("RICOCHET_CONFIG_PATH");
        let previous_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        let previous_sentinel = std::env::var_os(&sentinel_name);
        // This test is serial and all persistence remains inside its unique
        // temporary directory; the synthetic host variable must not be
        // imported into the guest-owned store.
        unsafe {
            std::env::set_var("RICOCHET_CONFIG_PATH", &config_path);
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
            std::env::set_var(&sentinel_name, &sentinel_value);
        }
        Self {
            root,
            config_path,
            sentinel_name,
            sentinel_value,
            previous_config,
            previous_volume,
            previous_sentinel,
        }
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.previous_config {
                std::env::set_var("RICOCHET_CONFIG_PATH", value);
            } else {
                std::env::remove_var("RICOCHET_CONFIG_PATH");
            }
            if let Some(value) = &self.previous_volume {
                std::env::set_var("RICOCHET_DEMO_VOLUME", value);
            } else {
                std::env::remove_var("RICOCHET_DEMO_VOLUME");
            }
            if let Some(value) = &self.previous_sentinel {
                std::env::set_var(&self.sentinel_name, value);
            } else {
                std::env::remove_var(&self.sentinel_name);
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug)]
struct VariableRead {
    bytes: Vec<u8>,
    buffer_after: Vec<u8>,
    matched_name: Option<String>,
    context: u32,
    variable_type: u32,
    registers: [u32; 16],
}

fn new_dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
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

fn write_guest_module(root: &std::path::Path, file_stem: &str, source: &str) {
    fs::write(root.join(format!("{file_stem}.bas64")), source).unwrap();
    fs::write(
        root.join(format!("{file_stem}.bas64.ricochetmeta")),
        format!(
            "Ricochet file metadata v1\nformat-version=1\nguest-name={file_stem}\nfile-type=0x00000064\nload-address=0x00000000\nexecution-address=0x00000000\nattributes=0x00000000\n"
        ),
    )
    .unwrap();
}

fn module_is_present(dispatcher: &mut SwiDispatcher, task: &mut Task, name: &str) -> bool {
    put_c_string(task, NAME_ADDRESS, name);
    let mut context = SwiContext::default();
    context.registers[0] = 1;
    context.registers[1] = NAME_ADDRESS;
    dispatcher
        .dispatch(RICOCHET_MODULE_LOOKUP, task, &mut context)
        .expect("public module lookup succeeds");
    !context.overflow && context.registers[1] != 0
}

fn read_c_string(task: &Task, address: u32, capacity: usize) -> String {
    String::from_utf8(task.memory.read_c_string(address, capacity).unwrap())
        .expect("the returned guest string is UTF-8")
}

fn assert_structured(error: &RuntimeError, wanted_name: &str, wanted_code: u32) {
    assert!(
        matches!(
            error,
            RuntimeError::Structured { type_name, code, .. }
                if type_name == wanted_name && *code == wanted_code
        ),
        "expected {wanted_name} code {wanted_code}, received {error:?}"
    );
}

fn direct_set(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    value: &[u8],
    variable_type: u32,
) -> Result<SwiContext, RuntimeError> {
    direct_set_with_terminator(dispatcher, task, name, value, variable_type, 0)
}

fn direct_set_with_terminator(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    value: &[u8],
    variable_type: u32,
    terminator: u8,
) -> Result<SwiContext, RuntimeError> {
    put_c_string(task, NAME_ADDRESS, name);
    task.memory
        .write_bytes(VALUE_ADDRESS, &vec![0xD3; MAX_VALUE_BYTES + 1])
        .unwrap();
    task.memory.write_bytes(VALUE_ADDRESS, value).unwrap();
    if variable_type == 0 {
        task.memory
            .write_byte(VALUE_ADDRESS + value.len() as u32, terminator)
            .unwrap();
    }
    let mut context = SwiContext::default();
    context.registers[0] = NAME_ADDRESS;
    context.registers[1] = VALUE_ADDRESS;
    context.registers[2] = value.len() as u32;
    context.registers[3] = 0;
    context.registers[4] = variable_type;
    let result = dispatcher.dispatch(OS_SET_VAR_VAL, task, &mut context);
    if result.is_ok() {
        assert_eq!(context.registers[0], NAME_ADDRESS, "OS_SetVarVal R0");
        assert_eq!(context.registers[1], VALUE_ADDRESS, "OS_SetVarVal R1");
        assert_eq!(context.registers[2], value.len() as u32, "OS_SetVarVal R2");
        assert_eq!(context.registers[3], 0, "exact set returns no context");
        assert_eq!(
            context.registers[4], variable_type,
            "OS_SetVarVal returns type"
        );
        assert_eq!(
            task.memory.read_bytes(VALUE_ADDRESS, value.len()).unwrap(),
            value,
            "OS_SetVarVal must not mutate its caller's input bytes"
        );
    }
    result.map(|()| context)
}

fn direct_delete(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    pattern: &str,
) -> Result<SwiContext, RuntimeError> {
    put_c_string(task, NAME_ADDRESS, pattern);
    task.memory.write_bytes(VALUE_ADDRESS, &[0xB7; 32]).unwrap();
    let before = task.memory.read_bytes(VALUE_ADDRESS, 32).unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = NAME_ADDRESS;
    context.registers[1] = VALUE_ADDRESS;
    context.registers[2] = u32::MAX; // PRM negative-length delete form.
    context.registers[3] = 0;
    context.registers[4] = 0;
    let result = dispatcher.dispatch(OS_SET_VAR_VAL, task, &mut context);
    if result.is_ok() {
        assert_eq!(context.registers[0], NAME_ADDRESS, "delete preserves R0");
        assert_eq!(context.registers[1], VALUE_ADDRESS, "delete preserves R1");
        assert_eq!(context.registers[2], u32::MAX, "delete preserves R2");
        assert_eq!(context.registers[3], 0, "delete returns no context");
        assert_eq!(
            task.memory.read_bytes(VALUE_ADDRESS, 32).unwrap(),
            before,
            "delete must not touch the caller's unused value buffer"
        );
    }
    result.map(|()| context)
}

fn direct_read(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name_or_pattern: &str,
    capacity: usize,
    prior_context: u32,
    request_type: u32,
) -> Result<VariableRead, RuntimeError> {
    put_c_string(task, NAME_ADDRESS, name_or_pattern);
    task.memory
        .write_bytes(OUTPUT_ADDRESS, &vec![OUTPUT_SENTINEL; capacity])
        .unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = NAME_ADDRESS;
    context.registers[1] = OUTPUT_ADDRESS;
    context.registers[2] = capacity as u32;
    context.registers[3] = prior_context;
    context.registers[4] = request_type;
    let result = dispatcher.dispatch(OS_READ_VAR_VAL, task, &mut context);
    result?;
    let byte_count = context.registers[2] as usize;
    assert!(
        byte_count <= capacity,
        "returned length fits output capacity"
    );
    let bytes = task.memory.read_bytes(OUTPUT_ADDRESS, byte_count).unwrap();
    let buffer_after = task.memory.read_bytes(OUTPUT_ADDRESS, capacity).unwrap();
    let matched_name =
        (context.registers[3] != 0).then(|| read_c_string(task, context.registers[3], 33));
    assert_eq!(
        context.registers[0], NAME_ADDRESS,
        "OS_ReadVarVal preserves R0"
    );
    assert_eq!(
        context.registers[1], OUTPUT_ADDRESS,
        "OS_ReadVarVal preserves R1"
    );
    Ok(VariableRead {
        bytes,
        buffer_after,
        matched_name,
        context: context.registers[3],
        variable_type: context.registers[4],
        registers: context.registers,
    })
}

fn direct_probe(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name_or_pattern: &str,
    prior_context: u32,
) -> Result<(SwiContext, Vec<u8>), RuntimeError> {
    put_c_string(task, NAME_ADDRESS, name_or_pattern);
    let sentinel = vec![OUTPUT_SENTINEL; 32];
    task.memory.write_bytes(OUTPUT_ADDRESS, &sentinel).unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = NAME_ADDRESS;
    context.registers[1] = OUTPUT_ADDRESS;
    context.registers[2] = 0x8000_0000;
    context.registers[3] = prior_context;
    context.registers[4] = 0;
    dispatcher.dispatch(OS_READ_VAR_VAL, task, &mut context)?;
    assert_eq!(
        task.memory
            .read_bytes(OUTPUT_ADDRESS, sentinel.len())
            .unwrap(),
        sentinel,
        "OS_ReadVarVal length/existence probe must not write R1"
    );
    Ok((context, sentinel))
}

fn assert_x_error(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    number: u32,
    context: &mut SwiContext,
    wanted_code: u32,
) {
    dispatcher
        .dispatch(number | X_BIT, task, context)
        .expect("X-form errors return normally with V set");
    assert!(
        context.overflow,
        "X form must set V for error code {wanted_code}"
    );
    let error_address = context.registers[0];
    let code = u32::from_le_bytes(
        task.memory
            .read_bytes(error_address, 4)
            .expect("X form returns a checked caller error block")
            .try_into()
            .unwrap(),
    );
    assert_eq!(code, wanted_code, "X-form error block code");
    let _message = read_c_string(task, error_address + 4, 252);
}

fn normalize_output(output: &str) -> String {
    output
        .replace("\r\n", "\n")
        .replace("\n\r", "\n")
        .replace('\r', "\n")
        .trim_end_matches('\n')
        .to_owned()
}

fn cli(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    receiver: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> (Result<(), RuntimeError>, String) {
    let _ = receiver.try_iter().count();
    let scratch_before = task.memory.dynamic_area_count();
    put_c_string(task, CLI_ADDRESS, command);
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    if result.is_ok() {
        assert_eq!(context.registers[0], CLI_ADDRESS, "OS_CLI preserves R0");
    }
    assert_eq!(
        task.memory.dynamic_area_count(),
        scratch_before,
        "OS_CLI released command scratch after {command:?}"
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

fn help_command_names(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let name = line.trim_start().strip_prefix('*')?;
            Some(
                name.split_whitespace()
                    .next()
                    .expect("command Help row has a name")
                    .to_ascii_uppercase(),
            )
        })
        .collect()
}

fn run_stdio(config_path: &std::path::Path, volume: &std::path::Path, input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ricochet"));
    child
        .arg("--stdio")
        .env("RICOCHET_CONFIG_PATH", config_path)
        .env("RICOCHET_DEMO_VOLUME", volume)
        .env_remove("RICOCHET_BOOT_CAPSULE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child.spawn().expect("start the public stdio runtime");
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input)
        .expect("write a bounded command sequence");
    child
        .wait_with_output()
        .expect("wait for the public stdio runtime")
}

#[test]
fn command_variables_round_trip_through_cli_and_public_swis() {
    let _environment_guard = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = IsolatedEnvironment::new();
    let (mut dispatcher, display_receiver) = new_dispatcher();
    let mut mos = Task::trusted_mos_session(0xCA_0101);
    let mut ordinary = Task::new(mos.id);

    // STATUS already owns the one-letter S-prefix. Help must enumerate all
    // matching entries in the same order used by execution.
    let (help_result, help_output) = cli(&mut dispatcher, &mut mos, &display_receiver, "*HELP S.");
    help_result.expect("Help is public and read-only");
    let help_names = help_command_names(&help_output);
    let status_position = help_names.iter().position(|name| name == "STATUS").unwrap();
    let set_position = help_names.iter().position(|name| name == "SET").unwrap();
    let show_position = help_names.iter().position(|name| name == "SHOW").unwrap();
    assert!(
        status_position < set_position && set_position < show_position,
        "S. Help order: {help_names:?}"
    );
    let help_lower = help_output.to_ascii_lowercase();
    assert!(help_lower.contains("*set <name> [value]"));
    assert!(help_lower.contains("*show [pattern]"));
    let (unset_help_result, unset_help_output) =
        cli(&mut dispatcher, &mut mos, &display_receiver, "*HELP U.");
    unset_help_result.expect("Help lists every U-prefix command");
    assert!(
        unset_help_output
            .to_ascii_lowercase()
            .contains("*unset <pattern>"),
        "U-prefix Help advertises the UNSET syntax: {unset_help_output:?}"
    );
    let (status_result, status_output) =
        cli(&mut dispatcher, &mut mos, &display_receiver, "*STATUS");
    status_result.expect("STATUS remains available");
    let (s_prefix_result, s_prefix_output) =
        cli(&mut dispatcher, &mut mos, &display_receiver, "*S.");
    s_prefix_result.expect("the existing S. prefix resolves to STATUS");
    assert_eq!(
        normalize_output(&s_prefix_output),
        normalize_output(&status_output)
    );
    let (set_abbreviation_result, _) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*SE. RICO_ABBREVIATED value",
    );
    set_abbreviation_result.expect("SE. resolves to SET without stealing STATUS");
    let (show_abbreviation_result, show_abbreviation_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*SH. RICO_ABBREVIATED",
    );
    show_abbreviation_result.expect("SH. resolves to SHOW");
    assert!(show_abbreviation_output.contains("RICO_ABBREVIATED (String): value"));
    let (unset_abbreviation_result, _) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*UNS. RICO_ABBREVIATED",
    );
    unset_abbreviation_result.expect("UNS. resolves to UNSET");
    assert_structured(
        &direct_read(&mut dispatcher, &mut ordinary, "RICO_ABBREVIATED", 16, 0, 0).unwrap_err(),
        "SystemVariableNotFound",
        2,
    );

    // CLI and direct &24 writes share one runtime store. Case-insensitive
    // updates retain the spelling used when the variable was created.
    let cli_value = "alpha  beta  ";
    let (set_result, set_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        &format!("*SET RiCo_CliValue {cli_value}"),
    );
    set_result.expect("trusted MOS may create a string variable");
    assert!(!set_output.to_ascii_lowercase().contains("unsupported"));
    let read = direct_read(&mut dispatcher, &mut ordinary, "rico_clivalue", 64, 0, 0)
        .expect("ordinary tasks may read the guest store");
    assert_eq!(read.bytes, cli_value.as_bytes());
    assert_eq!(read.variable_type, 0);
    assert_eq!(read.registers[2], cli_value.len() as u32);
    assert_eq!(
        read.buffer_after[cli_value.len()],
        OUTPUT_SENTINEL,
        "value is length-delimited without a NUL"
    );

    let direct_value = b"written-through-os-setvarval";
    direct_set(&mut dispatcher, &mut mos, "RICO_DIRECT", direct_value, 0)
        .expect("trusted MOS may write through OS_SetVarVal");
    let (direct_probe_result, direct_probe_buffer) =
        direct_probe(&mut dispatcher, &mut ordinary, "RICO_DIRECT", 0).unwrap();
    assert_eq!(
        direct_probe_result.registers[2],
        !(direct_value.len() as u32),
        "present probe returns the complemented byte length"
    );
    assert_eq!(direct_probe_result.registers[4], 0);
    assert_eq!(direct_probe_buffer, vec![OUTPUT_SENTINEL; 32]);
    let (show_result, show_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*SHOW RICO_DIRECT",
    );
    show_result.expect("SHOW is public");
    assert!(show_output.contains("RICO_DIRECT (String): written-through-os-setvarval"));
    let (all_result, all_output) = cli(&mut dispatcher, &mut ordinary, &display_receiver, "*SHOW");
    all_result.expect("no-argument SHOW enumerates guest variables");
    assert!(all_output.contains("RiCo_CliValue (String): alpha  beta  "));
    assert!(all_output.contains("RICO_DIRECT (String): written-through-os-setvarval"));
    assert!(!all_output.contains(&environment.sentinel_name));
    assert!(!all_output.contains(&environment.sentinel_value));
    let (swi_inspect_result, swi_inspect_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*INSPECT SWI OS_ReadVarVal",
    );
    swi_inspect_result.expect("public SWI ownership metadata remains readable");
    assert!(
        swi_inspect_output
            .to_ascii_lowercase()
            .contains("os_readvarval")
    );
    assert!(swi_inspect_output.to_ascii_lowercase().contains("system"));
    assert!(swi_inspect_output.contains("&23"));
    let host_var = direct_read(
        &mut dispatcher,
        &mut ordinary,
        &environment.sentinel_name,
        64,
        0,
        0,
    )
    .unwrap_err();
    assert_structured(&host_var, "SystemVariableNotFound", 2);

    direct_set(&mut dispatcher, &mut mos, "rico_clivalue", b"updated", 0)
        .expect("case-insensitive update succeeds");
    let (_, mixed_case_show) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*SHOW RICO_CLIVALUE",
    );
    assert!(mixed_case_show.contains("RiCo_CliValue (String): updated"));

    // Empty and absent are distinct. The PRM high-bit probe reports !length
    // for an existing value, zero for absent, and never writes the R1 buffer.
    let (empty_result, _) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*SET RICO_EMPTY",
    );
    empty_result.expect("omitted SET value creates an empty string");
    let empty = direct_read(&mut dispatcher, &mut ordinary, "RICO_EMPTY", 8, 0, 0)
        .expect("empty string remains present");
    assert!(empty.bytes.is_empty());
    assert_eq!(empty.variable_type, 0);
    let (present_probe, present_buffer) =
        direct_probe(&mut dispatcher, &mut ordinary, "RICO_EMPTY", 0).unwrap();
    assert_eq!(
        present_probe.registers[2],
        u32::MAX,
        "empty value probes as present length zero"
    );
    assert_eq!(present_buffer, vec![OUTPUT_SENTINEL; 32]);
    let (missing_probe, missing_buffer) =
        direct_probe(&mut dispatcher, &mut ordinary, "RICO_ABSENT", 0).unwrap();
    assert_eq!(
        missing_probe.registers[2], 0,
        "missing value probes as absent"
    );
    assert_eq!(missing_buffer, vec![OUTPUT_SENTINEL; 32]);
    let (unmatched_wildcard_probe, unmatched_wildcard_buffer) =
        direct_probe(&mut dispatcher, &mut ordinary, "RICO_NO_WILDCARD_MATCH*", 0)
            .expect("an initial unmatched wildcard probe reports no match without writing R1");
    assert_eq!(unmatched_wildcard_probe.registers[2], 0);
    assert_eq!(unmatched_wildcard_probe.registers[3], 0);
    assert_eq!(unmatched_wildcard_buffer, vec![OUTPUT_SENTINEL; 32]);
    let missing = direct_read(&mut dispatcher, &mut ordinary, "RICO_ABSENT", 8, 0, 0).unwrap_err();
    assert_structured(&missing, "SystemVariableNotFound", 2);
    direct_delete(&mut dispatcher, &mut mos, "RICO_EMPTY")
        .expect("negative R2 deletes a single variable");
    assert_structured(
        &direct_read(&mut dispatcher, &mut ordinary, "RICO_EMPTY", 8, 0, 0).unwrap_err(),
        "SystemVariableNotFound",
        2,
    );
    assert_structured(
        &direct_delete(&mut dispatcher, &mut mos, "RICO_EMPTY").unwrap_err(),
        "SystemVariableNotFound",
        2,
    );
    let (missing_arg_set_result, missing_arg_set_output) =
        cli(&mut dispatcher, &mut mos, &display_receiver, "*SET");
    let missing_arg_set =
        format!("{missing_arg_set_result:?} {missing_arg_set_output}").to_ascii_lowercase();
    assert!(
        missing_arg_set.contains("syntax"),
        "malformed SET has visible syntax feedback: {missing_arg_set}"
    );
    let (cli_gs_result, cli_gs_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*SET RICO_CLI_GS value<0",
    );
    let cli_gs_text = format!("{cli_gs_result:?} {cli_gs_output}").to_ascii_lowercase();
    assert!(
        cli_gs_text.contains("gstrans")
            || cli_gs_text.contains("unsupported")
            || cli_gs_text.contains("type"),
        "unsupported GSTrans syntax is reported: {cli_gs_text}"
    );
    assert_structured(
        &direct_read(&mut dispatcher, &mut ordinary, "RICO_CLI_GS", 16, 0, 0).unwrap_err(),
        "SystemVariableNotFound",
        2,
    );

    // LiteralString is length-delimited raw UTF-8 and does not run GSTrans.
    let literal = "literal <not expanded>|still literal λ".as_bytes();
    direct_set(&mut dispatcher, &mut mos, "RICO_LITERAL", literal, 4)
        .expect("type 4 accepts raw literal bytes");
    let control_literal_error = direct_set(
        &mut dispatcher,
        &mut mos,
        "RICO_LITERAL_CONTROL",
        b"visible\x1B[2J",
        4,
    )
    .unwrap_err();
    assert_structured(&control_literal_error, "SystemVariableTypeError", 5);
    assert_structured(
        &direct_read(
            &mut dispatcher,
            &mut ordinary,
            "RICO_LITERAL_CONTROL",
            32,
            0,
            0,
        )
        .unwrap_err(),
        "SystemVariableNotFound",
        2,
    );
    let literal_read = direct_read(&mut dispatcher, &mut ordinary, "RICO_LITERAL", 64, 0, 3)
        .expect("type 3 requests supported string retrieval without evaluation");
    assert_eq!(literal_read.bytes, literal);
    assert_eq!(literal_read.variable_type, 4);
    let (_, literal_show) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*SHOW RICO_LITERAL",
    );
    assert!(
        literal_show
            .contains("RICO_LITERAL (LiteralString): literal <not expanded>|still literal λ")
    );
    assert!(
        literal_show
            .bytes()
            .all(|byte| { byte >= 0x20 && byte != 0x7F || matches!(byte, b'\r' | b'\n') }),
        "SHOW emits no raw VDU/control byte from a literal value: {literal_show:?}"
    );
}

#[test]
fn wildcard_reads_interleave_and_wildcard_updates_are_atomic() {
    let _environment_guard = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = IsolatedEnvironment::new();
    let (mut dispatcher, _display_receiver) = new_dispatcher();
    let mut mos = Task::trusted_mos_session(0xCA_0201);
    let mut ordinary = Task::new(0xCA_0202);

    // &23 wildcard enumeration returns matched names through a checked
    // caller-task logical context in R3, and the next call consumes that same
    // context. # matches exactly one byte; * matches zero or more.
    direct_set(&mut dispatcher, &mut mos, "RICO_ENUM_A", b"zero", 4).unwrap();
    direct_set(&mut dispatcher, &mut mos, "RICO_ENUM_A2", b"two", 4).unwrap();
    direct_set(&mut dispatcher, &mut mos, "RICO_ENUM_ALPHA", b"alpha", 4).unwrap();
    let enum_areas_before = ordinary.memory.dynamic_area_count();
    let first = direct_read(&mut dispatcher, &mut ordinary, "RICO_ENUM_A*", 64, 0, 0)
        .expect("wildcard read finds the first matching variable");
    assert_eq!(first.matched_name.as_deref(), Some("RICO_ENUM_A"));
    assert_eq!(first.bytes, b"zero");
    assert_ne!(first.context, 0);
    let second = direct_read(
        &mut dispatcher,
        &mut ordinary,
        "RICO_ENUM_A*",
        64,
        first.context,
        0,
    )
    .expect("the wildcard star also matches zero or more bytes");
    assert_eq!(second.matched_name.as_deref(), Some("RICO_ENUM_A2"));
    assert_eq!(second.bytes, b"two");
    let third = direct_read(
        &mut dispatcher,
        &mut ordinary,
        "RICO_ENUM_A*",
        64,
        second.context,
        0,
    )
    .expect("the returned context advances in case-folded name order");
    assert_eq!(third.matched_name.as_deref(), Some("RICO_ENUM_ALPHA"));
    assert_eq!(third.bytes, b"alpha");
    let mut exhausted = SwiContext::default();
    put_c_string(&mut ordinary, NAME_ADDRESS, "RICO_ENUM_A*");
    ordinary
        .memory
        .write_bytes(OUTPUT_ADDRESS, &[OUTPUT_SENTINEL; 64])
        .unwrap();
    exhausted.registers[0] = NAME_ADDRESS;
    exhausted.registers[1] = OUTPUT_ADDRESS;
    exhausted.registers[2] = 64;
    exhausted.registers[3] = third.context;
    exhausted.registers[4] = 0;
    assert_x_error(
        &mut dispatcher,
        &mut ordinary,
        OS_READ_VAR_VAL,
        &mut exhausted,
        2,
    );
    assert_eq!(
        ordinary.memory.read_bytes(OUTPUT_ADDRESS, 64).unwrap(),
        vec![OUTPUT_SENTINEL; 64],
        "exhausted wildcard enumeration must not modify the caller buffer"
    );
    assert_eq!(ordinary.memory.dynamic_area_count(), enum_areas_before);
    let one_wildcard = direct_read(&mut dispatcher, &mut ordinary, "RICO_ENUM_A#", 64, 0, 0)
        .expect("# matches exactly one byte");
    assert_eq!(one_wildcard.matched_name.as_deref(), Some("RICO_ENUM_A2"));
    let hash_exhausted = direct_read(
        &mut dispatcher,
        &mut ordinary,
        "RICO_ENUM_A#",
        64,
        one_wildcard.context,
        0,
    )
    .unwrap_err();
    assert_structured(&hash_exhausted, "SystemVariableNotFound", 2);

    // Once an enumeration has produced a context, a continuation failure
    // must release its task-local context area instead of leaking it or
    // leaving an unusable cursor behind.
    let continuation_areas_before = ordinary.memory.dynamic_area_count();
    let continuation = direct_read(&mut dispatcher, &mut ordinary, "RICO_ENUM_A*", 64, 0, 0)
        .expect("start a fresh enumeration for the failing continuation");
    assert_eq!(
        ordinary.memory.dynamic_area_count(),
        continuation_areas_before + 1
    );
    put_c_string(&mut ordinary, NAME_ADDRESS, "RICO_ENUM_A*");
    ordinary
        .memory
        .write_bytes(OUTPUT_ADDRESS, &[OUTPUT_SENTINEL; 2])
        .unwrap();
    let mut undersized_continuation = SwiContext::default();
    undersized_continuation.registers[0] = NAME_ADDRESS;
    undersized_continuation.registers[1] = OUTPUT_ADDRESS;
    undersized_continuation.registers[2] = 2;
    undersized_continuation.registers[3] = continuation.context;
    undersized_continuation.registers[4] = 0;
    let continuation_error = dispatcher
        .dispatch(OS_READ_VAR_VAL, &mut ordinary, &mut undersized_continuation)
        .unwrap_err();
    assert_structured(&continuation_error, "SystemVariableBufferError", 3);
    assert_eq!(
        ordinary.memory.read_bytes(OUTPUT_ADDRESS, 2).unwrap(),
        vec![OUTPUT_SENTINEL; 2],
        "failed continuation leaves its undersized output buffer unchanged"
    );
    assert_eq!(
        ordinary.memory.dynamic_area_count(),
        continuation_areas_before,
        "failed continuation releases its context scratch area"
    );

    let malformed_context_areas_before = ordinary.memory.dynamic_area_count();
    let malformed_context =
        direct_read(&mut dispatcher, &mut ordinary, "RICO_ENUM_A*", 64, 0, 0).unwrap();
    let mut invalid_selector_with_context = SwiContext::default();
    invalid_selector_with_context.registers[0] = u32::MAX;
    invalid_selector_with_context.registers[1] = OUTPUT_ADDRESS;
    invalid_selector_with_context.registers[2] = 64;
    invalid_selector_with_context.registers[3] = malformed_context.context;
    invalid_selector_with_context.registers[4] = 0;
    assert!(matches!(
        dispatcher.dispatch(
            OS_READ_VAR_VAL,
            &mut ordinary,
            &mut invalid_selector_with_context
        ),
        Err(RuntimeError::Memory(_))
    ));
    assert_eq!(
        ordinary.memory.dynamic_area_count(),
        malformed_context_areas_before,
        "invalid selector pointer releases its incoming cursor"
    );

    let exact_context_areas_before = ordinary.memory.dynamic_area_count();
    let exact_context =
        direct_read(&mut dispatcher, &mut ordinary, "RICO_ENUM_A*", 64, 0, 0).unwrap();
    put_c_string(&mut ordinary, NAME_ADDRESS, "RICO_ENUM_A");
    let mut exact_selector_with_context = SwiContext::default();
    exact_selector_with_context.registers[0] = NAME_ADDRESS;
    exact_selector_with_context.registers[1] = OUTPUT_ADDRESS;
    exact_selector_with_context.registers[2] = 64;
    exact_selector_with_context.registers[3] = exact_context.context;
    exact_selector_with_context.registers[4] = 0;
    let exact_selector_error = dispatcher
        .dispatch(
            OS_READ_VAR_VAL,
            &mut ordinary,
            &mut exact_selector_with_context,
        )
        .unwrap_err();
    assert_structured(&exact_selector_error, "SystemVariableNameError", 1);
    assert_eq!(
        ordinary.memory.dynamic_area_count(),
        exact_context_areas_before,
        "exact selector cannot retain a wildcard cursor"
    );

    // Separate R3 caller contexts can be interleaved without replacing or
    // leaking one another: start A and B, then advance A and B independently.
    for (name, value) in [
        ("RICO_INTER_A1", b"a-one".as_slice()),
        ("RICO_INTER_A2", b"a-two".as_slice()),
        ("RICO_INTER_B1", b"b-one".as_slice()),
        ("RICO_INTER_B2", b"b-two".as_slice()),
    ] {
        direct_set(&mut dispatcher, &mut mos, name, value, 4).unwrap();
    }
    let interleave_areas_before = ordinary.memory.dynamic_area_count();
    let interleave_a1 =
        direct_read(&mut dispatcher, &mut ordinary, "RICO_INTER_A*", 32, 0, 0).unwrap();
    let interleave_b1 =
        direct_read(&mut dispatcher, &mut ordinary, "RICO_INTER_B*", 32, 0, 0).unwrap();
    assert_eq!(interleave_a1.matched_name.as_deref(), Some("RICO_INTER_A1"));
    assert_eq!(interleave_b1.matched_name.as_deref(), Some("RICO_INTER_B1"));
    assert_ne!(interleave_a1.context, interleave_b1.context);
    let interleave_a2 = direct_read(
        &mut dispatcher,
        &mut ordinary,
        "RICO_INTER_A*",
        32,
        interleave_a1.context,
        0,
    )
    .unwrap();
    let interleave_b2 = direct_read(
        &mut dispatcher,
        &mut ordinary,
        "RICO_INTER_B*",
        32,
        interleave_b1.context,
        0,
    )
    .unwrap();
    assert_eq!(interleave_a2.matched_name.as_deref(), Some("RICO_INTER_A2"));
    assert_eq!(interleave_b2.matched_name.as_deref(), Some("RICO_INTER_B2"));
    for (pattern, context) in [
        ("RICO_INTER_A*", interleave_a2.context),
        ("RICO_INTER_B*", interleave_b2.context),
    ] {
        let mut exhausted = SwiContext::default();
        put_c_string(&mut ordinary, NAME_ADDRESS, pattern);
        exhausted.registers[0] = NAME_ADDRESS;
        exhausted.registers[1] = OUTPUT_ADDRESS;
        exhausted.registers[2] = 32;
        exhausted.registers[3] = context;
        exhausted.registers[4] = 0;
        assert_x_error(
            &mut dispatcher,
            &mut ordinary,
            OS_READ_VAR_VAL,
            &mut exhausted,
            2,
        );
    }
    assert_eq!(
        ordinary.memory.dynamic_area_count(),
        interleave_areas_before
    );
}

#[test]
fn variable_types_buffers_names_and_store_limits_are_checked() {
    let _environment_guard = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _environment = IsolatedEnvironment::new();
    let (mut dispatcher, _) = new_dispatcher();
    let mut mos = Task::trusted_mos_session(0xCA_0301);
    let mut ordinary = Task::new(0xCA_0302);
    let direct_value = b"written-through-os-setvarval";
    direct_set(&mut dispatcher, &mut mos, "RICO_DIRECT", direct_value, 0).unwrap();

    // Failed reads are atomic and do not append a NUL to successful values.
    let too_small_sentinel = vec![0xC7; 2];
    ordinary
        .memory
        .write_bytes(OUTPUT_ADDRESS, &too_small_sentinel)
        .unwrap();
    put_c_string(&mut ordinary, NAME_ADDRESS, "RICO_DIRECT");
    let mut too_small = SwiContext::default();
    too_small.registers[0] = NAME_ADDRESS;
    too_small.registers[1] = OUTPUT_ADDRESS;
    too_small.registers[2] = 2;
    too_small.registers[4] = 0;
    let error = dispatcher
        .dispatch(OS_READ_VAR_VAL, &mut ordinary, &mut too_small)
        .unwrap_err();
    assert_structured(&error, "SystemVariableBufferError", 3);
    assert_eq!(
        ordinary.memory.read_bytes(OUTPUT_ADDRESS, 2).unwrap(),
        too_small_sentinel
    );
    let mut too_small_x = SwiContext::default();
    too_small_x.registers[0] = NAME_ADDRESS;
    too_small_x.registers[1] = OUTPUT_ADDRESS;
    too_small_x.registers[2] = 2;
    too_small_x.registers[4] = 0;
    assert_x_error(
        &mut dispatcher,
        &mut ordinary,
        OS_READ_VAR_VAL,
        &mut too_small_x,
        3,
    );
    assert_eq!(
        ordinary.memory.read_bytes(OUTPUT_ADDRESS, 2).unwrap(),
        vec![0xC7; 2]
    );

    // The hosted implementation deliberately limits names to visible ASCII,
    // type 0 to the GSTrans-free string subset, and supported types to
    // String/LiteralString; all rejections are transactional.
    let max_name = format!("N{}", "A".repeat(31));
    assert_eq!(max_name.len(), 32);
    direct_set(&mut dispatcher, &mut mos, &max_name, b"", 4).expect("a 32-byte name is accepted");
    let too_long_name = format!("N{}", "A".repeat(32));
    let name_error = direct_set(&mut dispatcher, &mut mos, &too_long_name, b"", 4).unwrap_err();
    assert_structured(&name_error, "SystemVariableNameError", 1);
    let empty_name_error = direct_set(&mut dispatcher, &mut mos, "", b"", 4).unwrap_err();
    assert_structured(&empty_name_error, "SystemVariableNameError", 1);
    let spaced_name = direct_set(&mut dispatcher, &mut mos, "RICO BADNAME", b"", 4).unwrap_err();
    assert_structured(&spaced_name, "SystemVariableNameError", 1);
    let non_ascii_name = direct_set(&mut dispatcher, &mut mos, "RICO_λ", b"", 4).unwrap_err();
    assert_structured(&non_ascii_name, "SystemVariableNameError", 1);
    let wildcard_create = direct_set(&mut dispatcher, &mut mos, "RICO_NEW_*", b"x", 4).unwrap_err();
    assert_structured(&wildcard_create, "SystemVariableNotFound", 2);
    let unsupported_type =
        direct_set(&mut dispatcher, &mut mos, "RICO_BADTYPE", b"x", 1).unwrap_err();
    assert_structured(&unsupported_type, "SystemVariableTypeError", 5);
    for variable_type in [2, 3, 16] {
        let error = direct_set(
            &mut dispatcher,
            &mut mos,
            &format!("RICO_BADTYPE_{variable_type}"),
            b"x",
            variable_type,
        )
        .unwrap_err();
        assert_structured(&error, "SystemVariableTypeError", 5);
    }
    put_c_string(&mut mos, NAME_ADDRESS, "RICO_X_BADTYPE");
    mos.memory.write_bytes(VALUE_ADDRESS, b"x\0").unwrap();
    let mut unsupported_type_x = SwiContext::default();
    unsupported_type_x.registers[0] = NAME_ADDRESS;
    unsupported_type_x.registers[1] = VALUE_ADDRESS;
    unsupported_type_x.registers[2] = 1;
    unsupported_type_x.registers[4] = 1;
    assert_x_error(
        &mut dispatcher,
        &mut mos,
        OS_SET_VAR_VAL,
        &mut unsupported_type_x,
        5,
    );
    assert_structured(
        &direct_read(&mut dispatcher, &mut ordinary, "RICO_X_BADTYPE", 16, 0, 0).unwrap_err(),
        "SystemVariableNotFound",
        2,
    );
    // PRM R4=3 is the only conversion request; other input values mean raw
    // retrieval. In particular R4=4 is also a valid *write* type and must not
    // panic when supplied as this read flag. The output R4 is the stored type.
    for request_type in [1, 4] {
        let raw_read = direct_read(
            &mut dispatcher,
            &mut ordinary,
            "RICO_DIRECT",
            32,
            0,
            request_type,
        )
        .unwrap_or_else(|error| panic!("R4={request_type} should request raw read: {error:?}"));
        assert_eq!(raw_read.bytes, direct_value);
        assert_eq!(raw_read.variable_type, 0);
        assert_eq!(raw_read.registers[4], 0);
    }
    put_c_string(&mut ordinary, NAME_ADDRESS, "RICO_DIRECT");
    ordinary
        .memory
        .write_bytes(OUTPUT_ADDRESS, &[OUTPUT_SENTINEL; 32])
        .unwrap();
    let mut raw_read_x = SwiContext::default();
    raw_read_x.registers[0] = NAME_ADDRESS;
    raw_read_x.registers[1] = OUTPUT_ADDRESS;
    raw_read_x.registers[2] = 32;
    raw_read_x.registers[4] = 4;
    dispatcher
        .dispatch(OS_READ_VAR_VAL | X_BIT, &mut ordinary, &mut raw_read_x)
        .expect("X-form raw read with R4=4 succeeds without setting V");
    assert!(!raw_read_x.overflow, "successful X read leaves V clear");
    assert_eq!(raw_read_x.registers[0], NAME_ADDRESS);
    assert_eq!(raw_read_x.registers[1], OUTPUT_ADDRESS);
    assert_eq!(raw_read_x.registers[2], direct_value.len() as u32);
    assert_eq!(
        raw_read_x.registers[4], 0,
        "actual stored type replaces request"
    );
    assert_eq!(
        ordinary
            .memory
            .read_bytes(OUTPUT_ADDRESS, direct_value.len())
            .unwrap(),
        direct_value
    );
    let gst_value_error = direct_set(
        &mut dispatcher,
        &mut mos,
        "RICO_GST_UNSUPPORTED",
        b"value<0",
        0,
    )
    .unwrap_err();
    assert_structured(&gst_value_error, "SystemVariableExpansionError", 7);
    direct_set_with_terminator(
        &mut dispatcher,
        &mut mos,
        "RICO_LF_TERMINATOR",
        b"line-feed",
        0,
        b'\n',
    )
    .expect("PRM String accepts LF at R1+R2 as its terminator");
    direct_set_with_terminator(
        &mut dispatcher,
        &mut mos,
        "RICO_CR_TERMINATOR",
        b"carriage-return",
        0,
        b'\r',
    )
    .expect("PRM String accepts CR at R1+R2 as its terminator");
    assert_eq!(
        direct_read(
            &mut dispatcher,
            &mut ordinary,
            "RICO_LF_TERMINATOR",
            32,
            0,
            0,
        )
        .unwrap()
        .bytes,
        b"line-feed"
    );
    assert_eq!(
        direct_read(
            &mut dispatcher,
            &mut ordinary,
            "RICO_CR_TERMINATOR",
            32,
            0,
            0,
        )
        .unwrap()
        .bytes,
        b"carriage-return"
    );
    let mut bad_type0_terminator = SwiContext::default();
    put_c_string(&mut mos, NAME_ADDRESS, "RICO_BADTERMINATOR");
    mos.memory.write_bytes(VALUE_ADDRESS, b"valueX\0").unwrap();
    bad_type0_terminator.registers[0] = NAME_ADDRESS;
    bad_type0_terminator.registers[1] = VALUE_ADDRESS;
    bad_type0_terminator.registers[2] = 5;
    bad_type0_terminator.registers[4] = 0;
    let terminator_error = dispatcher
        .dispatch(OS_SET_VAR_VAL, &mut mos, &mut bad_type0_terminator)
        .unwrap_err();
    assert_structured(&terminator_error, "SystemVariableBufferError", 3);
    assert_structured(
        &direct_read(
            &mut dispatcher,
            &mut ordinary,
            "RICO_BADTERMINATOR",
            16,
            0,
            0,
        )
        .unwrap_err(),
        "SystemVariableNotFound",
        2,
    );

    // Type-0 maximum and type-4 raw values are both byte bounded. Rejected
    // over-limit writes cannot publish a prefix.
    let max_value = vec![b'v'; MAX_VALUE_BYTES];
    direct_set(&mut dispatcher, &mut mos, "RICO_MAX_VALUE", &max_value, 4)
        .expect("exactly 256 value bytes are supported");
    let max_read = direct_read(
        &mut dispatcher,
        &mut ordinary,
        "RICO_MAX_VALUE",
        MAX_VALUE_BYTES,
        0,
        0,
    )
    .expect("the maximum value reads into an equal-sized buffer");
    assert_eq!(max_read.bytes, max_value);
    let larger_capacity = direct_read(
        &mut dispatcher,
        &mut ordinary,
        "RICO_MAX_VALUE",
        MAX_VALUE_BYTES + 1,
        0,
        0,
    )
    .expect("capacity may exceed the maximum stored value");
    assert_eq!(larger_capacity.bytes, max_value);
    assert_eq!(
        larger_capacity.buffer_after[MAX_VALUE_BYTES], OUTPUT_SENTINEL,
        "unused capacity remains caller-owned"
    );
    let over_value = vec![b'x'; MAX_VALUE_BYTES + 1];
    let value_limit_error =
        direct_set(&mut dispatcher, &mut mos, "RICO_TOO_BIG", &over_value, 4).unwrap_err();
    assert_structured(&value_limit_error, "SystemVariableLimitError", 4);
    assert_structured(
        &direct_read(&mut dispatcher, &mut ordinary, "RICO_TOO_BIG", 8, 0, 0).unwrap_err(),
        "SystemVariableNotFound",
        2,
    );

    // A length-only probe does not access or validate R1. Ordinary reads do
    // validate the exact caller output range and X form reports that memory
    // failure through a standard error block rather than a host pointer.
    put_c_string(&mut ordinary, NAME_ADDRESS, "RICO_DIRECT");
    let mut invalid_probe = SwiContext::default();
    invalid_probe.registers[0] = NAME_ADDRESS;
    invalid_probe.registers[1] = u32::MAX;
    invalid_probe.registers[2] = 0x8000_0000;
    invalid_probe.registers[4] = 0;
    dispatcher
        .dispatch(OS_READ_VAR_VAL, &mut ordinary, &mut invalid_probe)
        .expect("a length probe does not dereference R1");
    assert_eq!(invalid_probe.registers[2], !(direct_value.len() as u32));
    let mut invalid_read = SwiContext::default();
    invalid_read.registers[0] = NAME_ADDRESS;
    invalid_read.registers[1] = u32::MAX;
    invalid_read.registers[2] = 32;
    invalid_read.registers[4] = 0;
    assert!(matches!(
        dispatcher.dispatch(OS_READ_VAR_VAL, &mut ordinary, &mut invalid_read),
        Err(RuntimeError::Memory(_))
    ));
    let mut invalid_read_x = SwiContext::default();
    invalid_read_x.registers[0] = NAME_ADDRESS;
    invalid_read_x.registers[1] = u32::MAX;
    invalid_read_x.registers[2] = 32;
    invalid_read_x.registers[4] = 0;
    assert_x_error(
        &mut dispatcher,
        &mut ordinary,
        OS_READ_VAR_VAL,
        &mut invalid_read_x,
        5,
    );
    put_c_string(&mut mos, NAME_ADDRESS, "RICO_WRITE_BAD_POINTER");
    let mut invalid_write = SwiContext::default();
    invalid_write.registers[0] = NAME_ADDRESS;
    invalid_write.registers[1] = u32::MAX;
    invalid_write.registers[2] = 1;
    invalid_write.registers[4] = 4;
    assert!(matches!(
        dispatcher.dispatch(OS_SET_VAR_VAL, &mut mos, &mut invalid_write),
        Err(RuntimeError::Memory(_))
    ));
    assert_structured(
        &direct_read(
            &mut dispatcher,
            &mut ordinary,
            "RICO_WRITE_BAD_POINTER",
            16,
            0,
            0,
        )
        .unwrap_err(),
        "SystemVariableNotFound",
        2,
    );
}

#[test]
fn variable_write_authority_is_task_scoped_and_runtime_local() {
    let _environment_guard = ENVIRONMENT_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let environment = IsolatedEnvironment::new();
    let (mut dispatcher, display_receiver) = new_dispatcher();
    let mut mos = Task::trusted_mos_session(0xCA_0401);
    let mut ordinary = Task::new(mos.id);
    let mut source_only = Task::trusted_source_inspector(0xCA_0402);
    let mut module_only = Task::trusted_module_manager(0xCA_0403);
    let mut configuration_only = Task::trusted_configuration_manager(0xCA_0404);
    for (address, fill) in OLD_BRIDGE_SCRATCH_ADDRESSES
        .into_iter()
        .zip([0x61, 0x72, 0x83])
    {
        mos.memory.write_bytes(address, &[fill; 64]).unwrap();
    }
    let bridge_sentinels =
        OLD_BRIDGE_SCRATCH_ADDRESSES.map(|address| mos.memory.read_bytes(address, 64).unwrap());
    let config_before = fs::read(&environment.config_path).ok();

    // Loading through the real management boundary must not let a guest
    // request the private Rust variable-store capability it would need to
    // mint authority or bypass the BASIC64 mutation policy.
    write_guest_module(
        &environment.root,
        "ForgedVariableWriter",
        FORGED_VARIABLE_WRITER,
    );
    put_c_string(&mut mos, MODULE_PATH_ADDRESS, "ForgedVariableWriter");
    let mut forged_module_load = SwiContext::default();
    forged_module_load.registers[0] = 1;
    forged_module_load.registers[1] = MODULE_PATH_ADDRESS;
    let forged_module_result = dispatcher.dispatch(OS_MODULE, &mut mos, &mut forged_module_load);
    assert!(
        matches!(
            &forged_module_result,
            Err(RuntimeError::Structured { type_name, .. })
                if type_name == "ModuleCapabilityDenied"
        ),
        "guest variable-store primitive import must be rejected: {forged_module_result:?}"
    );
    assert!(
        !module_is_present(&mut dispatcher, &mut mos, "ForgedVariableWriter"),
        "rejected capability request must not publish a module"
    );

    let direct_value = b"written-through-os-setvarval";
    direct_set(&mut dispatcher, &mut mos, "RICO_DIRECT", direct_value, 0).unwrap();

    // A wildcard update is permitted only when it identifies one existing
    // variable; multi-match update is rejected atomically, while wildcard
    // deletion removes every matching variable.
    direct_set(&mut dispatcher, &mut mos, "RICO_SINGLE_A", b"before", 4).unwrap();
    direct_set(&mut dispatcher, &mut mos, "RICO_MULTI_A", b"old-a", 4).unwrap();
    direct_set(&mut dispatcher, &mut mos, "RICO_MULTI_B", b"old-b", 4).unwrap();
    direct_set(&mut dispatcher, &mut mos, "RICO_SINGLE_*", b"after", 4)
        .expect("one matching variable may be updated by wildcard");
    assert_eq!(
        direct_read(&mut dispatcher, &mut ordinary, "RICO_SINGLE_A", 32, 0, 0)
            .unwrap()
            .bytes,
        b"after"
    );
    let (wildcard_cli_set_result, _) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*SET RICO_SINGLE_* through-cli",
    );
    wildcard_cli_set_result.expect("CLI may update a wildcard that identifies one variable");
    assert_eq!(
        direct_read(&mut dispatcher, &mut ordinary, "RICO_SINGLE_A", 32, 0, 0)
            .unwrap()
            .bytes,
        b"through-cli"
    );
    let ambiguous =
        direct_set(&mut dispatcher, &mut mos, "RICO_MULTI_*", b"changed", 4).unwrap_err();
    assert_structured(&ambiguous, "SystemVariablePatternAmbiguous", 6);
    let (ambiguous_cli_result, ambiguous_cli_output) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*SET RICO_MULTI_* cli-change",
    );
    let ambiguous_cli_text =
        format!("{ambiguous_cli_result:?} {ambiguous_cli_output}").to_ascii_lowercase();
    assert!(
        ambiguous_cli_text.contains("ambiguous") || ambiguous_cli_text.contains("pattern"),
        "CLI reports a wildcard update ambiguity: {ambiguous_cli_text}"
    );
    assert_eq!(
        direct_read(&mut dispatcher, &mut ordinary, "RICO_MULTI_A", 32, 0, 0)
            .unwrap()
            .bytes,
        b"old-a"
    );
    assert_eq!(
        direct_read(&mut dispatcher, &mut ordinary, "RICO_MULTI_B", 32, 0, 0)
            .unwrap()
            .bytes,
        b"old-b"
    );
    direct_delete(&mut dispatcher, &mut mos, "RICO_MULTI_*")
        .expect("negative R2 removes every matching variable");
    for name in ["RICO_MULTI_A", "RICO_MULTI_B"] {
        assert_structured(
            &direct_read(&mut dispatcher, &mut ordinary, name, 32, 0, 0).unwrap_err(),
            "SystemVariableNotFound",
            2,
        );
    }
    direct_set(&mut dispatcher, &mut mos, "RICO_UNSET_A", b"one", 4).unwrap();
    direct_set(&mut dispatcher, &mut mos, "RICO_UNSET_B", b"two", 4).unwrap();
    let (unset_result, _) = cli(
        &mut dispatcher,
        &mut mos,
        &display_receiver,
        "*UNSET RICO_UNSET_*",
    );
    unset_result.expect("CLI UNSET removes matching variables");
    for name in ["RICO_UNSET_A", "RICO_UNSET_B"] {
        assert_structured(
            &direct_read(&mut dispatcher, &mut ordinary, name, 32, 0, 0).unwrap_err(),
            "SystemVariableNotFound",
            2,
        );
    }

    // VariableWrite is separate from source, module, and configuration rights.
    // Reusing the MOS task's numeric ID cannot forge its private Task authority.
    for (task, name) in [
        (&mut ordinary, "RICO_DENIED_ORDINARY"),
        (&mut source_only, "RICO_DENIED_SOURCE"),
        (&mut module_only, "RICO_DENIED_MODULE"),
        (&mut configuration_only, "RICO_DENIED_CONFIG"),
    ] {
        let denied = direct_set(&mut dispatcher, task, name, b"no", 4).unwrap_err();
        assert_structured(&denied, "TaskAuthorizationDenied", 8);
    }
    for name in [
        "RICO_DENIED_ORDINARY",
        "RICO_DENIED_SOURCE",
        "RICO_DENIED_MODULE",
        "RICO_DENIED_CONFIG",
    ] {
        assert_structured(
            &direct_read(&mut dispatcher, &mut ordinary, name, 16, 0, 0).unwrap_err(),
            "SystemVariableNotFound",
            2,
        );
    }
    let mut forged_same_id = Task::new(mos.id);
    assert_structured(
        &direct_set(
            &mut dispatcher,
            &mut forged_same_id,
            "RICO_FORGED",
            b"no",
            4,
        )
        .unwrap_err(),
        "TaskAuthorizationDenied",
        8,
    );
    let denied_wildcard_update = direct_set(
        &mut dispatcher,
        &mut ordinary,
        "RICO_SINGLE_*",
        b"unauthorized",
        4,
    )
    .unwrap_err();
    assert_structured(&denied_wildcard_update, "TaskAuthorizationDenied", 8);
    assert_eq!(
        direct_read(&mut dispatcher, &mut ordinary, "RICO_SINGLE_A", 32, 0, 0)
            .unwrap()
            .bytes,
        b"through-cli",
        "authority denial must precede wildcard matching and preserve the single match"
    );
    let mut denied_bad_pointer = SwiContext::default();
    denied_bad_pointer.registers[0] = u32::MAX;
    denied_bad_pointer.registers[1] = u32::MAX;
    denied_bad_pointer.registers[2] = 1;
    denied_bad_pointer.registers[4] = 4;
    let denied_bad_pointer_result =
        dispatcher.dispatch(OS_SET_VAR_VAL, &mut ordinary, &mut denied_bad_pointer);
    assert_structured(
        &denied_bad_pointer_result.unwrap_err(),
        "TaskAuthorizationDenied",
        8,
    );
    let mut denied_bad_pointer_x = SwiContext::default();
    denied_bad_pointer_x.registers[0] = u32::MAX;
    denied_bad_pointer_x.registers[1] = u32::MAX;
    denied_bad_pointer_x.registers[2] = 1;
    denied_bad_pointer_x.registers[4] = 4;
    assert_x_error(
        &mut dispatcher,
        &mut ordinary,
        OS_SET_VAR_VAL,
        &mut denied_bad_pointer_x,
        8,
    );
    let (denied_cli_result, denied_cli_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*SET RICO_DENIED_CLI no",
    );
    let denied_text = format!("{denied_cli_result:?} {denied_cli_output}").to_ascii_lowercase();
    assert!(
        denied_text.contains("authorization") || denied_text.contains("authority"),
        "denied nested OS_CLI write must report its authority failure: {denied_text}"
    );
    assert_structured(
        &direct_read(&mut dispatcher, &mut ordinary, "RICO_DENIED_CLI", 16, 0, 0).unwrap_err(),
        "SystemVariableNotFound",
        2,
    );
    direct_set(&mut dispatcher, &mut mos, "RICO_DENIED_UNSET", b"keep", 4).unwrap();
    let (denied_unset_result, denied_unset_output) = cli(
        &mut dispatcher,
        &mut ordinary,
        &display_receiver,
        "*UNSET RICO_DENIED_UNSET",
    );
    let denied_unset =
        format!("{denied_unset_result:?} {denied_unset_output}").to_ascii_lowercase();
    assert!(denied_unset.contains("authorization") || denied_unset.contains("authority"));
    assert_eq!(
        direct_read(
            &mut dispatcher,
            &mut ordinary,
            "RICO_DENIED_UNSET",
            16,
            0,
            0
        )
        .unwrap()
        .bytes,
        b"keep",
        "denied UNSET leaves the store unchanged"
    );

    // CLI scratch, old fixed bridge addresses, and failed output buffers all
    // remain caller-owned. Direct variable writes do not persist to settings.
    for (address, expected) in OLD_BRIDGE_SCRATCH_ADDRESSES
        .into_iter()
        .zip(bridge_sentinels.iter())
    {
        assert_eq!(mos.memory.read_bytes(address, 64).unwrap(), *expected);
    }
    assert_eq!(fs::read(&environment.config_path).ok(), config_before);

    // A distinct dispatcher/runtime using the same settings path starts with
    // an empty variable store, while tasks in the original dispatcher shared
    // the values. This also rules out persistence and host-process storage.
    let (mut second_runtime, _) = new_dispatcher();
    let mut second_task = Task::new(0xCA_0201);
    assert_structured(
        &direct_read(
            &mut second_runtime,
            &mut second_task,
            "RICO_DIRECT",
            64,
            0,
            0,
        )
        .unwrap_err(),
        "SystemVariableNotFound",
        2,
    );
    direct_set(
        &mut second_runtime,
        &mut Task::trusted_mos_session(0xCA_0202),
        "RICO_DIRECT",
        b"isolated",
        4,
    )
    .unwrap();
    assert_eq!(
        direct_read(&mut dispatcher, &mut ordinary, "RICO_DIRECT", 64, 0, 0)
            .unwrap()
            .bytes,
        direct_value
    );

    // Count and aggregate limits are independently checked in fresh stores.
    let (mut count_runtime, _) = new_dispatcher();
    let mut count_task = Task::trusted_mos_session(0xCA_0301);
    for index in 0..128 {
        direct_set(
            &mut count_runtime,
            &mut count_task,
            &format!("Q{index:03}"),
            b"",
            4,
        )
        .unwrap_or_else(|error| panic!("variable {index} of 128 was rejected: {error:?}"));
    }
    let count_error = direct_set(&mut count_runtime, &mut count_task, "QOVER", b"", 4).unwrap_err();
    assert_structured(&count_error, "SystemVariableLimitError", 4);
    assert_structured(
        &direct_read(&mut count_runtime, &mut count_task, "QOVER", 8, 0, 0).unwrap_err(),
        "SystemVariableNotFound",
        2,
    );

    let (mut aggregate_runtime, _) = new_dispatcher();
    let mut aggregate_task = Task::trusted_mos_session(0xCA_0302);
    let max_payload = vec![b'a'; MAX_VALUE_BYTES];
    for index in 0..113 {
        let name = format!("A{:031}", index);
        direct_set(
            &mut aggregate_runtime,
            &mut aggregate_task,
            &name,
            &max_payload,
            4,
        )
        .unwrap_or_else(|error| panic!("aggregate entry {index} should fit: {error:?}"));
    }
    let aggregate_error = direct_set(
        &mut aggregate_runtime,
        &mut aggregate_task,
        &format!("A{:031}", 113),
        &max_payload,
        4,
    )
    .unwrap_err();
    assert_structured(&aggregate_error, "SystemVariableLimitError", 4);

    // The actual --stdio route accepts successive commands without dropping
    // the first character or falling through to an unrelated bridge.
    let stdio_config = environment.root.join("stdio-configure");
    let process = run_stdio(
        &stdio_config,
        &environment.root,
        b"*SET RICO_STDIO_VALUE before-unset\n*SHOW RICO_STDIO_VALUE\n*UNSET RICO_STDIO_VALUE\n*SHOW RICO_STDIO_VALUE\n*QUIT\n",
    );
    assert!(
        process.status.success(),
        "stdio command sequence failed: {process:?}"
    );
    let stdout = String::from_utf8_lossy(&process.stdout);
    assert!(
        stdout.contains("RICO_STDIO_VALUE (String): before-unset"),
        "stdio did not show the live variable: {stdout:?}"
    );
    assert!(
        !stdout.contains("Bad command"),
        "stdio lost a command character: {stdout:?}"
    );
    assert!(
        !stdout.to_ascii_lowercase().contains("unsupported"),
        "variable command fell through: {stdout:?}"
    );
}
