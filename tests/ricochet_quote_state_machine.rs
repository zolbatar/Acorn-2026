use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
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
const NAME: u32 = 0x3000;
const VALUE: u32 = 0x4000;
const OUTPUT: u32 = 0x8000;

fn setup() -> (PathBuf, SwiDispatcher, Task) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ricochet-quote-state-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    let (_input_tx, input_rx) = mpsc::channel();
    let (display_tx, _display_rx) = mpsc::channel::<DisplayEvent>();
    (
        root,
        SwiDispatcher::windowed(HostConsole::windowed(input_rx), display_tx),
        Task::trusted_mos_session(0xCA_7721),
    )
}

fn c_string(task: &mut Task, address: u32, value: &[u8]) {
    task.memory.write_bytes(address, value).unwrap();
    task.memory
        .write_byte(address + value.len() as u32, 0)
        .unwrap();
}

fn set(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
    value: &[u8],
    kind: u32,
) -> Result<(), RuntimeError> {
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
    context.registers[4] = kind;
    dispatcher.dispatch(WRITE, task, &mut context)
}

fn get(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
) -> Result<Vec<u8>, RuntimeError> {
    c_string(task, NAME, name.as_bytes());
    let mut context = SwiContext::default();
    context.registers[0] = NAME;
    context.registers[1] = OUTPUT;
    context.registers[2] = 256;
    dispatcher.dispatch(READ, task, &mut context)?;
    task.memory
        .read_bytes(OUTPUT, context.registers[2] as usize)
        .map_err(Into::into)
}

#[test]
fn quote_and_escape_state_table_preserves_the_bounded_subset() {
    let (root, mut dispatcher, mut task) = setup();

    // Each case goes through OS_SetVarVal and OS_ReadVarVal. The source forms
    // exercise both whole-input delimiters and escaped quote data.
    let cases: &[(&str, &[u8], &[u8])] = &[
        ("EMPTY_QUOTED", b"\"\"", b""),
        ("EMPTY_PLAIN", b"", b""),
        ("ESCAPE_BEGIN", b"|\"begin", b"\"begin"),
        ("ESCAPE_MIDDLE", b"mid|\"dle", b"mid\"dle"),
        ("ESCAPE_END", b"end|\"", b"end\""),
        ("QUOTED_ESCAPED_END", b"\"a|\"\"", b"a\""),
        ("QUOTED_DOUBLED", b"\"a\"\"b\"", b"a\"b"),
        ("QUOTED_TRIPLE_TAIL", b"\"a\"\"\"", b"a\""),
        ("QUOTED_FOUR_QUOTES", b"\"\"\"\"", b"\""),
        ("PIPE_ANGLE", b"|<x|>", b"<x>"),
        ("PIPE_PIPE", b"left||right", b"left|right"),
        ("PIPE_AND_QUOTE", b"a|||\"b", b"a|\"b"),
        ("UTF8_ESCAPED", "雪|\"é".as_bytes(), "雪\"é".as_bytes()),
        (
            "QUOTED_UTF8",
            "\"  雪|\"é  \"".as_bytes(),
            "  雪\"é  ".as_bytes(),
        ),
    ];
    for (name, source, expected) in cases {
        set(&mut dispatcher, &mut task, name, source, 0)
            .unwrap_or_else(|error| panic!("valid form {name} failed: {error:?}"));
        assert_eq!(
            get(&mut dispatcher, &mut task, name).unwrap(),
            *expected,
            "decoded bytes for {name}"
        );
    }

    // An escaped quote is data and cannot serve as the final delimiter.
    set(&mut dispatcher, &mut task, "ATOMIC", b"preserved", 4).unwrap();
    for malformed in [
        b"\"unterminated".as_slice(),
        b"\"a|\"",
        b"unquoted\"quote",
        b"\"closed\"tail\"",
        b"|",
        b"|x",
    ] {
        assert!(
            set(&mut dispatcher, &mut task, "ATOMIC", malformed, 0).is_err(),
            "malformed form should fail: {malformed:?}"
        );
        assert_eq!(
            get(&mut dispatcher, &mut task, "ATOMIC").unwrap(),
            b"preserved"
        );
    }

    // Type 4 bypasses quote and pipe parsing, and substituted bytes are
    // appended once rather than parsed again.
    set(&mut dispatcher, &mut task, "RAW", b"\"|\"<MISSING>", 4).unwrap();
    assert_eq!(
        get(&mut dispatcher, &mut task, "RAW").unwrap(),
        b"\"|\"<MISSING>"
    );
    set(&mut dispatcher, &mut task, "PAYLOAD", b"<MISSING>", 4).unwrap();
    set(&mut dispatcher, &mut task, "COMPOSED", b"<PAYLOAD>", 0).unwrap();
    assert_eq!(
        get(&mut dispatcher, &mut task, "COMPOSED").unwrap(),
        b"<MISSING>"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn reported_set_forms_work_through_public_stdio() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ricochet-quote-cli-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ricochet"))
        .arg("--stdio")
        .env("RICOCHET_CONFIG_PATH", root.join("configure"))
        .env("RICOCHET_DEMO_VOLUME", root.join("volume"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"*SET Empty \"\"\r*SET Quoted \"a|\"\"\r*SHOW Empty\r*SHOW Quoted\r*QUIT\r")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Empty (String):"), "{stdout}");
    assert!(stdout.contains("Quoted (String): a\""), "{stdout}");
    let _ = fs::remove_dir_all(root);
}
