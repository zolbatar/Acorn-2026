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
    ir::{
        AbiParam, InstBuilder, UserFuncName,
        condcodes::{FloatCC, IntCC},
        types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};

const GRID_WIDTH: u32 = 80;
const GRID_HEIGHT: u32 = 50;
const ITERATION_LIMIT: i32 = 8192;
const INTERPRETER_SAMPLES: usize = 3;
const NATIVE_SAMPLES: usize = 7;

type MandelbrotIteration = extern "C" fn(f64, f64, i32) -> i32;

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = repository_root().join("examples/mandelbrot/iteration-benchmark.bbc");
    let program = TokenizedBasicProgram::load_file(&fixture)?;

    let jit_started = Instant::now();
    let isa_builder =
        cranelift_native::builder().map_err(|message| std::io::Error::other(message))?;
    let isa = isa_builder.finish(compiler_flags()?)?;
    let target = isa.triple().to_string();
    let mut jit_module = JITModule::new(JITBuilder::with_isa(isa, default_libcall_names()));
    let jit_function = define_iteration(&mut jit_module)?;
    jit_module.finalize_definitions()?;
    let jit_compile_time = jit_started.elapsed();

    // Cranelift returns executable memory for a function with this signature.
    // The signature is defined above and the module remains alive through all calls.
    let jit_address = jit_module.get_finalized_function(jit_function);
    let iterate: MandelbrotIteration = unsafe { std::mem::transmute(jit_address) };

    let (object_path, object_bytes, object_compile_time) = emit_object()?;
    let mut interpreter_times = Vec::with_capacity(INTERPRETER_SAMPLES);
    let mut interpreted_checksum = None;
    for _ in 0..INTERPRETER_SAMPLES {
        let (elapsed, checksum) = run_interpreter(&program)?;
        if interpreted_checksum.is_some_and(|previous| previous != checksum) {
            return Err("interpreter checksum varied between runs".into());
        }
        interpreted_checksum = Some(checksum);
        interpreter_times.push(elapsed);
    }

    let mut native_times = Vec::with_capacity(NATIVE_SAMPLES);
    let mut native_checksum = None;
    for _ in 0..NATIVE_SAMPLES {
        let started = Instant::now();
        let checksum = run_native_grid(iterate);
        let elapsed = started.elapsed();
        if native_checksum.is_some_and(|previous| previous != checksum) {
            return Err("JIT checksum varied between runs".into());
        }
        native_checksum = Some(checksum);
        native_times.push(elapsed);
    }

    let interpreted_checksum = interpreted_checksum.expect("at least one interpreter sample");
    let native_checksum = native_checksum.expect("at least one JIT sample");
    if interpreted_checksum != native_checksum {
        return Err(format!(
            "interpreter/JIT checksum mismatch: {interpreted_checksum} vs {native_checksum}"
        )
        .into());
    }

    let interpreter_median = median(interpreter_times);
    let native_median = median(native_times);
    let speedup = interpreter_median.as_secs_f64() / native_median.as_secs_f64();

    println!("target: {target}");
    println!("workload: {GRID_WIDTH} × {GRID_HEIGHT}, max {ITERATION_LIMIT} iterations per point");
    println!("checksum: {interpreted_checksum} (interpreter and JIT agree)");
    println!(
        "JIT compile and finalize: {}",
        format_duration(jit_compile_time)
    );
    println!(
        "AOT object compile: {}",
        format_duration(object_compile_time)
    );
    println!(
        "tokenized BASIC program median (full grid/control flow): {}",
        format_duration(interpreter_median)
    );
    println!(
        "Cranelift iteration kernel + Rust grid loop median: {}",
        format_duration(native_median)
    );
    println!("measured speedup: {speedup:.1}×");
    println!("object: {} ({} bytes)", object_path.display(), object_bytes);
    println!("note: BASIC grid/control flow remains interpreted in this spike.");
    Ok(())
}

fn compiler_flags() -> Result<settings::Flags, Box<dyn Error>> {
    let mut builder = settings::builder();
    builder.set("opt_level", "speed")?;
    Ok(settings::Flags::new(builder))
}

fn define_iteration<M: Module>(module: &mut M) -> Result<FuncId, Box<dyn Error>> {
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(types::F64));
    signature.params.push(AbiParam::new(types::F64));
    signature.params.push(AbiParam::new(types::I32));
    signature.returns.push(AbiParam::new(types::I32));

    let function = module.declare_function("mandelbrot_iteration", Linkage::Export, &signature)?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, function.as_u32());
    let mut builder_context = FunctionBuilderContext::new();
    let frontend_config = module.target_config();

    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        let loop_block = builder.create_block();
        let exit = builder.create_block();

        builder.append_block_params_for_function_params(entry);
        builder.append_block_param(loop_block, types::F64);
        builder.append_block_param(loop_block, types::F64);
        builder.append_block_param(loop_block, types::I32);
        builder.append_block_param(exit, types::I32);

        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let arguments = builder.block_params(entry).to_vec();
        let (real_c, imag_c, iteration_limit) = (arguments[0], arguments[1], arguments[2]);
        let zero_f64 = builder.ins().f64const(0.0);
        let zero_i32 = builder.ins().iconst(types::I32, 0);
        builder.ins().jump(
            loop_block,
            &[zero_f64.into(), zero_f64.into(), zero_i32.into()],
        );

        builder.switch_to_block(loop_block);
        let state = builder.block_params(loop_block).to_vec();
        let (real_z, imag_z, count) = (state[0], state[1], state[2]);

        let real_squared = builder.ins().fmul(real_z, real_z);
        let imag_squared = builder.ins().fmul(imag_z, imag_z);
        let squared_real = builder.ins().fsub(real_squared, imag_squared);
        let doubled_real = builder.ins().fadd(real_z, real_z);
        let next_imag = builder.ins().fmul(doubled_real, imag_z);
        let next_real = builder.ins().fadd(squared_real, real_c);
        let next_imag = builder.ins().fadd(next_imag, imag_c);
        let next_count = builder.ins().iadd_imm_s(count, 1);

        let abs_real = builder.ins().fabs(next_real);
        let abs_imag = builder.ins().fabs(next_imag);
        let magnitude = builder.ins().fadd(abs_real, abs_imag);
        let four = builder.ins().f64const(4.0);
        let escaped = builder.ins().fcmp(FloatCC::GreaterThan, magnitude, four);
        let finished = builder
            .ins()
            .icmp(IntCC::Equal, next_count, iteration_limit);
        let done = builder.ins().bor(escaped, finished);
        builder.ins().brif(
            done,
            exit,
            &[next_count.into()],
            loop_block,
            &[next_real.into(), next_imag.into(), next_count.into()],
        );

        builder.switch_to_block(exit);
        builder.seal_all_blocks();
        let result = builder.block_params(exit)[0];
        builder.ins().return_(&[result]);
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
        "mandelbrot_iteration_benchmark",
        default_libcall_names(),
    )?;
    let mut module = ObjectModule::new(object_builder);
    define_iteration(&mut module)?;
    let product = module.finish();
    let bytes = product.object.write()?;

    let output_path = repository_root()
        .join("target")
        .join("basic-jit-bench")
        .join("mandelbrot_iteration.o");
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, &bytes)?;
    Ok((output_path, bytes.len(), started.elapsed()))
}

fn run_interpreter(program: &TokenizedBasicProgram) -> Result<(Duration, i64), Box<dyn Error>> {
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
        .filter_map(|digits| digits.parse::<i64>().ok())
        .last()
        .ok_or_else(|| format!("interpreter did not print a checksum: {text:?}"))?;
    Ok((elapsed, checksum))
}

fn run_native_grid(iterate: MandelbrotIteration) -> i64 {
    let x_size = f64::from(GRID_WIDTH);
    let y_size = f64::from(GRID_HEIGHT);
    let aspect = y_size / x_size;
    let x_centre = -1.44251;
    let y_centre = -0.13409;
    let scale = 0.52707;
    let x_min = x_centre - (scale / 2.0);
    let x_max = x_centre + (scale / 2.0);
    let x_width = x_max - x_min;
    let y_min = y_centre + (scale * aspect / 2.0);
    let y_max = y_centre - (scale * aspect / 2.0);
    let y_width = y_max - y_min;

    let mut checksum = 0_i64;
    for x in 0..GRID_WIDTH {
        for y in 0..GRID_HEIGHT {
            let real_c = (x_width * f64::from(x) / x_size) + x_min;
            let imag_c = (y_width * f64::from(y) / y_size) + y_min;
            let count = iterate(
                std::hint::black_box(real_c),
                std::hint::black_box(imag_c),
                ITERATION_LIMIT,
            );
            checksum += i64::from(count);
        }
    }
    std::hint::black_box(checksum)
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
