#![cfg(feature = "experimental-jit")]

use std::sync::mpsc;

use ricochet::{
    basic_compat::{self, JitExecutionReport, StrictJitOptions},
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiDispatcher},
    tokenized_basic::TokenizedBasicProgram,
};

fn environment() -> (Task, SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_keys, input) = mpsc::channel();
    let (display, output) = mpsc::channel();
    (
        Task::new(31),
        SwiDispatcher::windowed(HostConsole::windowed(input), display),
        output,
    )
}

fn output_bytes(output: &mpsc::Receiver<DisplayEvent>) -> Vec<u8> {
    output
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect()
}

fn assert_strict_report(report: &JitExecutionReport) {
    assert!(report.strict_native);
    assert_eq!(report.interpreted_statement_count, 0);
    assert_eq!(report.interpreted_expression_count, 0);
    assert!(report.runtime_helper_calls > 0);
    assert!(!report.compiled_units.is_empty());
}

#[test]
fn strict_native_control_flow_strings_and_parameter_scopes_match_expected_output() {
    let source = "10 A%=0:FOR I%=1 TO 2:FOR J%=1 TO 3:A%=A%+1:NEXT:NEXT\n\
         20 K%=0:REPEAT:K%=K%+1:UNTILK%=3\n\
         30 S$=\"ABC\":MID$(S$,2,1)=\"Z\":S$=LEFT$(S$,2)+RIGHT$(S$,1)\n\
         40 PROCP(A%,S$):GOSUB100\n\
         50 PRINTA%;\",\";I%;\",\";J%;\",\";K%;\",\";S$;SPC(2);\"X\"\n\
         60 END\n\
         70 DEFPROCP(X%,Y$):IFX%=6ENDPROC:X%=99:Y$=\"changed\":ENDPROC\n\
         100 PRINT\"g\";:RETURN";
    let (mut reference_task, mut reference_dispatcher, reference_output) = environment();
    basic_compat::run_source(source, &mut reference_task, &mut reference_dispatcher).unwrap();
    let reference_bytes = output_bytes(&reference_output);

    let (mut task, mut dispatcher, output) = environment();
    let report = basic_compat::run_source_jit_strict_with_options(
        source,
        &mut task,
        &mut dispatcher,
        StrictJitOptions {
            benchmark_validation: true,
        },
    )
    .unwrap();
    assert_strict_report(&report);
    let output = String::from_utf8_lossy(&output_bytes(&output)).into_owned();
    assert_eq!(output.as_bytes(), reference_bytes);
    assert!(output.contains("g6,3,4,3,AZC  X"), "output: {output:?}");
}

#[test]
fn strict_native_arrays_data_and_print_formatting_match_interpreter() {
    let source = "10 DIM A(2):DATA 7,\"x\",9:READ A(0),S$,A(2)\n\
         20 RESTORE:READ B%,T$:PRINT A(0);\",\";S$;\",\";A(2);\",\";B%;\",\";T$\n\
         30 END";
    let (mut reference_task, mut reference_dispatcher, reference_output) = environment();
    basic_compat::run_source(source, &mut reference_task, &mut reference_dispatcher).unwrap();
    let reference_bytes = output_bytes(&reference_output);

    let (mut task, mut dispatcher, output) = environment();
    let report = basic_compat::run_source_jit_strict(source, &mut task, &mut dispatcher).unwrap();
    assert_strict_report(&report);
    assert_eq!(output_bytes(&output), reference_bytes);
    assert_eq!(String::from_utf8_lossy(&reference_bytes), "7,x,9,7,x\n\r");
}

#[test]
fn strict_native_call_uses_checked_mos_clock_memory_service() {
    let source = "10 P%=&2000:!P%=12345:P%?4=0\n\
         20 A%=4:X%=P%:Y%=0:CALL &FFF1\n\
         30 A%=3:X%=P%+16:Y%=0:CALL &FFF1\n\
         40 !&2100=A%:END";
    let (mut reference_task, mut reference_dispatcher, _) = environment();
    basic_compat::run_source(source, &mut reference_task, &mut reference_dispatcher).unwrap();

    let (mut task, mut dispatcher, _) = environment();
    let report = basic_compat::run_source_jit_strict(source, &mut task, &mut dispatcher).unwrap();
    assert_strict_report(&report);
    let read_ticks = |task: &Task| {
        task.memory
            .read_bytes(0x2010, 5)
            .unwrap()
            .iter()
            .enumerate()
            .fold(0_u64, |value, (index, byte)| {
                value | (u64::from(*byte) << (index * 8))
            })
    };
    let reference_ticks = read_ticks(&reference_task);
    let native_ticks = read_ticks(&task);
    assert!(
        (12345..12445).contains(&reference_ticks),
        "interpreter OSWORD returned {reference_ticks}"
    );
    assert!(
        (12345..12445).contains(&native_ticks),
        "native OSWORD returned {native_ticks}"
    );
    assert!(reference_ticks.abs_diff(native_ticks) <= 5);
    assert_eq!(
        reference_task.memory.read_bytes(0x2100, 4).unwrap(),
        task.memory.read_bytes(0x2100, 4).unwrap()
    );
    assert_eq!(
        task.memory.read_bytes(0x2100, 4).unwrap(),
        3_i32.to_le_bytes()
    );
}

#[test]
fn strict_native_rejects_unsupported_code_before_running_prior_statements() {
    let (mut task, mut dispatcher, output) = environment();
    let error = basic_compat::run_source_jit_strict(
        "10 A%=65:CALL &FFEE\n20 VDU 7\n30 END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("line 20"), "diagnostic: {error}");
    assert!(output_bytes(&output).is_empty());
}

#[test]
fn strict_native_guarded_unsupported_operations_fail_only_when_reached() {
    let source = "10 A%=0\n20 IF A% THEN VDU 7\n30 PRINT\"ok\"\n40 END";
    let (mut reference_task, mut reference_dispatcher, reference_output) = environment();
    basic_compat::run_source(source, &mut reference_task, &mut reference_dispatcher).unwrap();
    let reference_bytes = output_bytes(&reference_output);

    let (mut task, mut dispatcher, output) = environment();
    basic_compat::run_source_jit_strict(source, &mut task, &mut dispatcher).unwrap();
    assert_eq!(output_bytes(&output), reference_bytes);

    let (mut task, mut dispatcher, _) = environment();
    let error = basic_compat::run_source_jit_strict(
        "10 A%=1\n20 IF A% THEN VDU 7\n30 END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("line 20: unsupported native operation reached"),
        "{error}"
    );
}

#[test]
fn strict_native_array_and_division_errors_keep_source_locations() {
    for (source, expected) in [
        ("10 DIM A(1):A(2)=5:END", "line 10"),
        ("10 A%=1/0:END", "line 10: division by zero"),
    ] {
        let (mut reference_task, mut reference_dispatcher, _) = environment();
        let reference_error =
            basic_compat::run_source(source, &mut reference_task, &mut reference_dispatcher)
                .unwrap_err()
                .to_string();
        let (mut task, mut dispatcher, _) = environment();
        let error = basic_compat::run_source_jit_strict(source, &mut task, &mut dispatcher)
            .unwrap_err()
            .to_string();
        assert_eq!(error, reference_error, "{source}");
        assert!(error.contains(expected), "{source}: {error}");
    }
}

#[test]
fn strict_native_recursive_calls_stop_at_the_checked_depth_limit() {
    let (mut task, mut dispatcher, _) = environment();
    let error = basic_compat::run_source_jit_strict(
        "10 PROCP(300):END\n20 DEFPROCP(N%):PROCP(N%-1):ENDPROC",
        &mut task,
        &mut dispatcher,
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("line 20: native call depth limit reached"),
        "{error}"
    );
}

#[test]
fn strict_native_unknown_machine_code_addresses_keep_the_mos_error() {
    let source = "10 CALL &1234\n20 END";
    let (mut reference_task, mut reference_dispatcher, _) = environment();
    let reference_error =
        basic_compat::run_source(source, &mut reference_task, &mut reference_dispatcher)
            .unwrap_err()
            .to_string();

    let (mut task, mut dispatcher, _) = environment();
    let error = basic_compat::run_source_jit_strict(source, &mut task, &mut dispatcher)
        .unwrap_err()
        .to_string();
    assert_eq!(error, reference_error);
    assert!(error.contains("line 10"), "{error}");
}

fn assert_clocksp5_output(output: &[u8]) {
    assert!(
        !String::from_utf8_lossy(output).contains("Bad command"),
        "ClockSP5's hosted FX reset should be a quiet no-op"
    );
    for heading in [
        b"Real REPEAT loop".as_slice(),
        b"Variant REPEAT loop",
        b"Integer REPEAT loop",
        b"Real FOR loop",
        b"Variant FOR loop",
        b"Integer FOR loop",
        b"Trig/Log test",
        b"String manipulation",
        b"Procedure call",
        b"GOSUB call",
        b"Compared to a 2.00MHz",
    ] {
        let count = output
            .windows(heading.len())
            .filter(|window| *window == heading)
            .count();
        assert_eq!(
            count,
            3,
            "expected three instances of {}",
            String::from_utf8_lossy(heading)
        );
    }

    let mhz_values = output
        .windows(3)
        .enumerate()
        .filter_map(|(end, window)| {
            if window != b"MHz" {
                return None;
            }
            let mut start = end;
            while start > 0
                && (output[start - 1].is_ascii_digit() || matches!(output[start - 1], b'.' | b'-'))
            {
                start -= 1;
            }
            std::str::from_utf8(&output[start..end])
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
        })
        .collect::<Vec<_>>();
    assert!(
        mhz_values.len() >= 33,
        "expected numeric MHz output for each row in all three passes"
    );
    assert!(
        mhz_values
            .iter()
            .all(|value| value.is_finite() && *value > 0.0),
        "ClockSP5 produced a zero or invalid benchmark result: {mhz_values:?}"
    );
}

fn report_clocksp5_observation(label: &str, report: &JitExecutionReport, output: &[u8]) {
    eprintln!(
        "ClockSP5 {label}: {} native units, {} native calls, {} runtime helpers, {} interpreted statements, {} interpreted expressions (compile {:.2}s, run {:.2}s)",
        report.compiled_units.len(),
        report.compiled_calls,
        report.runtime_helper_calls,
        report.interpreted_statement_count,
        report.interpreted_expression_count,
        report.compile_time.as_secs_f64(),
        report.compiled_time.as_secs_f64(),
    );
    eprintln!("{}", String::from_utf8_lossy(output));
}

#[test]
fn strict_native_runs_all_clocksp5_source_passes() {
    let (mut task, mut dispatcher, output) = environment();
    let report = basic_compat::run_source_jit_strict_with_options(
        include_str!("../examples/clocksp5/ClockSP5.bas"),
        &mut task,
        &mut dispatcher,
        StrictJitOptions {
            benchmark_validation: true,
        },
    )
    .unwrap();
    assert_strict_report(&report);
    let output = output_bytes(&output);
    assert_clocksp5_output(&output);
    report_clocksp5_observation("source", &report, &output);
}

#[test]
fn strict_native_runs_all_clocksp5_tokenized_passes() {
    let (mut task, mut dispatcher, output) = environment();
    let program =
        TokenizedBasicProgram::decode(include_bytes!("../examples/clocksp5/ClockSP5.bbc")).unwrap();
    let report = basic_compat::run_program_jit_strict_with_options(
        &program,
        &mut task,
        &mut dispatcher,
        StrictJitOptions {
            benchmark_validation: true,
        },
    )
    .unwrap();
    assert_strict_report(&report);
    let output = output_bytes(&output);
    assert_clocksp5_output(&output);
    report_clocksp5_observation("tokenized", &report, &output);
}
