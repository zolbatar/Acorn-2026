//! Portable, source-located high-level IR for BASIC64 System Profile modules.
//!
//! The IR is backend-neutral data: it contains no Rust pointers, borrowed
//! interpreter state, or executable-code addresses. Its typed operations are
//! the semantic boundary for the reference interpreter and future compilers.
//! The current interpreter adapter lowers the IR's reference payload back to
//! the shared BASIC statement engine. Native module lowering is intentionally
//! rejected until a backend preserves every typed operation's invariant.

use std::collections::BTreeMap;

use crate::trellis::{
    DefinitionDescriptor, ModuleManifest, PrimitiveImport, RUNTIME_ABI_VERSION, RegisterKind,
    SemanticVersion,
};
use crate::{configure::BasicLanguageMode, graphics::GraphicsProfile};

use super::parser::{
    BinaryOp, Definition, DimDeclaration, Expr, LValue, LocatedStatement, MemoryWidth,
    ParsedProgram, PrintItem, ProgramOptions, Statement, SystemField, SystemType,
    SystemTypeDefinition, UnaryOp, VduArgument, VduFormat,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrBackend {
    Interpreter,
    HybridJit,
    StrictJit,
    Aot,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemIrSourceLocation {
    pub path: String,
    pub line: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemIrValueType {
    BasicNumber,
    BasicString,
    ExactInteger,
    System(SystemType),
    ImportedSymbolResult { module: String, symbol: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrStorage {
    ModuleWorkspace,
    Parameter,
    Register,
    Local,
    CallerMemory,
    ManagedRecord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrVisibility {
    Exported,
    Private,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrStoreRule {
    Mutable,
    InitializeOnce,
    ReadOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrUnaryOperator {
    Positive,
    Negative,
    Not,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrBinaryOperator {
    Add,
    Subtract,
    Multiply,
    Divide,
    IntegerDivide,
    Modulo,
    Power,
    ShiftLeft,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrMemoryWidth {
    Byte,
    Word,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrLanguageMode {
    Classic,
    Basic64,
    Hybrid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrGraphicsTarget {
    Hosted,
    Agon,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SystemIrTextProfile {
    #[default]
    Classic,
    Modern,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrVduFormat {
    Byte,
    Word,
    Padded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemIrBuiltin {
    Abs,
    Asc,
    Cos,
    Chr,
    Int,
    Inkey,
    Instr,
    Left,
    Length,
    Mid,
    NaturalLog,
    Log10,
    Random,
    Right,
    Sin,
    SquareRoot,
    String,
    Stringify,
    Tan,
    Val,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemIrProgramOptions {
    pub mode: SystemIrLanguageMode,
    pub target: SystemIrGraphicsTarget,
    pub profile: Option<String>,
    pub text_profile: SystemIrTextProfile,
    pub mode_declared: bool,
    pub target_declared: bool,
    pub profile_declared: bool,
    pub text_profile_declared: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemIrExpressionKind {
    NumberLiteral(u64),
    ExactIntegerLiteral(i128),
    StringLiteral(Vec<u8>),
    LoadBinding {
        name: String,
        storage: SystemIrStorage,
    },
    ArrayElement {
        name: String,
    },
    Unary {
        operator: SystemIrUnaryOperator,
    },
    NumericBinary {
        operator: SystemIrBinaryOperator,
    },
    FlagsCombine {
        operator: SystemIrBinaryOperator,
        left_type: String,
        right_type: String,
    },
    NominalCompare {
        operator: SystemIrBinaryOperator,
        left_type: SystemIrValueType,
        right_type: SystemIrValueType,
    },
    CheckedAddressOffset {
        operator: SystemIrBinaryOperator,
        width_bits: u8,
        owner: AddressOwner,
    },
    Builtin {
        builtin: SystemIrBuiltin,
    },
    CallFunction {
        name: String,
    },
    ImportedFunctionCall {
        module: String,
        name: String,
    },
    CheckedLogicalMemoryRead {
        width: SystemIrMemoryWidth,
        owner: AddressOwner,
    },
    RecordMember {
        name: String,
    },
    EnumConstant {
        type_name: String,
        member: String,
    },
    FlagsConstant {
        type_name: String,
        member: String,
    },
    DynamicMember {
        name: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressOwner {
    InvokingTask,
}

/// An expression node carries both a typed, backend-neutral operation and the
/// parser expression used by the reference-adapter. Address operations have
/// explicit task ownership and checked width in this representation.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemIrExpression {
    pub source: SystemIrSourceLocation,
    pub value_type: SystemIrValueType,
    pub kind: SystemIrExpressionKind,
    pub children: Vec<SystemIrExpression>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SystemIrPlaceKind {
    Binding {
        name: String,
        storage: SystemIrStorage,
    },
    ArrayElement {
        name: String,
        index: SystemIrExpression,
    },
    StringSlice {
        name: String,
        start: SystemIrExpression,
        length: SystemIrExpression,
    },
    RecordField {
        path: Vec<String>,
    },
    CheckedLogicalMemory {
        width: SystemIrMemoryWidth,
        owner: AddressOwner,
        address: SystemIrExpression,
    },
    CheckedLogicalMemoryOffset {
        width: SystemIrMemoryWidth,
        owner: AddressOwner,
        base: SystemIrExpression,
        offset: SystemIrExpression,
    },
    CheckedLogicalMemoryByteAt {
        owner: AddressOwner,
        base: SystemIrExpression,
        offset: SystemIrExpression,
    },
    CheckedLogicalMemoryString {
        owner: AddressOwner,
        address: SystemIrExpression,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemIrPlace {
    pub source: SystemIrSourceLocation,
    pub value_type: SystemIrValueType,
    pub kind: SystemIrPlaceKind,
    pub store_rule: SystemIrStoreRule,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SystemIrPrintItem {
    Value(SystemIrExpression),
    Spaces(SystemIrExpression),
    Tab(SystemIrExpression, SystemIrExpression),
    Comma,
    Semicolon,
    NewLine,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemIrVduArgument {
    pub value: SystemIrExpression,
    pub format: SystemIrVduFormat,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemIrDimDeclaration {
    pub name: String,
    pub dimensions: Vec<SystemIrExpression>,
    pub byte_block: bool,
}

/// Complete, portable payload for the traditional BASIC statements admitted
/// into a System Profile module. There is no parser-AST pointer/payload in the
/// IR; backends consume this data directly and the reference adapter can
/// reconstruct the shared interpreter form from it.
#[derive(Clone, Debug, PartialEq)]
pub enum SystemIrBasicStatement {
    Assign {
        target: SystemIrPlace,
        value: SystemIrExpression,
    },
    LocalReadOnlyDeclaration {
        name: String,
        value_type: SystemType,
        value: SystemIrExpression,
    },
    ProcedureCall {
        name: String,
        linkage: SystemIrCallLinkage,
        arguments: Vec<SystemIrExpression>,
    },
    ImportedProcedureCall {
        module: String,
        name: String,
        arguments: Vec<SystemIrExpression>,
    },
    PrimitiveCall {
        name: String,
        capability: Option<String>,
        arguments: Vec<Option<SystemIrExpression>>,
        results: Vec<String>,
    },
    Input(SystemIrPlace),
    Print(Vec<SystemIrPrintItem>),
    ClearScreen,
    ClearGraphics,
    Colour(Vec<SystemIrExpression>),
    PrintFormat(SystemIrExpression),
    Mode(SystemIrExpression),
    Vdu(Vec<SystemIrVduArgument>),
    Line([SystemIrExpression; 4]),
    Move(SystemIrExpression, SystemIrExpression),
    Draw(SystemIrExpression, SystemIrExpression),
    Plot(SystemIrExpression, SystemIrExpression, SystemIrExpression),
    Gcol(SystemIrExpression, SystemIrExpression),
    If {
        condition: SystemIrExpression,
        then_body: Vec<SystemIrBasicStatement>,
        else_body: Vec<SystemIrBasicStatement>,
    },
    IfBlock(SystemIrExpression),
    EndIf,
    Goto(u16),
    Gosub(u16),
    Dim(Vec<SystemIrDimDeclaration>),
    Read(Vec<SystemIrPlace>),
    Data(Vec<SystemIrExpression>),
    Restore(Option<u16>),
    For {
        variable: String,
        start: SystemIrExpression,
        end: SystemIrExpression,
        step: Option<SystemIrExpression>,
    },
    Next(Option<String>),
    Repeat,
    Until(SystemIrExpression),
    DefineProcedure {
        name: String,
        parameters: Vec<String>,
    },
    DefineFunction {
        name: String,
        parameters: Vec<String>,
    },
    Sys {
        name: Vec<u8>,
        arguments: Vec<Option<SystemIrExpression>>,
        results: Vec<String>,
        flags: Option<String>,
    },
    Try,
    Catch {
        binding: String,
        error_type: String,
    },
    EndTry,
    Throw {
        error_type: String,
        code: SystemIrExpression,
        message: SystemIrExpression,
    },
    FunctionReturn(SystemIrExpression),
    Return,
    EndProcedure,
    End,
    Call(SystemIrExpression),
    StarCommand(Vec<u8>),
    NoOp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SystemIrCallLinkage {
    ModuleLocal,
    Imported { module: String },
    Primitive { capability: String },
    Dynamic,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SystemIrOpcode {
    Assign {
        target: SystemIrPlace,
        value: SystemIrExpression,
    },
    LocalReadOnlyDeclaration {
        name: String,
        value_type: SystemIrValueType,
        value: SystemIrExpression,
    },
    ProcedureCall {
        name: String,
        linkage: SystemIrCallLinkage,
        arguments: Vec<SystemIrExpression>,
    },
    ImportedProcedureCall {
        module: String,
        name: String,
        arguments: Vec<SystemIrExpression>,
    },
    PrimitiveCall {
        name: String,
        capability: Option<String>,
        arguments: Vec<Option<SystemIrExpression>>,
        result_bindings: Vec<String>,
    },
    BeginTry,
    Catch {
        binding: String,
        error_type: String,
    },
    EndTry,
    Throw {
        error_type: String,
        code: SystemIrExpression,
        message: SystemIrExpression,
    },
    FunctionReturn {
        value: SystemIrExpression,
    },
    Basic {
        statement: SystemIrBasicStatement,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemIrInstruction {
    pub source: SystemIrSourceLocation,
    pub operation: SystemIrOpcode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemIrParameter {
    pub name: String,
    pub value_type: SystemIrValueType,
    pub source: SystemIrSourceLocation,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemIrDefinition {
    pub descriptor: DefinitionDescriptor,
    pub source: SystemIrSourceLocation,
    pub visibility: SystemIrVisibility,
    pub parameters: Vec<SystemIrParameter>,
    pub result: Option<SystemType>,
    pub throws: Option<String>,
    pub entry_index: usize,
    pub function: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SystemIrRoutine {
    pub entry_index: usize,
    pub parameters: Vec<String>,
    pub function: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemIrExecutionPlan {
    pub backend: SystemIrBackend,
    pub source_path: String,
    pub source_hash: String,
    pub instruction_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemIrLoweringError {
    pub backend: SystemIrBackend,
    pub source: SystemIrSourceLocation,
    pub operation: String,
}

impl std::fmt::Display for SystemIrLoweringError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let subject = if self.operation == "typed module definition" {
            "typed definitions"
        } else {
            &self.operation
        };
        let source = if self.source.line == 0 {
            self.source.path.clone()
        } else {
            format!("{}:{}", self.source.path, self.source.line)
        };
        match self.backend {
            SystemIrBackend::Interpreter => write!(
                formatter,
                "reference interpreter could not lower BASIC64 System Profile {} at {source}",
                self.operation
            ),
            SystemIrBackend::HybridJit => write!(
                formatter,
                "hybrid JIT does not lower BASIC64 System Profile {subject} at {source}; this is interpreter-only and Hybrid falls back"
            ),
            SystemIrBackend::StrictJit => write!(
                formatter,
                "strict native compilation does not support BASIC64 System Profile {subject} at {source}; select the interpreter or Hybrid mode"
            ),
            SystemIrBackend::Aot => write!(
                formatter,
                "AOT compilation does not support BASIC64 System Profile {subject} at {source}; use the reference interpreter"
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PortableSystemIr {
    /// The validated source manifest, including public exports, lifecycle,
    /// capability requests, replacement policy and link metadata.
    pub manifest: Option<ModuleManifest>,
    pub module_name: String,
    pub module_version: SemanticVersion,
    pub source_path: String,
    pub source_hash: String,
    /// Dependency names and minimum versions declared by this source unit.
    /// The registry resolves these to full source fingerprints when preparing
    /// an invocation or derived target.
    pub declared_dependencies: Vec<(String, SemanticVersion)>,
    pub language_profile: String,
    pub target_profile: String,
    pub runtime_abi: u32,
    pub type_definitions: BTreeMap<String, SystemTypeDefinition>,
    pub workspace: BTreeMap<String, SystemIrValueType>,
    pub readonly_workspace: std::collections::BTreeSet<String>,
    pub readonly_local_bindings: BTreeMap<String, BTreeMap<String, SystemType>>,
    pub routines: BTreeMap<String, SystemIrRoutine>,
    pub typed_parameters: BTreeMap<String, Vec<SystemType>>,
    pub typed_results: BTreeMap<String, SystemType>,
    pub throws_types: BTreeMap<String, String>,
    pub options: SystemIrProgramOptions,
    pub line_entries: BTreeMap<u16, usize>,
    pub definitions: BTreeMap<String, SystemIrDefinition>,
    pub instructions: Vec<SystemIrInstruction>,
}

impl PortableSystemIr {
    pub(crate) fn lower_module(
        manifest: &ModuleManifest,
        descriptors: &BTreeMap<String, DefinitionDescriptor>,
        program: &ParsedProgram,
    ) -> Self {
        Self::lower(
            Some(manifest.clone()),
            manifest.name.clone(),
            manifest.version,
            manifest.source_path.clone(),
            manifest.source_hash.clone(),
            manifest.dependencies.clone(),
            manifest.language_profile.clone(),
            manifest.target_profile.clone(),
            &manifest.primitive_imports,
            &manifest
                .exports
                .iter()
                .map(|export| {
                    (
                        export.definition_name.to_ascii_uppercase(),
                        export.contract.registers.clone(),
                    )
                })
                .collect(),
            descriptors,
            program,
        )
    }

    fn lower(
        manifest: Option<ModuleManifest>,
        module_name: String,
        module_version: SemanticVersion,
        source_path: String,
        source_hash: String,
        declared_dependencies: Vec<(String, SemanticVersion)>,
        language_profile: String,
        target_profile: String,
        primitive_imports: &[PrimitiveImport],
        register_contracts: &BTreeMap<String, Vec<crate::trellis::RegisterContract>>,
        descriptors: &BTreeMap<String, DefinitionDescriptor>,
        program: &ParsedProgram,
    ) -> Self {
        let imports = primitive_imports
            .iter()
            .map(|import| {
                (
                    import.name.to_ascii_uppercase(),
                    import.capability.as_str().to_owned(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut definitions = BTreeMap::new();
        for (key, descriptor) in descriptors {
            let (name, parameters, is_function) = if let Some(name) = key.strip_prefix("FN:") {
                let definition = program.functions.get(name);
                (name, definition, true)
            } else {
                (key.as_str(), program.procedures.get(key), false)
            };
            let Some(definition) = parameters else {
                continue;
            };
            let source_line = definition
                .entry
                .checked_sub(1)
                .and_then(|index| program.instructions.get(index))
                .map_or(0, |instruction| instruction.line_number);
            let source = SystemIrSourceLocation {
                path: source_path.clone(),
                line: source_line,
            };
            let parameter_types = program.typed_parameters.get(name);
            let parameters = definition
                .parameters
                .iter()
                .enumerate()
                .map(|(index, parameter)| SystemIrParameter {
                    name: parameter.clone(),
                    value_type: parameter_types
                        .and_then(|types| types.get(index))
                        .cloned()
                        .map(SystemIrValueType::System)
                        .unwrap_or_else(|| inferred_binding_type(parameter)),
                    source: source.clone(),
                })
                .collect();
            definitions.insert(
                key.clone(),
                SystemIrDefinition {
                    descriptor: descriptor.clone(),
                    source,
                    visibility: if manifest.as_ref().is_some_and(|manifest| {
                        manifest
                            .symbol_exports
                            .iter()
                            .any(|symbol| symbol.eq_ignore_ascii_case(key))
                    }) {
                        SystemIrVisibility::Exported
                    } else {
                        SystemIrVisibility::Private
                    },
                    parameters,
                    result: is_function
                        .then(|| program.typed_results.get(name).cloned())
                        .flatten(),
                    throws: program.throws_types.get(name).cloned(),
                    entry_index: definition.entry,
                    function: is_function,
                },
            );
        }

        let mut instructions = Vec::with_capacity(program.instructions.len());
        let mut current_definition = None::<String>;
        for located in &program.instructions {
            if let Statement::DefineProcedure(name, _) | Statement::DefineFunction(name, _) =
                &located.statement
            {
                current_definition = Some(name.clone());
            }
            let context = IrTypeContext {
                path: &source_path,
                program,
                imports: &imports,
                current_definition: current_definition.as_deref(),
                register_types: definition_register_types(
                    current_definition.as_deref(),
                    register_contracts,
                ),
            };
            instructions.push(lower_instruction(
                &located.statement,
                located.line_number,
                &context,
            ));
            if matches!(
                located.statement,
                Statement::EndProcedure | Statement::FunctionReturn(_)
            ) {
                current_definition = None;
            }
        }

        let workspace = program
            .module_state_types
            .iter()
            .map(|(name, ty)| (name.clone(), SystemIrValueType::System(ty.clone())))
            .collect();
        Self {
            manifest,
            module_name,
            module_version,
            source_path,
            source_hash,
            declared_dependencies,
            language_profile,
            target_profile,
            runtime_abi: RUNTIME_ABI_VERSION,
            type_definitions: program.system_types.clone(),
            workspace,
            readonly_workspace: program.readonly_bindings.clone(),
            readonly_local_bindings: program
                .readonly_local_bindings
                .clone()
                .into_iter()
                .collect(),
            routines: program
                .procedures
                .iter()
                .map(|(name, definition)| {
                    (
                        name.clone(),
                        SystemIrRoutine {
                            entry_index: definition.entry,
                            parameters: definition.parameters.clone(),
                            function: false,
                        },
                    )
                })
                .chain(program.functions.iter().map(|(name, definition)| {
                    (
                        format!("FN:{name}"),
                        SystemIrRoutine {
                            entry_index: definition.entry,
                            parameters: definition.parameters.clone(),
                            function: true,
                        },
                    )
                }))
                .collect(),
            typed_parameters: program.typed_parameters.clone().into_iter().collect(),
            typed_results: program.typed_results.clone().into_iter().collect(),
            throws_types: program.throws_types.clone().into_iter().collect(),
            options: lower_program_options(&program.options),
            line_entries: program.line_entries.clone(),
            definitions,
            instructions,
        }
    }

    pub fn prepare(
        &self,
        backend: SystemIrBackend,
    ) -> Result<SystemIrExecutionPlan, SystemIrLoweringError> {
        if backend != SystemIrBackend::Interpreter {
            let first = self.instructions.first();
            let source = first.map_or_else(
                || SystemIrSourceLocation {
                    path: self.source_path.clone(),
                    line: 0,
                },
                |instruction| instruction.source.clone(),
            );
            let operation = first.map_or_else(
                || "typed module definition".to_owned(),
                |instruction| match &instruction.operation {
                    SystemIrOpcode::Basic { .. } => "BASIC64 module definition".to_owned(),
                    operation => opcode_name(operation).to_owned(),
                },
            );
            return Err(SystemIrLoweringError {
                backend,
                source,
                operation,
            });
        }
        Ok(SystemIrExecutionPlan {
            backend,
            source_path: self.source_path.clone(),
            source_hash: self.source_hash.clone(),
            instruction_count: self.instructions.len(),
        })
    }

    /// Reconstitutes the BASIC statement program consumed by the reference
    /// interpreter. The executable module still enters through this IR; this
    /// adapter preserves one authoritative implementation of BASIC runtime
    /// semantics while typed checks stay attached to the portable operations.
    pub(crate) fn lower_for_reference_interpreter(&self) -> Result<ParsedProgram, String> {
        let mut program = ParsedProgram {
            instructions: self
                .instructions
                .iter()
                .map(|instruction| {
                    Ok(LocatedStatement {
                        line_number: instruction.source.line,
                        statement: instruction_to_reference(instruction)?,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?,
            line_entries: self.line_entries.clone(),
            procedures: Default::default(),
            functions: Default::default(),
            typed_parameters: self.typed_parameters.clone().into_iter().collect(),
            typed_results: self.typed_results.clone().into_iter().collect(),
            throws_types: self.throws_types.clone().into_iter().collect(),
            system_types: self.type_definitions.clone(),
            module_state_types: self
                .workspace
                .iter()
                .filter_map(|(name, ty)| match ty {
                    SystemIrValueType::System(ty) => Some((name.clone(), ty.clone())),
                    _ => None,
                })
                .collect(),
            readonly_bindings: self.readonly_workspace.clone(),
            readonly_local_bindings: self.readonly_local_bindings.clone().into_iter().collect(),
            options: parser_program_options(&self.options),
        };
        for (name, routine) in &self.routines {
            let definition = Definition {
                entry: routine.entry_index,
                parameters: routine.parameters.clone(),
            };
            if routine.function {
                program.functions.insert(
                    name.strip_prefix("FN:").unwrap_or(name).to_owned(),
                    definition,
                );
            } else {
                program.procedures.insert(name.clone(), definition);
            }
        }
        Ok(program)
    }

    #[cfg(feature = "experimental-jit")]
    pub(crate) fn native_boundary_for_program(
        program: &ParsedProgram,
        backend: SystemIrBackend,
    ) -> Result<(), String> {
        if !program_uses_system_profile(program) {
            return Ok(());
        }
        let source_path = "<BASIC64 source>".to_owned();
        let empty_descriptors = BTreeMap::new();
        let ir = Self::lower(
            None,
            "<anonymous>".into(),
            SemanticVersion::new(0, 0, 0),
            source_path,
            String::new(),
            Vec::new(),
            "BASIC64-SYSTEM-0.1".into(),
            "HOSTED".into(),
            &[],
            &BTreeMap::new(),
            &empty_descriptors,
            program,
        );
        ir.prepare(backend).map(|_| ()).map_err(|error| {
            let mode_advice = match backend {
                SystemIrBackend::HybridJit => "; interpreted fallback is required",
                SystemIrBackend::StrictJit => "; choose Interpreter or Hybrid mode",
                SystemIrBackend::Aot => "; no module AOT backend is implemented",
                SystemIrBackend::Interpreter => "",
            };
            format!("{error}{mode_advice}")
        })
    }
}

struct IrTypeContext<'a> {
    path: &'a str,
    program: &'a ParsedProgram,
    imports: &'a BTreeMap<String, String>,
    current_definition: Option<&'a str>,
    register_types: BTreeMap<String, SystemIrValueType>,
}

fn lower_instruction(
    statement: &Statement,
    line: u16,
    context: &IrTypeContext<'_>,
) -> SystemIrInstruction {
    let source = location(context.path, line);
    let operation = match statement {
        Statement::Assign(place, expression) => SystemIrOpcode::Assign {
            target: lower_place(place, line, context),
            value: lower_expression(expression, line, context),
        },
        Statement::LocalReadOnly {
            name,
            value_type,
            value,
        } => SystemIrOpcode::LocalReadOnlyDeclaration {
            name: name.clone(),
            value_type: SystemIrValueType::System(value_type.clone()),
            value: lower_expression(value, line, context),
        },
        Statement::ProcedureCall(name, arguments) => SystemIrOpcode::ProcedureCall {
            name: name.clone(),
            linkage: call_linkage(name, context),
            arguments: arguments
                .iter()
                .map(|expression| lower_expression(expression, line, context))
                .collect(),
        },
        Statement::ImportedProcedureCall {
            module,
            name,
            arguments,
        } => SystemIrOpcode::ImportedProcedureCall {
            module: module.clone(),
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|expression| lower_expression(expression, line, context))
                .collect(),
        },
        Statement::PrimitiveCall {
            name,
            arguments,
            results,
        } => SystemIrOpcode::PrimitiveCall {
            name: name.to_ascii_uppercase(),
            capability: context.imports.get(&name.to_ascii_uppercase()).cloned(),
            arguments: arguments
                .iter()
                .map(|expression| {
                    expression
                        .as_ref()
                        .map(|expression| lower_expression(expression, line, context))
                })
                .collect(),
            result_bindings: results.clone(),
        },
        Statement::Try => SystemIrOpcode::BeginTry,
        Statement::Catch {
            error_name,
            error_type,
        } => SystemIrOpcode::Catch {
            binding: error_name.clone(),
            error_type: error_type.clone(),
        },
        Statement::EndTry => SystemIrOpcode::EndTry,
        Statement::Throw {
            error_type,
            code,
            message,
        } => SystemIrOpcode::Throw {
            error_type: error_type.clone(),
            code: lower_expression(code, line, context),
            message: lower_expression(message, line, context),
        },
        Statement::FunctionReturn(value) => SystemIrOpcode::FunctionReturn {
            value: lower_expression(value, line, context),
        },
        _ => SystemIrOpcode::Basic {
            statement: lower_basic_statement(statement, line, context),
        },
    };
    SystemIrInstruction { source, operation }
}

fn lower_basic_statement(
    statement: &Statement,
    line: u16,
    context: &IrTypeContext<'_>,
) -> SystemIrBasicStatement {
    let expression = |value: &Expr| lower_expression(value, line, context);
    match statement {
        Statement::Assign(target, value) => SystemIrBasicStatement::Assign {
            target: lower_place(target, line, context),
            value: expression(value),
        },
        Statement::LocalReadOnly {
            name,
            value_type,
            value,
        } => SystemIrBasicStatement::LocalReadOnlyDeclaration {
            name: name.clone(),
            value_type: value_type.clone(),
            value: expression(value),
        },
        Statement::ProcedureCall(name, arguments) => SystemIrBasicStatement::ProcedureCall {
            name: name.clone(),
            linkage: call_linkage(name, context),
            arguments: arguments.iter().map(expression).collect(),
        },
        Statement::ImportedProcedureCall {
            module,
            name,
            arguments,
        } => SystemIrBasicStatement::ImportedProcedureCall {
            module: module.clone(),
            name: name.clone(),
            arguments: arguments.iter().map(expression).collect(),
        },
        Statement::PrimitiveCall {
            name,
            arguments,
            results,
        } => SystemIrBasicStatement::PrimitiveCall {
            name: name.to_ascii_uppercase(),
            capability: context.imports.get(&name.to_ascii_uppercase()).cloned(),
            arguments: arguments
                .iter()
                .map(|value| value.as_ref().map(expression))
                .collect(),
            results: results.clone(),
        },
        Statement::Input(place) => SystemIrBasicStatement::Input(lower_place(place, line, context)),
        Statement::Print(items) => SystemIrBasicStatement::Print(
            items
                .iter()
                .map(|item| match item {
                    PrintItem::Value(value) => SystemIrPrintItem::Value(expression(value)),
                    PrintItem::Spaces(value) => SystemIrPrintItem::Spaces(expression(value)),
                    PrintItem::Tab(x, y) => SystemIrPrintItem::Tab(expression(x), expression(y)),
                    PrintItem::Comma => SystemIrPrintItem::Comma,
                    PrintItem::Semicolon => SystemIrPrintItem::Semicolon,
                    PrintItem::NewLine => SystemIrPrintItem::NewLine,
                })
                .collect(),
        ),
        Statement::ClearScreen => SystemIrBasicStatement::ClearScreen,
        Statement::ClearGraphics => SystemIrBasicStatement::ClearGraphics,
        Statement::Colour(values) => {
            SystemIrBasicStatement::Colour(values.iter().map(expression).collect())
        }
        Statement::PrintFormat(value) => SystemIrBasicStatement::PrintFormat(expression(value)),
        Statement::Mode(value) => SystemIrBasicStatement::Mode(expression(value)),
        Statement::Vdu(values) => SystemIrBasicStatement::Vdu(
            values
                .iter()
                .map(|argument: &VduArgument| SystemIrVduArgument {
                    value: expression(&argument.value),
                    format: match argument.format {
                        VduFormat::Byte => SystemIrVduFormat::Byte,
                        VduFormat::Word => SystemIrVduFormat::Word,
                        VduFormat::Padded => SystemIrVduFormat::Padded,
                    },
                })
                .collect(),
        ),
        Statement::Line(a, b, c, d) => SystemIrBasicStatement::Line([
            expression(a),
            expression(b),
            expression(c),
            expression(d),
        ]),
        Statement::Move(a, b) => SystemIrBasicStatement::Move(expression(a), expression(b)),
        Statement::Draw(a, b) => SystemIrBasicStatement::Draw(expression(a), expression(b)),
        Statement::Plot(a, b, c) => {
            SystemIrBasicStatement::Plot(expression(a), expression(b), expression(c))
        }
        Statement::Gcol(a, b) => SystemIrBasicStatement::Gcol(expression(a), expression(b)),
        Statement::If(condition, then_body, else_body) => SystemIrBasicStatement::If {
            condition: expression(condition),
            then_body: then_body
                .iter()
                .map(|statement| lower_basic_statement(statement, line, context))
                .collect(),
            else_body: else_body
                .iter()
                .map(|statement| lower_basic_statement(statement, line, context))
                .collect(),
        },
        Statement::IfBlock(condition) => SystemIrBasicStatement::IfBlock(expression(condition)),
        Statement::EndIf => SystemIrBasicStatement::EndIf,
        Statement::Goto(target) => SystemIrBasicStatement::Goto(*target),
        Statement::Gosub(target) => SystemIrBasicStatement::Gosub(*target),
        Statement::Dim(items) => SystemIrBasicStatement::Dim(
            items
                .iter()
                .map(|item: &DimDeclaration| SystemIrDimDeclaration {
                    name: item.name.clone(),
                    dimensions: item.dimensions.iter().map(expression).collect(),
                    byte_block: item.byte_block,
                })
                .collect(),
        ),
        Statement::Read(places) => SystemIrBasicStatement::Read(
            places
                .iter()
                .map(|place| lower_place(place, line, context))
                .collect(),
        ),
        Statement::Data(values) => {
            SystemIrBasicStatement::Data(values.iter().map(expression).collect())
        }
        Statement::Restore(target) => SystemIrBasicStatement::Restore(*target),
        Statement::For {
            variable,
            start,
            end,
            step,
        } => SystemIrBasicStatement::For {
            variable: variable.clone(),
            start: expression(start),
            end: expression(end),
            step: step.as_ref().map(expression),
        },
        Statement::Next(variable) => SystemIrBasicStatement::Next(variable.clone()),
        Statement::Repeat => SystemIrBasicStatement::Repeat,
        Statement::Until(condition) => SystemIrBasicStatement::Until(expression(condition)),
        Statement::DefineProcedure(name, parameters) => SystemIrBasicStatement::DefineProcedure {
            name: name.clone(),
            parameters: parameters.clone(),
        },
        Statement::DefineFunction(name, parameters) => SystemIrBasicStatement::DefineFunction {
            name: name.clone(),
            parameters: parameters.clone(),
        },
        Statement::Sys {
            name,
            arguments,
            results,
            flags,
        } => SystemIrBasicStatement::Sys {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|value| value.as_ref().map(expression))
                .collect(),
            results: results.clone(),
            flags: flags.clone(),
        },
        Statement::Try => SystemIrBasicStatement::Try,
        Statement::Catch {
            error_name,
            error_type,
        } => SystemIrBasicStatement::Catch {
            binding: error_name.clone(),
            error_type: error_type.clone(),
        },
        Statement::EndTry => SystemIrBasicStatement::EndTry,
        Statement::Throw {
            error_type,
            code,
            message,
        } => SystemIrBasicStatement::Throw {
            error_type: error_type.clone(),
            code: expression(code),
            message: expression(message),
        },
        Statement::FunctionReturn(value) => {
            SystemIrBasicStatement::FunctionReturn(expression(value))
        }
        Statement::Return => SystemIrBasicStatement::Return,
        Statement::EndProcedure => SystemIrBasicStatement::EndProcedure,
        Statement::End => SystemIrBasicStatement::End,
        Statement::Call(value) => SystemIrBasicStatement::Call(expression(value)),
        Statement::StarCommand(command) => SystemIrBasicStatement::StarCommand(command.clone()),
        Statement::NoOp => SystemIrBasicStatement::NoOp,
    }
}

fn lower_expression(
    expression: &Expr,
    line: u16,
    context: &IrTypeContext<'_>,
) -> SystemIrExpression {
    let children = expression_children(expression)
        .into_iter()
        .map(|child| lower_expression(child, line, context))
        .collect::<Vec<_>>();
    let value_type = expression_type(expression, &children, context);
    let kind = match expression {
        Expr::Number(value) => SystemIrExpressionKind::NumberLiteral(value.to_bits()),
        Expr::Integer(value) => SystemIrExpressionKind::ExactIntegerLiteral(*value),
        Expr::String(value) => SystemIrExpressionKind::StringLiteral(value.clone()),
        Expr::Variable(name) => SystemIrExpressionKind::LoadBinding {
            name: name.clone(),
            storage: binding_storage(name, context),
        },
        Expr::ArrayElement(name, _) => SystemIrExpressionKind::ArrayElement { name: name.clone() },
        Expr::Unary(operator, _) => SystemIrExpressionKind::Unary {
            operator: ir_unary_operator(*operator),
        },
        Expr::Binary(_, operator, _)
            if matches!(
                children.first().map(|child| &child.value_type),
                Some(SystemIrValueType::System(SystemType::Address32))
            ) && matches!(operator, BinaryOp::Add | BinaryOp::Subtract) =>
        {
            SystemIrExpressionKind::CheckedAddressOffset {
                operator: ir_binary_operator(*operator),
                width_bits: 32,
                owner: AddressOwner::InvokingTask,
            }
        }
        Expr::Binary(_, operator, _)
            if matches!(operator, BinaryOp::And | BinaryOp::Or)
                && matches!(
                    children.as_slice(),
                    [
                        SystemIrExpression {
                            value_type: SystemIrValueType::System(SystemType::Flags(_)),
                            ..
                        },
                        SystemIrExpression {
                            value_type: SystemIrValueType::System(SystemType::Flags(_)),
                            ..
                        }
                    ]
                ) =>
        {
            let SystemIrValueType::System(SystemType::Flags(left_type)) = &children[0].value_type
            else {
                unreachable!("matched flags operand")
            };
            let SystemIrValueType::System(SystemType::Flags(right_type)) = &children[1].value_type
            else {
                unreachable!("matched flags operand")
            };
            SystemIrExpressionKind::FlagsCombine {
                operator: ir_binary_operator(*operator),
                left_type: left_type.clone(),
                right_type: right_type.clone(),
            }
        }
        Expr::Binary(_, operator, _)
            if is_comparison_operator(*operator)
                && children.iter().any(|child| {
                    matches!(
                        child.value_type,
                        SystemIrValueType::System(SystemType::Enum(_) | SystemType::Flags(_))
                    )
                }) =>
        {
            SystemIrExpressionKind::NominalCompare {
                operator: ir_binary_operator(*operator),
                left_type: children[0].value_type.clone(),
                right_type: children[1].value_type.clone(),
            }
        }
        Expr::Binary(_, operator, _) => SystemIrExpressionKind::NumericBinary {
            operator: ir_binary_operator(*operator),
        },
        Expr::Builtin(token, _) => SystemIrExpressionKind::Builtin {
            builtin: builtin_from_token(*token),
        },
        Expr::UserFunction(name, _) => SystemIrExpressionKind::CallFunction { name: name.clone() },
        Expr::ImportedFunction { module, name, .. } => {
            SystemIrExpressionKind::ImportedFunctionCall {
                module: module.clone(),
                name: name.clone(),
            }
        }
        Expr::MemoryRead(width, _) => SystemIrExpressionKind::CheckedLogicalMemoryRead {
            width: ir_memory_width(*width),
            owner: AddressOwner::InvokingTask,
        },
        Expr::Member(base, name) => match enum_flag_member(base, context) {
            Some((type_name, false)) => SystemIrExpressionKind::EnumConstant {
                type_name,
                member: name.clone(),
            },
            Some((type_name, true)) => SystemIrExpressionKind::FlagsConstant {
                type_name,
                member: name.clone(),
            },
            None if children
                .first()
                .and_then(|base| record_field_type(&base.value_type, name, context))
                .is_some() =>
            {
                SystemIrExpressionKind::RecordMember { name: name.clone() }
            }
            None => SystemIrExpressionKind::DynamicMember { name: name.clone() },
        },
    };
    SystemIrExpression {
        source: location(context.path, line),
        value_type,
        kind,
        children,
    }
}

fn lower_place(place: &LValue, line: u16, context: &IrTypeContext<'_>) -> SystemIrPlace {
    let (kind, value_type, store_rule) = match place {
        LValue::Variable(name) => {
            let storage = binding_storage(name, context);
            let store_rule = if context
                .program
                .readonly_bindings
                .contains(&name.to_ascii_uppercase())
            {
                SystemIrStoreRule::InitializeOnce
            } else if context.current_definition.is_some_and(|definition| {
                context
                    .program
                    .readonly_local_bindings
                    .get(definition)
                    .is_some_and(|bindings| bindings.contains_key(&name.to_ascii_uppercase()))
            }) {
                SystemIrStoreRule::ReadOnly
            } else {
                SystemIrStoreRule::Mutable
            };
            (
                SystemIrPlaceKind::Binding {
                    name: name.clone(),
                    storage,
                },
                binding_type(name, context),
                store_rule,
            )
        }
        LValue::RecordField(base, name) => {
            let mut path = vec![base.clone(), name.clone()];
            let ty = record_path_type(&path, context).unwrap_or(SystemIrValueType::BasicNumber);
            let read_only = record_path_readonly(&path, context);
            (
                SystemIrPlaceKind::RecordField {
                    path: std::mem::take(&mut path),
                },
                ty,
                if read_only {
                    SystemIrStoreRule::ReadOnly
                } else {
                    SystemIrStoreRule::Mutable
                },
            )
        }
        LValue::RecordPath(path) => {
            let ty = record_path_type(path, context).unwrap_or(SystemIrValueType::BasicNumber);
            let read_only = record_path_readonly(path, context);
            (
                SystemIrPlaceKind::RecordField { path: path.clone() },
                ty,
                if read_only {
                    SystemIrStoreRule::ReadOnly
                } else {
                    SystemIrStoreRule::Mutable
                },
            )
        }
        LValue::Memory(width, address) => (
            SystemIrPlaceKind::CheckedLogicalMemory {
                width: ir_memory_width(*width),
                owner: AddressOwner::InvokingTask,
                address: lower_expression(address, line, context),
            },
            memory_type(*width),
            SystemIrStoreRule::Mutable,
        ),
        LValue::MemoryByteAt(base, offset) => (
            SystemIrPlaceKind::CheckedLogicalMemoryByteAt {
                owner: AddressOwner::InvokingTask,
                base: lower_expression(base, line, context),
                offset: lower_expression(offset, line, context),
            },
            SystemIrValueType::System(SystemType::Byte),
            SystemIrStoreRule::Mutable,
        ),
        LValue::MemoryOffset(width, base, offset) => (
            SystemIrPlaceKind::CheckedLogicalMemoryOffset {
                width: ir_memory_width(*width),
                owner: AddressOwner::InvokingTask,
                base: lower_expression(base, line, context),
                offset: lower_expression(offset, line, context),
            },
            memory_type(*width),
            SystemIrStoreRule::Mutable,
        ),
        LValue::MemoryString(address) => (
            SystemIrPlaceKind::CheckedLogicalMemoryString {
                owner: AddressOwner::InvokingTask,
                address: lower_expression(address, line, context),
            },
            SystemIrValueType::BasicString,
            SystemIrStoreRule::Mutable,
        ),
        LValue::ArrayElement(name, index) => (
            SystemIrPlaceKind::ArrayElement {
                name: name.clone(),
                index: lower_expression(index, line, context),
            },
            binding_type(name, context),
            SystemIrStoreRule::Mutable,
        ),
        LValue::StringSlice(name, start, length) => (
            SystemIrPlaceKind::StringSlice {
                name: name.clone(),
                start: lower_expression(start, line, context),
                length: lower_expression(length, line, context),
            },
            SystemIrValueType::BasicString,
            SystemIrStoreRule::Mutable,
        ),
    };
    SystemIrPlace {
        source: location(context.path, line),
        value_type,
        kind,
        store_rule,
    }
}

fn expression_type(
    expression: &Expr,
    children: &[SystemIrExpression],
    context: &IrTypeContext<'_>,
) -> SystemIrValueType {
    match expression {
        Expr::Number(_) | Expr::Unary(_, _) | Expr::Builtin(_, _) => SystemIrValueType::BasicNumber,
        Expr::Integer(_) => SystemIrValueType::ExactInteger,
        Expr::String(_) => SystemIrValueType::BasicString,
        Expr::Variable(name) | Expr::ArrayElement(name, _) => binding_type(name, context),
        Expr::Binary(_, operator, _) => {
            if children.first().is_some_and(|child| {
                child.value_type == SystemIrValueType::System(SystemType::Address32)
            }) && matches!(operator, BinaryOp::Add | BinaryOp::Subtract)
            {
                SystemIrValueType::System(SystemType::Address32)
            } else if matches!(operator, BinaryOp::And | BinaryOp::Or)
                && let [
                    SystemIrExpression {
                        value_type: SystemIrValueType::System(SystemType::Flags(left_type)),
                        ..
                    },
                    SystemIrExpression {
                        value_type: SystemIrValueType::System(SystemType::Flags(right_type)),
                        ..
                    },
                ] = children
                && left_type == right_type
            {
                SystemIrValueType::System(SystemType::Flags(left_type.clone()))
            } else if children.first().is_some_and(|child| {
                matches!(
                    child.value_type,
                    SystemIrValueType::System(SystemType::UInt64 | SystemType::Int64)
                )
            }) && matches!(
                operator,
                BinaryOp::Add
                    | BinaryOp::Subtract
                    | BinaryOp::Multiply
                    | BinaryOp::IntegerDivide
                    | BinaryOp::Modulo
                    | BinaryOp::ShiftLeft
            ) {
                children[0].value_type.clone()
            } else {
                SystemIrValueType::BasicNumber
            }
        }
        Expr::UserFunction(name, _) => context
            .program
            .typed_results
            .get(name)
            .cloned()
            .map(SystemIrValueType::System)
            .unwrap_or_else(|| inferred_binding_type(name)),
        Expr::ImportedFunction { module, name, .. } => SystemIrValueType::ImportedSymbolResult {
            module: module.clone(),
            symbol: format!("FN:{name}"),
        },
        Expr::MemoryRead(width, _) => memory_type(*width),
        Expr::Member(base, name) => enum_flag_member(base, context)
            .map(|(type_name, is_flags)| {
                SystemIrValueType::System(if is_flags {
                    SystemType::Flags(type_name)
                } else {
                    SystemType::Enum(type_name)
                })
            })
            .or_else(|| {
                children
                    .first()
                    .and_then(|base| record_field_type(&base.value_type, name, context))
            })
            .unwrap_or(SystemIrValueType::BasicNumber),
    }
}

fn enum_flag_member(base: &Expr, context: &IrTypeContext<'_>) -> Option<(String, bool)> {
    let Expr::Variable(type_name) = base else {
        return None;
    };
    let type_name = type_name.to_ascii_uppercase();
    match context.program.system_types.get(&type_name) {
        Some(SystemTypeDefinition::Enum { .. }) => Some((type_name, false)),
        Some(SystemTypeDefinition::Flags { .. }) => Some((type_name, true)),
        _ => None,
    }
}

fn is_comparison_operator(operator: BinaryOp) -> bool {
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

fn binding_type(name: &str, context: &IrTypeContext<'_>) -> SystemIrValueType {
    let canonical = name.to_ascii_uppercase();
    if let Some(kind) = context.register_types.get(&canonical) {
        return kind.clone();
    }
    if canonical == "PC%" {
        return SystemIrValueType::System(SystemType::Address32);
    }
    if let Some(kind) = context.program.module_state_types.get(&canonical) {
        return SystemIrValueType::System(kind.clone());
    }
    if let Some(definition) = context.current_definition {
        if let Some(kind) = context
            .program
            .readonly_local_bindings
            .get(definition)
            .and_then(|bindings| bindings.get(&canonical))
        {
            return SystemIrValueType::System(kind.clone());
        }
        if let (Some(parameters), Some(types)) = (
            context
                .program
                .procedures
                .get(definition)
                .or_else(|| context.program.functions.get(definition)),
            context.program.typed_parameters.get(definition),
        ) {
            if let Some(index) = parameters
                .parameters
                .iter()
                .position(|parameter| parameter.eq_ignore_ascii_case(&canonical))
            {
                if let Some(kind) = types.get(index) {
                    return SystemIrValueType::System(kind.clone());
                }
            }
        }
    }
    inferred_binding_type(name)
}

fn inferred_binding_type(name: &str) -> SystemIrValueType {
    if name.ends_with('$') {
        SystemIrValueType::BasicString
    } else {
        SystemIrValueType::BasicNumber
    }
}

fn binding_storage(name: &str, context: &IrTypeContext<'_>) -> SystemIrStorage {
    let canonical = name.to_ascii_uppercase();
    if context.program.module_state_types.contains_key(&canonical) {
        SystemIrStorage::ModuleWorkspace
    } else if context.register_types.contains_key(&canonical) || canonical == "PC%" {
        SystemIrStorage::Register
    } else if context.current_definition.is_some_and(|definition| {
        context
            .program
            .procedures
            .get(definition)
            .or_else(|| context.program.functions.get(definition))
            .is_some_and(|routine| {
                routine
                    .parameters
                    .iter()
                    .any(|parameter| parameter.eq_ignore_ascii_case(&canonical))
            })
    }) {
        SystemIrStorage::Parameter
    } else {
        SystemIrStorage::Local
    }
}

fn record_path_type(path: &[String], context: &IrTypeContext<'_>) -> Option<SystemIrValueType> {
    let (base, fields) = path.split_first()?;
    let mut value_type = binding_type(base, context);
    for field in fields {
        value_type = record_field_type(&value_type, field, context)?;
    }
    Some(value_type)
}

fn record_field_type(
    base: &SystemIrValueType,
    field_name: &str,
    context: &IrTypeContext<'_>,
) -> Option<SystemIrValueType> {
    let SystemIrValueType::System(SystemType::Record(type_name) | SystemType::Error(type_name)) =
        base
    else {
        return None;
    };
    let definition = context.program.system_types.get(type_name)?;
    let fields: &[SystemField] = match definition {
        SystemTypeDefinition::Record { fields } | SystemTypeDefinition::Error { fields } => fields,
        _ => return None,
    };
    fields
        .iter()
        .find(|field| field.name.eq_ignore_ascii_case(field_name))
        .map(|field| SystemIrValueType::System(field.value_type.clone()))
}

fn record_path_readonly(path: &[String], context: &IrTypeContext<'_>) -> bool {
    let Some((base, fields)) = path.split_first() else {
        return false;
    };
    let mut value_type = binding_type(base, context);
    for field_name in fields {
        let SystemIrValueType::System(SystemType::Record(type_name) | SystemType::Error(type_name)) =
            value_type
        else {
            return false;
        };
        let Some(definition) = context.program.system_types.get(&type_name) else {
            return false;
        };
        let fields = match definition {
            SystemTypeDefinition::Record { fields } | SystemTypeDefinition::Error { fields } => {
                fields
            }
            _ => return false,
        };
        let Some(field) = fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(field_name))
        else {
            return false;
        };
        if field.read_only {
            return true;
        }
        value_type = SystemIrValueType::System(field.value_type.clone());
    }
    false
}

fn memory_type(width: MemoryWidth) -> SystemIrValueType {
    match width {
        MemoryWidth::Byte => SystemIrValueType::System(SystemType::Byte),
        MemoryWidth::Word => SystemIrValueType::System(SystemType::UInt16),
    }
}

fn ir_memory_width(width: MemoryWidth) -> SystemIrMemoryWidth {
    match width {
        MemoryWidth::Byte => SystemIrMemoryWidth::Byte,
        MemoryWidth::Word => SystemIrMemoryWidth::Word,
    }
}

fn ir_unary_operator(operator: UnaryOp) -> SystemIrUnaryOperator {
    match operator {
        UnaryOp::Plus => SystemIrUnaryOperator::Positive,
        UnaryOp::Minus => SystemIrUnaryOperator::Negative,
        UnaryOp::Not => SystemIrUnaryOperator::Not,
    }
}

fn ir_binary_operator(operator: BinaryOp) -> SystemIrBinaryOperator {
    match operator {
        BinaryOp::Add => SystemIrBinaryOperator::Add,
        BinaryOp::Subtract => SystemIrBinaryOperator::Subtract,
        BinaryOp::Multiply => SystemIrBinaryOperator::Multiply,
        BinaryOp::Divide => SystemIrBinaryOperator::Divide,
        BinaryOp::IntegerDivide => SystemIrBinaryOperator::IntegerDivide,
        BinaryOp::Modulo => SystemIrBinaryOperator::Modulo,
        BinaryOp::Power => SystemIrBinaryOperator::Power,
        BinaryOp::ShiftLeft => SystemIrBinaryOperator::ShiftLeft,
        BinaryOp::Equal => SystemIrBinaryOperator::Equal,
        BinaryOp::NotEqual => SystemIrBinaryOperator::NotEqual,
        BinaryOp::Less => SystemIrBinaryOperator::Less,
        BinaryOp::LessEqual => SystemIrBinaryOperator::LessEqual,
        BinaryOp::Greater => SystemIrBinaryOperator::Greater,
        BinaryOp::GreaterEqual => SystemIrBinaryOperator::GreaterEqual,
        BinaryOp::And => SystemIrBinaryOperator::And,
        BinaryOp::Or => SystemIrBinaryOperator::Or,
    }
}

fn builtin_from_token(token: u8) -> SystemIrBuiltin {
    match token {
        0x94 => SystemIrBuiltin::Abs,
        0x97 => SystemIrBuiltin::Asc,
        0x9B => SystemIrBuiltin::Cos,
        0xBD => SystemIrBuiltin::Chr,
        0xA8 => SystemIrBuiltin::Int,
        0xA6 => SystemIrBuiltin::Inkey,
        0xA7 => SystemIrBuiltin::Instr,
        0xC0 => SystemIrBuiltin::Left,
        0xA9 => SystemIrBuiltin::Length,
        0xC1 => SystemIrBuiltin::Mid,
        0xAA => SystemIrBuiltin::NaturalLog,
        0xAB => SystemIrBuiltin::Log10,
        0xB3 => SystemIrBuiltin::Random,
        0xC2 => SystemIrBuiltin::Right,
        0xB5 => SystemIrBuiltin::Sin,
        0xB6 => SystemIrBuiltin::SquareRoot,
        0xC4 => SystemIrBuiltin::String,
        0xC3 => SystemIrBuiltin::Stringify,
        0xB7 => SystemIrBuiltin::Tan,
        0xBC => SystemIrBuiltin::Val,
        _ => unreachable!("parser only constructs supported BASIC built-ins"),
    }
}

fn builtin_token(builtin: SystemIrBuiltin) -> u8 {
    match builtin {
        SystemIrBuiltin::Abs => 0x94,
        SystemIrBuiltin::Asc => 0x97,
        SystemIrBuiltin::Cos => 0x9B,
        SystemIrBuiltin::Chr => 0xBD,
        SystemIrBuiltin::Int => 0xA8,
        SystemIrBuiltin::Inkey => 0xA6,
        SystemIrBuiltin::Instr => 0xA7,
        SystemIrBuiltin::Left => 0xC0,
        SystemIrBuiltin::Length => 0xA9,
        SystemIrBuiltin::Mid => 0xC1,
        SystemIrBuiltin::NaturalLog => 0xAA,
        SystemIrBuiltin::Log10 => 0xAB,
        SystemIrBuiltin::Random => 0xB3,
        SystemIrBuiltin::Right => 0xC2,
        SystemIrBuiltin::Sin => 0xB5,
        SystemIrBuiltin::SquareRoot => 0xB6,
        SystemIrBuiltin::String => 0xC4,
        SystemIrBuiltin::Stringify => 0xC3,
        SystemIrBuiltin::Tan => 0xB7,
        SystemIrBuiltin::Val => 0xBC,
    }
}

fn definition_register_types(
    current_definition: Option<&str>,
    register_contracts: &BTreeMap<String, Vec<crate::trellis::RegisterContract>>,
) -> BTreeMap<String, SystemIrValueType> {
    let Some(current_definition) = current_definition else {
        return BTreeMap::new();
    };
    let Some(registers) = register_contracts.get(&current_definition.to_ascii_uppercase()) else {
        return BTreeMap::new();
    };
    registers
        .iter()
        .map(|register| {
            let kind = match &register.kind {
                RegisterKind::Unsigned { bits: 8 } => SystemType::Byte,
                RegisterKind::Unsigned { bits: 16 } => SystemType::UInt16,
                RegisterKind::Unsigned { bits: 32 } => SystemType::UInt32,
                RegisterKind::Signed { bits: 32 } => SystemType::Int32,
                RegisterKind::LogicalAddress { bits: 32 } => SystemType::Address32,
                RegisterKind::OpaqueHandle { type_name } => SystemType::Handle(type_name.clone()),
                _ => {
                    return (
                        format!("R{}%", register.register),
                        SystemIrValueType::BasicNumber,
                    );
                }
            };
            (
                format!("R{}%", register.register),
                SystemIrValueType::System(kind),
            )
        })
        .collect()
}

pub(super) fn expression_children(expression: &Expr) -> Vec<&Expr> {
    match expression {
        Expr::Number(_) | Expr::Integer(_) | Expr::String(_) | Expr::Variable(_) => Vec::new(),
        Expr::ArrayElement(_, index) | Expr::Unary(_, index) | Expr::MemoryRead(_, index) => {
            vec![index]
        }
        Expr::Binary(left, _, right) => vec![left, right],
        Expr::Builtin(_, arguments) | Expr::UserFunction(_, arguments) => {
            arguments.iter().collect()
        }
        Expr::ImportedFunction { arguments, .. } => arguments.iter().collect(),
        Expr::Member(value, _) => vec![value],
    }
}

pub(super) fn statement_expressions(statement: &Statement) -> Vec<&Expr> {
    match statement {
        Statement::Input(place) => place_expressions(place),
        Statement::Print(items) => items
            .iter()
            .flat_map(|item| match item {
                super::parser::PrintItem::Value(value)
                | super::parser::PrintItem::Spaces(value) => vec![value],
                super::parser::PrintItem::Tab(x, y) => vec![x, y],
                _ => Vec::new(),
            })
            .collect(),
        Statement::Colour(values) | Statement::Data(values) => values.iter().collect(),
        Statement::PrintFormat(value) | Statement::Mode(value) => vec![value],
        Statement::Vdu(values) => values.iter().map(|value| &value.value).collect(),
        Statement::Line(a, b, c, d) => vec![a, b, c, d],
        Statement::Move(a, b) | Statement::Draw(a, b) => vec![a, b],
        Statement::Plot(a, b, c) => vec![a, b, c],
        Statement::Gcol(a, b) => vec![a, b],
        Statement::If(condition, _, _)
        | Statement::IfBlock(condition)
        | Statement::Until(condition) => vec![condition],
        Statement::Dim(items) => items
            .iter()
            .flat_map(|item| item.dimensions.iter())
            .collect(),
        Statement::Read(places) => places.iter().flat_map(place_expressions).collect(),
        Statement::For {
            start, end, step, ..
        } => {
            let mut expressions = vec![start, end];
            expressions.extend(step.iter());
            expressions
        }
        Statement::ProcedureCall(_, arguments) => arguments.iter().collect(),
        Statement::ImportedProcedureCall { arguments, .. } => arguments.iter().collect(),
        Statement::LocalReadOnly { value, .. } => vec![value],
        Statement::Sys { arguments, .. } | Statement::PrimitiveCall { arguments, .. } => {
            arguments.iter().filter_map(Option::as_ref).collect()
        }
        Statement::Throw { code, message, .. } => vec![code, message],
        Statement::FunctionReturn(value) | Statement::Call(value) => vec![value],
        Statement::Assign(place, value) => place_expressions(place)
            .into_iter()
            .chain([value])
            .collect(),
        _ => Vec::new(),
    }
}

fn place_expressions(place: &LValue) -> Vec<&Expr> {
    match place {
        LValue::Variable(_) => Vec::new(),
        LValue::ArrayElement(_, index) => vec![index],
        LValue::Memory(_, address) | LValue::MemoryString(address) => vec![address],
        LValue::MemoryByteAt(address, index) | LValue::MemoryOffset(_, address, index) => {
            vec![address, index]
        }
        LValue::StringSlice(_, start, end) => vec![start, end],
        LValue::RecordField(_, _) | LValue::RecordPath(_) => Vec::new(),
    }
}

fn call_linkage(name: &str, context: &IrTypeContext<'_>) -> SystemIrCallLinkage {
    if context
        .program
        .procedures
        .contains_key(&name.to_ascii_uppercase())
    {
        return SystemIrCallLinkage::ModuleLocal;
    }
    SystemIrCallLinkage::Dynamic
}

fn instruction_to_reference(instruction: &SystemIrInstruction) -> Result<Statement, String> {
    let fail = || {
        format!(
            "malformed typed IR instruction at {}:{}",
            instruction.source.path, instruction.source.line
        )
    };
    match &instruction.operation {
        SystemIrOpcode::Assign { target, value } => Ok(Statement::Assign(
            place_from_ir(target).map_err(|_| fail())?,
            expression_from_ir(value).map_err(|_| fail())?,
        )),
        SystemIrOpcode::LocalReadOnlyDeclaration {
            name,
            value_type,
            value,
        } => {
            let SystemIrValueType::System(value_type) = value_type else {
                return Err(fail());
            };
            Ok(Statement::LocalReadOnly {
                name: name.clone(),
                value_type: value_type.clone(),
                value: expression_from_ir(value).map_err(|_| fail())?,
            })
        }
        SystemIrOpcode::ProcedureCall {
            name, arguments, ..
        } => Ok(Statement::ProcedureCall(
            name.clone(),
            arguments
                .iter()
                .map(expression_from_ir)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| fail())?,
        )),
        SystemIrOpcode::ImportedProcedureCall {
            module,
            name,
            arguments,
        } => Ok(Statement::ImportedProcedureCall {
            module: module.clone(),
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(expression_from_ir)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| fail())?,
        }),
        SystemIrOpcode::PrimitiveCall {
            name,
            arguments,
            result_bindings,
            ..
        } => Ok(Statement::PrimitiveCall {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|argument| argument.as_ref().map(expression_from_ir).transpose())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| fail())?,
            results: result_bindings.clone(),
        }),
        SystemIrOpcode::BeginTry => Ok(Statement::Try),
        SystemIrOpcode::Catch {
            binding,
            error_type,
        } => Ok(Statement::Catch {
            error_name: binding.clone(),
            error_type: error_type.clone(),
        }),
        SystemIrOpcode::EndTry => Ok(Statement::EndTry),
        SystemIrOpcode::Throw {
            error_type,
            code,
            message,
        } => Ok(Statement::Throw {
            error_type: error_type.clone(),
            code: expression_from_ir(code).map_err(|_| fail())?,
            message: expression_from_ir(message).map_err(|_| fail())?,
        }),
        SystemIrOpcode::FunctionReturn { value } => Ok(Statement::FunctionReturn(
            expression_from_ir(value).map_err(|_| fail())?,
        )),
        SystemIrOpcode::Basic { statement } => {
            basic_statement_from_ir(statement).map_err(|_| fail())
        }
    }
}

fn expression_from_ir(expression: &SystemIrExpression) -> Result<Expr, String> {
    let children = expression
        .children
        .iter()
        .map(expression_from_ir)
        .collect::<Result<Vec<_>, _>>()?;
    let child = |index: usize| {
        children
            .get(index)
            .cloned()
            .ok_or_else(|| "typed IR expression is missing an operand".to_owned())
    };
    let binary = |operator: SystemIrBinaryOperator| {
        Ok::<_, String>(Expr::Binary(
            Box::new(child(0)?),
            binary_operator_from_ir(operator),
            Box::new(child(1)?),
        ))
    };
    match &expression.kind {
        SystemIrExpressionKind::NumberLiteral(bits) => Ok(Expr::Number(f64::from_bits(*bits))),
        SystemIrExpressionKind::ExactIntegerLiteral(value) => Ok(Expr::Integer(*value)),
        SystemIrExpressionKind::StringLiteral(value) => Ok(Expr::String(value.clone())),
        SystemIrExpressionKind::LoadBinding { name, .. } => Ok(Expr::Variable(name.clone())),
        SystemIrExpressionKind::ArrayElement { name } => {
            Ok(Expr::ArrayElement(name.clone(), Box::new(child(0)?)))
        }
        SystemIrExpressionKind::Unary { operator } => Ok(Expr::Unary(
            match operator {
                SystemIrUnaryOperator::Positive => UnaryOp::Plus,
                SystemIrUnaryOperator::Negative => UnaryOp::Minus,
                SystemIrUnaryOperator::Not => UnaryOp::Not,
            },
            Box::new(child(0)?),
        )),
        SystemIrExpressionKind::NumericBinary { operator }
        | SystemIrExpressionKind::FlagsCombine { operator, .. }
        | SystemIrExpressionKind::CheckedAddressOffset { operator, .. }
        | SystemIrExpressionKind::NominalCompare { operator, .. } => binary(*operator),
        SystemIrExpressionKind::Builtin { builtin } => {
            Ok(Expr::Builtin(builtin_token(*builtin), children))
        }
        SystemIrExpressionKind::CallFunction { name } => {
            Ok(Expr::UserFunction(name.clone(), children))
        }
        SystemIrExpressionKind::ImportedFunctionCall { module, name } => {
            Ok(Expr::ImportedFunction {
                module: module.clone(),
                name: name.clone(),
                arguments: children,
            })
        }
        SystemIrExpressionKind::CheckedLogicalMemoryRead { width, .. } => Ok(Expr::MemoryRead(
            memory_width_from_ir(*width),
            Box::new(child(0)?),
        )),
        SystemIrExpressionKind::RecordMember { name }
        | SystemIrExpressionKind::DynamicMember { name } => {
            Ok(Expr::Member(Box::new(child(0)?), name.clone()))
        }
        SystemIrExpressionKind::EnumConstant { type_name, member }
        | SystemIrExpressionKind::FlagsConstant { type_name, member } => {
            let base = children
                .into_iter()
                .next()
                .unwrap_or_else(|| Expr::Variable(type_name.clone()));
            Ok(Expr::Member(Box::new(base), member.clone()))
        }
    }
}

fn place_from_ir(place: &SystemIrPlace) -> Result<LValue, String> {
    let expression = |index: usize| {
        place
            .kind
            .expressions()
            .get(index)
            .ok_or_else(|| "typed IR place is missing an expression".to_owned())
            .and_then(|expression| expression_from_ir(expression))
    };
    match &place.kind {
        SystemIrPlaceKind::Binding { name, .. } => Ok(LValue::Variable(name.clone())),
        SystemIrPlaceKind::ArrayElement { name, .. } => {
            Ok(LValue::ArrayElement(name.clone(), expression(0)?))
        }
        SystemIrPlaceKind::StringSlice { name, .. } => Ok(LValue::StringSlice(
            name.clone(),
            expression(0)?,
            expression(1)?,
        )),
        SystemIrPlaceKind::RecordField { path } if path.len() == 2 => {
            Ok(LValue::RecordField(path[0].clone(), path[1].clone()))
        }
        SystemIrPlaceKind::RecordField { path } => Ok(LValue::RecordPath(path.clone())),
        SystemIrPlaceKind::CheckedLogicalMemory { width, .. } => {
            Ok(LValue::Memory(memory_width_from_ir(*width), expression(0)?))
        }
        SystemIrPlaceKind::CheckedLogicalMemoryOffset { width, .. } => Ok(LValue::MemoryOffset(
            memory_width_from_ir(*width),
            expression(0)?,
            expression(1)?,
        )),
        SystemIrPlaceKind::CheckedLogicalMemoryByteAt { .. } => {
            Ok(LValue::MemoryByteAt(expression(0)?, expression(1)?))
        }
        SystemIrPlaceKind::CheckedLogicalMemoryString { .. } => {
            Ok(LValue::MemoryString(expression(0)?))
        }
    }
}

impl SystemIrPlaceKind {
    fn expressions(&self) -> Vec<&SystemIrExpression> {
        match self {
            Self::Binding { .. } | Self::RecordField { .. } => Vec::new(),
            Self::ArrayElement { index, .. } => vec![index],
            Self::StringSlice { start, length, .. } => vec![start, length],
            Self::CheckedLogicalMemory { address, .. }
            | Self::CheckedLogicalMemoryString { address, .. } => vec![address],
            Self::CheckedLogicalMemoryOffset { base, offset, .. }
            | Self::CheckedLogicalMemoryByteAt { base, offset, .. } => vec![base, offset],
        }
    }
}

fn basic_statement_from_ir(statement: &SystemIrBasicStatement) -> Result<Statement, String> {
    let expr = expression_from_ir;
    let place = place_from_ir;
    match statement {
        SystemIrBasicStatement::Assign { target, value } => {
            Ok(Statement::Assign(place(target)?, expr(value)?))
        }
        SystemIrBasicStatement::LocalReadOnlyDeclaration {
            name,
            value_type,
            value,
        } => Ok(Statement::LocalReadOnly {
            name: name.clone(),
            value_type: value_type.clone(),
            value: expr(value)?,
        }),
        SystemIrBasicStatement::ProcedureCall {
            name, arguments, ..
        } => Ok(Statement::ProcedureCall(
            name.clone(),
            arguments.iter().map(expr).collect::<Result<_, _>>()?,
        )),
        SystemIrBasicStatement::ImportedProcedureCall {
            module,
            name,
            arguments,
        } => Ok(Statement::ImportedProcedureCall {
            module: module.clone(),
            name: name.clone(),
            arguments: arguments.iter().map(expr).collect::<Result<_, _>>()?,
        }),
        SystemIrBasicStatement::PrimitiveCall {
            name,
            arguments,
            results,
            ..
        } => Ok(Statement::PrimitiveCall {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|argument| argument.as_ref().map(expr).transpose())
                .collect::<Result<_, _>>()?,
            results: results.clone(),
        }),
        SystemIrBasicStatement::Input(target) => Ok(Statement::Input(place(target)?)),
        SystemIrBasicStatement::Print(items) => Ok(Statement::Print(
            items
                .iter()
                .map(|item| match item {
                    SystemIrPrintItem::Value(value) => expr(value).map(PrintItem::Value),
                    SystemIrPrintItem::Spaces(value) => expr(value).map(PrintItem::Spaces),
                    SystemIrPrintItem::Tab(x, y) => Ok(PrintItem::Tab(expr(x)?, expr(y)?)),
                    SystemIrPrintItem::Comma => Ok(PrintItem::Comma),
                    SystemIrPrintItem::Semicolon => Ok(PrintItem::Semicolon),
                    SystemIrPrintItem::NewLine => Ok(PrintItem::NewLine),
                })
                .collect::<Result<_, String>>()?,
        )),
        SystemIrBasicStatement::ClearScreen => Ok(Statement::ClearScreen),
        SystemIrBasicStatement::ClearGraphics => Ok(Statement::ClearGraphics),
        SystemIrBasicStatement::Colour(values) => Ok(Statement::Colour(
            values.iter().map(expr).collect::<Result<_, _>>()?,
        )),
        SystemIrBasicStatement::PrintFormat(value) => Ok(Statement::PrintFormat(expr(value)?)),
        SystemIrBasicStatement::Mode(value) => Ok(Statement::Mode(expr(value)?)),
        SystemIrBasicStatement::Vdu(values) => Ok(Statement::Vdu(
            values
                .iter()
                .map(|argument| {
                    Ok(VduArgument {
                        value: expr(&argument.value)?,
                        format: match argument.format {
                            SystemIrVduFormat::Byte => VduFormat::Byte,
                            SystemIrVduFormat::Word => VduFormat::Word,
                            SystemIrVduFormat::Padded => VduFormat::Padded,
                        },
                    })
                })
                .collect::<Result<_, String>>()?,
        )),
        SystemIrBasicStatement::Line([a, b, c, d]) => {
            Ok(Statement::Line(expr(a)?, expr(b)?, expr(c)?, expr(d)?))
        }
        SystemIrBasicStatement::Move(x, y) => Ok(Statement::Move(expr(x)?, expr(y)?)),
        SystemIrBasicStatement::Draw(x, y) => Ok(Statement::Draw(expr(x)?, expr(y)?)),
        SystemIrBasicStatement::Plot(a, b, c) => Ok(Statement::Plot(expr(a)?, expr(b)?, expr(c)?)),
        SystemIrBasicStatement::Gcol(a, b) => Ok(Statement::Gcol(expr(a)?, expr(b)?)),
        SystemIrBasicStatement::If {
            condition,
            then_body,
            else_body,
        } => Ok(Statement::If(
            expr(condition)?,
            then_body
                .iter()
                .map(basic_statement_from_ir)
                .collect::<Result<_, _>>()?,
            else_body
                .iter()
                .map(basic_statement_from_ir)
                .collect::<Result<_, _>>()?,
        )),
        SystemIrBasicStatement::IfBlock(condition) => Ok(Statement::IfBlock(expr(condition)?)),
        SystemIrBasicStatement::EndIf => Ok(Statement::EndIf),
        SystemIrBasicStatement::Goto(target) => Ok(Statement::Goto(*target)),
        SystemIrBasicStatement::Gosub(target) => Ok(Statement::Gosub(*target)),
        SystemIrBasicStatement::Dim(items) => Ok(Statement::Dim(
            items
                .iter()
                .map(|item| {
                    Ok(DimDeclaration {
                        name: item.name.clone(),
                        dimensions: item.dimensions.iter().map(expr).collect::<Result<_, _>>()?,
                        byte_block: item.byte_block,
                    })
                })
                .collect::<Result<_, String>>()?,
        )),
        SystemIrBasicStatement::Read(places) => Ok(Statement::Read(
            places.iter().map(place).collect::<Result<_, _>>()?,
        )),
        SystemIrBasicStatement::Data(values) => Ok(Statement::Data(
            values.iter().map(expr).collect::<Result<_, _>>()?,
        )),
        SystemIrBasicStatement::Restore(target) => Ok(Statement::Restore(*target)),
        SystemIrBasicStatement::For {
            variable,
            start,
            end,
            step,
        } => Ok(Statement::For {
            variable: variable.clone(),
            start: expr(start)?,
            end: expr(end)?,
            step: step.as_ref().map(expr).transpose()?,
        }),
        SystemIrBasicStatement::Next(variable) => Ok(Statement::Next(variable.clone())),
        SystemIrBasicStatement::Repeat => Ok(Statement::Repeat),
        SystemIrBasicStatement::Until(value) => Ok(Statement::Until(expr(value)?)),
        SystemIrBasicStatement::DefineProcedure { name, parameters } => {
            Ok(Statement::DefineProcedure(name.clone(), parameters.clone()))
        }
        SystemIrBasicStatement::DefineFunction { name, parameters } => {
            Ok(Statement::DefineFunction(name.clone(), parameters.clone()))
        }
        SystemIrBasicStatement::Sys {
            name,
            arguments,
            results,
            flags,
        } => Ok(Statement::Sys {
            name: name.clone(),
            arguments: arguments
                .iter()
                .map(|argument| argument.as_ref().map(expr).transpose())
                .collect::<Result<_, _>>()?,
            results: results.clone(),
            flags: flags.clone(),
        }),
        SystemIrBasicStatement::Try => Ok(Statement::Try),
        SystemIrBasicStatement::Catch {
            binding,
            error_type,
        } => Ok(Statement::Catch {
            error_name: binding.clone(),
            error_type: error_type.clone(),
        }),
        SystemIrBasicStatement::EndTry => Ok(Statement::EndTry),
        SystemIrBasicStatement::Throw {
            error_type,
            code,
            message,
        } => Ok(Statement::Throw {
            error_type: error_type.clone(),
            code: expr(code)?,
            message: expr(message)?,
        }),
        SystemIrBasicStatement::FunctionReturn(value) => {
            Ok(Statement::FunctionReturn(expr(value)?))
        }
        SystemIrBasicStatement::Return => Ok(Statement::Return),
        SystemIrBasicStatement::EndProcedure => Ok(Statement::EndProcedure),
        SystemIrBasicStatement::End => Ok(Statement::End),
        SystemIrBasicStatement::Call(value) => Ok(Statement::Call(expr(value)?)),
        SystemIrBasicStatement::StarCommand(command) => Ok(Statement::StarCommand(command.clone())),
        SystemIrBasicStatement::NoOp => Ok(Statement::NoOp),
    }
}

fn memory_width_from_ir(width: SystemIrMemoryWidth) -> MemoryWidth {
    match width {
        SystemIrMemoryWidth::Byte => MemoryWidth::Byte,
        SystemIrMemoryWidth::Word => MemoryWidth::Word,
    }
}

fn lower_program_options(options: &ProgramOptions) -> SystemIrProgramOptions {
    SystemIrProgramOptions {
        mode: match options.mode {
            BasicLanguageMode::Classic => SystemIrLanguageMode::Classic,
            BasicLanguageMode::Basic64 => SystemIrLanguageMode::Basic64,
            BasicLanguageMode::Hybrid => SystemIrLanguageMode::Hybrid,
        },
        target: match options.target {
            GraphicsProfile::Hosted => SystemIrGraphicsTarget::Hosted,
            GraphicsProfile::Agon => SystemIrGraphicsTarget::Agon,
        },
        profile: options.profile.clone(),
        text_profile: match options.text_profile {
            crate::graphics::TextRenderingProfile::Classic => SystemIrTextProfile::Classic,
            crate::graphics::TextRenderingProfile::Modern => SystemIrTextProfile::Modern,
        },
        mode_declared: options.mode_declared,
        target_declared: options.target_declared,
        profile_declared: options.profile_declared,
        text_profile_declared: options.text_profile_declared,
    }
}

fn parser_program_options(options: &SystemIrProgramOptions) -> ProgramOptions {
    ProgramOptions {
        mode: match options.mode {
            SystemIrLanguageMode::Classic => BasicLanguageMode::Classic,
            SystemIrLanguageMode::Basic64 => BasicLanguageMode::Basic64,
            SystemIrLanguageMode::Hybrid => BasicLanguageMode::Hybrid,
        },
        target: match options.target {
            SystemIrGraphicsTarget::Hosted => GraphicsProfile::Hosted,
            SystemIrGraphicsTarget::Agon => GraphicsProfile::Agon,
        },
        profile: options.profile.clone(),
        text_profile: match options.text_profile {
            SystemIrTextProfile::Classic => crate::graphics::TextRenderingProfile::Classic,
            SystemIrTextProfile::Modern => crate::graphics::TextRenderingProfile::Modern,
        },
        mode_declared: options.mode_declared,
        target_declared: options.target_declared,
        profile_declared: options.profile_declared,
        text_profile_declared: options.text_profile_declared,
    }
}

fn binary_operator_from_ir(operator: SystemIrBinaryOperator) -> BinaryOp {
    match operator {
        SystemIrBinaryOperator::Add => BinaryOp::Add,
        SystemIrBinaryOperator::Subtract => BinaryOp::Subtract,
        SystemIrBinaryOperator::Multiply => BinaryOp::Multiply,
        SystemIrBinaryOperator::Divide => BinaryOp::Divide,
        SystemIrBinaryOperator::IntegerDivide => BinaryOp::IntegerDivide,
        SystemIrBinaryOperator::Modulo => BinaryOp::Modulo,
        SystemIrBinaryOperator::Power => BinaryOp::Power,
        SystemIrBinaryOperator::ShiftLeft => BinaryOp::ShiftLeft,
        SystemIrBinaryOperator::Equal => BinaryOp::Equal,
        SystemIrBinaryOperator::NotEqual => BinaryOp::NotEqual,
        SystemIrBinaryOperator::Less => BinaryOp::Less,
        SystemIrBinaryOperator::LessEqual => BinaryOp::LessEqual,
        SystemIrBinaryOperator::Greater => BinaryOp::Greater,
        SystemIrBinaryOperator::GreaterEqual => BinaryOp::GreaterEqual,
        SystemIrBinaryOperator::And => BinaryOp::And,
        SystemIrBinaryOperator::Or => BinaryOp::Or,
    }
}

fn opcode_name(opcode: &SystemIrOpcode) -> &'static str {
    match opcode {
        SystemIrOpcode::Assign { .. } => "typed assignment",
        SystemIrOpcode::LocalReadOnlyDeclaration { .. } => "typed read-only local declaration",
        SystemIrOpcode::ProcedureCall { .. } => "typed procedure call",
        SystemIrOpcode::ImportedProcedureCall { .. } => "linked imported procedure call",
        SystemIrOpcode::PrimitiveCall { .. } => "capability-checked primitive call",
        SystemIrOpcode::BeginTry => "structured error region",
        SystemIrOpcode::Catch { .. } => "typed error catch",
        SystemIrOpcode::EndTry => "structured error region end",
        SystemIrOpcode::Throw { .. } => "typed error throw",
        SystemIrOpcode::FunctionReturn { .. } => "typed function result",
        SystemIrOpcode::Basic { .. } => "BASIC statement",
    }
}

#[cfg(feature = "experimental-jit")]
fn program_uses_system_profile(program: &ParsedProgram) -> bool {
    program.options.mode == BasicLanguageMode::Basic64
        || !program.typed_parameters.is_empty()
        || !program.typed_results.is_empty()
        || !program.throws_types.is_empty()
        || !program.system_types.is_empty()
        || !program.module_state_types.is_empty()
        || !program.readonly_bindings.is_empty()
        || !program.readonly_local_bindings.is_empty()
        || program
            .instructions
            .iter()
            .any(|instruction| statement_uses_system_profile(&instruction.statement))
}

#[cfg(feature = "experimental-jit")]
fn statement_uses_system_profile(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::ImportedProcedureCall { .. }
            | Statement::LocalReadOnly { .. }
            | Statement::PrimitiveCall { .. }
            | Statement::Try
            | Statement::Catch { .. }
            | Statement::EndTry
            | Statement::Throw { .. }
    ) || statement_expressions(statement)
        .iter()
        .any(|expression| expression_uses_imported_symbol(expression))
        || matches!(statement, Statement::If(_, then_body, else_body)
            if then_body.iter().chain(else_body).any(statement_uses_system_profile))
}

#[cfg(feature = "experimental-jit")]
fn expression_uses_imported_symbol(expression: &Expr) -> bool {
    matches!(expression, Expr::ImportedFunction { .. })
        || expression_children(expression)
            .into_iter()
            .any(expression_uses_imported_symbol)
}

fn location(path: &str, line: u16) -> SystemIrSourceLocation {
    SystemIrSourceLocation {
        path: path.to_owned(),
        line,
    }
}
