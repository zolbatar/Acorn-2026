//! Whole-program native compiler for strict BASICJIT execution.
//!
//! Source and tokenized programs enter through `ParsedProgram`. Each BASIC
//! instruction is lowered to Cranelift control flow before execution. Numeric
//! expressions and loop arithmetic become native operations; generated code
//! calls the checked runtime only for dynamic strings, arrays, I/O, clocks,
//! guest memory, and SWI/MOS services.

use std::{
    collections::{BTreeSet, HashMap},
    ffi::c_void,
    time::Instant,
};

use cranelift_codegen::{
    ir::{
        AbiParam, InstBuilder, MemFlagsData, Signature, StackSlotData, StackSlotKind, UserFuncName,
        Value as IrValue, condcodes::FloatCC, types,
    },
    settings::{self, Configurable},
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};

use crate::{error::RuntimeError, memory::Task, swi::SwiDispatcher};

use super::{
    JitExecutionReport, StrictJitOptions,
    native_runtime::{self, NativeExecutionContext, NativeProgramLayout, NativeValue},
    parser::{BinaryOp, Expr, LValue, MemoryWidth, ParsedProgram, PrintItem, Statement, UnaryOp},
};

type NativeMain = extern "C" fn(*mut c_void, *mut f64) -> i32;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum UnitKey {
    Main,
    Procedure(String),
    Function(String),
    Subroutine(u16),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnitKind {
    Main,
    Procedure,
    Function { returns_string: bool },
    Subroutine,
}

#[derive(Clone, Debug)]
struct NativeRoutine {
    id: FuncId,
    signature: Signature,
    key: UnitKey,
    name: String,
    kind: UnitKind,
    start: usize,
    end: usize,
    parameters: Vec<String>,
    line: u16,
}

#[derive(Clone, Copy)]
struct LoopSlots {
    end: cranelift_codegen::ir::StackSlot,
    step: cranelift_codegen::ir::StackSlot,
}

#[derive(Clone, Copy)]
enum CompiledExpr {
    Number(IrValue),
    String(IrValue),
}

struct CompiledNativeProgram {
    _module: JITModule,
    entry: NativeMain,
    layout: NativeProgramLayout,
    units: Vec<String>,
    compile_time: std::time::Duration,
}

pub(super) fn run_parsed_program_with_options(
    parsed: ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    options: StrictJitOptions,
) -> Result<JitExecutionReport, RuntimeError> {
    let compiler = StrictCompiler::new(&parsed, options).map_err(RuntimeError::Program)?;
    let compiled = compiler.compile().map_err(RuntimeError::Program)?;
    let mut runtime = NativeExecutionContext::new(compiled.layout, task, dispatcher);
    let context = runtime.as_opaque();
    let numeric_slots = runtime.numeric_slots_ptr();
    let started = Instant::now();
    let status = (compiled.entry)(context, numeric_slots);
    let compiled_time = started.elapsed();
    let helper_calls = runtime.helper_calls();
    let compiled_calls = runtime.compiled_calls();
    if let Some(error) = runtime.take_error() {
        return Err(error);
    }
    if status == 0 {
        return Err(RuntimeError::Program(
            "strict native execution stopped without a runtime error".into(),
        ));
    }
    Ok(JitExecutionReport {
        compiled_units: compiled.units,
        compiled_calls,
        interpreted_statement_count: 0,
        interpreted_expression_count: 0,
        runtime_helper_calls: helper_calls,
        strict_native: true,
        compiled_time,
        compile_time: compiled.compile_time,
        fallback_reason: None,
        ..JitExecutionReport::default()
    })
}

struct StrictCompiler<'a> {
    program: &'a ParsedProgram,
    module: JITModule,
    pointer_type: cranelift_codegen::ir::Type,
    helpers: HashMap<&'static str, FuncId>,
    routines: HashMap<UnitKey, NativeRoutine>,
    routine_order: Vec<UnitKey>,
    numeric_names: Vec<String>,
    numeric_indices: HashMap<String, usize>,
    string_names: Vec<String>,
    string_indices: HashMap<String, usize>,
    array_names: Vec<String>,
    array_indices: HashMap<String, usize>,
    constant_strings: Vec<Vec<u8>>,
    constant_indices: HashMap<Vec<u8>, i32>,
    temporary_string_count: usize,
    for_pairs: HashMap<usize, usize>,
    repeat_pairs: HashMap<usize, usize>,
    if_pairs: HashMap<usize, usize>,
    data: Vec<(u16, NativeValue)>,
}

impl<'a> StrictCompiler<'a> {
    fn new(program: &'a ParsedProgram, options: StrictJitOptions) -> Result<Self, String> {
        super::system_ir::PortableSystemIr::native_boundary_for_program(
            program,
            super::system_ir::SystemIrBackend::StrictJit,
        )?;
        let isa_builder = cranelift_native::builder().map_err(|message| message.to_string())?;
        let mut flags_builder = settings::builder();
        flags_builder
            .set(
                "opt_level",
                if options.benchmark_validation {
                    "none"
                } else {
                    "speed"
                },
            )
            .map_err(|error| error.to_string())?;
        let isa = isa_builder
            .finish(settings::Flags::new(flags_builder))
            .map_err(|error| error.to_string())?;
        let mut builder = JITBuilder::with_isa(isa, default_libcall_names());
        register_symbols(&mut builder);
        let mut module = JITModule::new(builder);
        let pointer_type = module.target_config().pointer_type();
        let helpers = declare_helpers(&mut module, pointer_type)?;
        let (numeric_names, string_names, array_names) = collect_slots(program);
        let numeric_indices = index_names(&numeric_names);
        let string_indices = index_names(&string_names);
        let array_indices = index_names(&array_names);
        let constant_strings = collect_string_constants(program);
        let constant_indices = constant_strings
            .iter()
            .enumerate()
            .map(|(index, value)| {
                i32::try_from(string_names.len() + index)
                    .map(|slot| (value.clone(), slot))
                    .map_err(|_| "too many strict JIT string constants".to_string())
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let for_pairs = pair_for_loops(program)?;
        let repeat_pairs = pair_repeat_loops(program)?;
        let if_pairs = pair_if_blocks(program)?;
        let data = collect_data(program)?;
        let mut compiler = Self {
            program,
            module,
            pointer_type,
            helpers,
            routines: HashMap::new(),
            routine_order: Vec::new(),
            numeric_names,
            numeric_indices,
            string_names,
            string_indices,
            array_names,
            array_indices,
            constant_strings,
            constant_indices,
            temporary_string_count: 0,
            for_pairs,
            repeat_pairs,
            if_pairs,
            data,
        };
        compiler.declare_routines()?;
        Ok(compiler)
    }

    fn compile(mut self) -> Result<CompiledNativeProgram, String> {
        let compile_started = Instant::now();
        let order = self.routine_order.clone();
        for key in order {
            let routine = self.routines.get(&key).cloned().expect("declared routine");
            self.define_routine(&routine)?;
        }
        self.module
            .finalize_definitions()
            .map_err(|error| error.to_string())?;
        let main = self.routines.get(&UnitKey::Main).expect("main is declared");
        let entry = unsafe {
            std::mem::transmute::<*const u8, NativeMain>(
                self.module.get_finalized_function(main.id),
            )
        };
        let units = self
            .routine_order
            .iter()
            .filter_map(|key| {
                let routine = self.routines.get(key)?;
                match routine.kind {
                    UnitKind::Main => Some("whole BASIC program".to_string()),
                    UnitKind::Procedure => Some(format!("PROC {}", routine.name)),
                    UnitKind::Function { .. } => Some(format!("FN {}", routine.name)),
                    UnitKind::Subroutine => Some(format!("GOSUB {}", routine.name)),
                }
            })
            .collect();
        let layout = NativeProgramLayout {
            numeric_variables: self.numeric_names,
            string_variables: self.string_names,
            arrays: self.array_names,
            constant_strings: self.constant_strings,
            temporary_strings: self.temporary_string_count,
            data: self.data,
        };
        Ok(CompiledNativeProgram {
            _module: self.module,
            entry,
            layout,
            units,
            compile_time: compile_started.elapsed(),
        })
    }

    fn declare_routines(&mut self) -> Result<(), String> {
        let main = NativeRoutine {
            id: FuncId::from_u32(0),
            signature: self.module.make_signature(),
            key: UnitKey::Main,
            name: "main".into(),
            kind: UnitKind::Main,
            start: 0,
            end: self.program.instructions.len(),
            parameters: Vec::new(),
            line: self
                .program
                .instructions
                .first()
                .map_or(0, |item| item.line_number),
        };
        self.declare_routine(main)?;

        let mut definitions = Vec::new();
        for (name, definition) in &self.program.procedures {
            definitions.push((
                UnitKey::Procedure(name.clone()),
                name.clone(),
                UnitKind::Procedure,
                definition.entry,
                definition.parameters.clone(),
            ));
        }
        for (name, definition) in &self.program.functions {
            definitions.push((
                UnitKey::Function(name.clone()),
                name.clone(),
                UnitKind::Function {
                    returns_string: name.ends_with('$'),
                },
                definition.entry,
                definition.parameters.clone(),
            ));
        }
        definitions.sort_by(|left, right| left.1.cmp(&right.1));
        for (key, name, kind, start, parameters) in definitions {
            let end = self.definition_end(start, kind, &name)?;
            let line = self
                .program
                .instructions
                .get(start)
                .map_or(0, |item| item.line_number);
            self.declare_routine(NativeRoutine {
                id: FuncId::from_u32(0),
                signature: self.module.make_signature(),
                key,
                name,
                kind,
                start,
                end,
                parameters,
                line,
            })?;
        }

        let mut gosubs = BTreeSet::new();
        for item in &self.program.instructions {
            if let Statement::Gosub(line) = item.statement {
                gosubs.insert(line);
            }
        }
        for target in gosubs {
            let start = self
                .program
                .line_entries
                .get(&target)
                .copied()
                .ok_or_else(|| {
                    compile_error(0, format!("GOSUB target line {target} does not exist"))
                })?;
            let end = self
                .subroutine_end(start)
                .ok_or_else(|| compile_error(target, "GOSUB body has no RETURN"))?;
            self.declare_routine(NativeRoutine {
                id: FuncId::from_u32(0),
                signature: self.module.make_signature(),
                key: UnitKey::Subroutine(target),
                name: target.to_string(),
                kind: UnitKind::Subroutine,
                start,
                end,
                parameters: Vec::new(),
                line: target,
            })?;
        }
        Ok(())
    }

    fn declare_routine(&mut self, mut routine: NativeRoutine) -> Result<(), String> {
        let mut signature = self.module.make_signature();
        signature.params.push(AbiParam::new(self.pointer_type));
        signature.params.push(AbiParam::new(self.pointer_type));
        for parameter in &routine.parameters {
            signature
                .params
                .push(AbiParam::new(if parameter.ends_with('$') {
                    types::I32
                } else {
                    types::F64
                }));
        }
        match routine.kind {
            UnitKind::Function {
                returns_string: true,
            } => signature.returns.push(AbiParam::new(types::I32)),
            UnitKind::Function {
                returns_string: false,
            } => signature.returns.push(AbiParam::new(types::F64)),
            _ => signature.returns.push(AbiParam::new(types::I32)),
        }
        let symbol = match &routine.key {
            UnitKey::Main => "basic_strict_main".to_string(),
            UnitKey::Procedure(name) => format!("basic_strict_proc_{}", sanitize(name)),
            UnitKey::Function(name) => format!("basic_strict_fn_{}", sanitize(name)),
            UnitKey::Subroutine(line) => format!("basic_strict_gosub_{line}"),
        };
        routine.id = self
            .module
            .declare_function(&symbol, Linkage::Local, &signature)
            .map_err(|error| compile_error(routine.line, error.to_string()))?;
        routine.signature = signature;
        self.routine_order.push(routine.key.clone());
        self.routines.insert(routine.key.clone(), routine);
        Ok(())
    }

    fn definition_end(&self, start: usize, kind: UnitKind, name: &str) -> Result<usize, String> {
        match kind {
            UnitKind::Procedure => {
                for (offset, item) in self.program.instructions.iter().enumerate().skip(start) {
                    if contains_end_procedure(&item.statement) {
                        return Ok(offset + 1);
                    }
                }
                Err(compile_error(
                    self.program
                        .instructions
                        .get(start)
                        .map_or(0, |item| item.line_number),
                    format!("PROC {name} has no ENDPROC"),
                ))
            }
            UnitKind::Function { .. } => {
                let next_definition = self
                    .program
                    .instructions
                    .iter()
                    .enumerate()
                    .skip(start)
                    .find_map(|(index, item)| {
                        (index > start
                            && matches!(
                                item.statement,
                                Statement::DefineFunction(_, _) | Statement::DefineProcedure(_, _)
                            ))
                        .then_some(index)
                    });
                Ok(next_definition.unwrap_or(self.program.instructions.len()))
            }
            _ => Err(compile_error(0, "invalid routine definition kind")),
        }
    }

    fn subroutine_end(&self, start: usize) -> Option<usize> {
        self.program
            .instructions
            .iter()
            .enumerate()
            .skip(start)
            .find_map(|(index, item)| {
                matches!(item.statement, Statement::Return).then_some(index + 1)
            })
    }

    fn define_routine(&mut self, routine: &NativeRoutine) -> Result<(), String> {
        let mut context = self.module.make_context();
        context.func.signature = routine.signature.clone();
        context.func.name = UserFuncName::user(0, routine.id.as_u32());
        let frontend_config = self.module.target_config();
        let mut builder_context = FunctionBuilderContext::new();
        let mut builder = FunctionBuilder::new(&mut context.func, &mut builder_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        let params = builder.block_params(entry).to_vec();
        let ctx = params[0];
        let numeric_slots = params[1];
        let routine_args = &params[2..];
        let error_block = builder.create_block();
        let finish_block = builder.create_block();
        let return_block = if let UnitKind::Function { .. } = routine.kind {
            let block = builder.create_block();
            let return_type = if matches!(
                routine.kind,
                UnitKind::Function {
                    returns_string: true
                }
            ) {
                types::I32
            } else {
                types::F64
            };
            builder.append_block_param(block, return_type);
            Some(block)
        } else {
            None
        };

        let mut blocks = HashMap::new();
        for address in routine.start..routine.end {
            blocks.insert(address, builder.create_block());
        }
        if routine.start >= routine.end {
            return Err(compile_error(
                routine.line,
                format!("{} has an empty body", routine.name),
            ));
        }

        builder.switch_to_block(entry);
        let line = iconst_i32(&mut builder, i32::from(routine.line));
        let scope_mark = if !routine.parameters.is_empty()
            && matches!(
                routine.kind,
                UnitKind::Procedure | UnitKind::Function { .. }
            ) {
            Some(self.call_helper(&mut builder, "strict_native_scope_begin", &[ctx, line]))
        } else {
            None
        };
        if scope_mark.is_some() {
            self.guard_context(&mut builder, ctx, error_block);
        }
        let enter = self.call_helper(&mut builder, "strict_native_enter", &[ctx, line]);
        self.guard_status(&mut builder, enter, error_block);
        if scope_mark.is_some() {
            for (parameter, argument) in routine.parameters.iter().zip(routine_args.iter().copied())
            {
                if parameter.ends_with('$') {
                    let slot = *self.string_indices.get(parameter).ok_or_else(|| {
                        compile_error(routine.line, format!("missing string slot for {parameter}"))
                    })?;
                    let slot = iconst_i32(&mut builder, slot as i32);
                    let saved = self.call_helper(
                        &mut builder,
                        "strict_native_scope_save_string",
                        &[ctx, slot, line],
                    );
                    self.guard_status(&mut builder, saved, error_block);
                    let bound = self.call_helper(
                        &mut builder,
                        "strict_native_set_string",
                        &[ctx, slot, argument, line],
                    );
                    self.guard_status(&mut builder, bound, error_block);
                } else {
                    let slot = *self.numeric_indices.get(parameter).ok_or_else(|| {
                        compile_error(
                            routine.line,
                            format!("missing numeric slot for {parameter}"),
                        )
                    })?;
                    let slot_value = iconst_i32(&mut builder, slot as i32);
                    let saved = self.call_helper(
                        &mut builder,
                        "strict_native_scope_save_number",
                        &[ctx, slot_value, line],
                    );
                    self.guard_status(&mut builder, saved, error_block);
                    let destination =
                        numeric_slot_ptr(&mut builder, numeric_slots, slot, self.pointer_type);
                    let value =
                        coerce_numeric_slot(&mut builder, argument, parameter.ends_with('%'));
                    builder
                        .ins()
                        .store(MemFlagsData::trusted(), value, destination, 0);
                }
            }
        }
        builder.ins().jump(blocks[&routine.start], &[]);

        let loops = self.create_loop_slots(&mut builder, routine)?;
        for address in routine.start..routine.end {
            let block = blocks[&address];
            builder.switch_to_block(block);
            let item = &self.program.instructions[address];
            let next = blocks.get(&(address + 1)).copied().unwrap_or(finish_block);
            self.emit_statement(
                &mut builder,
                routine,
                Some(address),
                &item.statement,
                item.line_number,
                next,
                finish_block,
                &blocks,
                &loops,
                return_block,
                error_block,
                ctx,
                numeric_slots,
            )?;
        }

        builder.switch_to_block(finish_block);
        if matches!(routine.kind, UnitKind::Function { .. }) {
            let line = iconst_i32(&mut builder, i32::from(routine.line));
            let reason = iconst_i32(&mut builder, 3);
            let _ = self.call_helper(&mut builder, "strict_native_fail", &[ctx, reason, line]);
            builder.ins().jump(error_block, &[]);
        } else {
            if let Some(mark) = scope_mark {
                let line = iconst_i32(&mut builder, i32::from(routine.line));
                let restored = self.call_helper(
                    &mut builder,
                    "strict_native_scope_restore",
                    &[ctx, mark, line],
                );
                self.guard_status(&mut builder, restored, error_block);
            }
            let line = iconst_i32(&mut builder, i32::from(routine.line));
            let exited = self.call_helper(&mut builder, "strict_native_exit", &[ctx, line]);
            let success = builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                exited,
                0,
            );
            let return_ok = builder.create_block();
            let return_error = builder.create_block();
            builder
                .ins()
                .brif(success, return_ok, &[], return_error, &[]);
            builder.switch_to_block(return_ok);
            let one = builder.ins().iconst(types::I32, 1);
            builder.ins().return_(&[one]);
            builder.switch_to_block(return_error);
            let zero = builder.ins().iconst(types::I32, 0);
            builder.ins().return_(&[zero]);
        }

        builder.switch_to_block(error_block);
        if let Some(mark) = scope_mark {
            let line = iconst_i32(&mut builder, i32::from(routine.line));
            let _ = self.call_helper(
                &mut builder,
                "strict_native_scope_restore",
                &[ctx, mark, line],
            );
        }
        let line = iconst_i32(&mut builder, i32::from(routine.line));
        let _ = self.call_helper(&mut builder, "strict_native_exit", &[ctx, line]);
        match routine.kind {
            UnitKind::Function {
                returns_string: true,
            } => {
                let zero = builder.ins().iconst(types::I32, 0);
                builder.ins().return_(&[zero]);
            }
            UnitKind::Function {
                returns_string: false,
            } => {
                let zero = builder.ins().f64const(0.0);
                builder.ins().return_(&[zero]);
            }
            _ => {
                let zero = builder.ins().iconst(types::I32, 0);
                builder.ins().return_(&[zero]);
            }
        }

        if let Some(block) = return_block {
            builder.switch_to_block(block);
            if let Some(mark) = scope_mark {
                let line = iconst_i32(&mut builder, i32::from(routine.line));
                let restored = self.call_helper(
                    &mut builder,
                    "strict_native_scope_restore",
                    &[ctx, mark, line],
                );
                self.guard_status(&mut builder, restored, error_block);
            }
            let line = iconst_i32(&mut builder, i32::from(routine.line));
            let _ = self.call_helper(&mut builder, "strict_native_exit", &[ctx, line]);
            let value = builder.block_params(block)[0];
            builder.ins().return_(&[value]);
        }

        builder.seal_all_blocks();
        builder.finalize(frontend_config);
        self.module
            .define_function(routine.id, &mut context)
            .map_err(|error| compile_error(routine.line, error.to_string()))?;
        self.module.clear_context(&mut context);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_statement(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        routine: &NativeRoutine,
        address: Option<usize>,
        statement: &Statement,
        line: u16,
        next: cranelift_codegen::ir::Block,
        finish: cranelift_codegen::ir::Block,
        blocks: &HashMap<usize, cranelift_codegen::ir::Block>,
        loops: &HashMap<usize, LoopSlots>,
        return_block: Option<cranelift_codegen::ir::Block>,
        error_block: cranelift_codegen::ir::Block,
        ctx: IrValue,
        numeric_slots: IrValue,
    ) -> Result<(), String> {
        let line_value = iconst_i32(builder, i32::from(line));
        match statement {
            Statement::NoOp
            | Statement::Data(_)
            | Statement::DefineProcedure(_, _)
            | Statement::DefineFunction(_, _)
            | Statement::EndIf => {
                builder.ins().jump(next, &[]);
            }
            Statement::Assign(target, expression) => {
                let value = self.compile_expression(
                    builder,
                    expression,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                self.emit_assignment(
                    builder,
                    target,
                    value,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                builder.ins().jump(next, &[]);
            }
            Statement::Input(target) => {
                self.emit_input(builder, target, line_value, line, ctx, error_block)?;
                builder.ins().jump(next, &[]);
            }
            Statement::Print(items) => {
                for (index, item) in items.iter().enumerate() {
                    match item {
                        PrintItem::Value(expression) => {
                            let value = self.compile_expression(
                                builder,
                                expression,
                                line,
                                routine,
                                blocks,
                                ctx,
                                numeric_slots,
                                error_block,
                            )?;
                            match value {
                                CompiledExpr::Number(value) => {
                                    let result = self.call_helper(
                                        builder,
                                        "strict_native_print_number",
                                        &[ctx, value, line_value],
                                    );
                                    self.guard_status(builder, result, error_block);
                                }
                                CompiledExpr::String(value) => {
                                    let result = self.call_helper(
                                        builder,
                                        "strict_native_print_string",
                                        &[ctx, value, line_value],
                                    );
                                    self.guard_status(builder, result, error_block);
                                }
                            }
                        }
                        PrintItem::Spaces(expression) => {
                            let value = self.number_expression(
                                builder,
                                expression,
                                line,
                                routine,
                                blocks,
                                ctx,
                                numeric_slots,
                                error_block,
                            )?;
                            let result = self.call_helper(
                                builder,
                                "strict_native_print_spaces",
                                &[ctx, value, line_value],
                            );
                            self.guard_status(builder, result, error_block);
                        }
                        PrintItem::Tab(x, y) => {
                            let x = self.number_expression(
                                builder,
                                x,
                                line,
                                routine,
                                blocks,
                                ctx,
                                numeric_slots,
                                error_block,
                            )?;
                            let y = self.number_expression(
                                builder,
                                y,
                                line,
                                routine,
                                blocks,
                                ctx,
                                numeric_slots,
                                error_block,
                            )?;
                            let result = self.call_helper(
                                builder,
                                "strict_native_print_tab",
                                &[ctx, x, y, line_value],
                            );
                            self.guard_status(builder, result, error_block);
                        }
                        PrintItem::Comma => {
                            let result = self.call_helper(
                                builder,
                                "strict_native_print_comma",
                                &[ctx, line_value],
                            );
                            self.guard_status(builder, result, error_block);
                        }
                        PrintItem::NewLine => {
                            let result = self.call_helper(
                                builder,
                                "strict_native_print_newline",
                                &[ctx, line_value],
                            );
                            self.guard_status(builder, result, error_block);
                        }
                        PrintItem::Semicolon => {}
                    }
                    let _ = index;
                }
                if !matches!(items.last(), Some(PrintItem::Semicolon | PrintItem::Comma)) {
                    let result = self.call_helper(
                        builder,
                        "strict_native_print_newline",
                        &[ctx, line_value],
                    );
                    self.guard_status(builder, result, error_block);
                }
                builder.ins().jump(next, &[]);
            }
            Statement::PrintFormat(expression) => {
                let value = self.number_expression(
                    builder,
                    expression,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let result = self.call_helper(
                    builder,
                    "strict_native_print_format",
                    &[ctx, value, line_value],
                );
                self.guard_status(builder, result, error_block);
                builder.ins().jump(next, &[]);
            }
            Statement::Dim(declarations) => {
                for declaration in declarations {
                    if declaration.dimensions.is_empty() {
                        return Err(compile_error(
                            line,
                            "DIM without dimensions is not supported in strict native mode",
                        ));
                    }
                    let mut length = builder.ins().f64const(1.0);
                    for dimension in &declaration.dimensions {
                        let upper = self.number_expression(
                            builder,
                            dimension,
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?;
                        let one = builder.ins().f64const(1.0);
                        let axis = builder.ins().fadd(upper, one);
                        length = builder.ins().fmul(length, axis);
                    }
                    let array = *self.array_indices.get(&declaration.name).ok_or_else(|| {
                        compile_error(line, format!("missing array slot for {}", declaration.name))
                    })?;
                    let array = iconst_i32(builder, array as i32);
                    let byte_block = iconst_i32(builder, i32::from(declaration.byte_block));
                    let result = self.call_helper(
                        builder,
                        "strict_native_array_dim",
                        &[ctx, array, length, byte_block, line_value],
                    );
                    self.guard_status(builder, result, error_block);
                }
                builder.ins().jump(next, &[]);
            }
            Statement::Read(targets) => {
                for target in targets {
                    let is_string = self.lvalue_is_string(target);
                    if is_string {
                        let destination = self.temp_string()?;
                        let destination_value = iconst_i32(builder, destination);
                        let result = self.call_helper(
                            builder,
                            "strict_native_data_read_string",
                            &[ctx, destination_value, line_value],
                        );
                        self.guard_status(builder, result, error_block);
                        self.emit_assignment(
                            builder,
                            target,
                            CompiledExpr::String(destination_value),
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?;
                    } else {
                        let value = self.call_helper(
                            builder,
                            "strict_native_data_read_number",
                            &[ctx, line_value],
                        );
                        self.guard_context(builder, ctx, error_block);
                        self.emit_assignment(
                            builder,
                            target,
                            CompiledExpr::Number(value),
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?;
                    }
                }
                builder.ins().jump(next, &[]);
            }
            Statement::Restore(target) => {
                let cursor = target.map_or(0, |target| {
                    self.data.partition_point(|(line, _)| *line < target)
                });
                let cursor = i32::try_from(cursor)
                    .map_err(|_| compile_error(line, "DATA table is too large"))?;
                let cursor = iconst_i32(builder, cursor);
                let result = self.call_helper(
                    builder,
                    "strict_native_data_restore",
                    &[ctx, cursor, line_value],
                );
                self.guard_status(builder, result, error_block);
                builder.ins().jump(next, &[]);
            }
            Statement::If(condition, consequent, alternative) => {
                let condition = self.number_expression(
                    builder,
                    condition,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let zero = builder.ins().f64const(0.0);
                let truth = builder.ins().fcmp(FloatCC::NotEqual, condition, zero);
                let then_entry = builder.create_block();
                let else_entry = if alternative.is_empty() {
                    next
                } else {
                    builder.create_block()
                };
                builder.ins().brif(truth, then_entry, &[], else_entry, &[]);
                self.emit_sequence(
                    builder,
                    routine,
                    consequent,
                    line,
                    then_entry,
                    next,
                    finish,
                    blocks,
                    loops,
                    return_block,
                    error_block,
                    ctx,
                    numeric_slots,
                )?;
                if !alternative.is_empty() {
                    self.emit_sequence(
                        builder,
                        routine,
                        alternative,
                        line,
                        else_entry,
                        next,
                        finish,
                        blocks,
                        loops,
                        return_block,
                        error_block,
                        ctx,
                        numeric_slots,
                    )?;
                }
            }
            Statement::IfBlock(condition) => {
                let address = address.ok_or_else(|| {
                    compile_error(line, "nested block IF requires a source instruction")
                })?;
                let end = *self
                    .if_pairs
                    .get(&address)
                    .ok_or_else(|| compile_error(line, "IF has no matching ENDIF"))?;
                let destination = blocks.get(&(end + 1)).copied().unwrap_or(finish);
                let condition = self.number_expression(
                    builder,
                    condition,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let zero = builder.ins().f64const(0.0);
                let truth = builder.ins().fcmp(FloatCC::NotEqual, condition, zero);
                builder.ins().brif(truth, next, &[], destination, &[]);
            }
            Statement::Goto(target) => {
                let destination_address =
                    *self.program.line_entries.get(target).ok_or_else(|| {
                        compile_error(line, format!("line {target} does not exist"))
                    })?;
                let destination = *blocks.get(&destination_address).ok_or_else(|| {
                    compile_error(line, format!("GOTO line {target} leaves {}", routine.name))
                })?;
                if address.is_some_and(|address| destination_address < address) {
                    self.emit_tick(builder, ctx, line_value, error_block);
                }
                builder.ins().jump(destination, &[]);
            }
            Statement::Gosub(target) => {
                let key = UnitKey::Subroutine(*target);
                self.emit_routine_call(builder, &key, &[], ctx, numeric_slots, line, error_block)?;
                builder.ins().jump(next, &[]);
            }
            Statement::For {
                variable,
                start,
                end,
                step,
            } => {
                let address = address
                    .ok_or_else(|| compile_error(line, "FOR inside an inline IF is unsupported"))?;
                let next_address = *self
                    .for_pairs
                    .get(&address)
                    .ok_or_else(|| compile_error(line, "FOR has no matching NEXT"))?;
                let storage = loops
                    .get(&address)
                    .ok_or_else(|| compile_error(line, "FOR loop storage is missing"))?;
                let start_value = self.number_expression(
                    builder,
                    start,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let end_value = self.number_expression(
                    builder,
                    end,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let step_value = if let Some(step) = step {
                    self.number_expression(
                        builder,
                        step,
                        line,
                        routine,
                        blocks,
                        ctx,
                        numeric_slots,
                        error_block,
                    )?
                } else {
                    builder.ins().f64const(1.0)
                };
                let zero = builder.ins().f64const(0.0);
                let invalid_step = builder.ins().fcmp(FloatCC::Equal, step_value, zero);
                let valid_block = builder.create_block();
                let fail_block = builder.create_block();
                builder
                    .ins()
                    .brif(invalid_step, fail_block, &[], valid_block, &[]);
                builder.switch_to_block(fail_block);
                let reason = iconst_i32(builder, 1);
                let _ = self.call_helper(builder, "strict_native_fail", &[ctx, reason, line_value]);
                builder.ins().jump(error_block, &[]);
                builder.switch_to_block(valid_block);
                let end_ptr = builder.ins().stack_addr(self.pointer_type, storage.end, 0);
                let step_ptr = builder.ins().stack_addr(self.pointer_type, storage.step, 0);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), end_value, end_ptr, 0);
                builder
                    .ins()
                    .store(MemFlagsData::trusted(), step_value, step_ptr, 0);
                self.store_number_variable(builder, variable, start_value, line, numeric_slots)?;
                let positive_step = builder.ins().fcmp(FloatCC::GreaterThan, step_value, zero);
                let positive_enters =
                    builder
                        .ins()
                        .fcmp(FloatCC::LessThanOrEqual, start_value, end_value);
                let negative_enters =
                    builder
                        .ins()
                        .fcmp(FloatCC::GreaterThanOrEqual, start_value, end_value);
                let enters = builder
                    .ins()
                    .select(positive_step, positive_enters, negative_enters);
                let body = blocks.get(&(address + 1)).copied().unwrap_or(finish);
                let after = blocks.get(&(next_address + 1)).copied().unwrap_or(finish);
                builder.ins().brif(enters, body, &[], after, &[]);
            }
            Statement::Next(requested) => {
                let address = address.ok_or_else(|| {
                    compile_error(line, "NEXT inside an inline IF is unsupported")
                })?;
                let (&for_address, _) =
                    self.for_pairs
                        .iter()
                        .find(|(_, next)| **next == address)
                        .ok_or_else(|| compile_error(line, "NEXT has no matching FOR"))?;
                let Statement::For { variable, .. } =
                    &self.program.instructions[for_address].statement
                else {
                    return Err(compile_error(line, "NEXT loop pairing is invalid"));
                };
                if requested
                    .as_ref()
                    .is_some_and(|requested| !requested.eq_ignore_ascii_case(variable))
                {
                    return Err(compile_error(
                        line,
                        format!("NEXT variable does not match FOR {variable}"),
                    ));
                }
                let storage = loops
                    .get(&for_address)
                    .ok_or_else(|| compile_error(line, "NEXT loop storage is missing"))?;
                let end_ptr = builder.ins().stack_addr(self.pointer_type, storage.end, 0);
                let step_ptr = builder.ins().stack_addr(self.pointer_type, storage.step, 0);
                let end_value = builder
                    .ins()
                    .load(types::F64, MemFlagsData::trusted(), end_ptr, 0);
                let step_value =
                    builder
                        .ins()
                        .load(types::F64, MemFlagsData::trusted(), step_ptr, 0);
                let current = self.load_number_variable(builder, variable, numeric_slots, line)?;
                let next_value = builder.ins().fadd(current, step_value);
                self.store_number_variable(builder, variable, next_value, line, numeric_slots)?;
                let zero = builder.ins().f64const(0.0);
                let positive_step = builder.ins().fcmp(FloatCC::GreaterThan, step_value, zero);
                let positive_continues =
                    builder
                        .ins()
                        .fcmp(FloatCC::LessThanOrEqual, next_value, end_value);
                let negative_continues =
                    builder
                        .ins()
                        .fcmp(FloatCC::GreaterThanOrEqual, next_value, end_value);
                let continues =
                    builder
                        .ins()
                        .select(positive_step, positive_continues, negative_continues);
                let body = blocks.get(&(for_address + 1)).copied().unwrap_or(finish);
                let backedge = builder.create_block();
                builder.ins().brif(continues, backedge, &[], next, &[]);
                builder.switch_to_block(backedge);
                self.emit_tick(builder, ctx, line_value, error_block);
                builder.ins().jump(body, &[]);
            }
            Statement::Repeat => {
                builder.ins().jump(next, &[]);
            }
            Statement::Until(condition) => {
                let address = address.ok_or_else(|| {
                    compile_error(line, "UNTIL inside an inline IF is unsupported")
                })?;
                let repeat_address = *self
                    .repeat_pairs
                    .get(&address)
                    .ok_or_else(|| compile_error(line, "UNTIL has no matching REPEAT"))?;
                let body = blocks
                    .get(&(repeat_address + 1))
                    .copied()
                    .ok_or_else(|| compile_error(line, "REPEAT body is empty"))?;
                let condition = self.number_expression(
                    builder,
                    condition,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let zero = builder.ins().f64const(0.0);
                let truth = builder.ins().fcmp(FloatCC::NotEqual, condition, zero);
                let backedge = builder.create_block();
                builder.ins().brif(truth, next, &[], backedge, &[]);
                builder.switch_to_block(backedge);
                self.emit_tick(builder, ctx, line_value, error_block);
                builder.ins().jump(body, &[]);
            }
            Statement::ProcedureCall(name, arguments) => {
                let key = UnitKey::Procedure(name.clone());
                let values = self.compile_arguments(
                    builder,
                    arguments,
                    &key,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                self.emit_routine_call(
                    builder,
                    &key,
                    &values,
                    ctx,
                    numeric_slots,
                    line,
                    error_block,
                )?;
                builder.ins().jump(next, &[]);
            }
            Statement::FunctionReturn(expression) => {
                let Some(return_block) = return_block else {
                    let reason = iconst_i32(builder, 3);
                    let _ =
                        self.call_helper(builder, "strict_native_fail", &[ctx, reason, line_value]);
                    builder.ins().jump(error_block, &[]);
                    return Ok(());
                };
                let returns_string = matches!(
                    routine.kind,
                    UnitKind::Function {
                        returns_string: true
                    }
                );
                let value = self.compile_expression(
                    builder,
                    expression,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let result = if returns_string {
                    let CompiledExpr::String(source) = value else {
                        return Err(compile_error(
                            line,
                            "string FN return requires a string expression",
                        ));
                    };
                    let destination = self.temp_string()?;
                    let destination_value = iconst_i32(builder, destination);
                    let copied = self.call_helper(
                        builder,
                        "strict_native_copy_string",
                        &[ctx, destination_value, source, line_value],
                    );
                    self.guard_status(builder, copied, error_block);
                    destination_value
                } else {
                    let CompiledExpr::Number(value) = value else {
                        return Err(compile_error(
                            line,
                            "numeric FN return requires a numeric expression",
                        ));
                    };
                    value
                };
                builder.ins().jump(return_block, &[result.into()]);
            }
            Statement::Return => {
                if matches!(routine.kind, UnitKind::Subroutine) {
                    builder.ins().jump(finish, &[]);
                } else {
                    let reason = iconst_i32(builder, 2);
                    let _ =
                        self.call_helper(builder, "strict_native_fail", &[ctx, reason, line_value]);
                    builder.ins().jump(error_block, &[]);
                }
            }
            Statement::EndProcedure => {
                if matches!(routine.kind, UnitKind::Procedure) {
                    builder.ins().jump(finish, &[]);
                } else {
                    let reason = iconst_i32(builder, 2);
                    let _ =
                        self.call_helper(builder, "strict_native_fail", &[ctx, reason, line_value]);
                    builder.ins().jump(error_block, &[]);
                }
            }
            Statement::End => {
                if matches!(routine.kind, UnitKind::Main) {
                    builder.ins().jump(finish, &[]);
                } else {
                    return Err(compile_error(
                        line,
                        "END inside a callable routine is outside the strict native subset",
                    ));
                }
            }
            Statement::Call(expression) => {
                let address = self.number_expression(
                    builder,
                    expression,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let result =
                    self.call_helper(builder, "strict_native_call", &[ctx, address, line_value]);
                self.guard_status(builder, result, error_block);
                builder.ins().jump(next, &[]);
            }
            Statement::StarCommand(command) => {
                let slot = self.constant_string_slot(command)?;
                let slot = iconst_i32(builder, slot);
                let result =
                    self.call_helper(builder, "strict_native_star", &[ctx, slot, line_value]);
                self.guard_status(builder, result, error_block);
                builder.ins().jump(next, &[]);
            }
            Statement::ClearScreen
            | Statement::ClearGraphics
            | Statement::Colour(_)
            | Statement::Mode(_)
            | Statement::Vdu(_)
            | Statement::Line(_, _, _, _)
            | Statement::Move(_, _)
            | Statement::Draw(_, _)
            | Statement::Plot(_, _, _)
            | Statement::Gcol(_, _)
            | Statement::Sys { .. }
            | Statement::PrimitiveCall { .. }
            | Statement::ImportedProcedureCall { .. }
            | Statement::LocalReadOnly { .. }
            | Statement::Try
            | Statement::Catch { .. }
            | Statement::EndTry
            | Statement::Throw { .. } => {
                if address.is_some() {
                    return Err(compile_error(
                        line,
                        format!("{statement:?} is not yet supported by strict native lowering"),
                    ));
                }
                let reason = iconst_i32(builder, 4);
                let _ = self.call_helper(builder, "strict_native_fail", &[ctx, reason, line_value]);
                builder.ins().jump(error_block, &[]);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_sequence(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        routine: &NativeRoutine,
        statements: &[Statement],
        line: u16,
        start: cranelift_codegen::ir::Block,
        continuation: cranelift_codegen::ir::Block,
        finish: cranelift_codegen::ir::Block,
        blocks: &HashMap<usize, cranelift_codegen::ir::Block>,
        loops: &HashMap<usize, LoopSlots>,
        return_block: Option<cranelift_codegen::ir::Block>,
        error_block: cranelift_codegen::ir::Block,
        ctx: IrValue,
        numeric_slots: IrValue,
    ) -> Result<(), String> {
        if statements.is_empty() {
            builder.switch_to_block(start);
            builder.ins().jump(continuation, &[]);
            return Ok(());
        }
        let mut current = start;
        for (index, statement) in statements.iter().enumerate() {
            let next = if index + 1 == statements.len() {
                continuation
            } else {
                builder.create_block()
            };
            builder.switch_to_block(current);
            self.emit_statement(
                builder,
                routine,
                None,
                statement,
                line,
                next,
                finish,
                blocks,
                loops,
                return_block,
                error_block,
                ctx,
                numeric_slots,
            )?;
            current = next;
        }
        Ok(())
    }

    fn emit_tick(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        ctx: IrValue,
        line: IrValue,
        error_block: cranelift_codegen::ir::Block,
    ) {
        let status = self.call_helper(builder, "strict_native_tick", &[ctx, line]);
        self.guard_status(builder, status, error_block);
    }

    fn compile_expression(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        expression: &Expr,
        line: u16,
        routine: &NativeRoutine,
        blocks: &HashMap<usize, cranelift_codegen::ir::Block>,
        ctx: IrValue,
        numeric_slots: IrValue,
        error_block: cranelift_codegen::ir::Block,
    ) -> Result<CompiledExpr, String> {
        let line_value = iconst_i32(builder, i32::from(line));
        match expression {
            Expr::Number(value) => Ok(CompiledExpr::Number(builder.ins().f64const(*value))),
            Expr::Integer(value)
                if self.program.options.mode != crate::configure::BasicLanguageMode::Basic64 =>
            {
                // Preserve the pre-BASIC64 floating number model for classic
                // source while the typed BASIC64 path remains interpreter-only.
                Ok(CompiledExpr::Number(builder.ins().f64const(*value as f64)))
            }
            Expr::Integer(_) => Err(compile_error(
                line,
                "exact BASIC64 integer literals are not supported by strict native lowering",
            )),
            Expr::String(value) => Ok(CompiledExpr::String(iconst_i32(
                builder,
                self.constant_string_slot(value)?,
            ))),
            Expr::Variable(name) if name.eq_ignore_ascii_case("TIME") => {
                let value = self.call_helper(builder, "strict_native_time", &[ctx, line_value]);
                self.guard_context(builder, ctx, error_block);
                Ok(CompiledExpr::Number(value))
            }
            Expr::Variable(name) if name.ends_with('$') => {
                let slot = self.string_indices.get(name).copied().ok_or_else(|| {
                    compile_error(line, format!("missing string slot for {name}"))
                })?;
                Ok(CompiledExpr::String(iconst_i32(builder, slot as i32)))
            }
            Expr::Variable(name) => {
                let slot = *self.numeric_indices.get(name).ok_or_else(|| {
                    compile_error(line, format!("missing numeric slot for {name}"))
                })?;
                let pointer = numeric_slot_ptr(builder, numeric_slots, slot, self.pointer_type);
                let value = builder
                    .ins()
                    .load(types::F64, MemFlagsData::trusted(), pointer, 0);
                Ok(CompiledExpr::Number(value))
            }
            Expr::ArrayElement(name, index) => {
                let array = *self
                    .array_indices
                    .get(name)
                    .ok_or_else(|| compile_error(line, format!("missing array slot for {name}")))?;
                let array = iconst_i32(builder, array as i32);
                let index = self.number_expression(
                    builder,
                    index,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                if name.ends_with('$') {
                    let destination = self.temp_string()?;
                    let destination_value = iconst_i32(builder, destination);
                    let status = self.call_helper(
                        builder,
                        "strict_native_array_get_string",
                        &[ctx, array, index, destination_value, line_value],
                    );
                    self.guard_status(builder, status, error_block);
                    Ok(CompiledExpr::String(destination_value))
                } else {
                    let value = self.call_helper(
                        builder,
                        "strict_native_array_get_number",
                        &[ctx, array, index, line_value],
                    );
                    self.guard_context(builder, ctx, error_block);
                    Ok(CompiledExpr::Number(value))
                }
            }
            Expr::Unary(operator, operand) => {
                let value = self.number_expression(
                    builder,
                    operand,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                match operator {
                    UnaryOp::Plus => Ok(CompiledExpr::Number(value)),
                    UnaryOp::Minus => Ok(CompiledExpr::Number(builder.ins().fneg(value))),
                    UnaryOp::Not => {
                        let operation = iconst_i32(builder, 5);
                        let zero = builder.ins().f64const(0.0);
                        let value = self.call_helper(
                            builder,
                            "strict_native_integer_operation",
                            &[ctx, operation, value, zero, line_value],
                        );
                        self.guard_context(builder, ctx, error_block);
                        Ok(CompiledExpr::Number(value))
                    }
                }
            }
            Expr::Binary(left, operator, right) => {
                let left = self.compile_expression(
                    builder,
                    left,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let right = self.compile_expression(
                    builder,
                    right,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                if *operator == BinaryOp::Add {
                    if let (CompiledExpr::String(left), CompiledExpr::String(right)) = (left, right)
                    {
                        let destination = self.temp_string()?;
                        let destination_value = iconst_i32(builder, destination);
                        let status = self.call_helper(
                            builder,
                            "strict_native_string_concat",
                            &[ctx, left, right, destination_value, line_value],
                        );
                        self.guard_status(builder, status, error_block);
                        return Ok(CompiledExpr::String(destination_value));
                    }
                }
                if is_comparison(*operator) {
                    match (left, right) {
                        (CompiledExpr::String(left), CompiledExpr::String(right)) => {
                            let code = comparison_code(*operator).expect("comparison operator");
                            let code = iconst_i32(builder, code);
                            let value = self.call_helper(
                                builder,
                                "strict_native_string_compare",
                                &[ctx, left, right, code, line_value],
                            );
                            self.guard_context(builder, ctx, error_block);
                            return Ok(CompiledExpr::Number(value));
                        }
                        (CompiledExpr::Number(left), CompiledExpr::Number(right)) => {
                            let code = comparison_float(builder, *operator, left, right);
                            return Ok(CompiledExpr::Number(code));
                        }
                        _ => {
                            return Err(compile_error(
                                line,
                                "cannot compare a number with a string",
                            ));
                        }
                    }
                }
                let (CompiledExpr::Number(left), CompiledExpr::Number(right)) = (left, right)
                else {
                    return Err(compile_error(
                        line,
                        "numeric operator used with a string expression",
                    ));
                };
                let value = match operator {
                    BinaryOp::Add => builder.ins().fadd(left, right),
                    BinaryOp::Subtract => builder.ins().fsub(left, right),
                    BinaryOp::Multiply => builder.ins().fmul(left, right),
                    BinaryOp::Divide => {
                        let zero = builder.ins().f64const(0.0);
                        let is_zero = builder.ins().fcmp(FloatCC::Equal, right, zero);
                        let fail = builder.create_block();
                        let good = builder.create_block();
                        builder.ins().brif(is_zero, fail, &[], good, &[]);
                        builder.switch_to_block(fail);
                        let reason = iconst_i32(builder, 0);
                        let _ = self.call_helper(
                            builder,
                            "strict_native_fail",
                            &[ctx, reason, line_value],
                        );
                        builder.ins().jump(error_block, &[]);
                        builder.switch_to_block(good);
                        builder.ins().fdiv(left, right)
                    }
                    BinaryOp::IntegerDivide
                    | BinaryOp::Modulo
                    | BinaryOp::ShiftLeft
                    | BinaryOp::And
                    | BinaryOp::Or => {
                        let operation = match operator {
                            BinaryOp::IntegerDivide => 0,
                            BinaryOp::Modulo => 1,
                            BinaryOp::ShiftLeft => 2,
                            BinaryOp::And => 3,
                            BinaryOp::Or => 4,
                            _ => unreachable!(),
                        };
                        let operation = iconst_i32(builder, operation);
                        let value = self.call_helper(
                            builder,
                            "strict_native_integer_operation",
                            &[ctx, operation, left, right, line_value],
                        );
                        self.guard_context(builder, ctx, error_block);
                        value
                    }
                    BinaryOp::Power => {
                        let value = self.call_helper(
                            builder,
                            "strict_native_pow",
                            &[ctx, left, right, line_value],
                        );
                        self.guard_context(builder, ctx, error_block);
                        value
                    }
                    _ => return Err(compile_error(line, "unsupported numeric operator")),
                };
                Ok(CompiledExpr::Number(value))
            }
            Expr::Builtin(token, arguments) => self.compile_builtin(
                builder,
                *token,
                arguments,
                line,
                routine,
                blocks,
                ctx,
                numeric_slots,
                error_block,
            ),
            Expr::UserFunction(name, arguments) => {
                let key = UnitKey::Function(name.clone());
                let values = self.compile_arguments(
                    builder,
                    arguments,
                    &key,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let value = self.emit_routine_call(
                    builder,
                    &key,
                    &values,
                    ctx,
                    numeric_slots,
                    line,
                    error_block,
                )?;
                if name.ends_with('$') {
                    Ok(CompiledExpr::String(value))
                } else {
                    Ok(CompiledExpr::Number(value))
                }
            }
            Expr::MemoryRead(width, address) => {
                let address = self.number_expression(
                    builder,
                    address,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let width = iconst_i32(builder, memory_width_code(*width));
                let value = self.call_helper(
                    builder,
                    "strict_native_read_memory",
                    &[ctx, width, address, line_value],
                );
                self.guard_context(builder, ctx, error_block);
                Ok(CompiledExpr::Number(value))
            }
            Expr::Member(_, _) => Err(compile_error(
                line,
                "BASIC64 record and enum member access is not supported by strict native lowering",
            )),
            Expr::ImportedFunction { .. } => Err(compile_error(
                line,
                "qualified BASIC64 function imports are not supported by strict native lowering",
            )),
        }
    }

    fn compile_builtin(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        token: u8,
        arguments: &[Expr],
        line: u16,
        routine: &NativeRoutine,
        blocks: &HashMap<usize, cranelift_codegen::ir::Block>,
        ctx: IrValue,
        numeric_slots: IrValue,
        error_block: cranelift_codegen::ir::Block,
    ) -> Result<CompiledExpr, String> {
        let line_value = iconst_i32(builder, i32::from(line));
        match token {
            0xA6 => {
                if arguments.len() > 1 {
                    return Err(compile_error(line, "INKEY expects zero or one argument"));
                }
                let argument = if let Some(argument) = arguments.first() {
                    self.number_expression(
                        builder,
                        argument,
                        line,
                        routine,
                        blocks,
                        ctx,
                        numeric_slots,
                        error_block,
                    )?
                } else {
                    builder.ins().f64const(0.0)
                };
                let has_argument = iconst_i32(builder, i32::from(!arguments.is_empty()));
                let value = self.call_helper(
                    builder,
                    "strict_native_inkey",
                    &[ctx, has_argument, line_value],
                );
                let _ = argument;
                self.guard_context(builder, ctx, error_block);
                Ok(CompiledExpr::Number(value))
            }
            0xB3 => {
                if arguments.len() > 1 {
                    return Err(compile_error(line, "RND expects zero or one argument"));
                }
                let argument = if let Some(argument) = arguments.first() {
                    self.number_expression(
                        builder,
                        argument,
                        line,
                        routine,
                        blocks,
                        ctx,
                        numeric_slots,
                        error_block,
                    )?
                } else {
                    builder.ins().f64const(0.0)
                };
                let has_argument = iconst_i32(builder, i32::from(!arguments.is_empty()));
                let value = self.call_helper(
                    builder,
                    "strict_native_random",
                    &[ctx, has_argument, argument, line_value],
                );
                self.guard_context(builder, ctx, error_block);
                Ok(CompiledExpr::Number(value))
            }
            0xA9 | 0x97 | 0xBC => {
                if arguments.len() != 1 {
                    return Err(compile_error(line, "string built-in expects one argument"));
                }
                let CompiledExpr::String(source) = self.compile_expression(
                    builder,
                    &arguments[0],
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?
                else {
                    return Err(compile_error(
                        line,
                        "string built-in requires a string argument",
                    ));
                };
                let helper = match token {
                    0xA9 => "strict_native_string_len",
                    0x97 => "strict_native_string_asc",
                    _ => "strict_native_string_val",
                };
                let value = self.call_helper(builder, helper, &[ctx, source, line_value]);
                self.guard_context(builder, ctx, error_block);
                Ok(CompiledExpr::Number(value))
            }
            0xA7 => {
                if !(2..=3).contains(&arguments.len()) {
                    return Err(compile_error(line, "INSTR expects two or three arguments"));
                }
                let CompiledExpr::String(text) = self.compile_expression(
                    builder,
                    &arguments[0],
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?
                else {
                    return Err(compile_error(line, "INSTR text must be a string"));
                };
                let CompiledExpr::String(pattern) = self.compile_expression(
                    builder,
                    &arguments[1],
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?
                else {
                    return Err(compile_error(line, "INSTR pattern must be a string"));
                };
                let (start, has_start) = if let Some(start) = arguments.get(2) {
                    (
                        self.number_expression(
                            builder,
                            start,
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?,
                        1,
                    )
                } else {
                    (builder.ins().f64const(0.0), 0)
                };
                let has_start = iconst_i32(builder, has_start);
                let value = self.call_helper(
                    builder,
                    "strict_native_string_instr",
                    &[ctx, text, pattern, start, has_start, line_value],
                );
                self.guard_context(builder, ctx, error_block);
                Ok(CompiledExpr::Number(value))
            }
            0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC4 | 0xBD => {
                let destination = self.temp_string()?;
                let destination_value = iconst_i32(builder, destination);
                let status = match token {
                    0xC0 | 0xC2 => {
                        if arguments.len() != 2 {
                            return Err(compile_error(line, "LEFT$/RIGHT$ expects two arguments"));
                        }
                        let CompiledExpr::String(source) = self.compile_expression(
                            builder,
                            &arguments[0],
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?
                        else {
                            return Err(compile_error(
                                line,
                                "LEFT$/RIGHT$ requires a string argument",
                            ));
                        };
                        let count = self.number_expression(
                            builder,
                            &arguments[1],
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?;
                        let helper = if token == 0xC0 {
                            "strict_native_string_left"
                        } else {
                            "strict_native_string_right"
                        };
                        self.call_helper(
                            builder,
                            helper,
                            &[ctx, source, count, destination_value, line_value],
                        )
                    }
                    0xC1 => {
                        if !(2..=3).contains(&arguments.len()) {
                            return Err(compile_error(line, "MID$ expects two or three arguments"));
                        }
                        let CompiledExpr::String(source) = self.compile_expression(
                            builder,
                            &arguments[0],
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?
                        else {
                            return Err(compile_error(line, "MID$ requires a string argument"));
                        };
                        let start = self.number_expression(
                            builder,
                            &arguments[1],
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?;
                        let (length, has_length) = if let Some(length) = arguments.get(2) {
                            (
                                self.number_expression(
                                    builder,
                                    length,
                                    line,
                                    routine,
                                    blocks,
                                    ctx,
                                    numeric_slots,
                                    error_block,
                                )?,
                                1,
                            )
                        } else {
                            (builder.ins().f64const(0.0), 0)
                        };
                        let has_length = iconst_i32(builder, has_length);
                        self.call_helper(
                            builder,
                            "strict_native_string_mid",
                            &[
                                ctx,
                                source,
                                start,
                                length,
                                has_length,
                                destination_value,
                                line_value,
                            ],
                        )
                    }
                    0xC3 => {
                        if arguments.len() != 1 {
                            return Err(compile_error(line, "STR$ expects one argument"));
                        }
                        let value = self.number_expression(
                            builder,
                            &arguments[0],
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?;
                        self.call_helper(
                            builder,
                            "strict_native_string_str",
                            &[ctx, value, destination_value, line_value],
                        )
                    }
                    0xBD => {
                        if arguments.len() != 1 {
                            return Err(compile_error(line, "CHR$ expects one argument"));
                        }
                        let value = self.number_expression(
                            builder,
                            &arguments[0],
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?;
                        self.call_helper(
                            builder,
                            "strict_native_string_chr",
                            &[ctx, value, destination_value, line_value],
                        )
                    }
                    0xC4 => {
                        if arguments.len() != 2 {
                            return Err(compile_error(line, "STRING$ expects two arguments"));
                        }
                        let count = self.number_expression(
                            builder,
                            &arguments[0],
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?;
                        let CompiledExpr::String(pattern) = self.compile_expression(
                            builder,
                            &arguments[1],
                            line,
                            routine,
                            blocks,
                            ctx,
                            numeric_slots,
                            error_block,
                        )?
                        else {
                            return Err(compile_error(line, "STRING$ pattern must be a string"));
                        };
                        self.call_helper(
                            builder,
                            "strict_native_string_string",
                            &[ctx, count, pattern, destination_value, line_value],
                        )
                    }
                    _ => unreachable!(),
                };
                self.guard_status(builder, status, error_block);
                Ok(CompiledExpr::String(destination_value))
            }
            0x94 | 0x9B | 0xA8 | 0xAA | 0xAB | 0xB5 | 0xB6 | 0xB7 => {
                if arguments.len() != 1 {
                    return Err(compile_error(line, "numeric built-in expects one argument"));
                }
                let value = self.number_expression(
                    builder,
                    &arguments[0],
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let token_value = iconst_i32(builder, i32::from(token));
                let result = self.call_helper(
                    builder,
                    "strict_native_number_builtin",
                    &[ctx, token_value, value, line_value],
                );
                self.guard_context(builder, ctx, error_block);
                Ok(CompiledExpr::Number(result))
            }
            _ => Err(compile_error(
                line,
                format!("built-in token &{token:02X} is outside the strict native subset"),
            )),
        }
    }

    fn number_expression(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        expression: &Expr,
        line: u16,
        routine: &NativeRoutine,
        blocks: &HashMap<usize, cranelift_codegen::ir::Block>,
        ctx: IrValue,
        numeric_slots: IrValue,
        error_block: cranelift_codegen::ir::Block,
    ) -> Result<IrValue, String> {
        match self.compile_expression(
            builder,
            expression,
            line,
            routine,
            blocks,
            ctx,
            numeric_slots,
            error_block,
        )? {
            CompiledExpr::Number(value) => Ok(value),
            CompiledExpr::String(_) => Err(compile_error(line, "expected a numeric expression")),
        }
    }

    fn compile_arguments(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        arguments: &[Expr],
        key: &UnitKey,
        line: u16,
        routine: &NativeRoutine,
        blocks: &HashMap<usize, cranelift_codegen::ir::Block>,
        ctx: IrValue,
        numeric_slots: IrValue,
        error_block: cranelift_codegen::ir::Block,
    ) -> Result<Vec<IrValue>, String> {
        let target = self
            .routines
            .get(key)
            .ok_or_else(|| compile_error(line, format!("unknown native routine {key:?}")))?
            .clone();
        if arguments.len() != target.parameters.len() {
            return Err(compile_error(
                line,
                format!("{} argument count mismatch", target.name),
            ));
        }
        let mut values = Vec::with_capacity(arguments.len());
        for (argument, parameter) in arguments.iter().zip(&target.parameters) {
            let value = self.compile_expression(
                builder,
                argument,
                line,
                routine,
                blocks,
                ctx,
                numeric_slots,
                error_block,
            )?;
            match (parameter.ends_with('$'), value) {
                (true, CompiledExpr::String(slot)) => values.push(slot),
                (false, CompiledExpr::Number(value)) => values.push(value),
                (true, _) => {
                    return Err(compile_error(
                        line,
                        format!("{} requires a string argument", target.name),
                    ));
                }
                (false, _) => {
                    return Err(compile_error(
                        line,
                        format!("{} requires a numeric argument", target.name),
                    ));
                }
            }
        }
        Ok(values)
    }

    fn emit_routine_call(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        key: &UnitKey,
        arguments: &[IrValue],
        ctx: IrValue,
        numeric_slots: IrValue,
        line: u16,
        error_block: cranelift_codegen::ir::Block,
    ) -> Result<IrValue, String> {
        let routine = self
            .routines
            .get(key)
            .ok_or_else(|| compile_error(line, format!("unknown routine {key:?}")))?
            .clone();
        if arguments.len() != routine.parameters.len() {
            return Err(compile_error(
                line,
                format!("{} argument count mismatch", routine.name),
            ));
        }
        let mut call_arguments = Vec::with_capacity(arguments.len() + 2);
        call_arguments.push(ctx);
        call_arguments.push(numeric_slots);
        call_arguments.extend_from_slice(arguments);
        let function = self.module.declare_func_in_func(routine.id, builder.func);
        let call = builder.ins().call(function, &call_arguments);
        let result = builder.inst_results(call)[0];
        match routine.kind {
            UnitKind::Function { .. } => self.guard_context(builder, ctx, error_block),
            UnitKind::Procedure | UnitKind::Subroutine => {
                self.guard_status(builder, result, error_block)
            }
            UnitKind::Main => {
                return Err(compile_error(line, "cannot call the BASIC main routine"));
            }
        }
        Ok(result)
    }

    fn temp_string(&mut self) -> Result<i32, String> {
        let index = self
            .string_names
            .len()
            .checked_add(self.constant_strings.len())
            .and_then(|index| index.checked_add(self.temporary_string_count))
            .ok_or_else(|| "strict JIT string slot overflowed".to_string())?;
        self.temporary_string_count += 1;
        i32::try_from(index).map_err(|_| "too many strict JIT string slots".to_string())
    }

    fn constant_string_slot(&self, value: &[u8]) -> Result<i32, String> {
        self.constant_indices
            .get(value)
            .copied()
            .ok_or_else(|| "strict JIT string constant was not collected".to_string())
    }

    fn store_number_variable(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        name: &str,
        value: IrValue,
        line: u16,
        numeric_slots: IrValue,
    ) -> Result<(), String> {
        if name.eq_ignore_ascii_case("TIME") {
            return Err(compile_error(
                line,
                "TIME is not a FOR control variable in strict native mode",
            ));
        }
        let index = *self
            .numeric_indices
            .get(name)
            .ok_or_else(|| compile_error(line, format!("missing numeric slot for {name}")))?;
        let destination = numeric_slot_ptr(builder, numeric_slots, index, self.pointer_type);
        let value = coerce_numeric_slot(builder, value, name.ends_with('%'));
        builder
            .ins()
            .store(MemFlagsData::trusted(), value, destination, 0);
        Ok(())
    }

    fn load_number_variable(
        &self,
        builder: &mut FunctionBuilder<'_>,
        name: &str,
        numeric_slots: IrValue,
        line: u16,
    ) -> Result<IrValue, String> {
        let index = *self
            .numeric_indices
            .get(name)
            .ok_or_else(|| compile_error(line, format!("missing numeric slot for {name}")))?;
        let source = numeric_slot_ptr(builder, numeric_slots, index, self.pointer_type);
        Ok(builder
            .ins()
            .load(types::F64, MemFlagsData::trusted(), source, 0))
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_assignment(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        target: &LValue,
        value: CompiledExpr,
        line: u16,
        routine: &NativeRoutine,
        blocks: &HashMap<usize, cranelift_codegen::ir::Block>,
        ctx: IrValue,
        numeric_slots: IrValue,
        error_block: cranelift_codegen::ir::Block,
    ) -> Result<(), String> {
        let line_value = iconst_i32(builder, i32::from(line));
        match target {
            LValue::Variable(name) if name.eq_ignore_ascii_case("TIME") => {
                let CompiledExpr::Number(value) = value else {
                    return Err(compile_error(line, "TIME requires a numeric value"));
                };
                let result =
                    self.call_helper(builder, "strict_native_set_time", &[ctx, value, line_value]);
                self.guard_status(builder, result, error_block);
            }
            LValue::Variable(name) if name.ends_with('$') => {
                let CompiledExpr::String(value) = value else {
                    return Err(compile_error(
                        line,
                        format!("{name} requires a string value"),
                    ));
                };
                let target = *self.string_indices.get(name).ok_or_else(|| {
                    compile_error(line, format!("missing string slot for {name}"))
                })?;
                let target = iconst_i32(builder, target as i32);
                let result = self.call_helper(
                    builder,
                    "strict_native_set_string",
                    &[ctx, target, value, line_value],
                );
                self.guard_status(builder, result, error_block);
            }
            LValue::Variable(name) => {
                let CompiledExpr::Number(value) = value else {
                    return Err(compile_error(
                        line,
                        format!("{name} requires a numeric value"),
                    ));
                };
                self.store_number_variable(builder, name, value, line, numeric_slots)?;
            }
            LValue::ArrayElement(name, index) => {
                let array = *self
                    .array_indices
                    .get(name)
                    .ok_or_else(|| compile_error(line, format!("missing array slot for {name}")))?;
                let array = iconst_i32(builder, array as i32);
                let index = self.number_expression(
                    builder,
                    index,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let result = match value {
                    CompiledExpr::String(value) => {
                        if !name.ends_with('$') {
                            return Err(compile_error(line, "string assigned to a numeric array"));
                        }
                        self.call_helper(
                            builder,
                            "strict_native_array_set_string",
                            &[ctx, array, index, value, line_value],
                        )
                    }
                    CompiledExpr::Number(value) => {
                        if name.ends_with('$') {
                            return Err(compile_error(line, "number assigned to a string array"));
                        }
                        self.call_helper(
                            builder,
                            "strict_native_array_set_number",
                            &[ctx, array, index, value, line_value],
                        )
                    }
                };
                self.guard_status(builder, result, error_block);
            }
            LValue::Memory(width, address) => {
                let CompiledExpr::Number(value) = value else {
                    return Err(compile_error(line, "memory word requires a number"));
                };
                let address = self.number_expression(
                    builder,
                    address,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let width = iconst_i32(builder, memory_width_code(*width));
                let result = self.call_helper(
                    builder,
                    "strict_native_write_memory",
                    &[ctx, width, address, value, line_value],
                );
                self.guard_status(builder, result, error_block);
            }
            LValue::MemoryByteAt(address, offset) => {
                let CompiledExpr::Number(value) = value else {
                    return Err(compile_error(line, "memory byte requires a number"));
                };
                let address = self.number_expression(
                    builder,
                    address,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let offset = self.number_expression(
                    builder,
                    offset,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let width = iconst_i32(builder, 0);
                let result = self.call_helper(
                    builder,
                    "strict_native_write_memory_offset",
                    &[ctx, width, address, offset, value, line_value],
                );
                self.guard_status(builder, result, error_block);
            }
            LValue::MemoryOffset(width, address, offset) => {
                let CompiledExpr::Number(value) = value else {
                    return Err(compile_error(line, "memory write requires a number"));
                };
                let address = self.number_expression(
                    builder,
                    address,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let offset = self.number_expression(
                    builder,
                    offset,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let width = iconst_i32(builder, memory_width_code(*width));
                let result = self.call_helper(
                    builder,
                    "strict_native_write_memory_offset",
                    &[ctx, width, address, offset, value, line_value],
                );
                self.guard_status(builder, result, error_block);
            }
            LValue::MemoryString(address) => {
                let CompiledExpr::String(value) = value else {
                    return Err(compile_error(
                        line,
                        "memory string assignment requires a string",
                    ));
                };
                let address = self.number_expression(
                    builder,
                    address,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let result = self.call_helper(
                    builder,
                    "strict_native_write_string_memory",
                    &[ctx, address, value, line_value],
                );
                self.guard_status(builder, result, error_block);
            }
            LValue::StringSlice(name, start, length) => {
                let CompiledExpr::String(value) = value else {
                    return Err(compile_error(line, "string slice requires a string value"));
                };
                let target = *self.string_indices.get(name).ok_or_else(|| {
                    compile_error(line, format!("missing string slot for {name}"))
                })?;
                let target = iconst_i32(builder, target as i32);
                let start = self.number_expression(
                    builder,
                    start,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let length = self.number_expression(
                    builder,
                    length,
                    line,
                    routine,
                    blocks,
                    ctx,
                    numeric_slots,
                    error_block,
                )?;
                let result = self.call_helper(
                    builder,
                    "strict_native_string_slice_assign",
                    &[ctx, target, start, length, value, line_value],
                );
                self.guard_status(builder, result, error_block);
            }
            LValue::RecordField(_, _) | LValue::RecordPath(_) => {
                return Err(compile_error(
                    line,
                    "BASIC64 record field assignment is not supported by strict native lowering",
                ));
            }
        }
        Ok(())
    }

    fn lvalue_is_string(&self, target: &LValue) -> bool {
        match target {
            LValue::Variable(name) | LValue::StringSlice(name, _, _) => name.ends_with('$'),
            LValue::ArrayElement(name, _) => name.ends_with('$'),
            LValue::MemoryString(_) => true,
            _ => false,
        }
    }

    fn emit_input(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        target: &LValue,
        line_value: IrValue,
        line: u16,
        ctx: IrValue,
        error_block: cranelift_codegen::ir::Block,
    ) -> Result<(), String> {
        let (slot, is_string) = match target {
            LValue::Variable(name) if name.ends_with('$') => (
                *self.string_indices.get(name).ok_or_else(|| {
                    compile_error(line, format!("missing string slot for {name}"))
                })?,
                true,
            ),
            LValue::Variable(name) => (
                *self.numeric_indices.get(name).ok_or_else(|| {
                    compile_error(line, format!("missing numeric slot for {name}"))
                })?,
                false,
            ),
            _ => {
                return Err(compile_error(
                    line,
                    "INPUT target must be a scalar variable",
                ));
            }
        };
        let slot = iconst_i32(builder, slot as i32);
        let is_string = iconst_i32(builder, i32::from(is_string));
        let result = self.call_helper(
            builder,
            "strict_native_input",
            &[ctx, slot, is_string, line_value],
        );
        self.guard_status(builder, result, error_block);
        Ok(())
    }

    fn create_loop_slots(
        &self,
        builder: &mut FunctionBuilder<'_>,
        routine: &NativeRoutine,
    ) -> Result<HashMap<usize, LoopSlots>, String> {
        let mut result = HashMap::new();
        for (address, item) in self
            .program
            .instructions
            .iter()
            .enumerate()
            .take(routine.end)
            .skip(routine.start)
        {
            if matches!(item.statement, Statement::For { .. }) {
                if self
                    .for_pairs
                    .get(&address)
                    .is_none_or(|next| *next >= routine.end)
                {
                    return Err(compile_error(
                        item.line_number,
                        "FOR has no matching NEXT in this routine",
                    ));
                }
                let end = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                let step = builder.create_sized_stack_slot(StackSlotData::new(
                    StackSlotKind::ExplicitSlot,
                    8,
                    3,
                ));
                result.insert(address, LoopSlots { end, step });
            }
        }
        Ok(result)
    }

    fn call_helper(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        name: &'static str,
        args: &[IrValue],
    ) -> IrValue {
        let function = self
            .module
            .declare_func_in_func(self.helpers[name], builder.func);
        let call = builder.ins().call(function, args);
        builder.inst_results(call)[0]
    }

    fn guard_status(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        status: IrValue,
        error: cranelift_codegen::ir::Block,
    ) {
        let ok =
            builder
                .ins()
                .icmp_imm_s(cranelift_codegen::ir::condcodes::IntCC::NotEqual, status, 0);
        let continuation = builder.create_block();
        builder.ins().brif(ok, continuation, &[], error, &[]);
        builder.switch_to_block(continuation);
    }

    fn guard_context(
        &mut self,
        builder: &mut FunctionBuilder<'_>,
        ctx: IrValue,
        error: cranelift_codegen::ir::Block,
    ) {
        let status = self.call_helper(builder, "strict_native_context_ok", &[ctx]);
        self.guard_status(builder, status, error);
    }
}

fn contains_end_procedure(statement: &Statement) -> bool {
    match statement {
        Statement::EndProcedure => true,
        Statement::If(_, consequent, alternative) => consequent
            .iter()
            .chain(alternative)
            .any(contains_end_procedure),
        _ => false,
    }
}

fn compile_error(line: u16, message: impl AsRef<str>) -> String {
    if line == 0 {
        format!("BASICJIT strict compile: {}", message.as_ref())
    } else {
        format!(
            "BASICJIT strict compile at line {line}: {}",
            message.as_ref()
        )
    }
}

#[cfg(test)]
mod system_profile_boundary_tests {
    use super::*;

    #[test]
    fn strict_jit_reports_system_profile_types_as_unsupported_instead_of_miscompiling() {
        let mut program = ParsedProgram::default();
        program.system_types.insert(
            "FILEINFO".into(),
            super::super::parser::SystemTypeDefinition::Record { fields: Vec::new() },
        );
        let error = match StrictCompiler::new(&program, StrictJitOptions::default()) {
            Ok(_) => {
                panic!("System Profile records must not silently enter strict native compilation")
            }
            Err(error) => error,
        };
        assert!(
            error.contains("strict native compilation does not support BASIC64 System Profile")
        );
        assert!(error.contains("select the interpreter or Hybrid mode"));

        let mut typed = ParsedProgram::default();
        typed
            .typed_parameters
            .insert("ENTRY".into(), vec![super::super::parser::SystemType::Byte]);
        let error = match StrictCompiler::new(&typed, StrictJitOptions::default()) {
            Ok(_) => {
                panic!("typed System Profile parameters must not enter strict native compilation")
            }
            Err(error) => error,
        };
        assert!(error.contains("typed definitions"));

        let mut basic64 = ParsedProgram::default();
        basic64.options.mode = crate::configure::BasicLanguageMode::Basic64;
        basic64
            .instructions
            .push(super::super::parser::LocatedStatement {
                line_number: 10,
                statement: Statement::Assign(
                    LValue::Variable("VALUE".into()),
                    Expr::Integer(9_007_199_254_740_993),
                ),
            });
        let error = match StrictCompiler::new(&basic64, StrictJitOptions::default()) {
            Ok(_) => panic!("BASIC64 integer operations must not be lowered as f64"),
            Err(error) => error,
        };
        assert!(error.contains("BASIC64 System Profile typed assignment"));
        assert!(error.contains("select the interpreter or Hybrid mode"));
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn iconst_i32(builder: &mut FunctionBuilder<'_>, value: i32) -> IrValue {
    builder.ins().iconst(types::I32, i64::from(value))
}

fn numeric_slot_ptr(
    builder: &mut FunctionBuilder<'_>,
    base: IrValue,
    index: usize,
    pointer_type: cranelift_codegen::ir::Type,
) -> IrValue {
    let offset = builder.ins().iconst(pointer_type, (index * 8) as i64);
    builder.ins().iadd(base, offset)
}

fn coerce_numeric_slot(
    builder: &mut FunctionBuilder<'_>,
    value: IrValue,
    integer: bool,
) -> IrValue {
    if !integer {
        return value;
    }
    let low = builder.ins().f64const(f64::from(i32::MIN));
    let high = builder.ins().f64const(f64::from(i32::MAX));
    let zero = builder.ins().f64const(0.0);
    let is_nan = builder.ins().fcmp(FloatCC::Unordered, value, value);
    let below = builder.ins().fcmp(FloatCC::LessThan, value, low);
    let above = builder.ins().fcmp(FloatCC::GreaterThan, value, high);
    let clamped_low = builder.ins().select(below, low, value);
    let clamped = builder.ins().select(above, high, clamped_low);
    let truncated = builder.ins().trunc(clamped);
    builder.ins().select(is_nan, zero, truncated)
}

fn register_symbols(builder: &mut JITBuilder) {
    macro_rules! symbol {
        ($name:literal, $function:path) => {
            builder.symbol($name, $function as *const () as *const u8);
        };
    }
    symbol!(
        "strict_native_context_ok",
        native_runtime::strict_native_context_ok
    );
    symbol!("strict_native_enter", native_runtime::strict_native_enter);
    symbol!("strict_native_exit", native_runtime::strict_native_exit);
    symbol!("strict_native_tick", native_runtime::strict_native_tick);
    symbol!("strict_native_fail", native_runtime::strict_native_fail);
    symbol!(
        "strict_native_scope_begin",
        native_runtime::strict_native_scope_begin
    );
    symbol!(
        "strict_native_scope_save_number",
        native_runtime::strict_native_scope_save_number
    );
    symbol!(
        "strict_native_scope_save_string",
        native_runtime::strict_native_scope_save_string
    );
    symbol!(
        "strict_native_scope_restore",
        native_runtime::strict_native_scope_restore
    );
    symbol!(
        "strict_native_integer_operation",
        native_runtime::strict_native_integer_operation
    );
    symbol!("strict_native_divide", native_runtime::strict_native_divide);
    symbol!("strict_native_pow", native_runtime::strict_native_pow);
    symbol!("strict_native_time", native_runtime::strict_native_time);
    symbol!("strict_native_inkey", native_runtime::strict_native_inkey);
    symbol!(
        "strict_native_set_time",
        native_runtime::strict_native_set_time
    );
    symbol!(
        "strict_native_set_string",
        native_runtime::strict_native_set_string
    );
    symbol!(
        "strict_native_copy_string",
        native_runtime::strict_native_copy_string
    );
    symbol!(
        "strict_native_string_concat",
        native_runtime::strict_native_string_concat
    );
    symbol!(
        "strict_native_string_compare",
        native_runtime::strict_native_string_compare
    );
    symbol!(
        "strict_native_string_len",
        native_runtime::strict_native_string_len
    );
    symbol!(
        "strict_native_string_asc",
        native_runtime::strict_native_string_asc
    );
    symbol!(
        "strict_native_string_left",
        native_runtime::strict_native_string_left
    );
    symbol!(
        "strict_native_string_right",
        native_runtime::strict_native_string_right
    );
    symbol!(
        "strict_native_string_mid",
        native_runtime::strict_native_string_mid
    );
    symbol!(
        "strict_native_string_str",
        native_runtime::strict_native_string_str
    );
    symbol!(
        "strict_native_string_chr",
        native_runtime::strict_native_string_chr
    );
    symbol!(
        "strict_native_string_string",
        native_runtime::strict_native_string_string
    );
    symbol!(
        "strict_native_string_val",
        native_runtime::strict_native_string_val
    );
    symbol!(
        "strict_native_string_instr",
        native_runtime::strict_native_string_instr
    );
    symbol!(
        "strict_native_number_builtin",
        native_runtime::strict_native_number_builtin
    );
    symbol!("strict_native_random", native_runtime::strict_native_random);
    symbol!(
        "strict_native_array_dim",
        native_runtime::strict_native_array_dim
    );
    symbol!(
        "strict_native_array_get_number",
        native_runtime::strict_native_array_get_number
    );
    symbol!(
        "strict_native_array_get_string",
        native_runtime::strict_native_array_get_string
    );
    symbol!(
        "strict_native_array_set_number",
        native_runtime::strict_native_array_set_number
    );
    symbol!(
        "strict_native_array_set_string",
        native_runtime::strict_native_array_set_string
    );
    symbol!(
        "strict_native_data_read_number",
        native_runtime::strict_native_data_read_number
    );
    symbol!(
        "strict_native_data_read_string",
        native_runtime::strict_native_data_read_string
    );
    symbol!(
        "strict_native_data_restore",
        native_runtime::strict_native_data_restore
    );
    symbol!(
        "strict_native_print_number",
        native_runtime::strict_native_print_number
    );
    symbol!(
        "strict_native_print_string",
        native_runtime::strict_native_print_string
    );
    symbol!(
        "strict_native_print_spaces",
        native_runtime::strict_native_print_spaces
    );
    symbol!(
        "strict_native_print_comma",
        native_runtime::strict_native_print_comma
    );
    symbol!(
        "strict_native_print_newline",
        native_runtime::strict_native_print_newline
    );
    symbol!(
        "strict_native_print_tab",
        native_runtime::strict_native_print_tab
    );
    symbol!(
        "strict_native_print_format",
        native_runtime::strict_native_print_format
    );
    symbol!("strict_native_call", native_runtime::strict_native_call);
    symbol!("strict_native_star", native_runtime::strict_native_star);
    symbol!(
        "strict_native_read_memory",
        native_runtime::strict_native_read_memory
    );
    symbol!(
        "strict_native_write_memory",
        native_runtime::strict_native_write_memory
    );
    symbol!(
        "strict_native_write_memory_offset",
        native_runtime::strict_native_write_memory_offset
    );
    symbol!(
        "strict_native_write_string_memory",
        native_runtime::strict_native_write_string_memory
    );
    symbol!("strict_native_input", native_runtime::strict_native_input);
    symbol!(
        "strict_native_string_slice_assign",
        native_runtime::strict_native_string_slice_assign
    );
}

fn declare_helpers<M: Module>(
    module: &mut M,
    pointer_type: cranelift_codegen::ir::Type,
) -> Result<HashMap<&'static str, FuncId>, String> {
    let p = pointer_type;
    let i = types::I32;
    let f = types::F64;
    let specs: Vec<(
        &'static str,
        Vec<cranelift_codegen::ir::Type>,
        cranelift_codegen::ir::Type,
    )> = vec![
        ("strict_native_context_ok", vec![p], i),
        ("strict_native_enter", vec![p, i], i),
        ("strict_native_exit", vec![p, i], i),
        ("strict_native_tick", vec![p, i], i),
        ("strict_native_fail", vec![p, i, i], i),
        ("strict_native_scope_begin", vec![p, i], i),
        ("strict_native_scope_save_number", vec![p, i, i], i),
        ("strict_native_scope_save_string", vec![p, i, i], i),
        ("strict_native_scope_restore", vec![p, i, i], i),
        ("strict_native_integer_operation", vec![p, i, f, f, i], f),
        ("strict_native_divide", vec![p, f, f, i], f),
        ("strict_native_pow", vec![p, f, f, i], f),
        ("strict_native_time", vec![p, i], f),
        ("strict_native_inkey", vec![p, i, i], f),
        ("strict_native_set_time", vec![p, f, i], i),
        ("strict_native_set_string", vec![p, i, i, i], i),
        ("strict_native_copy_string", vec![p, i, i, i], i),
        ("strict_native_string_concat", vec![p, i, i, i, i], i),
        ("strict_native_string_compare", vec![p, i, i, i, i], f),
        ("strict_native_string_len", vec![p, i, i], f),
        ("strict_native_string_asc", vec![p, i, i], f),
        ("strict_native_string_left", vec![p, i, f, i, i], i),
        ("strict_native_string_right", vec![p, i, f, i, i], i),
        ("strict_native_string_mid", vec![p, i, f, f, i, i, i], i),
        ("strict_native_string_str", vec![p, f, i, i], i),
        ("strict_native_string_chr", vec![p, f, i, i], i),
        ("strict_native_string_string", vec![p, f, i, i, i], i),
        ("strict_native_string_val", vec![p, i, i], f),
        ("strict_native_string_instr", vec![p, i, i, f, i, i], f),
        ("strict_native_number_builtin", vec![p, i, f, i], f),
        ("strict_native_random", vec![p, i, f, i], f),
        ("strict_native_array_dim", vec![p, i, f, i, i], i),
        ("strict_native_array_get_number", vec![p, i, f, i], f),
        ("strict_native_array_get_string", vec![p, i, f, i, i], i),
        ("strict_native_array_set_number", vec![p, i, f, f, i], i),
        ("strict_native_array_set_string", vec![p, i, f, i, i], i),
        ("strict_native_data_read_number", vec![p, i], f),
        ("strict_native_data_read_string", vec![p, i, i], i),
        ("strict_native_data_restore", vec![p, i, i], i),
        ("strict_native_print_number", vec![p, f, i], i),
        ("strict_native_print_string", vec![p, i, i], i),
        ("strict_native_print_spaces", vec![p, f, i], i),
        ("strict_native_print_comma", vec![p, i], i),
        ("strict_native_print_newline", vec![p, i], i),
        ("strict_native_print_tab", vec![p, f, f, i], i),
        ("strict_native_print_format", vec![p, f, i], i),
        ("strict_native_call", vec![p, f, i], i),
        ("strict_native_star", vec![p, i, i], i),
        ("strict_native_read_memory", vec![p, i, f, i], f),
        ("strict_native_write_memory", vec![p, i, f, f, i], i),
        (
            "strict_native_write_memory_offset",
            vec![p, i, f, f, f, i],
            i,
        ),
        ("strict_native_write_string_memory", vec![p, f, i, i], i),
        ("strict_native_input", vec![p, i, i, i], i),
        (
            "strict_native_string_slice_assign",
            vec![p, i, f, f, i, i],
            i,
        ),
    ];
    let mut result = HashMap::new();
    for (name, parameters, return_type) in specs {
        let mut signature = module.make_signature();
        signature
            .params
            .extend(parameters.into_iter().map(AbiParam::new));
        signature.returns.push(AbiParam::new(return_type));
        let id = module
            .declare_function(name, Linkage::Import, &signature)
            .map_err(|error| error.to_string())?;
        result.insert(name, id);
    }
    Ok(result)
}

#[derive(Default)]
struct ProgramScan {
    numbers: BTreeSet<String>,
    strings: BTreeSet<String>,
    arrays: BTreeSet<String>,
    string_constants: BTreeSet<Vec<u8>>,
}

fn collect_slots(program: &ParsedProgram) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut scan = ProgramScan::default();
    for item in &program.instructions {
        scan_statement(&item.statement, &mut scan);
    }
    for definition in program
        .procedures
        .values()
        .chain(program.functions.values())
    {
        for name in &definition.parameters {
            scan_name(name, &mut scan);
        }
    }
    for name in ["A%", "X%", "Y%", "C%"] {
        scan_name(name, &mut scan);
    }
    (
        scan.numbers.into_iter().collect(),
        scan.strings.into_iter().collect(),
        scan.arrays.into_iter().collect(),
    )
}

fn collect_string_constants(program: &ParsedProgram) -> Vec<Vec<u8>> {
    let mut scan = ProgramScan::default();
    for item in &program.instructions {
        scan_statement(&item.statement, &mut scan);
    }
    scan.string_constants.into_iter().collect()
}

fn scan_name(name: &str, scan: &mut ProgramScan) {
    if name.eq_ignore_ascii_case("TIME") {
        return;
    }
    if name.ends_with('$') {
        scan.strings.insert(name.to_owned());
    } else {
        scan.numbers.insert(name.to_owned());
    }
}

fn scan_expression(expression: &Expr, scan: &mut ProgramScan) {
    match expression {
        Expr::Number(_) | Expr::Integer(_) => {}
        Expr::String(value) => {
            scan.string_constants.insert(value.clone());
        }
        Expr::Variable(name) => scan_name(name, scan),
        Expr::ArrayElement(name, index) => {
            scan.arrays.insert(name.clone());
            scan_expression(index, scan);
        }
        Expr::Unary(_, operand) => scan_expression(operand, scan),
        Expr::Binary(left, _, right) => {
            scan_expression(left, scan);
            scan_expression(right, scan);
        }
        Expr::Builtin(_, arguments) | Expr::UserFunction(_, arguments) => {
            for argument in arguments {
                scan_expression(argument, scan);
            }
        }
        Expr::ImportedFunction { arguments, .. } => {
            for argument in arguments {
                scan_expression(argument, scan);
            }
        }
        Expr::MemoryRead(_, address) => scan_expression(address, scan),
        Expr::Member(base, _) => scan_expression(base, scan),
    }
}

fn scan_lvalue(target: &LValue, scan: &mut ProgramScan) {
    match target {
        LValue::Variable(name) => scan_name(name, scan),
        LValue::ArrayElement(name, index) => {
            scan.arrays.insert(name.clone());
            scan_expression(index, scan);
        }
        LValue::Memory(_, address) | LValue::MemoryString(address) => {
            scan_expression(address, scan)
        }
        LValue::MemoryByteAt(address, offset) | LValue::MemoryOffset(_, address, offset) => {
            scan_expression(address, scan);
            scan_expression(offset, scan);
        }
        LValue::StringSlice(name, start, length) => {
            scan_name(name, scan);
            scan_expression(start, scan);
            scan_expression(length, scan);
        }
        LValue::RecordField(name, _) => scan_name(name, scan),
        LValue::RecordPath(path) => {
            if let Some(name) = path.first() {
                scan_name(name, scan);
            }
        }
    }
}

fn scan_statement(statement: &Statement, scan: &mut ProgramScan) {
    match statement {
        Statement::Assign(target, value) => {
            scan_lvalue(target, scan);
            scan_expression(value, scan);
        }
        Statement::Input(target) => scan_lvalue(target, scan),
        Statement::Print(items) => {
            for item in items {
                match item {
                    PrintItem::Value(value) | PrintItem::Spaces(value) => {
                        scan_expression(value, scan)
                    }
                    PrintItem::Tab(x, y) => {
                        scan_expression(x, scan);
                        scan_expression(y, scan);
                    }
                    PrintItem::Comma | PrintItem::Semicolon | PrintItem::NewLine => {}
                }
            }
        }
        Statement::Colour(values) => {
            for value in values {
                scan_expression(value, scan);
            }
        }
        Statement::PrintFormat(value)
        | Statement::Mode(value)
        | Statement::Call(value)
        | Statement::Until(value)
        | Statement::FunctionReturn(value) => scan_expression(value, scan),
        Statement::Vdu(arguments) => {
            for argument in arguments {
                scan_expression(&argument.value, scan);
            }
        }
        Statement::Line(a, b, c, d) => {
            for value in [a, b, c, d] {
                scan_expression(value, scan);
            }
        }
        Statement::Move(x, y) | Statement::Draw(x, y) | Statement::Gcol(x, y) => {
            scan_expression(x, scan);
            scan_expression(y, scan);
        }
        Statement::Plot(code, x, y) => {
            scan_expression(code, scan);
            scan_expression(x, scan);
            scan_expression(y, scan);
        }
        Statement::If(condition, consequent, alternative) => {
            scan_expression(condition, scan);
            for nested in consequent.iter().chain(alternative) {
                scan_statement(nested, scan);
            }
        }
        Statement::IfBlock(condition) => scan_expression(condition, scan),
        Statement::Dim(declarations) => {
            for declaration in declarations {
                scan.arrays.insert(declaration.name.clone());
                if declaration.byte_block {
                    scan_name(&declaration.name, scan);
                }
                for dimension in &declaration.dimensions {
                    scan_expression(dimension, scan);
                }
            }
        }
        Statement::Read(targets) => {
            for target in targets {
                scan_lvalue(target, scan);
            }
        }
        Statement::Data(values) => {
            for value in values {
                scan_expression(value, scan);
            }
        }
        Statement::For {
            variable,
            start,
            end,
            step,
        } => {
            scan_name(variable, scan);
            scan_expression(start, scan);
            scan_expression(end, scan);
            if let Some(step) = step {
                scan_expression(step, scan);
            }
        }
        Statement::Next(variable) => {
            if let Some(variable) = variable {
                scan_name(variable, scan);
            }
        }
        Statement::ProcedureCall(_, arguments) => {
            for argument in arguments {
                scan_expression(argument, scan);
            }
        }
        Statement::ImportedProcedureCall { arguments, .. } => {
            for argument in arguments {
                scan_expression(argument, scan);
            }
        }
        Statement::LocalReadOnly {
            name,
            value_type: _,
            value,
        } => {
            scan_name(name, scan);
            scan_expression(value, scan);
        }
        Statement::DefineProcedure(_, parameters) | Statement::DefineFunction(_, parameters) => {
            for name in parameters {
                scan_name(name, scan);
            }
        }
        Statement::Sys {
            name,
            arguments,
            results,
            flags,
        } => {
            scan.string_constants.insert(name.clone());
            for argument in arguments.iter().flatten() {
                scan_expression(argument, scan);
            }
            for result in results {
                scan_name(result, scan);
            }
            if let Some(flags) = flags {
                scan_name(flags, scan);
            }
        }
        Statement::PrimitiveCall {
            name,
            arguments,
            results,
        } => {
            scan.string_constants.insert(name.as_bytes().to_vec());
            for argument in arguments.iter().flatten() {
                scan_expression(argument, scan);
            }
            for result in results {
                scan_name(result, scan);
            }
        }
        Statement::StarCommand(command) => {
            scan.string_constants.insert(command.clone());
        }
        Statement::ClearScreen
        | Statement::ClearGraphics
        | Statement::Goto(_)
        | Statement::Gosub(_)
        | Statement::Restore(_)
        | Statement::Repeat
        | Statement::Return
        | Statement::EndProcedure
        | Statement::End
        | Statement::EndIf
        | Statement::Try
        | Statement::Catch { .. }
        | Statement::EndTry
        | Statement::Throw { .. }
        | Statement::NoOp => {}
    }
}

fn collect_data(program: &ParsedProgram) -> Result<Vec<(u16, NativeValue)>, String> {
    let mut data = Vec::new();
    for item in &program.instructions {
        if let Statement::Data(values) = &item.statement {
            for value in values {
                let value = match value {
                    Expr::Number(value) => NativeValue::Number(*value),
                    Expr::String(value) => NativeValue::String(value.clone()),
                    _ => {
                        return Err(compile_error(
                            item.line_number,
                            "DATA values must be literal numbers or strings in strict native mode",
                        ));
                    }
                };
                data.push((item.line_number, value));
            }
        }
    }
    Ok(data)
}

fn index_names(names: &[String]) -> HashMap<String, usize> {
    names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.clone(), index))
        .collect()
}

fn pair_for_loops(program: &ParsedProgram) -> Result<HashMap<usize, usize>, String> {
    let mut stack = Vec::new();
    let mut pairs = HashMap::new();
    for (address, item) in program.instructions.iter().enumerate() {
        match item.statement {
            Statement::For { .. } => stack.push(address),
            Statement::Next(_) => {
                let start = stack
                    .pop()
                    .ok_or_else(|| compile_error(item.line_number, "NEXT has no matching FOR"))?;
                pairs.insert(start, address);
            }
            _ => {}
        }
    }
    if let Some(start) = stack.pop() {
        return Err(compile_error(
            program.instructions[start].line_number,
            "FOR has no matching NEXT",
        ));
    }
    Ok(pairs)
}

fn pair_repeat_loops(program: &ParsedProgram) -> Result<HashMap<usize, usize>, String> {
    let mut stack = Vec::new();
    let mut pairs = HashMap::new();
    for (address, item) in program.instructions.iter().enumerate() {
        match item.statement {
            Statement::Repeat => stack.push(address),
            Statement::Until(_) => {
                let start = stack.pop().ok_or_else(|| {
                    compile_error(item.line_number, "UNTIL has no matching REPEAT")
                })?;
                pairs.insert(address, start);
            }
            _ => {}
        }
    }
    if let Some(start) = stack.pop() {
        return Err(compile_error(
            program.instructions[start].line_number,
            "REPEAT has no matching UNTIL",
        ));
    }
    Ok(pairs)
}

fn pair_if_blocks(program: &ParsedProgram) -> Result<HashMap<usize, usize>, String> {
    let mut stack = Vec::new();
    let mut pairs = HashMap::new();
    for (address, item) in program.instructions.iter().enumerate() {
        match item.statement {
            Statement::IfBlock(_) => stack.push(address),
            Statement::EndIf => {
                let start = stack
                    .pop()
                    .ok_or_else(|| compile_error(item.line_number, "ENDIF has no matching IF"))?;
                pairs.insert(start, address);
            }
            _ => {}
        }
    }
    if let Some(start) = stack.pop() {
        return Err(compile_error(
            program.instructions[start].line_number,
            "IF has no matching ENDIF",
        ));
    }
    Ok(pairs)
}

fn memory_width_code(width: MemoryWidth) -> i32 {
    match width {
        MemoryWidth::Byte => 0,
        MemoryWidth::Word => 1,
    }
}

fn is_comparison(operator: BinaryOp) -> bool {
    matches!(
        operator,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    )
}

fn comparison_code(operator: BinaryOp) -> Option<i32> {
    Some(match operator {
        BinaryOp::Equal => 0,
        BinaryOp::NotEqual => 1,
        BinaryOp::Less => 2,
        BinaryOp::LessEqual => 3,
        BinaryOp::Greater => 4,
        BinaryOp::GreaterEqual => 5,
        _ => return None,
    })
}

fn comparison_float(
    builder: &mut FunctionBuilder<'_>,
    operator: BinaryOp,
    left: IrValue,
    right: IrValue,
) -> IrValue {
    let condition = builder.ins().fcmp(
        match operator {
            BinaryOp::Equal => FloatCC::UnorderedOrEqual,
            BinaryOp::NotEqual => FloatCC::OrderedNotEqual,
            BinaryOp::Less => FloatCC::LessThan,
            BinaryOp::LessEqual => FloatCC::UnorderedOrLessThanOrEqual,
            BinaryOp::Greater => FloatCC::GreaterThan,
            BinaryOp::GreaterEqual => FloatCC::UnorderedOrGreaterThanOrEqual,
            _ => unreachable!("comparison_float only receives comparisons"),
        },
        left,
        right,
    );
    let yes = builder.ins().f64const(1.0);
    let no = builder.ins().f64const(0.0);
    builder.ins().select(condition, yes, no)
}
