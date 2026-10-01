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
    let root = std::env::temp_dir().join(format!("ricochet-quote-regressions-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    let (input_tx, input_rx) = mpsc::channel();
    let (display_tx, _display_rx) = mpsc::channel::<DisplayEvent>();
    drop(input_tx);
    (
        root,
        SwiDispatcher::windowed(HostConsole::windowed(input_rx), display_tx),
        Task::trusted_mos_session(0xCA_7711),
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
    let mut ctx = SwiContext::default();
    ctx.registers[0] = NAME;
    ctx.registers[1] = VALUE;
    ctx.registers[2] = value.len() as u32;
    ctx.registers[4] = kind;
    dispatcher.dispatch(WRITE, task, &mut ctx)?;
    Ok(())
}

fn get(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    name: &str,
) -> Result<Vec<u8>, RuntimeError> {
    c_string(task, NAME, name.as_bytes());
    let mut ctx = SwiContext::default();
    ctx.registers[0] = NAME;
    ctx.registers[1] = OUTPUT;
    ctx.registers[2] = 256;
    dispatcher.dispatch(READ, task, &mut ctx)?;
    task.memory
        .read_bytes(OUTPUT, ctx.registers[2] as usize)
        .map_err(Into::into)
}

#[test]
fn escape_aware_quotes_cover_boundaries_and_reject_malformed_forms_atomically() {
    let (root, mut dispatcher, mut task) = setup();
    for (key, source, expected) in [
        ("Q_BEGIN", b"|\"begin".as_slice(), b"\"begin".as_slice()),
        ("Q_MIDDLE", b"mid|\"dle", b"mid\"dle"),
        ("Q_END", b"end|\"", b"end\""),
        ("Q_QUOTED", b"\"  quoted  \"", b"  quoted  "),
        ("Q_DOUBLE", b"\"a\"\"b\"", b"a\"b"),
        ("Q_TRIPLE", b"\"a\"\"\"", b"a\""),
        ("Q_UTF8", "é|\"雪".as_bytes(), "é\"雪".as_bytes()),
    ] {
        set(&mut dispatcher, &mut task, key, source, 0).unwrap();
        assert_eq!(
            get(&mut dispatcher, &mut task, key).unwrap(),
            expected,
            "{key}"
        );
    }

    set(&mut dispatcher, &mut task, "Q_ATOMIC", b"kept", 4).unwrap();
    for malformed in [
        b"bare\"quote".as_slice(),
        b"\"unfinished",
        b"\"escaped end|\"",
        b"\"even terminal\"\"",
        b"\"a\"b\"",
    ] {
        assert!(
            set(&mut dispatcher, &mut task, "Q_ATOMIC", malformed, 0).is_err(),
            "{malformed:?}"
        );
        assert_eq!(
            get(&mut dispatcher, &mut task, "Q_ATOMIC").unwrap(),
            b"kept"
        );
    }

    set(
        &mut dispatcher,
        &mut task,
        "Q_RAW",
        b"before|\"after\"<missing>",
        4,
    )
    .unwrap();
    assert_eq!(
        get(&mut dispatcher, &mut task, "Q_RAW").unwrap(),
        b"before|\"after\"<missing>"
    );
    set(&mut dispatcher, &mut task, "Q_PAYLOAD", b"<Q_ABSENT>", 4).unwrap();
    set(&mut dispatcher, &mut task, "Q_RESULT", b"<Q_PAYLOAD>", 0).unwrap();
    assert_eq!(
        get(&mut dispatcher, &mut task, "Q_RESULT").unwrap(),
        b"<Q_ABSENT>"
    );
    assert!(set(&mut dispatcher, &mut task, "Q_BOUND", &vec![b'x'; 257], 0).is_err());
    set(&mut dispatcher, &mut task, "Q_BOUND", &vec![b'x'; 256], 0).unwrap();
    assert_eq!(
        get(&mut dispatcher, &mut task, "Q_BOUND").unwrap().len(),
        256
    );

    let mut ordinary = Task::new(0xCA_7712);
    c_string(&mut ordinary, NAME, b"Q_DENIED");
    ordinary.memory.write_bytes(VALUE, b"x").unwrap();
    ordinary.memory.write_byte(VALUE + 1, 0).unwrap();
    let mut ctx = SwiContext::default();
    ctx.registers[0] = NAME;
    ctx.registers[1] = VALUE;
    ctx.registers[2] = 1;
    assert!(dispatcher.dispatch(WRITE, &mut ordinary, &mut ctx).is_err());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn public_stdio_accepts_an_unquoted_escaped_quote() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("ricochet-quote-stdio-{nonce}"));
    fs::create_dir_all(&root).unwrap();
    let config = root.join("config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_ricochet"))
        .arg("--stdio")
        .env("RICOCHET_CONFIG_PATH", config)
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
        .write_all(b"*SET RootQuote before|\"after\r*SHOW RootQuote\r*QUIT\r")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("before\"after"), "{stdout}");
    let _ = fs::remove_dir_all(root);
}
