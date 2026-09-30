use ricochet::{
    basic_compat,
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, SWI_ERROR_BLOCK_SIZE, Task},
    swi::{DisplayEvent, OS_BYTE, SwiContext, SwiDispatcher},
    tokenized_basic::TokenizedBasicProgram,
};
use std::sync::mpsc;

fn environment() -> (Task, SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_keys, input) = mpsc::channel();
    let (display, output) = mpsc::channel();
    (
        Task::new(1),
        SwiDispatcher::windowed(HostConsole::windowed(input), display),
        output,
    )
}

fn timer_round_trip(jit: bool) {
    let (mut task, mut dispatcher, _) = environment();
    // Both blocks are above 64K. Use the split pointer to set the timer and
    // the full-X convention to read it, including a deliberately stale Y%.
    let source = "10 P%=&10020:!P%=12345:P%?4=0\n\
                  20 A%=4:X%=&20:Y%=&100:C%=1:CALL &FFF1\n\
                  30 A%=3:X%=&10030:Y%=7:CALL &FFF1\n\
                  40 !&10100=A%:!&10104=X%:!&10108=Y%:!&1010C=C%\n\
                  50 END";
    if jit {
        basic_compat::run_source_jit(source, &mut task, &mut dispatcher).unwrap();
    } else {
        basic_compat::run_source(source, &mut task, &mut dispatcher).unwrap();
    }
    let bytes = task.memory.read_bytes(0x10030, 5).unwrap();
    let ticks = bytes.iter().enumerate().fold(0_u64, |value, (i, byte)| {
        value | (u64::from(*byte) << (i * 8))
    });
    assert!((12345..12445).contains(&ticks), "timer value: {ticks}");
    // CALL has no SYS TO return list; input register variables stay intact.
    for (offset, expected) in [(0, 3_u32), (4, 0x10030), (8, 7), (12, 1)] {
        let actual = task.memory.read_bytes(0x10100 + offset, 4).unwrap();
        assert_eq!(actual, expected.to_le_bytes());
    }
}

#[test]
fn call_osword_uses_checked_full_and_split_pointers() {
    timer_round_trip(false);
}

#[test]
fn tokenized_call_reaches_the_same_timer_service() {
    let (mut task, mut dispatcher, _) = environment();
    task.memory.write_bytes(8192, &[57, 48, 0, 0, 0]).unwrap();
    let body = b"A%=4:X%=8192:Y%=0:\xD6 &FFF1:A%=3:X%=8208:\xD6 &FFF1:\xE0";
    let mut saved = vec![13, 0, 10, (body.len() + 5) as u8];
    saved.extend_from_slice(body);
    saved.extend_from_slice(&[13, 13, 255]);
    let program = TokenizedBasicProgram::decode(&saved).unwrap();
    basic_compat::run_program(&program, &mut task, &mut dispatcher).unwrap();
    let bytes = task.memory.read_bytes(8208, 5).unwrap();
    let ticks = bytes.iter().enumerate().fold(0_u64, |value, (i, byte)| {
        value | (u64::from(*byte) << (i * 8))
    });
    assert!((12345..12445).contains(&ticks), "timer value: {ticks}");
}

#[test]
#[cfg(feature = "experimental-jit")]
fn hybrid_jit_can_use_the_same_mos_bridge() {
    timer_round_trip(true);
}

#[test]
fn mos_character_entrypoints_reach_output() {
    let (mut task, mut dispatcher, output) = environment();
    basic_compat::run_source(
        "10 A%=79:CALL &FFEE:A%=75:CALL &FFEE:A%=13:CALL &FFE3:CALL &FFE7:END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    let bytes: Vec<_> = output
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect();
    assert_eq!(bytes, b"OK\n\r\n\r");
}

#[test]
fn invalid_calls_do_not_alias_mos_entrypoints() {
    let crossing_end = format!(
        "10 A%=3:X%={}:Y%=0:CALL &FFF1",
        GUEST_MEMORY_BASE as usize + GUEST_MEMORY_SIZE + SWI_ERROR_BLOCK_SIZE - 2
    );
    for source in [
        "10 CALL &1234",
        "10 CALL 4295032814",
        crossing_end.as_str(),
        "10 A%=3:X%=0:Y%=&1000000:CALL &FFF1",
        "10 A%=99:X%=&2000:Y%=0:CALL &FFF1",
    ] {
        let (mut task, mut dispatcher, _) = environment();
        assert!(
            basic_compat::run_source(source, &mut task, &mut dispatcher).is_err(),
            "unexpected success for {source}"
        );
    }
}

#[test]
fn system_clock_is_shared_by_call_sys_and_basic_time() {
    let (mut task, mut dispatcher, _) = environment();
    basic_compat::run_source(
        "10 P%=&2000:!P%=4567:P%?4=0\n\
         20 A%=2:X%=P%:CALL &FFF1\n\
         30 !&2100=TIME\n\
         40 TIME=1234:SYS \"OS_Word\",1,P%\n\
         50 END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    for (address, expected) in [(0x2100, 4567_u32), (0x2000, 1234)] {
        let bytes: [u8; 4] = task
            .memory
            .read_bytes(address, 4)
            .unwrap()
            .try_into()
            .unwrap();
        let actual = u32::from_le_bytes(bytes);
        assert!((expected..expected + 100).contains(&actual));
    }
}

#[test]
fn osbyte_keyboard_queue_and_cli_use_existing_services() {
    let (mut task, mut dispatcher, _) = environment();
    basic_compat::run_source(
        "10 A%=138:X%=0:Y%=65:CALL &FFF4\n\
         20 SYS \"OS_Byte\",129,0,0 TO R%,K%,S%\n\
         30 !&2000=K%:!&2004=S%\n\
         40 P%=&2100:$P%=\"QUIT\":P%?5=88\n\
         50 X%=P%:CALL &FFF7:END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    assert_eq!(task.memory.read_byte(0x2000).unwrap(), 65);
    assert_eq!(task.memory.read_byte(0x2004).unwrap(), 0);
    assert!(dispatcher.quit_requested());
    assert_eq!(task.memory.read_byte(0x2104).unwrap(), 13);
    assert_eq!(task.memory.read_byte(0x2105).unwrap(), 88);
}

#[test]
fn osbyte_masks_inputs_preserves_registers_and_reports_input_status() {
    let (mut task, mut dispatcher, _) = environment();
    let mut context = SwiContext::default();
    context.registers[..3].copy_from_slice(&[0x18A, 0x100, 0x241]);
    dispatcher
        .dispatch(OS_BYTE, &mut task, &mut context)
        .unwrap();
    assert_eq!(&context.registers[..3], &[0x18A, 0x100, 0x241]);
    assert!(!context.carry);
    context.registers[..3].copy_from_slice(&[129, 17, 0]);
    dispatcher
        .dispatch(OS_BYTE, &mut task, &mut context)
        .unwrap();
    assert_eq!(&context.registers[..3], &[129, 65, 0]);
    for byte in [27, 66] {
        context.registers[..3].copy_from_slice(&[138, 0, byte]);
        dispatcher
            .dispatch(OS_BYTE, &mut task, &mut context)
            .unwrap();
    }
    context.registers[..3].copy_from_slice(&[129, 0, 0]);
    dispatcher
        .dispatch(OS_BYTE, &mut task, &mut context)
        .unwrap();
    assert_eq!(context.registers[2], 27);
    context.registers[..3].copy_from_slice(&[21, 0, 0]);
    dispatcher
        .dispatch(OS_BYTE, &mut task, &mut context)
        .unwrap();
    context.registers[..3].copy_from_slice(&[129, 0, 0]);
    dispatcher
        .dispatch(OS_BYTE, &mut task, &mut context)
        .unwrap();
    assert_eq!(context.registers[2], 255);
}

#[test]
fn mos_readc_consumes_input_without_overwriting_basic_registers() {
    let (mut task, mut dispatcher, _) = environment();
    basic_compat::run_source(
        "10 A%=138:X%=0:Y%=65:CALL &FFF4:Y%=66:CALL &FFF4\n\
         20 A%=99:CALL &FFE0:!&2000=A%\n\
         30 SYS \"OS_ReadC\" TO K%:!&2004=K%:END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    assert_eq!(task.memory.read_byte(0x2000).unwrap(), 99);
    assert_eq!(task.memory.read_byte(0x2004).unwrap(), 66);
}

#[test]
fn basic_prefetched_input_remains_available_to_mos_readc() {
    let (mut task, mut dispatcher, _) = environment();
    basic_compat::run_source(
        "10 A%=138:X%=0:Y%=65:CALL &FFF4\n\
         20 T%=TIME+2:REPEAT UNTIL TIME>=T%\n\
         30 SYS \"OS_ReadC\" TO K%:!&2000=K%:END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    assert_eq!(task.memory.read_byte(0x2000).unwrap(), 65);
}
