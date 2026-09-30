use std::{
    error::Error,
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use ricochet::{
    basic_compat::{
        compiler_api::{
            self, IntegerCondition, IntegerExpression, IntegerProgram, IntegerStatement,
        },
        run_program,
    },
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiDispatcher},
    tokenized_basic::TokenizedBasicProgram,
};
use cranelift_codegen::{
    ir::{AbiParam, InstBuilder, UserFuncName, Value, condcodes::IntCC, types},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

const INTERPRETER_SAMPLES: usize = 3;
const NATIVE_SAMPLES: usize = 7;

type CompiledIntegerProgram = extern "C" fn() -> i32;

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = repository_root().join("examples/clocksp5/integer-repeat-jit.bbc");
    let program = TokenizedBasicProgram::load_file(&fixture)?;

    let lowering_started = Instant::now();
    let integer_program = compiler_api::lower_integer_program(&program)?;
    let basic_lowering_time = lowering_started.elapsed();

    let jit_started = Instant::now();
    let isa_builder =
        cranelift_native::builder().map_err(|message| std::io::Error::other(message))?;
    let isa = isa_builder.finish(compiler_flags()?)?;
    let target = isa.triple().to_string();
    let mut jit_module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let function = define_integer_program(&mut jit_module, &integer_program)?;
    jit_module.finalize_definitions()?;
    let jit_compile_time = jit_started.elapsed();

    // SAFETY: this generated function has the `extern "C" fn() -> i32`
    // signature declared above, and its JIT module stays alive through all calls.
    let address = jit_module.get_finalized_function(function);
    let compiled_program: CompiledIntegerProgram = unsafe { std::mem::transmute(address) };

    let (object_path, object_bytes, object_compile_time) = emit_object(&integer_program)?;
    let mut interpreter_times = Vec::with_capacity(INTERPRETER_SAMPLES);
    let mut interpreter_checksum = None;
    for _ in 0..INTERPRETER_SAMPLES {
        let (elapsed, checksum) = run_interpreter(&program)?;
        if interpreter_checksum.is_some_and(|previous| previous != checksum) {
            return Err("interpreter checksum varied between runs".into());
        }
        interpreter_checksum = Some(checksum);
        interpreter_times.push(elapsed);
    }

    let mut native_times = Vec::with_capacity(NATIVE_SAMPLES);
    let mut native_checksum = None;
    for _ in 0..NATIVE_SAMPLES {
        let started = Instant::now();
        let checksum = std::hint::black_box(compiled_program());
        let elapsed = started.elapsed();
        if native_checksum.is_some_and(|previous| previous != checksum) {
            return Err("compiled checksum varied between runs".into());
        }
        native_checksum = Some(checksum);
        native_times.push(elapsed);
    }

    let interpreter_checksum = interpreter_checksum.expect("interpreter samples configured");
    let native_checksum = native_checksum.expect("native samples configured");
    if interpreter_checksum != native_checksum {
        return Err(format!(
            "interpreter/Cranelift checksum mismatch: {interpreter_checksum} vs {native_checksum}"
        )
        .into());
    }

    let interpreter_median = median(interpreter_times);
    let native_median = median(native_times);
    let speedup = interpreter_median.as_secs_f64() / native_median.as_secs_f64();

    println!("target: {target}");
    println!("source: examples/clocksp5/integer-repeat-jit.bas");
    println!(
        "lowered integer locals: {}, result PRINT at BASIC line {}",
        integer_program.locals.len(),
        integer_program.result_line
    );
    println!("checksum: {interpreter_checksum} (interpreter and Cranelift agree)");
    println!(
        "BASIC parse and typed-subset lowering: {}",
        format_duration(basic_lowering_time)
    );
    println!(
        "Cranelift JIT compile and finalize: {}",
        format_duration(jit_compile_time)
    );
    println!(
        "Cranelift AOT object compile: {}",
        format_duration(object_compile_time)
    );
    println!(
        "tokenized BASIC fixture median (parse + execution): {}",
        format_duration(interpreter_median)
    );
    println!(
        "Cranelift compiled function median: {}",
        format_duration(native_median)
    );
    println!("measured speedup: {speedup:.1}×");
    println!("object: {} ({} bytes)", object_path.display(), object_bytes);
    println!("note: BASIC PRINT is returned as the compiled function result for this benchmark.");
    Ok(())
}

fn compiler_flags() -> Result<settings::Flags, Box<dyn Error>> {
    let mut builder = settings::builder();
    builder.set("opt_level", "speed")?;
    Ok(settings::Flags::new(builder))
}

fn define_integer_program<M: Module>(
    module: &mut M,
    program: &IntegerProgram,
) -> Result<FuncId, Box<dyn Error>> {
    let mut signature = module.make_signature();
    signature.returns.push(AbiParam::new(types::I32));

    let function =
        module.declare_function("clocksp5_integer_repeat", Linkage::Export, &signature)?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, function.as_u32());
    let mut builder_context = FunctionBuilderContext::new();
    let frontend_config = module.target_config();

    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        builder.switch_to_block(entry);
        builder.seal_block(entry);

        let locals = program
            .locals
            .iter()
            .map(|_| builder.declare_var(types::I32))
            .collect::<Vec<_>>();
        for local in &locals {
            let zero = builder.ins().iconst(types::I32, 0);
            builder.def_var(*local, zero);
        }

        lower_statements(&mut builder, &program.statements, &locals)?;
        let result_variable = local_variable(&locals, program.result_local, program.result_line)?;
        let result = builder.use_var(result_variable);
        builder.ins().return_(&[result]);
        builder.seal_all_blocks();
        builder.finalize(frontend_config);
    }

    module.define_function(function, &mut context)?;
    module.clear_context(&mut context);
    Ok(function)
}

fn lower_statements(
    builder: &mut FunctionBuilder<'_>,
    statements: &[IntegerStatement],
    locals: &[Variable],
) -> Result<(), Box<dyn Error>> {
    for statement in statements {
        match statement {
            IntegerStatement::Assign { line, local, value } => {
                let variable = local_variable(locals, *local, *line)?;
                let value = lower_expression(builder, value, locals)
                    .map_err(|error| format!("BASIC line {line}: {error}"))?;
                let value = narrow_to_basic_integer(builder, value);
                builder.def_var(variable, value);
            }
            IntegerStatement::RepeatUntil {
                line: _,
                condition_line,
                body,
                condition,
            } => {
                let header = builder.create_block();
                let exit = builder.create_block();
                builder.ins().jump(header, &[]);
                builder.switch_to_block(header);
                lower_statements(builder, body, locals)?;
                let condition = lower_condition(builder, condition, locals)
                    .map_err(|error| format!("BASIC line {condition_line}: {error}"))?;
                builder.ins().brif(condition, exit, &[], header, &[]);
                builder.seal_block(header);
                builder.seal_block(exit);
                builder.switch_to_block(exit);
            }
        }
    }
    Ok(())
}

fn lower_condition(
    builder: &mut FunctionBuilder<'_>,
    condition: &IntegerCondition,
    locals: &[Variable],
) -> Result<Value, Box<dyn Error>> {
    match condition {
        IntegerCondition::GreaterThan(left, right) => {
            let left = lower_expression(builder, left, locals)?;
            let right = lower_expression(builder, right, locals)?;
            Ok(builder.ins().icmp(IntCC::SignedGreaterThan, left, right))
        }
    }
}

fn lower_expression(
    builder: &mut FunctionBuilder<'_>,
    expression: &IntegerExpression,
    locals: &[Variable],
) -> Result<Value, Box<dyn Error>> {
    match expression {
        IntegerExpression::Constant(value) => {
            Ok(builder.ins().iconst(types::I64, i64::from(*value)))
        }
        IntegerExpression::Local(index) => {
            let variable = local_variable(locals, *index, 0)?;
            let value = builder.use_var(variable);
            Ok(builder.ins().sextend(types::I64, value))
        }
        IntegerExpression::Add(left, right) => {
            let left = lower_expression(builder, left, locals)?;
            let right = lower_expression(builder, right, locals)?;
            Ok(builder.ins().iadd(left, right))
        }
    }
}

fn narrow_to_basic_integer(builder: &mut FunctionBuilder<'_>, value: Value) -> Value {
    // The interpreter converts to signed 32-bit when assigning a numeric
    // result to a `%` variable. Keep expression arithmetic widened until that
    // assignment boundary, then apply the interpreter's saturating conversion.
    let minimum = builder.ins().iconst(types::I64, i64::from(i32::MIN));
    let maximum = builder.ins().iconst(types::I64, i64::from(i32::MAX));
    let below = builder.ins().icmp(IntCC::SignedLessThan, value, minimum);
    let above = builder.ins().icmp(IntCC::SignedGreaterThan, value, maximum);
    let clamped_low = builder.ins().select(below, minimum, value);
    let clamped = builder.ins().select(above, maximum, clamped_low);
    builder.ins().ireduce(types::I32, clamped)
}

fn local_variable(locals: &[Variable], index: u32, line: u16) -> Result<Variable, Box<dyn Error>> {
    locals
        .get(index as usize)
        .copied()
        .ok_or_else(|| format!("BASIC line {line}: invalid local index {index}").into())
}

fn emit_object(program: &IntegerProgram) -> Result<(PathBuf, usize, Duration), Box<dyn Error>> {
    let started = Instant::now();
    let isa_builder =
        cranelift_native::builder().map_err(|message| std::io::Error::other(message))?;
    let isa = isa_builder.finish(compiler_flags()?)?;
    let object_builder = ObjectBuilder::new(
        isa,
        "clocksp5_integer_repeat_benchmark",
        default_libcall_names(),
    )?;
    let mut module = ObjectModule::new(object_builder);
    define_integer_program(&mut module, program)?;
    let product = module.finish();
    let bytes = product.object.write()?;

    let output_path = repository_root()
        .join("target")
        .join("basic-jit-bench")
        .join("clocksp5_integer_repeat.o");
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, &bytes)?;
    Ok((output_path, bytes.len(), started.elapsed()))
}

fn run_interpreter(program: &TokenizedBasicProgram) -> Result<(Duration, i32), Box<dyn Error>> {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    let mut task = Task::new(1);
    let mut dispatcher =
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
    let started = Instant::now();
    run_program(program, &mut task, &mut dispatcher)?;
    let elapsed = started.elapsed();

    let mut output = Vec::new();
    while let Ok(event) = display_receiver.try_recv() {
        if let DisplayEvent::WriteByte(byte) = event {
            output.push(byte);
        }
    }
    let text = String::from_utf8_lossy(&output);
    let checksum = text
        .split(|character: char| !character.is_ascii_digit())
        .filter_map(|digits| digits.parse::<i32>().ok())
        .last()
        .ok_or_else(|| format!("ClockSP5 fixture did not print a checksum: {text:?}"))?;
    Ok((elapsed, checksum))
}

fn median(mut samples: Vec<Duration>) -> Duration {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs_f64() < 1.0 {
        format!("{:.3} ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{:.3} s", duration.as_secs_f64())
    }
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
