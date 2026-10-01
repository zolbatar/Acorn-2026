use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    basic_compat,
    error::RuntimeError,
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    swi::{
        DisplayEvent, OS_BYTE, OS_CLI, OS_READ_C, OS_WORD, SwiContext, SwiDispatchRoute,
        SwiDispatcher,
    },
};

const CLI_ADDRESS: u32 = 0x2100;
const BLOCK_ADDRESS: u32 = 0x12000;
const X_BIT: u32 = 1 << 17;

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
            .expect("system time is valid")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ricochet-mos-ownership-{nonce}"));
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

    fn guest_path(&self, name: &str) -> String {
        format!("HostFS::DemoDisk.$.{name}")
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

fn dispatcher() -> (
    SwiDispatcher,
    mpsc::Sender<u8>,
    mpsc::Receiver<DisplayEvent>,
) {
    let (input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender),
        input_sender,
        display_receiver,
    )
}

fn dispatch(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    number: u32,
    registers: [u32; 3],
) -> (Result<(), RuntimeError>, SwiContext) {
    let mut context = SwiContext::default();
    context.registers[..3].copy_from_slice(&registers);
    let result = dispatcher.dispatch(number, task, &mut context);
    (result, context)
}

fn assert_module_owner(dispatcher: &SwiDispatcher, number: u32, definition: &str) {
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned {
                number: routed_number,
                name,
                module,
                definition: routed_definition,
                ..
            }) if *routed_number == number
                && name.eq_ignore_ascii_case(if number == OS_BYTE { "OS_Byte" } else { "OS_Word" })
                && module.eq_ignore_ascii_case("Mos")
                && routed_definition.eq_ignore_ascii_case(definition)
        ),
        "SWI &{number:X} did not use Mos::{definition}: {:?}",
        dispatcher.last_dispatch_route()
    );
    assert_eq!(dispatcher.transitional_dispatch_count(), 0);
}

fn cli(dispatcher: &mut SwiDispatcher, task: &mut Task, command: &str) -> Result<(), RuntimeError> {
    task.memory
        .write_bytes(CLI_ADDRESS, command.as_bytes())
        .unwrap();
    task.memory
        .write_byte(CLI_ADDRESS + command.len() as u32, 0)
        .unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    dispatcher.dispatch(OS_CLI, task, &mut context)
}

fn word_value(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .enumerate()
        .fold(0_u64, |value, (index, byte)| {
            value | (u64::from(*byte) << (index * 8))
        })
}

#[test]
fn mos_byte_word_and_call_routes_are_module_owned_and_keep_checked_contracts() {
    let environment = Environment::new();
    fs::write(environment.root.join("exec-input"), b"E").unwrap();
    let (mut dispatcher, keyboard, _display) = dispatcher();
    let mut task = Task::trusted_mos_session(0x0607);
    let active_modules = dispatcher
        .module_registry()
        .active_modules_sorted()
        .into_iter()
        .map(|module| module.manifest.name.as_str())
        .collect::<Vec<_>>();
    assert!(
        active_modules
            .iter()
            .any(|module| module.eq_ignore_ascii_case("Mos")),
        "public dispatcher startup did not publish Mos; active modules: {active_modules:?}"
    );

    // The module's narrow host grants enable the public service for an
    // ordinary caller; they do not require or borrow MOS-session authority.
    let mut ordinary_task = Task::new(0x0608);
    dispatch(
        &mut dispatcher,
        &mut ordinary_task,
        OS_BYTE,
        [138, 0, u32::from(b'O')],
    )
    .0
    .expect("ordinary task can use the public keyboard insertion service");
    let (ordinary_key, ordinary_key_context) =
        dispatch(&mut dispatcher, &mut ordinary_task, OS_BYTE, [129, 0, 0]);
    ordinary_key.expect("ordinary task reads back its public keyboard byte");
    assert_eq!(ordinary_key_context.registers[1], u32::from(b'O'));
    ordinary_task
        .memory
        .write_bytes(BLOCK_ADDRESS, &[0; 5])
        .unwrap();
    dispatch(
        &mut dispatcher,
        &mut ordinary_task,
        OS_WORD,
        [1, BLOCK_ADDRESS, 0],
    )
    .0
    .expect("ordinary task can use a checked five-byte clock block");

    // The public numeric identities are owned by BASIC64 Mos definitions,
    // not the transitional Rust handler path.
    let byte_r0 = 0xABCD_018A;
    let byte_r1 = 0xCAFE_0100;
    let byte_r2 = 0xFACE_0141;
    let (inserted, insert_context) = dispatch(
        &mut dispatcher,
        &mut task,
        OS_BYTE,
        [byte_r0, byte_r1, byte_r2],
    );
    inserted.expect("OS_Byte 138 inserts into the hosted keyboard queue");
    assert_eq!(
        &insert_context.registers[..3],
        &[byte_r0, byte_r1, byte_r2],
        "OS_Byte 138 preserves all bits in its input registers"
    );
    assert!(!insert_context.carry);
    assert_module_owner(&dispatcher, OS_BYTE, "ByteService");

    let (read, read_context) = dispatch(&mut dispatcher, &mut task, OS_BYTE, [129, 0, 0]);
    read.expect("zero-timeout OS_Byte 129 reads an inserted key");
    assert_eq!(&read_context.registers[..3], &[129, 65, 0]);
    assert!(!read_context.carry);
    assert_module_owner(&dispatcher, OS_BYTE, "ByteService");

    // A fixed-size full-queue condition sets C, but leaves every input
    // register intact. The first queued byte remains the first one returned.
    for _ in 0..256 {
        let (result, context) = dispatch(&mut dispatcher, &mut task, OS_BYTE, [138, 0, 66]);
        result.expect("all available keyboard queue slots accept one byte");
        assert_eq!(&context.registers[..3], &[138, 0, 66]);
        assert!(!context.carry);
    }
    let (full, full_context) = dispatch(&mut dispatcher, &mut task, OS_BYTE, [138, 0, 67]);
    full.expect("full queue is reported by carry, not an SWI error");
    assert_eq!(&full_context.registers[..3], &[138, 0, 67]);
    assert!(full_context.carry);

    let flush_registers = [0xABCD_0015, 0xCAFE_0100, 0xFACE_0100];
    let (flush, flush_context) = dispatch(&mut dispatcher, &mut task, OS_BYTE, flush_registers);
    flush.expect("OS_Byte 21 flushes the supported keyboard queue");
    assert_eq!(
        &flush_context.registers[..3],
        &flush_registers,
        "OS_Byte 21 preserves R0/R1 and hosted R2 as well"
    );
    assert_module_owner(&dispatcher, OS_BYTE, "ByteService");
    let (timeout, timeout_context) = dispatch(
        &mut dispatcher,
        &mut task,
        OS_BYTE,
        [0x1234_0081, 0xDEAD_0000, 0xBEEF_0000],
    );
    timeout.expect("zero-timeout read completes without input");
    assert_eq!(
        &timeout_context.registers[..3],
        &[0x1234_0081, 255, 255],
        "OS_Byte 129 preserves R0 and returns status in the low result words"
    );
    assert!(timeout_context.carry);

    // X-form errors use the standard caller-local error result and do not
    // select a separate legacy implementation.
    let (unsupported_x, unsupported_context) =
        dispatch(&mut dispatcher, &mut task, OS_BYTE | X_BIT, [198, 0, 0]);
    unsupported_x.expect("X-form unsupported reason returns with V set");
    assert!(unsupported_context.overflow);
    let x_error_address = unsupported_context.registers[0];
    let x_error_code = u32::from_le_bytes(
        task.memory
            .read_bytes(x_error_address, 4)
            .unwrap()
            .try_into()
            .unwrap(),
    );
    assert_ne!(x_error_code, 0);
    assert_module_owner(&dispatcher, OS_BYTE, "ByteService");

    let before_transitional = dispatcher.transitional_dispatch_count();
    // *FX is still a public bridge, but its effect must re-enter the same
    // module-owned OS_Byte endpoint.
    cli(&mut dispatcher, &mut task, "*FX 138,0,90").expect("*FX insert command");
    let (fx_key, fx_key_context) = dispatch(&mut dispatcher, &mut task, OS_BYTE, [129, 0, 0]);
    fx_key.expect("OS_Byte receives the key inserted through *FX");
    assert_eq!(&fx_key_context.registers[..3], &[129, 90, 0]);
    assert_eq!(
        dispatcher.transitional_dispatch_count(),
        before_transitional
    );

    // The legacy CALL adapters translate into the same public numeric SWIs;
    // direct SYS calls exercise that register contract from interpreted BASIC.
    basic_compat::run_source(
        "10 A%=138:X%=0:Y%=75:CALL &FFF4\n20 SYS \"OS_Byte\",129,0,0 TO R%,K%,S%\n30 !&2200=K%:!&2204=S%\n40 END",
        &mut task,
        &mut dispatcher,
    )
    .expect("BASIC CALL &FFF4 and normal SYS dispatch through Mos");
    assert_eq!(task.memory.read_byte(0x2200).unwrap(), 75);
    assert_eq!(task.memory.read_byte(0x2204).unwrap(), 0);
    assert_module_owner(&dispatcher, OS_BYTE, "ByteService");

    basic_compat::run_source(
        "10 SYS \"XOS_Byte\",198,0,0 TO A%,X%,Y% ; FLAGS%\n20 !&2210=FLAGS%\n30 END",
        &mut task,
        &mut dispatcher,
    )
    .expect("named XOS_Byte follows the public module-owned error route");
    assert_ne!(task.memory.read_byte(0x2210).unwrap() & 1, 0);
    assert_module_owner(&dispatcher, OS_BYTE, "ByteService");

    // OS_Word uses the full logical R1 pointer and exactly five little-endian
    // bytes. System and interval clocks are independent and 40-bit bounded.
    let system_value = 0x12_3456_789A_u64;
    task.memory
        .write_bytes(BLOCK_ADDRESS, &system_value.to_le_bytes()[..5])
        .unwrap();
    let (set_system, set_context) = dispatch(
        &mut dispatcher,
        &mut task,
        OS_WORD,
        [2, BLOCK_ADDRESS, 0xfeed],
    );
    set_system.expect("OS_Word 2 sets the system clock from five bytes");
    assert_eq!(set_context.registers[0], 2);
    assert_eq!(set_context.registers[1], BLOCK_ADDRESS);
    assert_module_owner(&dispatcher, OS_WORD, "WordService");

    task.memory.write_bytes(BLOCK_ADDRESS, &[0; 5]).unwrap();
    let (read_system, read_context) =
        dispatch(&mut dispatcher, &mut task, OS_WORD, [1, BLOCK_ADDRESS, 0]);
    read_system.expect("OS_Word 1 reads the system clock");
    assert_eq!(read_context.registers[0], 1);
    assert_eq!(read_context.registers[1], BLOCK_ADDRESS);
    assert!(
        (system_value..system_value + 100).contains(&word_value(
            &task.memory.read_bytes(BLOCK_ADDRESS, 5).unwrap()
        )),
        "five-byte system clock read did not preserve the high byte"
    );
    let system_before_interval = word_value(&task.memory.read_bytes(BLOCK_ADDRESS, 5).unwrap());

    let interval_value = 0x01_0203_0405_u64;
    task.memory
        .write_bytes(BLOCK_ADDRESS, &interval_value.to_le_bytes()[..5])
        .unwrap();
    dispatch(&mut dispatcher, &mut task, OS_WORD, [4, BLOCK_ADDRESS, 0])
        .0
        .expect("OS_Word 4 sets the independent interval timer");
    task.memory.write_bytes(BLOCK_ADDRESS, &[0; 5]).unwrap();
    dispatch(&mut dispatcher, &mut task, OS_WORD, [3, BLOCK_ADDRESS, 0])
        .0
        .expect("OS_Word 3 reads the interval timer");
    assert!(
        (interval_value..interval_value + 100).contains(&word_value(
            &task.memory.read_bytes(BLOCK_ADDRESS, 5).unwrap()
        )),
        "five-byte interval read lost upper bytes"
    );
    task.memory.write_bytes(BLOCK_ADDRESS, &[0; 5]).unwrap();
    dispatch(&mut dispatcher, &mut task, OS_WORD, [1, BLOCK_ADDRESS, 0])
        .0
        .expect("system clock remains readable after interval operations");
    assert!(
        (system_before_interval..system_before_interval + 100).contains(&word_value(
            &task.memory.read_bytes(BLOCK_ADDRESS, 5).unwrap()
        )),
        "OS_Word 3/4 interval access modified the system clock"
    );

    let max_clock = (1_u64 << 40) - 1;
    task.memory
        .write_bytes(BLOCK_ADDRESS, &(max_clock - 50).to_le_bytes()[..5])
        .unwrap();
    dispatch(&mut dispatcher, &mut task, OS_WORD, [2, BLOCK_ADDRESS, 0])
        .0
        .expect("system clock accepts the upper 40-bit range");
    task.memory.write_bytes(BLOCK_ADDRESS, &[0; 5]).unwrap();
    dispatch(&mut dispatcher, &mut task, OS_WORD, [1, BLOCK_ADDRESS, 0])
        .0
        .expect("system clock reads the upper 40-bit range");
    let near_wrap = word_value(&task.memory.read_bytes(BLOCK_ADDRESS, 5).unwrap());
    assert!(
        (max_clock - 50..=max_clock).contains(&near_wrap),
        "40-bit system clock upper bytes were not preserved: {near_wrap:#x}"
    );
    task.memory
        .write_bytes(BLOCK_ADDRESS, &max_clock.to_le_bytes()[..5])
        .unwrap();
    dispatch(&mut dispatcher, &mut task, OS_WORD, [2, BLOCK_ADDRESS, 0])
        .0
        .expect("system clock accepts its maximum 40-bit value");
    std::thread::sleep(std::time::Duration::from_millis(30));
    task.memory.write_bytes(BLOCK_ADDRESS, &[0; 5]).unwrap();
    dispatch(&mut dispatcher, &mut task, OS_WORD, [1, BLOCK_ADDRESS, 0])
        .0
        .expect("system clock remains readable after wrap");
    let after_wrap = word_value(&task.memory.read_bytes(BLOCK_ADDRESS, 5).unwrap());
    assert!(
        after_wrap < 100,
        "system clock did not wrap modulo 40 bits: {after_wrap:#x}"
    );

    // Invalid five-byte read/write spans are rejected before any prefix bytes
    // are changed and before a set operation can mutate its clock.
    let invalid_address = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32 - 2;
    let error_block_address = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32;
    task.memory
        .write_bytes(invalid_address, &[0xA1, 0xB2])
        .unwrap();
    task.memory
        .write_bytes(error_block_address, &[0xC3, 0xD4, 0xE5])
        .unwrap();
    let before_system = word_value(&task.memory.read_bytes(BLOCK_ADDRESS, 5).unwrap());
    let (invalid_read, _) = dispatch(&mut dispatcher, &mut task, OS_WORD, [1, invalid_address, 0]);
    assert!(
        invalid_read.is_err(),
        "cross-boundary clock output must fail"
    );
    assert_eq!(
        task.memory.read_bytes(invalid_address, 2).unwrap(),
        [0xA1, 0xB2]
    );
    assert_eq!(
        task.memory.read_bytes(error_block_address, 3).unwrap(),
        [0xC3, 0xD4, 0xE5],
        "a five-byte clock write must not spill into the reserved X-error block"
    );
    let (invalid_set, _) = dispatch(&mut dispatcher, &mut task, OS_WORD, [2, invalid_address, 0]);
    assert!(invalid_set.is_err(), "cross-boundary clock input must fail");
    assert_eq!(
        task.memory.read_bytes(error_block_address, 3).unwrap(),
        [0xC3, 0xD4, 0xE5],
        "a five-byte clock read must not spill into the reserved X-error block"
    );
    task.memory.write_bytes(BLOCK_ADDRESS, &[0; 5]).unwrap();
    dispatch(&mut dispatcher, &mut task, OS_WORD, [1, BLOCK_ADDRESS, 0])
        .0
        .expect("system clock remains readable after rejected writes");
    let after_system = word_value(&task.memory.read_bytes(BLOCK_ADDRESS, 5).unwrap());
    assert!(
        (before_system..before_system + 100).contains(&after_system),
        "invalid OS_Word write changed the system clock"
    );
    assert_module_owner(&dispatcher, OS_WORD, "WordService");

    basic_compat::run_source(
        "10 P%=&12000:!P%=98765:P%?4=0\n20 A%=2:X%=0:Y%=&120:C%=1:CALL &FFF1\n30 A%=1:X%=&12000:SYS \"OS_Word\",A%,X%\n40 END",
        &mut task,
        &mut dispatcher,
    )
    .expect("BASIC CALL &FFF1 split pointer and normal SYS share OS_Word");
    assert!((98765..98865).contains(&word_value(&task.memory.read_bytes(0x12000, 5).unwrap())));
    assert_module_owner(&dispatcher, OS_WORD, "WordService");

    // While *EXEC owns the task input stream, OS_Byte 129 cannot consume the
    // source or a queued host key. Stopping Exec reveals the untouched key.
    keyboard.send(b'H').unwrap();
    cli(
        &mut dispatcher,
        &mut task,
        &format!("*EXEC {}", environment.guest_path("exec-input")),
    )
    .expect("install checked guest Exec source");
    let (exec_timeout, exec_timeout_context) =
        dispatch(&mut dispatcher, &mut task, OS_BYTE, [129, 0, 0]);
    exec_timeout.expect("timed poll while Exec active does not block");
    assert_eq!(&exec_timeout_context.registers[..3], &[129, 255, 255]);
    let mut read_exec = SwiContext::default();
    dispatcher
        .dispatch(OS_READ_C, &mut task, &mut read_exec)
        .unwrap();
    assert_eq!(read_exec.registers[0], u32::from(b'E'));
    cli(&mut dispatcher, &mut task, "*EXEC").expect("bare Exec stops the source");
    let (host_key, host_key_context) = dispatch(&mut dispatcher, &mut task, OS_BYTE, [129, 0, 0]);
    host_key.expect("queued host byte survives timed poll while Exec is active");
    assert_eq!(&host_key_context.registers[..3], &[129, u32::from(b'H'), 0]);

    // This hosted slice intentionally does not publish OS_Byte 198 handle
    // control. The public OS_Byte identity still belongs to Mos and returns an
    // explicit unsupported-reason error rather than using a Rust fallback.
    let (exec_control, _) = dispatch(&mut dispatcher, &mut task, OS_BYTE, [198, 0, 0]);
    assert!(exec_control.is_err());
    assert_module_owner(&dispatcher, OS_BYTE, "ByteService");

    // Mos is a protected foundation module. A trusted host's quiesce can mark
    // its published services inactive; dispatch then errors rather than
    // falling back to the historical Rust implementation.
    dispatcher
        .basic64_module_manager()
        .quiesce("Mos", &mut task)
        .expect("Mos reaches quiescing through its module lifecycle");
    let (inactive, _) = dispatch(&mut dispatcher, &mut task, OS_BYTE, [138, 0, 88]);
    assert!(
        matches!(inactive, Err(RuntimeError::Program(ref message)) if message.contains("published") && message.contains("owning module")),
        "inactive published OS_Byte must not fall through to Rust: {inactive:?}"
    );
    assert_eq!(dispatcher.transitional_dispatch_count(), 0);
    let unload = dispatcher.basic64_module_manager().retire("Mos", &mut task);
    assert!(
        unload.is_err(),
        "a foundation MOS service module must not be unloadable"
    );
}
