use std::{
    error::Error,
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use acorn_2026::{
    basic_compat::run_program,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiDispatcher},
    tokenized_basic::TokenizedBasicProgram,
};
use cranelift_codegen::{
    ir::{AbiParam, InstBuilder, UserFuncName, condcodes::IntCC, types},
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

const B_INCREMENT: i32 = 1;
const I_START: i32 = 0;
const INNER_LIMIT: i32 = 100;
const OUTER_LIMIT: i32 = 10_000;
const INTERPRETER_SAMPLES: usize = 3;
const NATIVE_SAMPLES: usize = 7;

type IntegerRepeat = extern "C" fn(i32, i32, i32, i32) -> i32;

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = repository_root().join("examples/clocksp5/integer-repeat-jit.bbc");
    let program = TokenizedBasicProgram::load_file(&fixture)?;

    let jit_started = Instant::now();
    let isa_builder =
        cranelift_native::builder().map_err(|message| std::io::Error::other(message))?;
    let isa = isa_builder.finish(compiler_flags()?)?;
    let target = isa.triple().to_string();
    let mut jit_module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let function = define_integer_repeat(&mut jit_module)?;
    jit_module.finalize_definitions()?;
    let jit_compile_time = jit_started.elapsed();

    // SAFETY: the JIT function uses the matching extern "C" signature above,
    // and its module stays alive until every sample has completed.
    let address = jit_module.get_finalized_function(function);
    let integer_repeat: IntegerRepeat = unsafe { std::mem::transmute(address) };

    let (object_path, object_bytes, object_compile_time) = emit_object()?;
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
        let checksum = integer_repeat(
            std::hint::black_box(B_INCREMENT),
            std::hint::black_box(I_START),
            std::hint::black_box(INNER_LIMIT),
            std::hint::black_box(OUTER_LIMIT),
        );
        let elapsed = started.elapsed();
        if native_checksum.is_some_and(|previous| previous != checksum) {
            return Err("JIT checksum varied between runs".into());
        }
        native_checksum = Some(checksum);
        native_times.push(elapsed);
    }

    let interpreter_checksum = interpreter_checksum.expect("interpreter samples configured");
    let native_checksum = native_checksum.expect("native samples configured");
    if interpreter_checksum != native_checksum {
        return Err(format!(
            "interpreter/JIT checksum mismatch: {interpreter_checksum} vs {native_checksum}"
        )
        .into());
    }

    let interpreter_median = median(interpreter_times);
    let native_median = median(native_times);
    let speedup = interpreter_median.as_secs_f64() / native_median.as_secs_f64();

    println!("target: {target}");
    println!(
        "workload: ClockSP5 integer REPEAT section, B%={B_INCREMENT}, I%={I_START}, D%={INNER_LIMIT}, E%={OUTER_LIMIT}"
    );
    println!("checksum: {interpreter_checksum} (interpreter and JIT agree)");
    println!(
        "JIT compile and finalize: {}",
        format_duration(jit_compile_time)
    );
    println!(
        "AOT object compile: {}",
        format_duration(object_compile_time)
    );
    println!(
        "tokenized BASIC fixture median (parse + full loop): {}",
        format_duration(interpreter_median)
    );
    println!(
        "Cranelift integer loop median: {}",
        format_duration(native_median)
    );
    println!("measured speedup: {speedup:.1}×");
    println!("object: {} ({} bytes)", object_path.display(), object_bytes);
    println!("note: Cranelift IR is hand-built for this one ClockSP5 section.");
    Ok(())
}

fn compiler_flags() -> Result<settings::Flags, Box<dyn Error>> {
    let mut builder = settings::builder();
    builder.set("opt_level", "speed")?;
    Ok(settings::Flags::new(builder))
}

fn define_integer_repeat<M: Module>(module: &mut M) -> Result<FuncId, Box<dyn Error>> {
    let mut signature = module.make_signature();
    for _ in 0..4 {
        signature.params.push(AbiParam::new(types::I32));
    }
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
        let outer_loop = builder.create_block();
        let inner_loop = builder.create_block();
        let inner_exit = builder.create_block();
        let exit = builder.create_block();

        builder.append_block_params_for_function_params(entry);
        builder.append_block_param(outer_loop, types::I32);
        builder.append_block_param(inner_loop, types::I32);
        builder.append_block_param(inner_loop, types::I32);
        builder.append_block_param(inner_exit, types::I32);
        builder.append_block_param(inner_exit, types::I32);
        builder.append_block_param(exit, types::I32);
        builder.append_block_param(exit, types::I32);

        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let arguments = builder.block_params(entry).to_vec();
        let (increment, inner_start, inner_limit, outer_limit) =
            (arguments[0], arguments[1], arguments[2], arguments[3]);
        let zero = builder.ins().iconst(types::I32, 0);
        builder.ins().jump(outer_loop, &[zero.into()]);

        builder.switch_to_block(outer_loop);
        let outer_count = builder.block_params(outer_loop)[0];
        let next_outer_count = builder.ins().iadd(outer_count, increment);
        builder
            .ins()
            .jump(inner_loop, &[next_outer_count.into(), inner_start.into()]);

        builder.switch_to_block(inner_loop);
        let inner_state = builder.block_params(inner_loop).to_vec();
        let outer_count = inner_state[0];
        let inner_count = inner_state[1];
        let next_inner_count = builder.ins().iadd(inner_count, increment);
        let inner_done =
            builder
                .ins()
                .icmp(IntCC::SignedGreaterThan, next_inner_count, inner_limit);
        builder.ins().brif(
            inner_done,
            inner_exit,
            &[outer_count.into(), next_inner_count.into()],
            inner_loop,
            &[outer_count.into(), next_inner_count.into()],
        );

        builder.switch_to_block(inner_exit);
        let completed = builder.block_params(inner_exit).to_vec();
        let outer_count = completed[0];
        let inner_count = completed[1];
        let outer_done = builder
            .ins()
            .icmp(IntCC::SignedGreaterThan, outer_count, outer_limit);
        builder.ins().brif(
            outer_done,
            exit,
            &[outer_count.into(), inner_count.into()],
            outer_loop,
            &[outer_count.into()],
        );

        builder.switch_to_block(exit);
        builder.seal_all_blocks();
        let result_state = builder.block_params(exit).to_vec();
        let checksum = builder.ins().iadd(result_state[0], result_state[1]);
        builder.ins().return_(&[checksum]);
        builder.finalize(frontend_config);
    }

    module.define_function(function, &mut context)?;
    module.clear_context(&mut context);
    Ok(function)
}

fn emit_object() -> Result<(PathBuf, usize, Duration), Box<dyn Error>> {
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
    define_integer_repeat(&mut module)?;
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
