//! Experimental, fixture-focused Cranelift kernels used by `BASICJIT`.
//!
//! The compatibility interpreter handles statements outside verified hot
//! regions. The Mandelbrot frame kernel routes ColourTrans and OS_Plot through
//! checked runtime callbacks. Native entry points stay private to this module.

use std::{ffi::c_void, time::Instant};

use cranelift_codegen::{
    ir::{
        AbiParam, InstBuilder, MemFlagsData, UserFuncName,
        condcodes::{FloatCC, IntCC},
        types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};

use crate::{
    error::RuntimeError,
    memory::Task,
    swi::{SwiContext, SwiDispatcher},
    tokenized_basic::{TokenizedBasicProgram, TokenizedBasicRecordLayout},
};

use super::{
    JitExecutionReport,
    parser::{self, BinaryOp, Definition, Expr, LValue, ParsedProgram, Statement, TokenProfile},
    runtime::Interpreter,
};

type MandelbrotKernel = extern "C" fn(f64, f64, i32, *mut f64) -> i32;
type MandelbrotFrameKernel = extern "C" fn(i32, i32, f64, f64, f64, f64, i32, *mut c_void) -> i32;
type ClockSp5IntegerRegion = extern "C" fn(i32, i32, i32, i32, i32) -> i64;

const MAX_MANDELBROT_FRAME_PIXELS: u64 = 4_194_304;

#[derive(Clone, Copy, Debug)]
pub(super) struct MandelbrotFrameInputs {
    pub width: i32,
    pub height: i32,
    pub x_width: f64,
    pub x_min: f64,
    pub y_width: f64,
    pub y_min: f64,
    pub iteration_limit: i32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MandelbrotFramePixel {
    pub real_c: f64,
    pub imag_c: f64,
    pub iterations: i32,
    pub real_z: f64,
    pub imag_z: f64,
    pub u: f64,
    pub v: f64,
    pub hue: i32,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct MandelbrotFrameOutput {
    pub after: usize,
    pub width: i32,
    pub height: i32,
    pub last_pixel: MandelbrotFramePixel,
    pub rgb: [i32; 3],
}

#[derive(Clone, Copy, Debug)]
struct MandelbrotFrameRegion {
    start: usize,
    after: usize,
    line: u16,
}

struct MandelbrotPixelContext {
    task: *mut Task,
    dispatcher: *mut SwiDispatcher,
    pending_key: *mut Option<u8>,
    line: u16,
    iteration_limit: i32,
    pixels: u64,
    last_pixel: Option<MandelbrotFramePixel>,
    rgb: [i32; 3],
    error: Option<RuntimeError>,
}

extern "C" fn render_mandelbrot_pixel(
    context_pointer: *mut c_void,
    x: i32,
    y: i32,
    real_c: f64,
    imag_c: f64,
    iterations: i32,
    real_z: f64,
    imag_z: f64,
    u: f64,
    v: f64,
) -> i32 {
    if context_pointer.is_null() {
        return 0;
    }
    // SAFETY: the compiled frame receives this pointer from
    // `run_mandelbrot_frame`, which keeps the stack context alive for the
    // synchronous call. The generated JIT code only forwards it to this
    // callback.
    let context = unsafe { &mut *context_pointer.cast::<MandelbrotPixelContext>() };
    if context.error.is_some() {
        return 0;
    }

    let hue = (360.0
        - 360.0 * f64::from(iterations).log10() / f64::from(context.iteration_limit).log10())
    .trunc() as i32;
    let escaped = real_z.abs() + imag_z.abs() > 4.0;
    let result = (|| {
        // SAFETY: both pointers refer to the exclusively borrowed task and
        // dispatcher passed into `run_mandelbrot_frame`; neither outlives the
        // synchronous JIT call.
        let task = unsafe { &mut *context.task };
        let dispatcher = unsafe { &mut *context.dispatcher };
        let mut gcol = SwiContext::default();
        if escaped {
            let mut conversion = SwiContext::default();
            conversion.registers[0] = hue.wrapping_mul(0x1_0000) as u32;
            conversion.registers[1] = 0xFF00;
            conversion.registers[2] = 0xFF;
            dispatcher.dispatch_named_swi("COLOURTRANS_CONVERTHSVTORGB", &mut conversion)?;
            context.rgb = [
                conversion.registers[0] as i32,
                conversion.registers[1] as i32,
                conversion.registers[2] as i32,
            ];
            let [red, green, blue] = context.rgb.map(|channel| channel as u32);
            gcol.registers[0] = (blue << 24) | (green << 16) | (red << 8);
        }
        gcol.registers[3] = 0x100;
        gcol.registers[4] = 0;
        dispatcher.dispatch_named_swi("COLOURTRANS_SETGCOL", &mut gcol)?;
        let plot_x = x.checked_mul(2).ok_or_else(|| {
            RuntimeError::Program(format!(
                "BASIC line {}: graphics coordinate overflowed",
                context.line
            ))
        })?;
        let plot_y = y.checked_mul(2).ok_or_else(|| {
            RuntimeError::Program(format!(
                "BASIC line {}: graphics coordinate overflowed",
                context.line
            ))
        })?;
        dispatcher.plot(task, 4, plot_x, plot_y)?;
        dispatcher.plot(task, 5, plot_x, plot_y)?;
        Ok::<(), RuntimeError>(())
    })();
    if let Err(error) = result {
        context.error = Some(error);
        return 0;
    }

    context.pixels += 1;
    context.last_pixel = Some(MandelbrotFramePixel {
        real_c,
        imag_c,
        iterations,
        real_z,
        imag_z,
        u,
        v,
        hue,
    });
    if context.pixels & 0x3FF == 0 {
        // Mirror the interpreter's periodic input poll so a key queued while
        // the native frame is running is available to its final INKEY call.
        // SAFETY: the pending-key slot and dispatcher remain exclusively
        // borrowed for this synchronous native invocation.
        if let Some(key) = unsafe { (&mut *context.dispatcher).poll_key() } {
            unsafe { *context.pending_key = Some(key) };
        }
    }
    1
}

#[derive(Clone, Copy, Debug)]
struct ClockSp5Region {
    start: usize,
    after: usize,
    line: u16,
}

#[derive(Clone, Copy, Debug)]
struct MandelbrotInlineRegion {
    start: usize,
    after: usize,
    line: u16,
}

pub(super) struct JitProgram {
    // Keep the executable memory alive for every stored entry point.
    _module: JITModule,
    mandelbrot: Option<MandelbrotKernel>,
    mandelbrot_procedure: bool,
    mandelbrot_inline_region: Option<MandelbrotInlineRegion>,
    mandelbrot_frame_region: Option<MandelbrotFrameRegion>,
    mandelbrot_frame: Option<MandelbrotFrameKernel>,
    clocksp5_integer_region: Option<(ClockSp5Region, ClockSp5IntegerRegion)>,
    report: JitExecutionReport,
}

impl JitProgram {
    pub(super) fn compile(program: &ParsedProgram) -> Result<Option<Self>, String> {
        let compile_started = Instant::now();
        let compile_mandelbrot_procedure = has_compatible_mandelbrot_procedure(program);
        let mandelbrot_inline_region = find_mandelbrot_inline_region(program);
        let mandelbrot_frame_region = find_mandelbrot_frame_region(program);
        let clocksp5_region = find_clocksp5_integer_region(program);
        let compile_mandelbrot = compile_mandelbrot_procedure || mandelbrot_inline_region.is_some();
        if !compile_mandelbrot && mandelbrot_frame_region.is_none() && clocksp5_region.is_none() {
            return Ok(None);
        }

        let isa_builder = cranelift_native::builder().map_err(|message| message.to_string())?;
        let mut flags_builder = settings::builder();
        flags_builder
            .set("opt_level", "speed")
            .map_err(|error| error.to_string())?;
        let flags = settings::Flags::new(flags_builder);
        let isa = isa_builder
            .finish(flags)
            .map_err(|error| error.to_string())?;
        let mut jit_builder = JITBuilder::with_isa(isa, default_libcall_names());
        if mandelbrot_frame_region.is_some() {
            jit_builder.symbol(
                "acorn_mandelbrot_render_pixel",
                render_mandelbrot_pixel as *const () as *const u8,
            );
        }
        let mut module = JITModule::new(jit_builder);

        let mandelbrot_function = if compile_mandelbrot {
            Some(define_mandelbrot_kernel(&mut module).map_err(|error| error.to_string())?)
        } else {
            None
        };
        let clocksp5_function = if clocksp5_region.is_some() {
            Some(define_clocksp5_integer_region(&mut module).map_err(|error| error.to_string())?)
        } else {
            None
        };
        let mandelbrot_frame_function = if mandelbrot_frame_region.is_some() {
            Some(define_mandelbrot_frame_kernel(&mut module).map_err(|error| error.to_string())?)
        } else {
            None
        };

        module
            .finalize_definitions()
            .map_err(|error| error.to_string())?;
        let mandelbrot = mandelbrot_function.map(|function| {
            // SAFETY: this function was defined with the exact signature below,
            // finalized in this module, and the module remains owned by Self.
            unsafe {
                std::mem::transmute::<*const u8, MandelbrotKernel>(
                    module.get_finalized_function(function),
                )
            }
        });
        let clocksp5_integer_region =
            clocksp5_function
                .zip(clocksp5_region)
                .map(|(function, region)| {
                    // SAFETY: this function was defined with the five-i32 to i64
                    // signature below; its module remains owned by Self.
                    let entry = unsafe {
                        std::mem::transmute::<*const u8, ClockSp5IntegerRegion>(
                            module.get_finalized_function(function),
                        )
                    };
                    (region, entry)
                });
        let mandelbrot_frame = mandelbrot_frame_function.map(|function| {
            // SAFETY: this function was defined with the exact signature below,
            // finalized in this module, and the module remains owned by Self.
            unsafe {
                std::mem::transmute::<*const u8, MandelbrotFrameKernel>(
                    module.get_finalized_function(function),
                )
            }
        });

        let mut compiled_units = Vec::new();
        if mandelbrot.is_some() && compile_mandelbrot_procedure {
            compiled_units.push("Mandelbrot PROCit iteration loop".to_string());
        }
        if let Some(region) = mandelbrot_inline_region {
            compiled_units.push(format!(
                "Mandelbrot inline iteration loop at BASIC line {}",
                region.line
            ));
        }
        if let Some((region, _)) = clocksp5_integer_region {
            compiled_units.push(format!(
                "integer REPEAT region at BASIC line {}",
                region.line
            ));
        }

        Ok(Some(Self {
            _module: module,
            mandelbrot,
            mandelbrot_procedure: compile_mandelbrot_procedure,
            mandelbrot_inline_region,
            mandelbrot_frame_region,
            mandelbrot_frame,
            clocksp5_integer_region,
            report: JitExecutionReport {
                compiled_units,
                compile_time: compile_started.elapsed(),
                ..JitExecutionReport::default()
            },
        }))
    }

    pub(super) fn run_mandelbrot(
        &mut self,
        real_c: f64,
        imag_c: f64,
        iteration_limit: i32,
    ) -> Option<(i32, [f64; 4])> {
        if !self.mandelbrot_procedure {
            return None;
        }
        self.run_mandelbrot_kernel(real_c, imag_c, iteration_limit)
    }

    pub(super) fn mandelbrot_inline_start(&self) -> Option<usize> {
        self.mandelbrot_inline_region.map(|region| region.start)
    }

    pub(super) fn run_mandelbrot_inline(
        &mut self,
        real_c: f64,
        imag_c: f64,
        iteration_limit: i32,
    ) -> Option<(usize, i32, [f64; 4])> {
        let region = self.mandelbrot_inline_region?;
        self.run_mandelbrot_kernel(real_c, imag_c, iteration_limit)
            .map(|(iterations, state)| (region.after, iterations, state))
    }

    pub(super) fn mandelbrot_frame_start(&self) -> Option<usize> {
        self.mandelbrot_frame_region.map(|region| region.start)
    }

    pub(super) fn run_mandelbrot_frame(
        &mut self,
        inputs: MandelbrotFrameInputs,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
        pending_key: &mut Option<u8>,
        initial_rgb: [i32; 3],
    ) -> Result<Option<MandelbrotFrameOutput>, RuntimeError> {
        let (Some(region), Some(kernel)) = (self.mandelbrot_frame_region, self.mandelbrot_frame)
        else {
            return Ok(None);
        };
        let Some(pixel_count) = u64::try_from(inputs.width)
            .ok()
            .zip(u64::try_from(inputs.height).ok())
            .and_then(|(width, height)| width.checked_mul(height))
        else {
            return Ok(None);
        };
        if inputs.width <= 0
            || inputs.height <= 0
            || inputs.iteration_limit <= 0
            || pixel_count > MAX_MANDELBROT_FRAME_PIXELS
        {
            return Ok(None);
        }

        let mut callback_context = MandelbrotPixelContext {
            task: task as *mut Task,
            dispatcher: dispatcher as *mut SwiDispatcher,
            pending_key: pending_key as *mut Option<u8>,
            line: region.line,
            iteration_limit: inputs.iteration_limit,
            pixels: 0,
            last_pixel: None,
            rgb: initial_rgb,
            error: None,
        };
        let started = Instant::now();
        let status = kernel(
            inputs.width,
            inputs.height,
            inputs.x_width,
            inputs.x_min,
            inputs.y_width,
            inputs.y_min,
            inputs.iteration_limit,
            (&mut callback_context as *mut MandelbrotPixelContext).cast(),
        );
        self.report.compiled_calls += 1;
        self.report.compiled_time += started.elapsed();
        self.report.rendered_pixels += callback_context.pixels;

        if let Some(error) = callback_context.error {
            return Err(error);
        }
        if status == 0 {
            return Err(RuntimeError::Program(
                "BASICJIT Mandelbrot frame stopped without a runtime error".into(),
            ));
        }
        let Some(last_pixel) = callback_context.last_pixel else {
            return Ok(None);
        };

        self.report
            .compiled_units
            .retain(|unit| !unit.starts_with("Mandelbrot"));
        self.report.compiled_units.push(format!(
            "Mandelbrot full frame loop at BASIC line {}",
            region.line
        ));
        self.report.fallback_reason = Some(
            "mode setup and the final key wait used the interpreter; ColourTrans and OS_Plot used checked runtime services".into(),
        );
        Ok(Some(MandelbrotFrameOutput {
            after: region.after,
            width: inputs.width,
            height: inputs.height,
            last_pixel,
            rgb: callback_context.rgb,
        }))
    }

    fn run_mandelbrot_kernel(
        &mut self,
        real_c: f64,
        imag_c: f64,
        iteration_limit: i32,
    ) -> Option<(i32, [f64; 4])> {
        let kernel = self.mandelbrot?;
        let mut state = [0.0; 4];
        let started = Instant::now();
        let iterations = kernel(real_c, imag_c, iteration_limit, state.as_mut_ptr());
        self.report.compiled_calls += 1;
        self.report.compiled_time += started.elapsed();
        Some((iterations, state))
    }

    pub(super) fn clocksp5_region_start(&self) -> Option<usize> {
        self.clocksp5_integer_region.map(|(region, _)| region.start)
    }

    pub(super) fn run_clocksp5_integer_region(
        &mut self,
        values: [i32; 5],
    ) -> Option<(usize, i32, i32)> {
        let (region, kernel) = self.clocksp5_integer_region?;
        let started = Instant::now();
        let packed = kernel(values[0], values[1], values[2], values[3], values[4]);
        self.report.compiled_calls += 1;
        self.report.compiled_time += started.elapsed();
        Some((region.after, (packed >> 32) as i32, packed as u32 as i32))
    }

    pub(super) fn report(&self) -> JitExecutionReport {
        self.report.clone()
    }
}

pub(super) fn run_program_jit(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parse_for_jit(program)?;

    let mut interpreter = Interpreter::new(parsed.clone());
    match compile_for_jit(&parsed) {
        Ok(Some(jit)) => interpreter.install_jit(jit),
        Ok(None) => interpreter.set_jit_fallback(
            "no verified native regions matched; the entire program will be interpreted",
        ),
        Err(reason) => interpreter.set_jit_fallback(&format!(
            "native compilation failed ({reason}); the entire program will be interpreted"
        )),
    }

    interpreter.run(task, dispatcher)?;
    Ok(interpreter.jit_report())
}

fn define_mandelbrot_kernel<M: Module>(module: &mut M) -> Result<FuncId, String> {
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(types::F64));
    signature.params.push(AbiParam::new(types::F64));
    signature.params.push(AbiParam::new(types::I32));
    signature
        .params
        .push(AbiParam::new(module.target_config().pointer_type()));
    signature.returns.push(AbiParam::new(types::I32));

    let function = module
        .declare_function("mandelbrot_iteration", Linkage::Local, &signature)
        .map_err(|error| error.to_string())?;
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
        for ty in [types::F64, types::F64, types::I32, types::F64, types::F64] {
            builder.append_block_param(loop_block, ty);
        }
        for ty in [types::F64, types::F64, types::I32, types::F64, types::F64] {
            builder.append_block_param(exit, ty);
        }

        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let args = builder.block_params(entry).to_vec();
        let (real_c, imag_c, limit, output) = (args[0], args[1], args[2], args[3]);
        let zero_f64 = builder.ins().f64const(0.0);
        let zero_i32 = builder.ins().iconst(types::I32, 0);
        builder.ins().jump(
            loop_block,
            &[
                zero_f64.into(),
                zero_f64.into(),
                zero_i32.into(),
                zero_f64.into(),
                zero_f64.into(),
            ],
        );

        builder.switch_to_block(loop_block);
        let state = builder.block_params(loop_block).to_vec();
        let (real_z, imag_z, count, _, _) = (state[0], state[1], state[2], state[3], state[4]);
        let real_squared = builder.ins().fmul(real_z, real_z);
        let imag_squared = builder.ins().fmul(imag_z, imag_z);
        let next_u = builder.ins().fsub(real_squared, imag_squared);
        let doubled_real = builder.ins().fadd(real_z, real_z);
        let next_v = builder.ins().fmul(doubled_real, imag_z);
        let next_real = builder.ins().fadd(next_u, real_c);
        let next_imag = builder.ins().fadd(next_v, imag_c);
        let next_count = builder.ins().iadd_imm_s(count, 1);
        let abs_real = builder.ins().fabs(next_real);
        let abs_imag = builder.ins().fabs(next_imag);
        let magnitude = builder.ins().fadd(abs_real, abs_imag);
        let four = builder.ins().f64const(4.0);
        let escaped = builder.ins().fcmp(FloatCC::GreaterThan, magnitude, four);
        let reached_limit = builder.ins().icmp(IntCC::Equal, next_count, limit);
        let finished = builder.ins().bor(escaped, reached_limit);
        builder.ins().brif(
            finished,
            exit,
            &[
                next_real.into(),
                next_imag.into(),
                next_count.into(),
                next_u.into(),
                next_v.into(),
            ],
            loop_block,
            &[
                next_real.into(),
                next_imag.into(),
                next_count.into(),
                next_u.into(),
                next_v.into(),
            ],
        );

        builder.switch_to_block(exit);
        let result = builder.block_params(exit).to_vec();
        let pointer_type = module.target_config().pointer_type();
        for (index, value) in [result[0], result[1], result[3], result[4]]
            .into_iter()
            .enumerate()
        {
            let offset = builder.ins().iconst(pointer_type, (index * 8) as i64);
            let address = builder.ins().iadd(output, offset);
            builder.ins().store(MemFlagsData::new(), value, address, 0);
        }
        builder.ins().return_(&[result[2]]);
        builder.seal_all_blocks();
        builder.finalize(frontend_config);
    }

    module
        .define_function(function, &mut context)
        .map_err(|error| error.to_string())?;
    module.clear_context(&mut context);
    Ok(function)
}

fn define_mandelbrot_frame_kernel<M: Module>(module: &mut M) -> Result<FuncId, String> {
    let pointer_type = module.target_config().pointer_type();
    let mut callback_signature = module.make_signature();
    callback_signature.params.push(AbiParam::new(pointer_type));
    callback_signature.params.push(AbiParam::new(types::I32));
    callback_signature.params.push(AbiParam::new(types::I32));
    callback_signature.params.push(AbiParam::new(types::F64));
    callback_signature.params.push(AbiParam::new(types::F64));
    callback_signature.params.push(AbiParam::new(types::I32));
    callback_signature.params.push(AbiParam::new(types::F64));
    callback_signature.params.push(AbiParam::new(types::F64));
    callback_signature.params.push(AbiParam::new(types::F64));
    callback_signature.params.push(AbiParam::new(types::F64));
    callback_signature.returns.push(AbiParam::new(types::I32));
    let callback = module
        .declare_function(
            "acorn_mandelbrot_render_pixel",
            Linkage::Import,
            &callback_signature,
        )
        .map_err(|error| error.to_string())?;

    let mut signature = module.make_signature();
    for parameter_type in [
        types::I32,
        types::I32,
        types::F64,
        types::F64,
        types::F64,
        types::F64,
        types::I32,
        pointer_type,
    ] {
        signature.params.push(AbiParam::new(parameter_type));
    }
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function("mandelbrot_full_frame", Linkage::Local, &signature)
        .map_err(|error| error.to_string())?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, function.as_u32());
    let callback_ref = module.declare_func_in_func(callback, &mut context.func);
    let mut builder_context = FunctionBuilderContext::new();
    let frontend_config = module.target_config();

    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        let outer_condition = builder.create_block();
        let inner_condition = builder.create_block();
        let pixel_calculation = builder.create_block();
        let iteration = builder.create_block();
        let pixel_ready = builder.create_block();
        let advance_y = builder.create_block();
        let failed = builder.create_block();
        let complete = builder.create_block();

        builder.append_block_params_for_function_params(entry);
        builder.append_block_param(outer_condition, types::I32);
        builder.append_block_param(inner_condition, types::I32);
        builder.append_block_param(inner_condition, types::I32);
        for parameter_type in [
            types::I32,
            types::I32,
            types::F64,
            types::F64,
            types::F64,
            types::F64,
            types::I32,
        ] {
            builder.append_block_param(iteration, parameter_type);
        }
        for parameter_type in [
            types::I32,
            types::I32,
            types::F64,
            types::F64,
            types::I32,
            types::F64,
            types::F64,
            types::F64,
            types::F64,
        ] {
            builder.append_block_param(pixel_ready, parameter_type);
        }
        builder.append_block_param(advance_y, types::I32);
        builder.append_block_param(advance_y, types::I32);

        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let arguments = builder.block_params(entry).to_vec();
        let width = arguments[0];
        let height = arguments[1];
        let x_width = arguments[2];
        let x_min = arguments[3];
        let y_width = arguments[4];
        let y_min = arguments[5];
        let iteration_limit = arguments[6];
        let callback_context = arguments[7];
        let zero_i32 = builder.ins().iconst(types::I32, 0);
        builder.ins().jump(outer_condition, &[zero_i32.into()]);

        builder.switch_to_block(outer_condition);
        let x = builder.block_params(outer_condition)[0];
        let has_x = builder.ins().icmp(IntCC::SignedLessThan, x, width);
        builder.ins().brif(
            has_x,
            inner_condition,
            &[x.into(), zero_i32.into()],
            complete,
            &[],
        );

        builder.switch_to_block(inner_condition);
        let pixel_indices = builder.block_params(inner_condition).to_vec();
        let x = pixel_indices[0];
        let y = pixel_indices[1];
        let has_y = builder.ins().icmp(IntCC::SignedLessThan, y, height);
        let next_x = builder.ins().iadd_imm_s(x, 1);
        builder.ins().brif(
            has_y,
            pixel_calculation,
            &[],
            outer_condition,
            &[next_x.into()],
        );

        builder.switch_to_block(pixel_calculation);
        let x_as_f64 = builder.ins().fcvt_from_sint(types::F64, x);
        let y_as_f64 = builder.ins().fcvt_from_sint(types::F64, y);
        let width_as_f64 = builder.ins().fcvt_from_sint(types::F64, width);
        let height_as_f64 = builder.ins().fcvt_from_sint(types::F64, height);
        let real_scaled = builder.ins().fmul(x_width, x_as_f64);
        let real_fraction = builder.ins().fdiv(real_scaled, width_as_f64);
        let real_c = builder.ins().fadd(real_fraction, x_min);
        let imag_scaled = builder.ins().fmul(y_width, y_as_f64);
        let imag_fraction = builder.ins().fdiv(imag_scaled, height_as_f64);
        let imag_c = builder.ins().fadd(imag_fraction, y_min);
        let zero_f64 = builder.ins().f64const(0.0);
        builder.ins().jump(
            iteration,
            &[
                x.into(),
                y.into(),
                real_c.into(),
                imag_c.into(),
                zero_f64.into(),
                zero_f64.into(),
                zero_i32.into(),
            ],
        );

        builder.switch_to_block(iteration);
        let state = builder.block_params(iteration).to_vec();
        let (x, y, real_c, imag_c, real_z, imag_z, count) = (
            state[0], state[1], state[2], state[3], state[4], state[5], state[6],
        );
        let real_squared = builder.ins().fmul(real_z, real_z);
        let imag_squared = builder.ins().fmul(imag_z, imag_z);
        let u = builder.ins().fsub(real_squared, imag_squared);
        let doubled_real = builder.ins().fadd(real_z, real_z);
        let v = builder.ins().fmul(doubled_real, imag_z);
        let next_real = builder.ins().fadd(u, real_c);
        let next_imag = builder.ins().fadd(v, imag_c);
        let next_count = builder.ins().iadd_imm_s(count, 1);
        let abs_real = builder.ins().fabs(next_real);
        let abs_imag = builder.ins().fabs(next_imag);
        let magnitude = builder.ins().fadd(abs_real, abs_imag);
        let four = builder.ins().f64const(4.0);
        let escaped = builder.ins().fcmp(FloatCC::GreaterThan, magnitude, four);
        let reached_limit = builder
            .ins()
            .icmp(IntCC::Equal, next_count, iteration_limit);
        let finished = builder.ins().bor(escaped, reached_limit);
        builder.ins().brif(
            finished,
            pixel_ready,
            &[
                x.into(),
                y.into(),
                real_c.into(),
                imag_c.into(),
                next_count.into(),
                next_real.into(),
                next_imag.into(),
                u.into(),
                v.into(),
            ],
            iteration,
            &[
                x.into(),
                y.into(),
                real_c.into(),
                imag_c.into(),
                next_real.into(),
                next_imag.into(),
                next_count.into(),
            ],
        );

        builder.switch_to_block(pixel_ready);
        let pixel = builder.block_params(pixel_ready).to_vec();
        let callback_call = builder.ins().call(
            callback_ref,
            &[
                callback_context,
                pixel[0],
                pixel[1],
                pixel[2],
                pixel[3],
                pixel[4],
                pixel[5],
                pixel[6],
                pixel[7],
                pixel[8],
            ],
        );
        let callback_status = builder.inst_results(callback_call)[0];
        let callback_succeeded = builder
            .ins()
            .icmp(IntCC::NotEqual, callback_status, zero_i32);
        builder.ins().brif(
            callback_succeeded,
            advance_y,
            &[pixel[0].into(), pixel[1].into()],
            failed,
            &[],
        );

        builder.switch_to_block(advance_y);
        let indices = builder.block_params(advance_y).to_vec();
        let next_y = builder.ins().iadd_imm_s(indices[1], 1);
        builder
            .ins()
            .jump(inner_condition, &[indices[0].into(), next_y.into()]);

        builder.switch_to_block(failed);
        let failure_status = builder.ins().iconst(types::I32, 0);
        builder.ins().return_(&[failure_status]);

        builder.switch_to_block(complete);
        let success_status = builder.ins().iconst(types::I32, 1);
        builder.ins().return_(&[success_status]);

        builder.seal_all_blocks();
        builder.finalize(frontend_config);
    }

    module
        .define_function(function, &mut context)
        .map_err(|error| error.to_string())?;
    module.clear_context(&mut context);
    Ok(function)
}

fn define_clocksp5_integer_region<M: Module>(module: &mut M) -> Result<FuncId, String> {
    let mut signature = module.make_signature();
    for _ in 0..5 {
        signature.params.push(AbiParam::new(types::I32));
    }
    signature.returns.push(AbiParam::new(types::I64));
    let function = module
        .declare_function("clocksp5_integer_repeat_region", Linkage::Local, &signature)
        .map_err(|error| error.to_string())?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, function.as_u32());
    let mut builder_context = FunctionBuilderContext::new();
    let frontend_config = module.target_config();

    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        let outer = builder.create_block();
        let inner = builder.create_block();
        let outer_condition = builder.create_block();
        let exit = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.append_block_param(outer, types::I32);
        builder.append_block_param(inner, types::I32);
        builder.append_block_param(outer_condition, types::I32);
        builder.append_block_param(outer_condition, types::I32);
        builder.append_block_param(exit, types::I32);
        builder.append_block_param(exit, types::I32);

        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let args = builder.block_params(entry).to_vec();
        let (b, initial_l, initial_i, d, e) = (args[0], args[1], args[2], args[3], args[4]);
        builder.ins().jump(outer, &[initial_l.into()]);

        builder.switch_to_block(outer);
        let previous_l = builder.block_params(outer)[0];
        let l_wide = builder.ins().sextend(types::I64, previous_l);
        let b_wide = builder.ins().sextend(types::I64, b);
        let l_sum = builder.ins().iadd(l_wide, b_wide);
        let next_l = saturating_i32(&mut builder, l_sum);
        builder.ins().jump(inner, &[initial_i.into()]);

        builder.switch_to_block(inner);
        let previous_c = builder.block_params(inner)[0];
        let c_wide = builder.ins().sextend(types::I64, previous_c);
        let c_sum = builder.ins().iadd(c_wide, b_wide);
        let next_c = saturating_i32(&mut builder, c_sum);
        let c_done = builder.ins().icmp(IntCC::SignedGreaterThan, next_c, d);
        builder.ins().brif(
            c_done,
            outer_condition,
            &[next_l.into(), next_c.into()],
            inner,
            &[next_c.into()],
        );

        builder.switch_to_block(outer_condition);
        let outer_values = builder.block_params(outer_condition).to_vec();
        let l_value = outer_values[0];
        let c_value = outer_values[1];
        let l_done = builder.ins().icmp(IntCC::SignedGreaterThan, l_value, e);
        builder.ins().brif(
            l_done,
            exit,
            &[l_value.into(), c_value.into()],
            outer,
            &[l_value.into()],
        );

        builder.switch_to_block(exit);
        let values = builder.block_params(exit).to_vec();
        let l_wide = builder.ins().sextend(types::I64, values[0]);
        let packed_l = builder.ins().ishl_imm_s(l_wide, 32);
        let packed_c = builder.ins().uextend(types::I64, values[1]);
        let packed = builder.ins().bor(packed_l, packed_c);
        builder.ins().return_(&[packed]);
        builder.seal_all_blocks();
        builder.finalize(frontend_config);
    }

    module
        .define_function(function, &mut context)
        .map_err(|error| error.to_string())?;
    module.clear_context(&mut context);
    Ok(function)
}

fn saturating_i32(
    builder: &mut FunctionBuilder<'_>,
    value: cranelift_codegen::ir::Value,
) -> cranelift_codegen::ir::Value {
    let minimum = builder.ins().iconst(types::I64, i64::from(i32::MIN));
    let maximum = builder.ins().iconst(types::I64, i64::from(i32::MAX));
    let below = builder.ins().icmp(IntCC::SignedLessThan, value, minimum);
    let above = builder.ins().icmp(IntCC::SignedGreaterThan, value, maximum);
    let clamped_low = builder.ins().select(below, minimum, value);
    let clamped = builder.ins().select(above, maximum, clamped_low);
    builder.ins().ireduce(types::I32, clamped)
}

fn has_compatible_mandelbrot_procedure(program: &ParsedProgram) -> bool {
    let Some(definition) = program.procedures.get("IT") else {
        return false;
    };
    if definition.parameters.len() != 3
        || !variable_is(&definition.parameters[0], "A")
        || !variable_is(&definition.parameters[1], "B")
        || !variable_is(&definition.parameters[2], "ITER%")
    {
        return false;
    }

    let Some(end) = program.instructions[definition.entry..]
        .iter()
        .position(|instruction| matches!(instruction.statement, Statement::EndProcedure))
        .map(|offset| definition.entry + offset)
    else {
        return false;
    };
    if !has_no_branch_into_region(program, definition.entry, end + 1) {
        return false;
    }

    let body = procedure_body(program, definition);
    let body = body
        .into_iter()
        .filter(|instruction| !matches!(instruction.statement, Statement::NoOp))
        .collect::<Vec<_>>();
    if body.len() != 11 {
        return false;
    }

    let assignments_match = assignment_to(&body[0].statement, "IT%", &Expr::Number(0.0))
        && assignment_to(&body[1].statement, "E", &Expr::Number(0.0))
        && assignment_to(&body[2].statement, "F", &Expr::Number(0.0))
        && matches!(body[3].statement, Statement::Repeat)
        && assignment_to(
            &body[4].statement,
            "U",
            &binary(
                BinaryOp::Subtract,
                binary(
                    BinaryOp::Multiply,
                    Expr::Variable("E".into()),
                    Expr::Variable("E".into()),
                ),
                binary(
                    BinaryOp::Multiply,
                    Expr::Variable("F".into()),
                    Expr::Variable("F".into()),
                ),
            ),
        )
        && assignment_to(
            &body[5].statement,
            "V",
            &binary(
                BinaryOp::Multiply,
                binary(
                    BinaryOp::Multiply,
                    Expr::Number(2.0),
                    Expr::Variable("E".into()),
                ),
                Expr::Variable("F".into()),
            ),
        )
        && assignment_to(
            &body[6].statement,
            "E",
            &binary(
                BinaryOp::Add,
                Expr::Variable("U".into()),
                Expr::Variable("A".into()),
            ),
        )
        && assignment_to(
            &body[7].statement,
            "F",
            &binary(
                BinaryOp::Add,
                Expr::Variable("V".into()),
                Expr::Variable("B".into()),
            ),
        )
        && assignment_to(
            &body[8].statement,
            "IT%",
            &binary(
                BinaryOp::Add,
                Expr::Variable("IT%".into()),
                Expr::Number(1.0),
            ),
        )
        && until_is_mandelbrot_condition(&body[9].statement)
        && matches!(body[10].statement, Statement::EndProcedure);

    assignments_match
}

fn has_compatible_mandelbrot_sethsv_procedure(program: &ParsedProgram) -> bool {
    let Some(definition) = program.procedures.get("SETHSV") else {
        return false;
    };
    if definition.parameters.len() != 3
        || !variable_is(&definition.parameters[0], "H%")
        || !variable_is(&definition.parameters[1], "S%")
        || !variable_is(&definition.parameters[2], "V%")
    {
        return false;
    }
    let body = procedure_body(program, definition)
        .into_iter()
        .filter(|instruction| !matches!(instruction.statement, Statement::NoOp))
        .collect::<Vec<_>>();
    if body.len() != 3 {
        return false;
    }

    let convert_matches = matches!(
        &body[0].statement,
        Statement::Sys { name, arguments, results }
            if name.eq_ignore_ascii_case(b"ColourTrans_ConvertHSVToRGB")
                && arguments.len() == 3
                && arguments[0].as_ref().is_some_and(|value| expression_is(
                    value,
                    &binary(
                        BinaryOp::Multiply,
                        Expr::Variable("H%".into()),
                        Expr::Number(65_536.0),
                    ),
                ))
                && arguments[1].as_ref().is_some_and(|value| expression_is(
                    value,
                    &binary(
                        BinaryOp::Multiply,
                        Expr::Variable("S%".into()),
                        Expr::Number(256.0),
                    ),
                ))
                && arguments[2].as_ref().is_some_and(|value| expression_is(
                    value,
                    &Expr::Variable("V%".into()),
                ))
                && results.iter().map(String::as_str).eq(["R%", "G%", "B%"])
    );
    let packed_colour = binary(
        BinaryOp::Add,
        binary(
            BinaryOp::Add,
            binary(
                BinaryOp::ShiftLeft,
                Expr::Variable("B%".into()),
                Expr::Number(24.0),
            ),
            binary(
                BinaryOp::ShiftLeft,
                Expr::Variable("G%".into()),
                Expr::Number(16.0),
            ),
        ),
        binary(
            BinaryOp::ShiftLeft,
            Expr::Variable("R%".into()),
            Expr::Number(8.0),
        ),
    );
    let set_gcol_matches = matches!(
        &body[1].statement,
        Statement::Sys { name, arguments, results }
            if name.eq_ignore_ascii_case(b"ColourTrans_SetGCOL")
                && arguments.len() == 5
                && arguments[0].as_ref().is_some_and(|value| expression_is(value, &packed_colour))
                && arguments[1].is_none()
                && arguments[2].is_none()
                && arguments[3].as_ref().is_some_and(|value| expression_is(value, &Expr::Number(256.0)))
                && arguments[4].as_ref().is_some_and(|value| expression_is(value, &Expr::Number(0.0)))
                && results.is_empty()
    );
    convert_matches && set_gcol_matches && matches!(body[2].statement, Statement::EndProcedure)
}

fn find_mandelbrot_inline_region(program: &ParsedProgram) -> Option<MandelbrotInlineRegion> {
    let instructions = &program.instructions;
    for start in 3..instructions.len().saturating_sub(6) {
        let slice = &instructions[start..start + 7];
        let line = slice[0].line_number;
        if !matches!(slice[0].statement, Statement::Repeat)
            || !assignment_to(
                &slice[1].statement,
                "U",
                &binary(
                    BinaryOp::Subtract,
                    binary(
                        BinaryOp::Multiply,
                        Expr::Variable("e".into()),
                        Expr::Variable("e".into()),
                    ),
                    binary(
                        BinaryOp::Multiply,
                        Expr::Variable("f".into()),
                        Expr::Variable("f".into()),
                    ),
                ),
            )
            || !assignment_to(
                &slice[2].statement,
                "V",
                &binary(
                    BinaryOp::Multiply,
                    binary(
                        BinaryOp::Multiply,
                        Expr::Number(2.0),
                        Expr::Variable("e".into()),
                    ),
                    Expr::Variable("f".into()),
                ),
            )
            || !assignment_is_binary(&slice[3].statement, "e", "u", BinaryOp::Add, "a")
            || !assignment_is_binary(&slice[4].statement, "f", "v", BinaryOp::Add, "b")
            || !assignment_to(
                &slice[5].statement,
                "IT%",
                &binary(
                    BinaryOp::Add,
                    Expr::Variable("IT%".into()),
                    Expr::Number(1.0),
                ),
            )
            || !until_is_inline_mandelbrot_condition(&slice[6].statement)
        {
            continue;
        }
        let initialized = start >= 3
            && assignment_to(
                &instructions[start - 3].statement,
                "IT%",
                &Expr::Number(0.0),
            )
            && assignment_to(&instructions[start - 2].statement, "e", &Expr::Number(0.0))
            && assignment_to(&instructions[start - 1].statement, "f", &Expr::Number(0.0));
        if initialized && has_no_branch_into_region(program, start + 1, start + 7) {
            return Some(MandelbrotInlineRegion {
                start,
                after: start + 7,
                line,
            });
        }
    }
    None
}

fn find_mandelbrot_frame_region(program: &ParsedProgram) -> Option<MandelbrotFrameRegion> {
    if !has_compatible_mandelbrot_procedure(program)
        || !has_compatible_mandelbrot_sethsv_procedure(program)
    {
        return None;
    }
    let statements = program
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, instruction)| !matches!(instruction.statement, Statement::NoOp))
        .collect::<Vec<_>>();
    for window in statements.windows(11) {
        if !mandelbrot_frame_matches(window) {
            continue;
        }
        let start = window[0].0;
        let after = window[10].0 + 1;
        if has_no_branch_into_region(program, start, after) {
            return Some(MandelbrotFrameRegion {
                start,
                after,
                line: window[0].1.line_number,
            });
        }
    }
    None
}

fn mandelbrot_frame_matches(window: &[(usize, &parser::LocatedStatement)]) -> bool {
    let statement = |index: usize| &window[index].1.statement;
    for_loop_matches(statement(0), "X%", "XSIZE%")
        && for_loop_matches(statement(1), "Y%", "YSIZE%")
        && assignment_to(
            statement(2),
            "A",
            &binary(
                BinaryOp::Add,
                binary(
                    BinaryOp::Divide,
                    binary(
                        BinaryOp::Multiply,
                        Expr::Variable("XWIDTH".into()),
                        Expr::Variable("X%".into()),
                    ),
                    Expr::Variable("XSIZE%".into()),
                ),
                Expr::Variable("XMIN".into()),
            ),
        )
        && assignment_to(
            statement(3),
            "B",
            &binary(
                BinaryOp::Add,
                binary(
                    BinaryOp::Divide,
                    binary(
                        BinaryOp::Multiply,
                        Expr::Variable("YWIDTH".into()),
                        Expr::Variable("Y%".into()),
                    ),
                    Expr::Variable("YSIZE%".into()),
                ),
                Expr::Variable("YMIN".into()),
            ),
        )
        && matches!(
            statement(4),
            Statement::ProcedureCall(name, arguments)
                if name.eq_ignore_ascii_case("IT")
                    && arguments.len() == 3
                    && expression_is(&arguments[0], &Expr::Variable("A".into()))
                    && expression_is(&arguments[1], &Expr::Variable("B".into()))
                    && expression_is(&arguments[2], &Expr::Variable("MAX%".into()))
        )
        && assignment_to(
            statement(5),
            "H%",
            &binary(
                BinaryOp::Subtract,
                Expr::Number(360.0),
                binary(
                    BinaryOp::Divide,
                    binary(
                        BinaryOp::Multiply,
                        Expr::Number(360.0),
                        Expr::Builtin(0xAB, vec![Expr::Variable("IT%".into())]),
                    ),
                    Expr::Builtin(0xAB, vec![Expr::Variable("MAX%".into())]),
                ),
            ),
        )
        && if_matches_mandelbrot_colour(statement(6))
        && move_or_draw_matches(statement(7), "MOVE")
        && move_or_draw_matches(statement(8), "DRAW")
        && next_matches(statement(9), "Y%")
        && next_matches(statement(10), "X%")
}

fn for_loop_matches(statement: &Statement, variable: &str, limit_variable: &str) -> bool {
    matches!(statement, Statement::For { variable: actual, start, end, step }
        if variable_is(actual, variable)
            && expression_is(start, &Expr::Number(0.0))
            && expression_is(
                end,
                &binary(
                    BinaryOp::Subtract,
                    Expr::Variable(limit_variable.into()),
                    Expr::Number(1.0),
                ),
            )
            && step.as_ref().is_some_and(|step| expression_is(step, &Expr::Number(1.0))))
}

fn if_matches_mandelbrot_colour(statement: &Statement) -> bool {
    let Statement::If(condition, then_body, else_body) = statement else {
        return false;
    };
    let condition_matches = expression_is(
        condition,
        &binary(
            BinaryOp::Greater,
            binary(
                BinaryOp::Add,
                Expr::Builtin(0x94, vec![Expr::Variable("E".into())]),
                Expr::Builtin(0x94, vec![Expr::Variable("F".into())]),
            ),
            Expr::Number(4.0),
        ),
    );
    let then_matches = matches!(then_body.as_slice(), [Statement::ProcedureCall(name, arguments)]
        if name.eq_ignore_ascii_case("SETHSV")
            && arguments.len() == 3
            && expression_is(&arguments[0], &Expr::Variable("H%".into()))
            && expression_is(&arguments[1], &Expr::Number(255.0))
            && expression_is(&arguments[2], &Expr::Number(255.0)));
    let else_matches = matches!(else_body.as_slice(), [Statement::Sys { name, arguments, results }]
        if name.eq_ignore_ascii_case(b"ColourTrans_SetGCOL")
            && arguments.len() == 5
            && arguments[0].as_ref().is_some_and(|value| expression_is(value, &Expr::Number(0.0)))
            && arguments[1].is_none()
            && arguments[2].is_none()
            && arguments[3].as_ref().is_some_and(|value| expression_is(value, &Expr::Number(256.0)))
            && arguments[4].as_ref().is_some_and(|value| expression_is(value, &Expr::Number(0.0)))
            && results.is_empty());
    condition_matches && then_matches && else_matches
}

fn move_or_draw_matches(statement: &Statement, command: &str) -> bool {
    let (x, y) = match (command, statement) {
        ("MOVE", Statement::Move(x, y)) | ("DRAW", Statement::Draw(x, y)) => (x, y),
        _ => return false,
    };
    expression_is(
        x,
        &binary(
            BinaryOp::Multiply,
            Expr::Variable("X%".into()),
            Expr::Number(2.0),
        ),
    ) && expression_is(
        y,
        &binary(
            BinaryOp::Multiply,
            Expr::Variable("Y%".into()),
            Expr::Number(2.0),
        ),
    )
}

fn next_matches(statement: &Statement, variable: &str) -> bool {
    matches!(statement, Statement::Next(Some(actual)) if variable_is(actual, variable))
}

fn until_is_inline_mandelbrot_condition(statement: &Statement) -> bool {
    let Statement::Until(expression) = statement else {
        return false;
    };
    let expected = Expr::Binary(
        Box::new(Expr::Binary(
            Box::new(Expr::Variable("IT%".into())),
            BinaryOp::Equal,
            Box::new(Expr::Variable("MAX%".into())),
        )),
        BinaryOp::Or,
        Box::new(Expr::Binary(
            Box::new(Expr::Binary(
                Box::new(Expr::Builtin(0x94, vec![Expr::Variable("e".into())])),
                BinaryOp::Add,
                Box::new(Expr::Builtin(0x94, vec![Expr::Variable("f".into())])),
            )),
            BinaryOp::Greater,
            Box::new(Expr::Number(4.0)),
        )),
    );
    expression_is(expression, &expected)
}

fn procedure_body<'a>(
    program: &'a ParsedProgram,
    definition: &Definition,
) -> Vec<&'a parser::LocatedStatement> {
    program.instructions[definition.entry..]
        .iter()
        .take_while(|instruction| !matches!(instruction.statement, Statement::EndProcedure))
        .chain(
            program.instructions[definition.entry..]
                .iter()
                .find(|instruction| matches!(instruction.statement, Statement::EndProcedure)),
        )
        .collect()
}

fn until_is_mandelbrot_condition(statement: &Statement) -> bool {
    let Statement::Until(expression) = statement else {
        return false;
    };
    let expected = Expr::Binary(
        Box::new(Expr::Binary(
            Box::new(Expr::Variable("IT%".into())),
            BinaryOp::Equal,
            Box::new(Expr::Variable("ITER%".into())),
        )),
        BinaryOp::Or,
        Box::new(Expr::Binary(
            Box::new(Expr::Binary(
                Box::new(Expr::Builtin(0x94, vec![Expr::Variable("e".into())])),
                BinaryOp::Add,
                Box::new(Expr::Builtin(0x94, vec![Expr::Variable("f".into())])),
            )),
            BinaryOp::Greater,
            Box::new(Expr::Number(4.0)),
        )),
    );
    expression_is(expression, &expected)
}

fn find_clocksp5_integer_region(program: &ParsedProgram) -> Option<ClockSp5Region> {
    let instructions = &program.instructions;
    for start in 0..instructions.len().saturating_sub(6) {
        let slice = &instructions[start..start + 7];
        let line = slice[0].line_number;
        if !matches!(slice[0].statement, Statement::Repeat)
            || !assignment_is_binary(&slice[1].statement, "L%", "B%", BinaryOp::Add, "L%")
            || !assignment_is_variable(&slice[2].statement, "C%", "I%")
            || !matches!(slice[3].statement, Statement::Repeat)
            || !assignment_is_binary(&slice[4].statement, "C%", "B%", BinaryOp::Add, "C%")
            || !until_greater(&slice[5].statement, "C%", "D%")
            || !until_greater(&slice[6].statement, "L%", "E%")
        {
            continue;
        }
        if !has_no_branch_into_region(program, start + 1, start + 7) {
            continue;
        }
        return Some(ClockSp5Region {
            start,
            after: start + 7,
            line,
        });
    }
    None
}

fn has_no_branch_into_region(program: &ParsedProgram, start: usize, after: usize) -> bool {
    let interior_lines = program
        .line_entries
        .iter()
        .filter_map(|(line, address)| (*address >= start && *address < after).then_some(*line))
        .collect::<Vec<_>>();
    if interior_lines.is_empty() {
        return true;
    }

    !program.instructions.iter().any(|instruction| {
        let mut targets = Vec::new();
        collect_branch_targets(&instruction.statement, &mut targets);
        targets.iter().any(|target| interior_lines.contains(target))
    })
}

fn collect_branch_targets(statement: &Statement, targets: &mut Vec<u16>) {
    match statement {
        Statement::Goto(target) | Statement::Gosub(target) => targets.push(*target),
        Statement::If(_, consequent, alternative) => {
            for nested in consequent.iter().chain(alternative) {
                collect_branch_targets(nested, targets);
            }
        }
        _ => {}
    }
}

fn assignment_to(statement: &Statement, target: &str, expression: &Expr) -> bool {
    matches!(statement, Statement::Assign(LValue::Variable(name), actual) if variable_is(name, target) && expression_is(actual, expression))
}

fn expression_is(actual: &Expr, expected: &Expr) -> bool {
    match (actual, expected) {
        (Expr::Number(actual), Expr::Number(expected)) => actual == expected,
        (Expr::Variable(actual), Expr::Variable(expected)) => variable_is(actual, expected),
        (
            Expr::Binary(actual_left, actual_op, actual_right),
            Expr::Binary(expected_left, expected_op, expected_right),
        ) => {
            actual_op == expected_op
                && expression_is(actual_left, expected_left)
                && expression_is(actual_right, expected_right)
        }
        (
            Expr::Builtin(actual_token, actual_arguments),
            Expr::Builtin(expected_token, expected_arguments),
        ) => {
            actual_token == expected_token
                && actual_arguments.len() == expected_arguments.len()
                && actual_arguments
                    .iter()
                    .zip(expected_arguments)
                    .all(|(actual, expected)| expression_is(actual, expected))
        }
        _ => false,
    }
}

fn assignment_is_variable(statement: &Statement, target: &str, source: &str) -> bool {
    matches!(statement, Statement::Assign(LValue::Variable(name), Expr::Variable(value)) if variable_is(name, target) && variable_is(value, source))
}

fn assignment_is_binary(
    statement: &Statement,
    target: &str,
    left: &str,
    operator: BinaryOp,
    right: &str,
) -> bool {
    matches!(statement, Statement::Assign(LValue::Variable(name), Expr::Binary(lhs, op, rhs)) if variable_is(name, target) && *op == operator && variable_expr_is(lhs, left) && variable_expr_is(rhs, right))
}

fn until_greater(statement: &Statement, left: &str, right: &str) -> bool {
    matches!(statement, Statement::Until(Expr::Binary(lhs, BinaryOp::Greater, rhs)) if variable_expr_is(lhs, left) && variable_expr_is(rhs, right))
}

fn variable_expr_is(expression: &Expr, name: &str) -> bool {
    matches!(expression, Expr::Variable(variable) if variable_is(variable, name))
}

fn variable_is(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

fn binary(operator: BinaryOp, left: Expr, right: Expr) -> Expr {
    Expr::Binary(Box::new(left), operator, Box::new(right))
}

pub(super) fn parse_for_jit(
    program: &TokenizedBasicProgram,
) -> Result<ParsedProgram, RuntimeError> {
    let profile = match program.record_layout {
        Some(TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn) => {
            TokenProfile::SharedBoundaryCore
        }
        Some(TokenizedBasicRecordLayout::SeparateLineCarriageReturn) | None => {
            TokenProfile::ArmBasicV
        }
    };
    parser::parse_program(program, profile)
}

pub(super) fn compile_for_jit(program: &ParsedProgram) -> Result<Option<JitProgram>, String> {
    JitProgram::compile(program)
}
