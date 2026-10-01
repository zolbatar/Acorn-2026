//! Experimental Cranelift kernels used by the configured Hybrid engine.
//!
//! The compatibility interpreter handles statements outside verified hot
//! regions. The Mandelbrot frame kernel routes ColourTrans and OS_Plot through
//! checked runtime callbacks. Native entry points stay private to this module.

use std::{
    collections::{BTreeSet, HashMap},
    ffi::c_void,
    time::Instant,
};

use cranelift_codegen::{
    ir::{
        AbiParam, InstBuilder, MemFlagsData, UserFuncName, Value as IrValue,
        condcodes::{FloatCC, IntCC},
        types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};

use crate::{
    error::RuntimeError,
    memory::Task,
    swi::{SwiContext, SwiDispatcher},
};

use super::{
    JitExecutionReport,
    parser::{self, BinaryOp, Definition, Expr, LValue, ParsedProgram, Statement, UnaryOp},
    runtime::{self, Interpreter, NativeProcedureContext},
};

type MandelbrotKernel = extern "C" fn(f64, f64, i32, *mut f64) -> i32;
type MandelbrotFrameKernel = extern "C" fn(i32, i32, f64, f64, f64, f64, i32, *mut c_void) -> i32;
type ClockSp5IntegerRegion = extern "C" fn(i32, i32, i32, i32, i32) -> i64;
type NumericStatementKernel = extern "C" fn(*const f64, *mut f64);
type NumericProcedureKernel =
    extern "C" fn(*mut c_void, f64, f64, f64, f64, f64, f64, f64, f64) -> i32;

const MAX_MANDELBROT_FRAME_PIXELS: u64 = 4_194_304;
const MAX_NATIVE_PROCEDURE_PARAMETERS: usize = 8;
const MAX_NATIVE_PROCEDURE_DEPTH: u64 = 64;

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
            dispatcher.dispatch_named_swi("COLOURTRANS_CONVERTHSVTORGB", task, &mut conversion)?;
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
        dispatcher.dispatch_named_swi("COLOURTRANS_SETGCOL", task, &mut gcol)?;
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
        if let Some(key) = unsafe { (&mut *context.dispatcher).poll_key(&*context.task) } {
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

#[derive(Clone, Debug)]
struct NumericStatementRegion {
    start: usize,
    line: u16,
    variables: Vec<String>,
    expressions: Vec<Expr>,
}

struct CompiledNumericStatement {
    region: NumericStatementRegion,
    kernel: NumericStatementKernel,
}

#[derive(Clone, Debug)]
struct NumericProcedureRegion {
    name: String,
    entry: usize,
    after: usize,
    line: u16,
    parameters: Vec<String>,
    variable_names: Vec<String>,
    body: Vec<parser::LocatedStatement>,
    recursive_calls_per_path: u64,
}

struct CompiledNumericProcedure {
    region: NumericProcedureRegion,
    kernel: NumericProcedureKernel,
}

#[derive(Clone, Copy)]
struct NumericProcedureHelpers {
    enter: FuncId,
    tick: FuncId,
    context_ok: FuncId,
    get_variable: FuncId,
    set_variable: FuncId,
    graphics: FuncId,
    integer: FuncId,
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
    numeric_statements: Vec<CompiledNumericStatement>,
    numeric_procedures: Vec<CompiledNumericProcedure>,
    report: JitExecutionReport,
}

impl JitProgram {
    pub(super) fn compile(program: &ParsedProgram) -> Result<Option<Self>, String> {
        super::system_ir::PortableSystemIr::native_boundary_for_program(
            program,
            super::system_ir::SystemIrBackend::HybridJit,
        )?;
        let compile_started = Instant::now();
        let compile_mandelbrot_procedure = has_compatible_mandelbrot_procedure(program);
        let mandelbrot_inline_region = find_mandelbrot_inline_region(program);
        let mandelbrot_frame_region = find_mandelbrot_frame_region(program);
        let clocksp5_region = find_clocksp5_integer_region(program);
        let numeric_procedure_regions = find_numeric_procedure_regions(program);
        let numeric_regions = find_numeric_statement_regions(program, &numeric_procedure_regions);
        let compile_mandelbrot = compile_mandelbrot_procedure || mandelbrot_inline_region.is_some();
        if !compile_mandelbrot
            && mandelbrot_frame_region.is_none()
            && clocksp5_region.is_none()
            && numeric_regions.is_empty()
            && numeric_procedure_regions.is_empty()
        {
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
                "ricochet_mandelbrot_render_pixel",
                render_mandelbrot_pixel as *const () as *const u8,
            );
        }
        jit_builder.symbol("ricochet_basic_pow", basic_pow as *const () as *const u8);
        jit_builder.symbol("ricochet_basic_abs", basic_abs as *const () as *const u8);
        jit_builder.symbol("ricochet_basic_cos", basic_cos as *const () as *const u8);
        jit_builder.symbol("ricochet_basic_sin", basic_sin as *const () as *const u8);
        jit_builder.symbol("ricochet_basic_tan", basic_tan as *const () as *const u8);
        jit_builder.symbol("ricochet_basic_sqrt", basic_sqrt as *const () as *const u8);
        jit_builder.symbol(
            "ricochet_basic_floor",
            basic_floor as *const () as *const u8,
        );
        jit_builder.symbol("ricochet_basic_ln", basic_ln as *const () as *const u8);
        jit_builder.symbol(
            "ricochet_basic_log10",
            basic_log10 as *const () as *const u8,
        );
        if !numeric_procedure_regions.is_empty() {
            jit_builder.symbol(
                "ricochet_basic_jit_procedure_enter",
                runtime::native_procedure_enter as *const () as *const u8,
            );
            jit_builder.symbol(
                "ricochet_basic_jit_procedure_tick",
                runtime::native_procedure_tick as *const () as *const u8,
            );
            jit_builder.symbol(
                "ricochet_basic_jit_procedure_context_ok",
                runtime::native_procedure_context_ok as *const () as *const u8,
            );
            jit_builder.symbol(
                "ricochet_basic_jit_procedure_get_variable",
                runtime::native_procedure_get_variable as *const () as *const u8,
            );
            jit_builder.symbol(
                "ricochet_basic_jit_procedure_set_variable",
                runtime::native_procedure_set_variable as *const () as *const u8,
            );
            jit_builder.symbol(
                "ricochet_basic_jit_procedure_graphics",
                runtime::native_procedure_graphics as *const () as *const u8,
            );
            jit_builder.symbol(
                "ricochet_basic_jit_procedure_integer",
                runtime::native_procedure_integer as *const () as *const u8,
            );
        }
        let mut module = JITModule::new(jit_builder);
        let numeric_helpers = declare_numeric_helpers(&mut module)?;
        let numeric_procedure_helpers = if numeric_procedure_regions.is_empty() {
            None
        } else {
            Some(declare_numeric_procedure_helpers(&mut module)?)
        };

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
        let numeric_functions = numeric_regions
            .iter()
            .enumerate()
            .map(|(index, region)| {
                define_numeric_statement_kernel(&mut module, region, &numeric_helpers, index)
                    .map(|function| (region.clone(), function))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let numeric_procedure_functions = numeric_procedure_regions
            .iter()
            .enumerate()
            .map(|(index, region)| {
                define_numeric_procedure_kernel(
                    &mut module,
                    region,
                    &numeric_helpers,
                    numeric_procedure_helpers.expect("procedure helpers were declared"),
                    index,
                )
                .map(|function| (region.clone(), function))
            })
            .collect::<Result<Vec<_>, _>>()?;

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

        let numeric_statements = numeric_functions
            .into_iter()
            .map(|(region, function)| {
                // SAFETY: each function is defined with the two-pointer
                // signature in `define_numeric_statement_kernel`; the module
                // remains owned by this JitProgram.
                let kernel = unsafe {
                    std::mem::transmute::<*const u8, NumericStatementKernel>(
                        module.get_finalized_function(function),
                    )
                };
                CompiledNumericStatement { region, kernel }
            })
            .collect::<Vec<_>>();

        let numeric_procedures = numeric_procedure_functions
            .into_iter()
            .map(|(region, function)| {
                // SAFETY: every procedure entry uses the fixed context-plus-
                // eight-number ABI in `define_numeric_procedure_kernel`, and
                // its module remains owned by this JitProgram.
                let kernel = unsafe {
                    std::mem::transmute::<*const u8, NumericProcedureKernel>(
                        module.get_finalized_function(function),
                    )
                };
                CompiledNumericProcedure { region, kernel }
            })
            .collect::<Vec<_>>();

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
        for procedure in &numeric_procedures {
            compiled_units.push(format!(
                "countdown-recursive numeric procedure PROC {} at BASIC line {}",
                procedure.region.name, procedure.region.line
            ));
        }
        for numeric in &numeric_statements {
            let region = &numeric.region;
            compiled_units.push(format!(
                "numeric expression statement at BASIC line {}",
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
            numeric_statements,
            numeric_procedures,
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
                "Hybrid JIT Mandelbrot frame stopped without a runtime error".into(),
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
        self.report.fallback_reason =
            Some("mode setup and the final key wait used the interpreter".into());
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

    pub(super) fn numeric_statement_variables(&self, address: usize) -> Option<Vec<String>> {
        self.numeric_statements
            .iter()
            .find(|statement| statement.region.start == address)
            .map(|statement| statement.region.variables.clone())
    }

    pub(super) fn run_numeric_statement(
        &mut self,
        address: usize,
        inputs: &[f64],
    ) -> Option<Vec<f64>> {
        let statement = self
            .numeric_statements
            .iter()
            .find(|statement| statement.region.start == address)?;
        if inputs.len() != statement.region.variables.len() {
            return None;
        }
        let mut outputs = vec![0.0; statement.region.expressions.len()];
        let started = Instant::now();
        (statement.kernel)(inputs.as_ptr(), outputs.as_mut_ptr());
        self.report.compiled_calls += 1;
        self.report.compiled_time += started.elapsed();
        Some(outputs)
    }

    pub(super) fn native_procedure_variables(&self, name: &str) -> Option<Vec<String>> {
        self.numeric_procedures
            .iter()
            .find(|procedure| procedure.region.name.eq_ignore_ascii_case(name))
            .map(|procedure| procedure.region.variable_names.clone())
    }

    pub(super) fn native_procedure_is_safe(&self, name: &str, inputs: &[f64]) -> bool {
        let Some(procedure) = self
            .numeric_procedures
            .iter()
            .find(|procedure| procedure.region.name.eq_ignore_ascii_case(name))
        else {
            return false;
        };
        if inputs.len() != procedure.region.parameters.len() {
            return false;
        }
        let depth = inputs
            .first()
            .copied()
            .filter(|value| value.is_finite() && value.fract() == 0.0)
            .and_then(|value| u64::try_from(value as i64).ok());
        let Some(depth) = depth.filter(|depth| *depth < MAX_NATIVE_PROCEDURE_DEPTH) else {
            return false;
        };

        estimated_recursive_call_count(procedure.region.recursive_calls_per_path, depth)
            .is_some_and(|count| count <= runtime::MAX_NATIVE_PROCEDURE_CALLS)
    }

    pub(super) fn run_native_procedure(
        &mut self,
        name: &str,
        context: &mut NativeProcedureContext,
        inputs: &[f64],
    ) -> Result<bool, RuntimeError> {
        let Some(procedure) = self
            .numeric_procedures
            .iter()
            .find(|procedure| procedure.region.name.eq_ignore_ascii_case(name))
        else {
            return Ok(false);
        };
        if inputs.len() != procedure.region.parameters.len() {
            return Ok(false);
        }

        let mut arguments = [0.0; MAX_NATIVE_PROCEDURE_PARAMETERS];
        arguments[..inputs.len()].copy_from_slice(inputs);
        let started = Instant::now();
        let status = (procedure.kernel)(
            (context as *mut NativeProcedureContext).cast(),
            arguments[0],
            arguments[1],
            arguments[2],
            arguments[3],
            arguments[4],
            arguments[5],
            arguments[6],
            arguments[7],
        );
        self.report.compiled_calls += context.call_count();
        self.report.compiled_time += started.elapsed();

        if let Some(error) = context.take_error() {
            return Err(error);
        }
        if status == 0 {
            return Err(RuntimeError::Program(format!(
                "Hybrid JIT native procedure PROC {} stopped without a runtime error",
                procedure.region.name
            )));
        }
        self.report.fallback_reason =
            Some("top-level BASIC statements and unmatched code used the interpreter".into());
        Ok(true)
    }

    pub(super) fn report(&self) -> JitExecutionReport {
        self.report.clone()
    }
}

pub(super) fn run_parsed_program_jit(
    parsed: ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    let mut interpreter = Interpreter::new(parsed.clone());
    match compile_for_jit(&parsed) {
        Ok(Some(jit)) => interpreter.install_jit(jit),
        Ok(None) => interpreter.set_jit_fallback(
            "no BASIC regions were optimized; the entire program was interpreted",
        ),
        Err(reason) => interpreter.set_jit_fallback(&format!(
            "native compilation failed ({reason}); the entire program was interpreted"
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
            "ricochet_mandelbrot_render_pixel",
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
        Statement::Sys { name, arguments, results, .. }
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
        Statement::Sys { name, arguments, results, .. }
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
    let else_matches = matches!(else_body.as_slice(), [Statement::Sys { name, arguments, results, .. }]
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

pub(super) fn compile_for_jit(program: &ParsedProgram) -> Result<Option<JitProgram>, String> {
    JitProgram::compile(program)
}

pub(super) fn numeric_statement_with_results(
    statement: &Statement,
    values: &[f64],
) -> Option<Statement> {
    let mut values = values.iter().copied();
    let mut expression = || values.next().map(Expr::Number);
    let result = match statement {
        Statement::Assign(target, _) => Statement::Assign(target.clone(), expression()?),
        Statement::ProcedureCall(name, arguments) => Statement::ProcedureCall(
            name.clone(),
            (0..arguments.len())
                .map(|_| expression())
                .collect::<Option<Vec<_>>>()?,
        ),
        Statement::Line(_, _, _, _) => {
            Statement::Line(expression()?, expression()?, expression()?, expression()?)
        }
        Statement::Move(_, _) => Statement::Move(expression()?, expression()?),
        Statement::Draw(_, _) => Statement::Draw(expression()?, expression()?),
        Statement::Plot(_, _, _) => Statement::Plot(expression()?, expression()?, expression()?),
        Statement::Gcol(_, _) => Statement::Gcol(expression()?, expression()?),
        Statement::If(_, consequent, alternative) => {
            Statement::If(expression()?, consequent.clone(), alternative.clone())
        }
        Statement::IfBlock(_) => Statement::IfBlock(expression()?),
        _ => return None,
    };
    if values.next().is_some() {
        return None;
    }
    Some(result)
}

fn basic_pow(left: f64, right: f64) -> f64 {
    left.powf(right)
}

fn basic_abs(value: f64) -> f64 {
    value.abs()
}

fn basic_cos(value: f64) -> f64 {
    value.cos()
}

fn basic_sin(value: f64) -> f64 {
    value.sin()
}

fn basic_tan(value: f64) -> f64 {
    value.tan()
}

fn basic_sqrt(value: f64) -> f64 {
    value.sqrt()
}

fn basic_floor(value: f64) -> f64 {
    value.floor()
}

fn basic_ln(value: f64) -> f64 {
    value.ln()
}

fn basic_log10(value: f64) -> f64 {
    value.log10()
}

const NUMERIC_POWER: u8 = u8::MAX;

fn declare_numeric_helpers<M: Module>(module: &mut M) -> Result<HashMap<u8, FuncId>, String> {
    let mut helpers = HashMap::new();
    for (token, name, argument_count) in [
        (NUMERIC_POWER, "ricochet_basic_pow", 2),
        (0x94, "ricochet_basic_abs", 1),
        (0x9B, "ricochet_basic_cos", 1),
        (0xA8, "ricochet_basic_floor", 1),
        (0xAA, "ricochet_basic_ln", 1),
        (0xAB, "ricochet_basic_log10", 1),
        (0xB5, "ricochet_basic_sin", 1),
        (0xB6, "ricochet_basic_sqrt", 1),
        (0xB7, "ricochet_basic_tan", 1),
    ] {
        let mut signature = module.make_signature();
        signature
            .params
            .extend((0..argument_count).map(|_| AbiParam::new(types::F64)));
        signature.returns.push(AbiParam::new(types::F64));
        let function = module
            .declare_function(name, Linkage::Import, &signature)
            .map_err(|error| error.to_string())?;
        helpers.insert(token, function);
    }
    Ok(helpers)
}

fn declare_numeric_procedure_helpers<M: Module>(
    module: &mut M,
) -> Result<NumericProcedureHelpers, String> {
    let pointer_type = module.target_config().pointer_type();
    let mut status_signature = module.make_signature();
    status_signature.params.push(AbiParam::new(pointer_type));
    status_signature.returns.push(AbiParam::new(types::I32));
    let enter = module
        .declare_function(
            "ricochet_basic_jit_procedure_enter",
            Linkage::Import,
            &status_signature,
        )
        .map_err(|error| error.to_string())?;
    let mut tick_signature = module.make_signature();
    tick_signature.params.push(AbiParam::new(pointer_type));
    tick_signature.params.push(AbiParam::new(types::I32));
    tick_signature.returns.push(AbiParam::new(types::I32));
    let tick = module
        .declare_function(
            "ricochet_basic_jit_procedure_tick",
            Linkage::Import,
            &tick_signature,
        )
        .map_err(|error| error.to_string())?;
    let context_ok = module
        .declare_function(
            "ricochet_basic_jit_procedure_context_ok",
            Linkage::Import,
            &status_signature,
        )
        .map_err(|error| error.to_string())?;

    let mut get_signature = module.make_signature();
    get_signature.params.push(AbiParam::new(pointer_type));
    get_signature.params.push(AbiParam::new(types::I32));
    get_signature.returns.push(AbiParam::new(types::F64));
    let get_variable = module
        .declare_function(
            "ricochet_basic_jit_procedure_get_variable",
            Linkage::Import,
            &get_signature,
        )
        .map_err(|error| error.to_string())?;

    let mut set_signature = module.make_signature();
    set_signature.params.push(AbiParam::new(pointer_type));
    set_signature.params.push(AbiParam::new(types::I32));
    set_signature.params.push(AbiParam::new(types::F64));
    set_signature.returns.push(AbiParam::new(types::I32));
    let set_variable = module
        .declare_function(
            "ricochet_basic_jit_procedure_set_variable",
            Linkage::Import,
            &set_signature,
        )
        .map_err(|error| error.to_string())?;

    let mut graphics_signature = module.make_signature();
    graphics_signature.params.push(AbiParam::new(pointer_type));
    graphics_signature.params.push(AbiParam::new(types::I32));
    graphics_signature.params.push(AbiParam::new(types::F64));
    graphics_signature.params.push(AbiParam::new(types::F64));
    graphics_signature.returns.push(AbiParam::new(types::I32));
    let graphics = module
        .declare_function(
            "ricochet_basic_jit_procedure_graphics",
            Linkage::Import,
            &graphics_signature,
        )
        .map_err(|error| error.to_string())?;

    let mut integer_signature = module.make_signature();
    integer_signature.params.push(AbiParam::new(types::F64));
    integer_signature.returns.push(AbiParam::new(types::F64));
    let integer = module
        .declare_function(
            "ricochet_basic_jit_procedure_integer",
            Linkage::Import,
            &integer_signature,
        )
        .map_err(|error| error.to_string())?;

    Ok(NumericProcedureHelpers {
        enter,
        tick,
        context_ok,
        get_variable,
        set_variable,
        graphics,
        integer,
    })
}

fn find_numeric_procedure_regions(program: &ParsedProgram) -> Vec<NumericProcedureRegion> {
    let mut definitions = program.procedures.iter().collect::<Vec<_>>();
    definitions.sort_by(|(left, _), (right, _)| left.cmp(right));
    definitions
        .into_iter()
        .filter_map(|(name, definition)| {
            if definition.entry == 0
                || definition.parameters.is_empty()
                || definition.parameters.len() > MAX_NATIVE_PROCEDURE_PARAMETERS
                || definition
                    .parameters
                    .iter()
                    .any(|parameter| parameter.ends_with('$'))
            {
                return None;
            }
            let end = program.instructions[definition.entry..]
                .iter()
                .position(|instruction| matches!(instruction.statement, Statement::EndProcedure))
                .map(|offset| definition.entry + offset)?;
            let after = end + 1;
            if !has_no_branch_into_region(program, definition.entry, after) {
                return None;
            }

            let body = program.instructions[definition.entry..after].to_vec();
            let countdown_parameter = definition.parameters.first()?;
            let base_guard = body.iter().position(|statement| {
                matches!(
                    &statement.statement,
                    Statement::If(condition, then_body, else_body)
                        if expression_is_zero_comparison(condition, countdown_parameter)
                            && matches!(then_body.as_slice(), [Statement::EndProcedure])
                            && else_body.is_empty()
                )
            })?;
            if body.iter().any(|instruction| {
                statement_assigns_variable(&instruction.statement, countdown_parameter)
            }) {
                return None;
            }
            let mut recursive_calls = 0;
            if !body.iter().all(|instruction| {
                is_supported_numeric_procedure_statement(
                    &instruction.statement,
                    name,
                    &definition.parameters,
                )
            }) || !body.iter().all(|statement| {
                recursive_calls_use_countdown(
                    &statement.statement,
                    name,
                    countdown_parameter,
                    &mut recursive_calls,
                )
            }) || recursive_calls == 0
            {
                return None;
            }
            if body[..base_guard]
                .iter()
                .any(|instruction| statement_contains_self_call(&instruction.statement, name))
            {
                return None;
            }

            let body_statements = body
                .iter()
                .map(|instruction| instruction.statement.clone())
                .collect::<Vec<_>>();
            let calls_per_path = recursive_calls_per_path(&body_statements, name);
            if calls_per_path == 0 {
                return None;
            }
            let mut variables = BTreeSet::new();
            for instruction in &body {
                collect_procedure_variables(
                    &instruction.statement,
                    &definition.parameters,
                    &mut variables,
                );
            }
            let line = program.instructions[definition.entry - 1].line_number;
            Some(NumericProcedureRegion {
                name: name.clone(),
                entry: definition.entry,
                after,
                line,
                parameters: definition.parameters.clone(),
                variable_names: variables.into_iter().collect(),
                body,
                recursive_calls_per_path: calls_per_path,
            })
        })
        .collect()
}

fn is_supported_numeric_procedure_statement(
    statement: &Statement,
    procedure_name: &str,
    parameters: &[String],
) -> bool {
    match statement {
        Statement::NoOp | Statement::EndProcedure => true,
        Statement::Assign(LValue::Variable(name), expression)
            if is_supported_numeric_variable(name)
                && is_supported_numeric_expression(expression) =>
        {
            true
        }
        Statement::Gcol(action, colour) => {
            is_supported_numeric_expression(action) && is_supported_numeric_expression(colour)
        }
        Statement::Move(x, y) | Statement::Draw(x, y) => {
            is_supported_numeric_expression(x) && is_supported_numeric_expression(y)
        }
        Statement::ProcedureCall(name, arguments)
            if name.eq_ignore_ascii_case(procedure_name) && arguments.len() == parameters.len() =>
        {
            arguments.iter().all(is_supported_numeric_expression)
        }
        Statement::If(condition, then_body, else_body) => {
            is_supported_numeric_expression(condition)
                && then_body.iter().all(|statement| {
                    is_supported_numeric_procedure_statement(statement, procedure_name, parameters)
                })
                && else_body.iter().all(|statement| {
                    is_supported_numeric_procedure_statement(statement, procedure_name, parameters)
                })
        }
        _ => false,
    }
}

fn recursive_calls_use_countdown(
    statement: &Statement,
    procedure_name: &str,
    countdown_parameter: &str,
    found: &mut usize,
) -> bool {
    match statement {
        Statement::ProcedureCall(name, arguments) if name.eq_ignore_ascii_case(procedure_name) => {
            let Some(first_argument) = arguments.first() else {
                return false;
            };
            let expected = Expr::Binary(
                Box::new(Expr::Variable(countdown_parameter.to_owned())),
                BinaryOp::Subtract,
                Box::new(Expr::Number(1.0)),
            );
            if !expression_is(first_argument, &expected) {
                return false;
            }
            *found += 1;
            true
        }
        Statement::If(_, then_body, else_body) => then_body.iter().chain(else_body).all(|nested| {
            recursive_calls_use_countdown(nested, procedure_name, countdown_parameter, found)
        }),
        _ => true,
    }
}

fn expression_is_zero_comparison(expression: &Expr, variable: &str) -> bool {
    matches!(
        expression,
        Expr::Binary(left, BinaryOp::Equal, right)
            if (variable_expr_is(left, variable) && expression_is(right, &Expr::Number(0.0)))
                || (variable_expr_is(right, variable) && expression_is(left, &Expr::Number(0.0)))
    )
}

fn statement_assigns_variable(statement: &Statement, variable: &str) -> bool {
    match statement {
        Statement::Assign(LValue::Variable(name), _) => variable_is(name, variable),
        Statement::If(_, then_body, else_body) => then_body
            .iter()
            .chain(else_body)
            .any(|nested| statement_assigns_variable(nested, variable)),
        _ => false,
    }
}

fn statement_contains_self_call(statement: &Statement, name: &str) -> bool {
    match statement {
        Statement::ProcedureCall(actual, _) => actual.eq_ignore_ascii_case(name),
        Statement::If(_, then_body, else_body) => then_body
            .iter()
            .chain(else_body)
            .any(|nested| statement_contains_self_call(nested, name)),
        _ => false,
    }
}

fn recursive_calls_per_path(statements: &[Statement], name: &str) -> u64 {
    statements
        .iter()
        .map(|statement| recursive_call_path_budget(statement, name))
        .fold(0_u64, u64::saturating_add)
}

fn recursive_call_path_budget(statement: &Statement, name: &str) -> u64 {
    match statement {
        Statement::ProcedureCall(actual, _) if actual.eq_ignore_ascii_case(name) => 1,
        Statement::If(_, then_body, else_body) => {
            recursive_calls_per_path(then_body, name).max(recursive_calls_per_path(else_body, name))
        }
        _ => 0,
    }
}

fn estimated_recursive_call_count(calls_per_path: u64, depth: u64) -> Option<u64> {
    let mut total = 1_u64;
    let mut descendants = 1_u64;
    for _ in 0..depth {
        descendants = descendants.checked_mul(calls_per_path)?;
        total = total.checked_add(descendants)?;
        if total > runtime::MAX_NATIVE_PROCEDURE_CALLS {
            return None;
        }
    }
    Some(total)
}

fn collect_procedure_variables(
    statement: &Statement,
    parameters: &[String],
    variables: &mut BTreeSet<String>,
) {
    let mut referenced = Vec::new();
    match statement {
        Statement::Assign(LValue::Variable(name), expression) => {
            referenced.push(name.clone());
            collect_numeric_variables(expression, &mut referenced);
        }
        Statement::Gcol(action, colour) => {
            collect_numeric_variables(action, &mut referenced);
            collect_numeric_variables(colour, &mut referenced);
        }
        Statement::Move(x, y) | Statement::Draw(x, y) => {
            collect_numeric_variables(x, &mut referenced);
            collect_numeric_variables(y, &mut referenced);
        }
        Statement::ProcedureCall(_, arguments) => {
            for argument in arguments {
                collect_numeric_variables(argument, &mut referenced);
            }
        }
        Statement::If(condition, then_body, else_body) => {
            collect_numeric_variables(condition, &mut referenced);
            for nested in then_body.iter().chain(else_body) {
                collect_procedure_variables(nested, parameters, variables);
            }
        }
        _ => {}
    }
    for name in referenced {
        if !name.ends_with('$') && !parameters.iter().any(|parameter| parameter == &name) {
            variables.insert(name);
        }
    }
}

fn find_numeric_statement_regions(
    program: &ParsedProgram,
    native_procedures: &[NumericProcedureRegion],
) -> Vec<NumericStatementRegion> {
    program
        .instructions
        .iter()
        .enumerate()
        .filter_map(|(start, instruction)| {
            if native_procedures
                .iter()
                .any(|procedure| (procedure.entry..procedure.after).contains(&start))
            {
                return None;
            }
            if let Statement::ProcedureCall(name, _) = &instruction.statement
                && native_procedures
                    .iter()
                    .any(|procedure| procedure.name.eq_ignore_ascii_case(name))
            {
                return None;
            }
            let expressions = numeric_statement_expressions(&instruction.statement)?;
            if expressions.is_empty()
                || !expressions.iter().all(is_supported_numeric_expression)
                || !expressions.iter().any(expression_contains_native_operation)
            {
                return None;
            }

            let mut variables = Vec::new();
            for expression in &expressions {
                collect_numeric_variables(expression, &mut variables);
            }
            variables.sort();
            variables.dedup();
            Some(NumericStatementRegion {
                start,
                line: instruction.line_number,
                variables,
                expressions,
            })
        })
        .collect()
}

fn numeric_statement_expressions(statement: &Statement) -> Option<Vec<Expr>> {
    match statement {
        Statement::Assign(LValue::Variable(name), expression)
            if !name.ends_with('$') && !name.eq_ignore_ascii_case("TIME") =>
        {
            Some(vec![expression.clone()])
        }
        Statement::ProcedureCall(_, arguments) if !arguments.is_empty() => Some(arguments.clone()),
        Statement::Line(x1, y1, x2, y2) => {
            Some(vec![x1.clone(), y1.clone(), x2.clone(), y2.clone()])
        }
        Statement::Move(x, y) | Statement::Draw(x, y) => Some(vec![x.clone(), y.clone()]),
        Statement::Plot(code, x, y) => Some(vec![code.clone(), x.clone(), y.clone()]),
        Statement::Gcol(action, colour) => Some(vec![action.clone(), colour.clone()]),
        Statement::If(condition, _, _) | Statement::IfBlock(condition) => {
            Some(vec![condition.clone()])
        }
        _ => None,
    }
}

fn is_supported_numeric_expression(expression: &Expr) -> bool {
    match expression {
        Expr::Number(_) => true,
        Expr::Variable(name) => is_supported_numeric_variable(name),
        Expr::Unary(UnaryOp::Plus | UnaryOp::Minus, operand) => {
            is_supported_numeric_expression(operand)
        }
        Expr::Binary(left, operator, right)
            if matches!(
                operator,
                BinaryOp::Add
                    | BinaryOp::Subtract
                    | BinaryOp::Multiply
                    | BinaryOp::Power
                    | BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual
            ) =>
        {
            is_supported_numeric_expression(left) && is_supported_numeric_expression(right)
        }
        Expr::Builtin(token, arguments)
            if matches!(token, 0x94 | 0x9B | 0xA8 | 0xAA | 0xAB | 0xB5 | 0xB6 | 0xB7)
                && arguments.len() == 1 =>
        {
            is_supported_numeric_expression(&arguments[0])
        }
        _ => false,
    }
}

fn is_supported_numeric_variable(name: &str) -> bool {
    !name.ends_with('$')
        && !["TIME", "PAGE", "PTR", "LOMEM", "HIMEM"]
            .iter()
            .any(|pseudo| name.eq_ignore_ascii_case(pseudo))
}

fn expression_contains_native_operation(expression: &Expr) -> bool {
    match expression {
        Expr::Unary(UnaryOp::Plus | UnaryOp::Minus, operand) => {
            expression_contains_native_operation(operand)
        }
        Expr::Binary(_, _, _) | Expr::Builtin(_, _) => true,
        _ => false,
    }
}

fn collect_numeric_variables(expression: &Expr, variables: &mut Vec<String>) {
    match expression {
        Expr::Variable(name) => variables.push(name.clone()),
        Expr::Unary(_, operand) => collect_numeric_variables(operand, variables),
        Expr::Binary(left, _, right) => {
            collect_numeric_variables(left, variables);
            collect_numeric_variables(right, variables);
        }
        Expr::Builtin(_, arguments) => {
            for argument in arguments {
                collect_numeric_variables(argument, variables);
            }
        }
        _ => {}
    }
}

fn define_numeric_procedure_kernel<M: Module>(
    module: &mut M,
    region: &NumericProcedureRegion,
    numeric_helpers: &HashMap<u8, FuncId>,
    procedure_helpers: NumericProcedureHelpers,
    index: usize,
) -> Result<FuncId, String> {
    let pointer_type = module.target_config().pointer_type();
    let frontend_config = module.target_config();
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(pointer_type));
    signature
        .params
        .extend((0..MAX_NATIVE_PROCEDURE_PARAMETERS).map(|_| AbiParam::new(types::F64)));
    signature.returns.push(AbiParam::new(types::I32));
    let function = module
        .declare_function(
            &format!("basic_numeric_procedure_{index}"),
            Linkage::Local,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, function.as_u32());
    let function_ref = module.declare_func_in_func(function, &mut context.func);
    let enter_ref = module.declare_func_in_func(procedure_helpers.enter, &mut context.func);
    let tick_ref = module.declare_func_in_func(procedure_helpers.tick, &mut context.func);
    let context_ok_ref =
        module.declare_func_in_func(procedure_helpers.context_ok, &mut context.func);
    let get_variable_ref =
        module.declare_func_in_func(procedure_helpers.get_variable, &mut context.func);
    let set_variable_ref =
        module.declare_func_in_func(procedure_helpers.set_variable, &mut context.func);
    let graphics_ref = module.declare_func_in_func(procedure_helpers.graphics, &mut context.func);
    let integer_ref = module.declare_func_in_func(procedure_helpers.integer, &mut context.func);
    let mut builder_context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let entry = builder.create_block();
    let success_return = builder.create_block();
    let failure_return = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let arguments = builder.block_params(entry).to_vec();
    let context_pointer = arguments[0];
    let local_variables = region
        .parameters
        .iter()
        .map(|_| builder.declare_var(types::F64))
        .collect::<Vec<_>>();
    let parameter_variables = region
        .parameters
        .iter()
        .zip(&local_variables)
        .map(|(name, variable)| (name.as_str(), *variable))
        .collect::<HashMap<_, _>>();
    let global_variables = region
        .variable_names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index as i32))
        .collect::<HashMap<_, _>>();

    let enter_call = builder.ins().call(enter_ref, &[context_pointer]);
    let enter_status = builder.inst_results(enter_call)[0];
    guard_native_status(&mut builder, enter_status, failure_return);
    for (index, (name, variable)) in region.parameters.iter().zip(&local_variables).enumerate() {
        let mut value = arguments[index + 1];
        if name.ends_with('%') {
            let call = builder.ins().call(integer_ref, &[value]);
            value = builder.inst_results(call)[0];
        }
        builder.def_var(*variable, value);
    }

    let terminated = emit_numeric_procedure_statements(
        module,
        &mut builder,
        &region.body,
        true,
        &region.name,
        context_pointer,
        &parameter_variables,
        &global_variables,
        numeric_helpers,
        function_ref,
        tick_ref,
        context_ok_ref,
        get_variable_ref,
        set_variable_ref,
        graphics_ref,
        integer_ref,
        success_return,
        failure_return,
    )?;
    if !terminated {
        builder.ins().jump(success_return, &[]);
    }

    builder.switch_to_block(success_return);
    let success = builder.ins().iconst(types::I32, 1);
    builder.ins().return_(&[success]);
    builder.switch_to_block(failure_return);
    let failure = builder.ins().iconst(types::I32, 0);
    builder.ins().return_(&[failure]);
    builder.seal_all_blocks();
    builder.finalize(frontend_config);
    module
        .define_function(function, &mut context)
        .map_err(|error| error.to_string())?;
    module.clear_context(&mut context);
    Ok(function)
}

#[allow(clippy::too_many_arguments)]
fn emit_numeric_procedure_statements<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder,
    statements: &[parser::LocatedStatement],
    tick_statements: bool,
    procedure_name: &str,
    context_pointer: IrValue,
    parameter_variables: &HashMap<&str, Variable>,
    global_variables: &HashMap<&str, i32>,
    numeric_helpers: &HashMap<u8, FuncId>,
    function_ref: cranelift_codegen::ir::FuncRef,
    tick_ref: cranelift_codegen::ir::FuncRef,
    context_ok_ref: cranelift_codegen::ir::FuncRef,
    get_variable_ref: cranelift_codegen::ir::FuncRef,
    set_variable_ref: cranelift_codegen::ir::FuncRef,
    graphics_ref: cranelift_codegen::ir::FuncRef,
    integer_ref: cranelift_codegen::ir::FuncRef,
    success_return: cranelift_codegen::ir::Block,
    failure_return: cranelift_codegen::ir::Block,
) -> Result<bool, String> {
    for instruction in statements {
        if tick_statements {
            let line = builder
                .ins()
                .iconst(types::I32, i64::from(instruction.line_number));
            let call = builder.ins().call(tick_ref, &[context_pointer, line]);
            let status = builder.inst_results(call)[0];
            guard_native_status(builder, status, failure_return);
        }

        let terminated = match &instruction.statement {
            Statement::NoOp => false,
            Statement::EndProcedure => {
                builder.ins().jump(success_return, &[]);
                true
            }
            Statement::Assign(LValue::Variable(name), expression) => {
                let mut value = emit_numeric_procedure_expression(
                    module,
                    builder,
                    expression,
                    context_pointer,
                    parameter_variables,
                    global_variables,
                    numeric_helpers,
                    get_variable_ref,
                )?;
                guard_context_ok(builder, context_ok_ref, context_pointer, failure_return);
                if let Some(variable) = parameter_variables.get(name.as_str()) {
                    if name.ends_with('%') {
                        let call = builder.ins().call(integer_ref, &[value]);
                        value = builder.inst_results(call)[0];
                    }
                    builder.def_var(*variable, value);
                } else {
                    let index = global_variables
                        .get(name.as_str())
                        .copied()
                        .ok_or_else(|| format!("missing compiled variable slot for {name}"))?;
                    let index = builder.ins().iconst(types::I32, i64::from(index));
                    let call = builder
                        .ins()
                        .call(set_variable_ref, &[context_pointer, index, value]);
                    let status = builder.inst_results(call)[0];
                    guard_native_status(builder, status, failure_return);
                }
                false
            }
            Statement::Gcol(action, colour) => {
                let action = emit_numeric_procedure_expression(
                    module,
                    builder,
                    action,
                    context_pointer,
                    parameter_variables,
                    global_variables,
                    numeric_helpers,
                    get_variable_ref,
                )?;
                let colour = emit_numeric_procedure_expression(
                    module,
                    builder,
                    colour,
                    context_pointer,
                    parameter_variables,
                    global_variables,
                    numeric_helpers,
                    get_variable_ref,
                )?;
                guard_context_ok(builder, context_ok_ref, context_pointer, failure_return);
                let operation = builder.ins().iconst(types::I32, 0);
                let call = builder
                    .ins()
                    .call(graphics_ref, &[context_pointer, operation, action, colour]);
                let status = builder.inst_results(call)[0];
                guard_native_status(builder, status, failure_return);
                false
            }
            Statement::Move(x, y) | Statement::Draw(x, y) => {
                let x = emit_numeric_procedure_expression(
                    module,
                    builder,
                    x,
                    context_pointer,
                    parameter_variables,
                    global_variables,
                    numeric_helpers,
                    get_variable_ref,
                )?;
                let y = emit_numeric_procedure_expression(
                    module,
                    builder,
                    y,
                    context_pointer,
                    parameter_variables,
                    global_variables,
                    numeric_helpers,
                    get_variable_ref,
                )?;
                guard_context_ok(builder, context_ok_ref, context_pointer, failure_return);
                let operation = builder.ins().iconst(
                    types::I32,
                    if matches!(&instruction.statement, Statement::Move(_, _)) {
                        1
                    } else {
                        2
                    },
                );
                let call = builder
                    .ins()
                    .call(graphics_ref, &[context_pointer, operation, x, y]);
                let status = builder.inst_results(call)[0];
                guard_native_status(builder, status, failure_return);
                false
            }
            Statement::ProcedureCall(name, arguments) => {
                if !name.eq_ignore_ascii_case(procedure_name) {
                    return Err("native procedure contains a non-recursive call".into());
                }
                let mut values = Vec::with_capacity(arguments.len());
                for expression in arguments {
                    values.push(emit_numeric_procedure_expression(
                        module,
                        builder,
                        expression,
                        context_pointer,
                        parameter_variables,
                        global_variables,
                        numeric_helpers,
                        get_variable_ref,
                    )?);
                }
                guard_context_ok(builder, context_ok_ref, context_pointer, failure_return);
                let mut call_arguments = Vec::with_capacity(MAX_NATIVE_PROCEDURE_PARAMETERS + 1);
                call_arguments.push(context_pointer);
                call_arguments.extend(values);
                while call_arguments.len() < MAX_NATIVE_PROCEDURE_PARAMETERS + 1 {
                    call_arguments.push(builder.ins().f64const(0.0));
                }
                let call = builder.ins().call(function_ref, &call_arguments);
                let status = builder.inst_results(call)[0];
                guard_native_status(builder, status, failure_return);
                false
            }
            Statement::If(condition, then_body, else_body) => {
                let condition = emit_numeric_procedure_expression(
                    module,
                    builder,
                    condition,
                    context_pointer,
                    parameter_variables,
                    global_variables,
                    numeric_helpers,
                    get_variable_ref,
                )?;
                guard_context_ok(builder, context_ok_ref, context_pointer, failure_return);
                let zero = builder.ins().f64const(0.0);
                let condition = builder.ins().fcmp(FloatCC::NotEqual, condition, zero);
                let then_block = builder.create_block();
                let else_block = builder.create_block();
                let merge_block = builder.create_block();
                builder
                    .ins()
                    .brif(condition, then_block, &[], else_block, &[]);

                builder.switch_to_block(then_block);
                let then_terminated = emit_numeric_procedure_nested_statements(
                    module,
                    builder,
                    then_body,
                    procedure_name,
                    context_pointer,
                    parameter_variables,
                    global_variables,
                    numeric_helpers,
                    function_ref,
                    tick_ref,
                    context_ok_ref,
                    get_variable_ref,
                    set_variable_ref,
                    graphics_ref,
                    integer_ref,
                    success_return,
                    failure_return,
                )?;
                if !then_terminated {
                    builder.ins().jump(merge_block, &[]);
                }
                builder.seal_block(then_block);

                builder.switch_to_block(else_block);
                let else_terminated = emit_numeric_procedure_nested_statements(
                    module,
                    builder,
                    else_body,
                    procedure_name,
                    context_pointer,
                    parameter_variables,
                    global_variables,
                    numeric_helpers,
                    function_ref,
                    tick_ref,
                    context_ok_ref,
                    get_variable_ref,
                    set_variable_ref,
                    graphics_ref,
                    integer_ref,
                    success_return,
                    failure_return,
                )?;
                if !else_terminated {
                    builder.ins().jump(merge_block, &[]);
                }
                builder.seal_block(else_block);
                builder.seal_block(merge_block);
                if then_terminated && else_terminated {
                    true
                } else {
                    builder.switch_to_block(merge_block);
                    false
                }
            }
            _ => return Err("unsupported statement in native numeric procedure".into()),
        };
        if terminated {
            return Ok(true);
        }
    }
    Ok(false)
}

#[allow(clippy::too_many_arguments)]
fn emit_numeric_procedure_nested_statements<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder,
    statements: &[Statement],
    procedure_name: &str,
    context_pointer: IrValue,
    parameter_variables: &HashMap<&str, Variable>,
    global_variables: &HashMap<&str, i32>,
    numeric_helpers: &HashMap<u8, FuncId>,
    function_ref: cranelift_codegen::ir::FuncRef,
    tick_ref: cranelift_codegen::ir::FuncRef,
    context_ok_ref: cranelift_codegen::ir::FuncRef,
    get_variable_ref: cranelift_codegen::ir::FuncRef,
    set_variable_ref: cranelift_codegen::ir::FuncRef,
    graphics_ref: cranelift_codegen::ir::FuncRef,
    integer_ref: cranelift_codegen::ir::FuncRef,
    success_return: cranelift_codegen::ir::Block,
    failure_return: cranelift_codegen::ir::Block,
) -> Result<bool, String> {
    let located = statements
        .iter()
        .cloned()
        .map(|statement| parser::LocatedStatement {
            line_number: 0,
            statement,
        })
        .collect::<Vec<_>>();
    emit_numeric_procedure_statements(
        module,
        builder,
        &located,
        false,
        procedure_name,
        context_pointer,
        parameter_variables,
        global_variables,
        numeric_helpers,
        function_ref,
        tick_ref,
        context_ok_ref,
        get_variable_ref,
        set_variable_ref,
        graphics_ref,
        integer_ref,
        success_return,
        failure_return,
    )
}

fn guard_native_status(
    builder: &mut FunctionBuilder,
    status: IrValue,
    failure_return: cranelift_codegen::ir::Block,
) {
    let success = builder.create_block();
    let zero = builder.ins().iconst(types::I32, 0);
    let succeeded = builder.ins().icmp(IntCC::NotEqual, status, zero);
    builder
        .ins()
        .brif(succeeded, success, &[], failure_return, &[]);
    builder.switch_to_block(success);
}

fn guard_context_ok(
    builder: &mut FunctionBuilder,
    context_ok_ref: cranelift_codegen::ir::FuncRef,
    context_pointer: IrValue,
    failure_return: cranelift_codegen::ir::Block,
) {
    let call = builder.ins().call(context_ok_ref, &[context_pointer]);
    guard_native_status(builder, builder.inst_results(call)[0], failure_return);
}

#[allow(clippy::too_many_arguments)]
fn emit_numeric_procedure_expression<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder,
    expression: &Expr,
    context_pointer: IrValue,
    parameter_variables: &HashMap<&str, Variable>,
    global_variables: &HashMap<&str, i32>,
    numeric_helpers: &HashMap<u8, FuncId>,
    get_variable_ref: cranelift_codegen::ir::FuncRef,
) -> Result<IrValue, String> {
    match expression {
        Expr::Number(value) => Ok(builder.ins().f64const(*value)),
        Expr::Variable(name) => {
            if let Some(variable) = parameter_variables.get(name.as_str()) {
                Ok(builder.use_var(*variable))
            } else {
                let index = global_variables
                    .get(name.as_str())
                    .copied()
                    .ok_or_else(|| format!("missing compiled variable slot for {name}"))?;
                let index = builder.ins().iconst(types::I32, i64::from(index));
                let call = builder
                    .ins()
                    .call(get_variable_ref, &[context_pointer, index]);
                Ok(builder.inst_results(call)[0])
            }
        }
        Expr::Unary(UnaryOp::Plus, operand) => emit_numeric_procedure_expression(
            module,
            builder,
            operand,
            context_pointer,
            parameter_variables,
            global_variables,
            numeric_helpers,
            get_variable_ref,
        ),
        Expr::Unary(UnaryOp::Minus, operand) => {
            let value = emit_numeric_procedure_expression(
                module,
                builder,
                operand,
                context_pointer,
                parameter_variables,
                global_variables,
                numeric_helpers,
                get_variable_ref,
            )?;
            Ok(builder.ins().fneg(value))
        }
        Expr::Binary(left, operator, right) => {
            let left = emit_numeric_procedure_expression(
                module,
                builder,
                left,
                context_pointer,
                parameter_variables,
                global_variables,
                numeric_helpers,
                get_variable_ref,
            )?;
            let right = emit_numeric_procedure_expression(
                module,
                builder,
                right,
                context_pointer,
                parameter_variables,
                global_variables,
                numeric_helpers,
                get_variable_ref,
            )?;
            match operator {
                BinaryOp::Add => Ok(builder.ins().fadd(left, right)),
                BinaryOp::Subtract => Ok(builder.ins().fsub(left, right)),
                BinaryOp::Multiply => Ok(builder.ins().fmul(left, right)),
                BinaryOp::Power => call_numeric_helper(
                    module,
                    builder,
                    NUMERIC_POWER,
                    &[left, right],
                    numeric_helpers,
                ),
                comparison => emit_numeric_comparison(builder, *comparison, left, right),
            }
        }
        Expr::Builtin(token, arguments) if arguments.len() == 1 => {
            let argument = emit_numeric_procedure_expression(
                module,
                builder,
                &arguments[0],
                context_pointer,
                parameter_variables,
                global_variables,
                numeric_helpers,
                get_variable_ref,
            )?;
            call_numeric_helper(module, builder, *token, &[argument], numeric_helpers)
        }
        _ => Err("unsupported expression in native numeric procedure".into()),
    }
}

fn emit_numeric_comparison(
    builder: &mut FunctionBuilder,
    operator: BinaryOp,
    left: IrValue,
    right: IrValue,
) -> Result<IrValue, String> {
    let condition = match operator {
        BinaryOp::Equal => FloatCC::Equal,
        BinaryOp::NotEqual => FloatCC::NotEqual,
        BinaryOp::Less => FloatCC::LessThan,
        BinaryOp::LessEqual => FloatCC::LessThanOrEqual,
        BinaryOp::Greater => FloatCC::GreaterThan,
        BinaryOp::GreaterEqual => FloatCC::GreaterThanOrEqual,
        _ => return Err("unsupported comparison in native numeric expression".into()),
    };
    let comparison = builder.ins().fcmp(condition, left, right);
    let truth = builder.ins().f64const(-1.0);
    let falsity = builder.ins().f64const(0.0);
    Ok(builder.ins().select(comparison, truth, falsity))
}

fn define_numeric_statement_kernel<M: Module>(
    module: &mut M,
    region: &NumericStatementRegion,
    numeric_helpers: &HashMap<u8, FuncId>,
    index: usize,
) -> Result<FuncId, String> {
    let pointer_type = module.target_config().pointer_type();
    let frontend_config = module.target_config();
    let mut signature = module.make_signature();
    signature.params.push(AbiParam::new(pointer_type));
    signature.params.push(AbiParam::new(pointer_type));
    let function = module
        .declare_function(
            &format!("basic_numeric_statement_{index}"),
            Linkage::Local,
            &signature,
        )
        .map_err(|error| error.to_string())?;
    let mut context = module.make_context();
    context.func.signature = signature;
    context.func.name = UserFuncName::user(0, function.as_u32());
    let mut builder_context = FunctionBuilderContext::new();
    let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    let input_pointer = builder.block_params(entry)[0];
    let output_pointer = builder.block_params(entry)[1];
    let variables = region
        .variables
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect::<HashMap<_, _>>();

    for (index, expression) in region.expressions.iter().enumerate() {
        let value = emit_numeric_expression(
            module,
            &mut builder,
            expression,
            input_pointer,
            pointer_type,
            &variables,
            numeric_helpers,
        )?;
        let offset = builder.ins().iconst(pointer_type, (index * 8) as i64);
        let destination = builder.ins().iadd(output_pointer, offset);
        builder
            .ins()
            .store(MemFlagsData::trusted(), value, destination, 0);
    }
    builder.ins().return_(&[]);
    builder.seal_all_blocks();
    builder.finalize(frontend_config);
    module
        .define_function(function, &mut context)
        .map_err(|error| error.to_string())?;
    module.clear_context(&mut context);
    Ok(function)
}

fn emit_numeric_expression<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder,
    expression: &Expr,
    input_pointer: cranelift_codegen::ir::Value,
    pointer_type: cranelift_codegen::ir::Type,
    variables: &HashMap<&str, usize>,
    numeric_helpers: &HashMap<u8, FuncId>,
) -> Result<cranelift_codegen::ir::Value, String> {
    match expression {
        Expr::Number(value) => Ok(builder.ins().f64const(*value)),
        Expr::Variable(name) => {
            let index = variables
                .get(name.as_str())
                .copied()
                .ok_or_else(|| format!("missing native input for variable {name}"))?;
            let offset = builder.ins().iconst(pointer_type, (index * 8) as i64);
            let address = builder.ins().iadd(input_pointer, offset);
            Ok(builder
                .ins()
                .load(types::F64, MemFlagsData::trusted(), address, 0))
        }
        Expr::Unary(UnaryOp::Plus, operand) => emit_numeric_expression(
            module,
            builder,
            operand,
            input_pointer,
            pointer_type,
            variables,
            numeric_helpers,
        ),
        Expr::Unary(UnaryOp::Minus, operand) => {
            let value = emit_numeric_expression(
                module,
                builder,
                operand,
                input_pointer,
                pointer_type,
                variables,
                numeric_helpers,
            )?;
            Ok(builder.ins().fneg(value))
        }
        Expr::Binary(left, operator, right)
            if matches!(
                operator,
                BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply
            ) =>
        {
            let left = emit_numeric_expression(
                module,
                builder,
                left,
                input_pointer,
                pointer_type,
                variables,
                numeric_helpers,
            )?;
            let right = emit_numeric_expression(
                module,
                builder,
                right,
                input_pointer,
                pointer_type,
                variables,
                numeric_helpers,
            )?;
            Ok(match operator {
                BinaryOp::Add => builder.ins().fadd(left, right),
                BinaryOp::Subtract => builder.ins().fsub(left, right),
                BinaryOp::Multiply => builder.ins().fmul(left, right),
                _ => unreachable!(),
            })
        }
        Expr::Binary(left, BinaryOp::Power, right) => {
            let left = emit_numeric_expression(
                module,
                builder,
                left,
                input_pointer,
                pointer_type,
                variables,
                numeric_helpers,
            )?;
            let right = emit_numeric_expression(
                module,
                builder,
                right,
                input_pointer,
                pointer_type,
                variables,
                numeric_helpers,
            )?;
            call_numeric_helper(
                module,
                builder,
                NUMERIC_POWER,
                &[left, right],
                numeric_helpers,
            )
        }
        Expr::Binary(left, operator, right)
            if matches!(
                operator,
                BinaryOp::Equal
                    | BinaryOp::NotEqual
                    | BinaryOp::Less
                    | BinaryOp::LessEqual
                    | BinaryOp::Greater
                    | BinaryOp::GreaterEqual
            ) =>
        {
            let left = emit_numeric_expression(
                module,
                builder,
                left,
                input_pointer,
                pointer_type,
                variables,
                numeric_helpers,
            )?;
            let right = emit_numeric_expression(
                module,
                builder,
                right,
                input_pointer,
                pointer_type,
                variables,
                numeric_helpers,
            )?;
            emit_numeric_comparison(builder, *operator, left, right)
        }
        Expr::Builtin(token, arguments) if arguments.len() == 1 => {
            let argument = emit_numeric_expression(
                module,
                builder,
                &arguments[0],
                input_pointer,
                pointer_type,
                variables,
                numeric_helpers,
            )?;
            call_numeric_helper(module, builder, *token, &[argument], numeric_helpers)
        }
        _ => Err("unsupported expression in native numeric statement".into()),
    }
}

fn call_numeric_helper<M: Module>(
    module: &mut M,
    builder: &mut FunctionBuilder,
    token: u8,
    arguments: &[cranelift_codegen::ir::Value],
    numeric_helpers: &HashMap<u8, FuncId>,
) -> Result<cranelift_codegen::ir::Value, String> {
    let function = numeric_helpers
        .get(&token)
        .ok_or_else(|| format!("missing native helper for BASIC token {token:#04x}"))?;
    let function = module.declare_func_in_func(*function, builder.func);
    let call = builder.ins().call(function, arguments);
    Ok(builder.inst_results(call)[0])
}

#[cfg(test)]
mod system_profile_boundary_tests {
    use super::*;

    #[test]
    fn hybrid_jit_rejects_new_typed_parameters_and_explains_interpreter_fallback() {
        let mut program = ParsedProgram::default();
        program
            .typed_parameters
            .insert("ENTRY".into(), vec![parser::SystemType::Byte]);
        let error = match JitProgram::compile(&program) {
            Ok(_) => panic!("System Profile typed definitions must not enter the old JIT"),
            Err(error) => error,
        };
        assert!(error.contains("typed definitions"));
        assert!(error.contains("interpreter-only"));

        let mut basic64 = ParsedProgram::default();
        basic64.options.mode = crate::configure::BasicLanguageMode::Basic64;
        basic64.instructions.push(parser::LocatedStatement {
            line_number: 10,
            statement: Statement::Assign(
                LValue::Variable("VALUE".into()),
                Expr::Integer(9_007_199_254_740_993),
            ),
        });
        let error = match JitProgram::compile(&basic64) {
            Ok(_) => panic!("BASIC64 integer operations must not be lowered as f64"),
            Err(error) => error,
        };
        assert!(error.contains("BASIC64 System Profile typed assignment"));
        assert!(error.contains("interpreter-only"));
    }
}
