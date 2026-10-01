use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiContext, SwiDispatcher},
};

const READ: u32 = 0x23;
const WRITE: u32 = 0x24;
const X_BIT: u32 = 1 << 17;
const NAME: u32 = 0x3000;
const VALUE: u32 = 0x4000;
const OUTPUT: u32 = 0x8000;

fn setup() -> (PathBuf, SwiDispatcher, Task, mpsc::Receiver<DisplayEvent>) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ricochet-variable-expansion-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let (display_tx, display_rx) = mpsc::channel();
    drop(input_tx);
    let dispatcher = SwiDispatcher::windowed(HostConsole::windowed(input_rx), display_tx);
    (
        root,
        dispatcher,
        Task::trusted_mos_session(0xCA_9911),
        display_rx,
    )
}

fn c_string(task: &mut Task, address: u32, value: &[u8]) {
    task.memory.write_bytes(address, value).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn write(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    value: &[u8],
    kind: u32,
    r3: u32,
) -> Result<SwiContext, RuntimeError> {
    c_string(task, NAME, name.as_bytes());
    task.memory.write_bytes(VALUE, value).unwrap();
    if kind == 0 {
        task.memory
            .write_byte(VALUE + value.len() as u32, 0)
            .unwrap();
    }
    let mut context = SwiContext::default();
    context.registers[0] = NAME;
    context.registers[1] = VALUE;
    context.registers[2] = value.len() as u32;
    context.registers[3] = r3;
    context.registers[4] = kind;
    dispatcher.dispatch(WRITE, task, &mut context)?;
    Ok(context)
}

fn read(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    capacity_or_probe: u32,
    kind: u32,
) -> Result<SwiContext, RuntimeError> {
    c_string(task, NAME, name.as_bytes());
    task.memory.write_bytes(OUTPUT, &[0xA5; 300]).unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = NAME;
    context.registers[1] = OUTPUT;
    context.registers[2] = capacity_or_probe;
    context.registers[4] = kind;
    dispatcher.dispatch(READ, task, &mut context)?;
    Ok(context)
}

#[test]
fn bounded_string_expansion_and_literal_type_are_distinct() {
    let (root, mut dispatcher, mut task, _display) = setup();
    let first = write(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_INNER",
        b"<RICO_EXP_MISSING>",
        4,
        0,
    )
    .unwrap();
    assert_eq!(first.registers[4], 4);
    write(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_OUTER",
        b"\"  <RICO_EXP_INNER> |<x|> || |\"  \"",
        0,
        0,
    )
    .unwrap();
    let result = read(&mut dispatcher, &mut task, "RICO_EXP_OUTER", 256, 3).unwrap();
    let actual = task
        .memory
        .read_bytes(OUTPUT, result.registers[2] as usize)
        .unwrap();
    assert_eq!(actual, b"  <RICO_EXP_MISSING> <x> | \"  ");

    write(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_RAW",
        b"<NO-SUCH>|Q\"",
        4,
        0,
    )
    .unwrap();
    let raw = read(&mut dispatcher, &mut task, "RICO_EXP_RAW", 256, 0).unwrap();
    assert_eq!(
        task.memory
            .read_bytes(OUTPUT, raw.registers[2] as usize)
            .unwrap(),
        b"<NO-SUCH>|Q\""
    );

    for malformed in [b"<OPEN".as_slice(), b"|Q", b"<NO-SUCH>", b"<1>", b"\"OPEN"] {
        assert!(
            write(
                &mut dispatcher,
                &mut task,
                "RICO_EXP_OUTER",
                malformed,
                0,
                0
            )
            .is_err(),
            "{malformed:?}"
        );
        let unchanged = read(&mut dispatcher, &mut task, "RICO_EXP_OUTER", 256, 0).unwrap();
        assert_eq!(
            task.memory
                .read_bytes(OUTPUT, unchanged.registers[2] as usize)
                .unwrap(),
            actual
        );
    }

    let probe = read(&mut dispatcher, &mut task, "RICO_EXP_OUTER", 0x8000_0000, 3).unwrap();
    assert_eq!(probe.registers[2], 0x8000_0000 | actual.len() as u32);
    let missing_probe = read(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_MISSING",
        0x8000_0000,
        3,
    )
    .unwrap();
    assert_eq!(missing_probe.registers[2], 0);

    // The ABI's hosted selector is NUL-only; a CR terminator is not accepted.
    task.memory
        .write_bytes(NAME, b"RICO_EXP_OUTER\r\0")
        .unwrap();
    let mut selector = SwiContext::default();
    selector.registers[0] = NAME;
    selector.registers[1] = OUTPUT;
    selector.registers[2] = 256;
    assert!(dispatcher.dispatch(READ, &mut task, &mut selector).is_err());

    let nonzero_context =
        write(&mut dispatcher, &mut task, "RICO_EXP_REJECT", b"x", 0, 1).unwrap_err();
    assert!(
        matches!(nonzero_context, RuntimeError::Structured { ref type_name, .. } if type_name == "SystemVariableNameError")
    );
    let rejected_probe = read(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_REJECT",
        0x8000_0000,
        0,
    )
    .unwrap();
    assert_eq!(rejected_probe.registers[2], 0);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn substituted_values_are_not_rescanned_and_failures_do_not_mutate() {
    let (root, mut dispatcher, mut task, _display) = setup();
    write(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_PAYLOAD",
        b"<RICO_EXP_ABSENT>",
        4,
        0,
    )
    .unwrap();
    write(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_RESULT",
        b"<RICO_EXP_PAYLOAD>",
        0,
        0,
    )
    .unwrap();
    let value = read(&mut dispatcher, &mut task, "RICO_EXP_RESULT", 256, 0).unwrap();
    assert_eq!(
        task.memory
            .read_bytes(OUTPUT, value.registers[2] as usize)
            .unwrap(),
        b"<RICO_EXP_ABSENT>"
    );
    let error = write(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_RESULT",
        b"<RICO_EXP_ABSENT>",
        0,
        0,
    )
    .unwrap_err();
    assert!(
        matches!(error, RuntimeError::Structured { ref type_name, code: 2, .. } if type_name == "SystemVariableNotFound")
    );
    let unchanged = read(&mut dispatcher, &mut task, "RICO_EXP_RESULT", 256, 0).unwrap();
    assert_eq!(
        task.memory
            .read_bytes(OUTPUT, unchanged.registers[2] as usize)
            .unwrap(),
        b"<RICO_EXP_ABSENT>"
    );
    let bad_context = write(
        &mut dispatcher,
        &mut task,
        "RICO_EXP_REJECT",
        b"x",
        4,
        0x1234,
    )
    .unwrap_err();
    assert!(
        matches!(bad_context, RuntimeError::Structured { ref type_name, .. } if type_name == "SystemVariableNameError")
    );
    let _ = fs::remove_dir_all(root);
    let _ = X_BIT;
}
