//! Executable BASIC64 System Profile 0.1 module units.
//!
//! The implementation supports a deliberately small, interpreted BASIC64
//! system profile: manifest metadata, named value types, typed signatures,
//! structured error catches, and capability-linked primitive calls. It remains
//! an additive profile; ordinary BBC BASIC source is parsed through its legacy
//! lexer and grammar.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    error::RuntimeError,
    memory::Task,
    swi::{SwiContext, SwiDispatcher},
    tokenized_basic::TokenizedBasicLine,
    ricochet::{
        ArgumentDirection, CapabilityName, DefinitionDescriptor, IdentityAllocator,
        LogicalMemoryContract, ModuleId, ModuleLifecycle, ModuleManifest, ModuleSymbolImport,
        PrimitiveImport, PrimitiveRegistry, RegisterContract, RegisterKind, ReplacementPolicy,
        SemanticVersion, SwiContract, SwiExport,
    },
};

use super::{
    parser::{
        self, LexMode, LocatedStatement, ParsedProgram, ProgramOptions, Statement,
        record_statement, source_rem_comment, split_source_line_number,
    },
    runtime,
};
use crate::configure::BasicLanguageMode;

pub use super::parser::{SystemField, SystemType, SystemTypeDefinition};
pub use super::system_ir::{
    AddressOwner, PortableSystemIr, SystemIrBackend, SystemIrBinaryOperator, SystemIrCallLinkage,
    SystemIrExecutionPlan, SystemIrExpression, SystemIrExpressionKind, SystemIrInstruction,
    SystemIrLoweringError, SystemIrMemoryWidth, SystemIrOpcode, SystemIrParameter, SystemIrPlace,
    SystemIrPlaceKind, SystemIrSourceLocation, SystemIrStorage, SystemIrStoreRule,
    SystemIrUnaryOperator, SystemIrValueType, SystemIrVisibility,
};

#[derive(Clone, Debug)]
pub struct SystemModule {
    pub manifest: ModuleManifest,
    pub definitions: BTreeMap<String, DefinitionDescriptor>,
    persistent_state: BTreeMap<String, SystemType>,
    workspace: runtime::ModuleWorkspace,
    source_text: String,
    program: ParsedProgram,
    typed_ir: PortableSystemIr,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemModuleReflection {
    pub manifest: ModuleManifest,
    pub definitions: Vec<SystemDefinitionReflection>,
    pub types: BTreeMap<String, SystemTypeDefinition>,
    pub private_state: BTreeMap<String, SystemType>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemDefinitionReflection {
    pub descriptor: DefinitionDescriptor,
    pub source_line: u16,
    pub parameters: Vec<String>,
    pub parameter_types: Vec<Option<SystemType>>,
    pub result_type: Option<SystemType>,
    pub throws_type: Option<String>,
    pub public: bool,
}

impl SystemModule {
    pub fn parse(
        source: &str,
        source_path: impl Into<String>,
        allocator: &IdentityAllocator,
    ) -> Result<Self, RuntimeError> {
        let source_path = source_path.into();
        let mut program = ParsedProgram::default();
        let mut module_name = None;
        let mut module_version = None;
        let mut imports = Vec::new();
        let mut dependencies: Vec<(String, SemanticVersion)> = Vec::new();
        let mut symbol_imports: Vec<ModuleSymbolImport> = Vec::new();
        let mut requested_capabilities = BTreeSet::new();
        let mut exports: Vec<SwiExport> = Vec::new();
        let mut symbol_exports: BTreeSet<String> = BTreeSet::new();
        let mut private_symbols: BTreeSet<String> = BTreeSet::new();
        let mut lifecycle = ModuleLifecycle::default();
        let mut persistent_state = BTreeMap::new();
        let mut system_type_builder: Option<SystemTypeBuilder> = None;
        let mut replacement_policy = ReplacementPolicy::CompatibleImmediate;
        let mut replacement_seen = false;
        let mut profile_seen = false;
        let mut basic64_seen = false;
        let mut executable_seen = false;
        let mut next_line = 10_u16;

        for raw_line in source.lines() {
            let (line_number, text) = split_source_line_number(raw_line, next_line)?;
            next_line = line_number.saturating_add(10);
            let comment = source_rem_comment(text);
            let directive = comment.and_then(|comment| {
                let trimmed = comment.trim();
                trimmed
                    .strip_prefix('@')
                    .or_else(|| {
                        trimmed
                            .get(..1)
                            .filter(|marker| marker.eq_ignore_ascii_case("@"))
                    })
                    .map(|_| trimmed)
            });

            if system_type_builder.is_some() {
                if comment.is_some() || text.trim().is_empty() {
                    program.instructions.push(LocatedStatement {
                        line_number,
                        statement: Statement::NoOp,
                    });
                    continue;
                }
                let trimmed = text.trim();
                let closes = system_type_builder
                    .as_ref()
                    .is_some_and(|builder| trimmed.eq_ignore_ascii_case(builder.end_keyword()));
                if closes {
                    let finished = system_type_builder.take().expect("builder is present");
                    let name = finished.name().to_ascii_uppercase();
                    if program
                        .system_types
                        .insert(name, finished.finish())
                        .is_some()
                    {
                        return Err(module_error(
                            line_number,
                            "System Profile type is declared more than once",
                        ));
                    }
                } else {
                    system_type_builder
                        .as_mut()
                        .expect("builder is present")
                        .add_line(trimmed, line_number)?;
                }
                program
                    .line_entries
                    .insert(line_number, program.instructions.len());
                program.instructions.push(LocatedStatement {
                    line_number,
                    statement: Statement::NoOp,
                });
                continue;
            }
            let is_system_metadata = directive.is_some_and(|line| {
                let kind = line.split_ascii_whitespace().next().unwrap_or_default();
                [
                    "@SYSTEM_PROFILE",
                    "@MODULE",
                    "@IMPORT",
                    "@SWI",
                    "@DEPENDS",
                    "@IMPORT_MODULE",
                    "@IMPORT_SYMBOL",
                    "@CAPABILITY",
                    "@LIFECYCLE",
                    "@REPLACE",
                    "@STATE",
                    "@EXPORT",
                    "@PRIVATE",
                ]
                .iter()
                .any(|name| kind.eq_ignore_ascii_case(name))
            });

            if let Some(comment) = comment {
                if comment
                    .trim_start()
                    .to_ascii_uppercase()
                    .starts_with("@BASIC64")
                {
                    if executable_seen || basic64_seen {
                        return Err(module_error(
                            line_number,
                            "BASIC64 profile directive must appear once before executable source",
                        ));
                    }
                    let mut options = ProgramOptions::default();
                    parser::parse_basic64_directive(comment.as_bytes(), line_number, &mut options)?;
                    if options.mode != BasicLanguageMode::Basic64 {
                        return Err(module_error(
                            line_number,
                            "system modules require MODE=BASIC64",
                        ));
                    }
                    program.options = options;
                    basic64_seen = true;
                    program
                        .line_entries
                        .insert(line_number, program.instructions.len());
                    program.instructions.push(LocatedStatement {
                        line_number,
                        statement: Statement::NoOp,
                    });
                    continue;
                }
            }

            if is_system_metadata {
                if executable_seen {
                    return Err(module_error(
                        line_number,
                        "module metadata must appear before executable source",
                    ));
                }
                let metadata = directive.expect("checked metadata directive");
                let mut words = metadata.split_ascii_whitespace();
                let kind = words.next().unwrap_or_default().to_ascii_uppercase();
                match kind.as_str() {
                    "@SYSTEM_PROFILE" => {
                        if profile_seen || words.next() != Some("0.1") || words.next().is_some() {
                            return Err(module_error(
                                line_number,
                                "expected exactly one @SYSTEM_PROFILE 0.1 directive",
                            ));
                        }
                        profile_seen = true;
                    }
                    "@MODULE" => {
                        if module_name.is_some() {
                            return Err(module_error(
                                line_number,
                                "module identity is declared more than once",
                            ));
                        }
                        let name = words
                            .next()
                            .ok_or_else(|| module_error(line_number, "@MODULE requires a name"))?;
                        let version = words.next().ok_or_else(|| {
                            module_error(line_number, "@MODULE requires a semantic version")
                        })?;
                        if words.next().is_some() {
                            return Err(module_error(
                                line_number,
                                "@MODULE syntax is name major.minor.patch",
                            ));
                        }
                        module_name = Some(name.to_owned());
                        module_version = Some(
                            parse_version(version)
                                .map_err(|message| module_error(line_number, &message))?,
                        );
                    }
                    "@IMPORT" => {
                        let name = words.next().ok_or_else(|| {
                            module_error(line_number, "@IMPORT requires a primitive name")
                        })?;
                        let capability = words.next().ok_or_else(|| {
                            module_error(line_number, "@IMPORT requires a capability")
                        })?;
                        let capability = CapabilityName::new(capability)
                            .map_err(|error| module_error(line_number, &error.to_string()))?;
                        if words.next().is_some() {
                            return Err(module_error(
                                line_number,
                                "@IMPORT syntax is primitive capability",
                            ));
                        }
                        if imports
                            .iter()
                            .any(|import: &PrimitiveImport| import.name == name)
                        {
                            return Err(module_error(
                                line_number,
                                "primitive import is declared more than once",
                            ));
                        }
                        imports.push(PrimitiveImport {
                            name: name.to_ascii_uppercase(),
                            capability,
                        });
                    }
                    "@DEPENDS" => {
                        let dependency = words.next().ok_or_else(|| {
                            module_error(line_number, "@DEPENDS requires a module name")
                        })?;
                        let version = words.next().ok_or_else(|| {
                            module_error(
                                line_number,
                                "@DEPENDS requires a minimum semantic version",
                            )
                        })?;
                        if words.next().is_some()
                            || dependencies
                                .iter()
                                .any(|(name, _)| name.eq_ignore_ascii_case(dependency))
                        {
                            return Err(module_error(
                                line_number,
                                "duplicate or malformed module dependency",
                            ));
                        }
                        dependencies.push((
                            dependency.to_owned(),
                            parse_version(version)
                                .map_err(|message| module_error(line_number, &message))?,
                        ));
                    }
                    "@IMPORT_MODULE" => {
                        let dependency = words.next().ok_or_else(|| {
                            module_error(line_number, "@IMPORT_MODULE requires a module name")
                        })?;
                        let version = words.next().ok_or_else(|| {
                            module_error(
                                line_number,
                                "@IMPORT_MODULE requires a minimum semantic version",
                            )
                        })?;
                        if words.next().is_some()
                            || dependencies
                                .iter()
                                .any(|(name, _)| name.eq_ignore_ascii_case(dependency))
                        {
                            return Err(module_error(
                                line_number,
                                "duplicate or malformed module import",
                            ));
                        }
                        dependencies.push((
                            dependency.to_owned(),
                            parse_version(version)
                                .map_err(|message| module_error(line_number, &message))?,
                        ));
                    }
                    "@IMPORT_SYMBOL" => {
                        let dependency = words.next().ok_or_else(|| {
                            module_error(line_number, "@IMPORT_SYMBOL requires a module name")
                        })?;
                        let symbol_kind = words.next().ok_or_else(|| {
                            module_error(line_number, "@IMPORT_SYMBOL requires PROC or FN")
                        })?;
                        let symbol_name = words
                            .next()
                            .ok_or_else(|| {
                                module_error(line_number, "@IMPORT_SYMBOL requires a symbol name")
                            })?
                            .to_ascii_uppercase();
                        if words.next().is_some() || !is_basic_identifier(&symbol_name) {
                            return Err(module_error(
                                line_number,
                                "@IMPORT_SYMBOL syntax is module PROC|FN name",
                            ));
                        }
                        let symbol = match symbol_kind.to_ascii_uppercase().as_str() {
                            "PROC" => symbol_name,
                            "FN" => format!("FN:{symbol_name}"),
                            _ => {
                                return Err(module_error(
                                    line_number,
                                    "@IMPORT_SYMBOL kind must be PROC or FN",
                                ));
                            }
                        };
                        symbol_imports.push(ModuleSymbolImport {
                            module: dependency.to_owned(),
                            symbol,
                        });
                    }
                    "@EXPORT" | "@PRIVATE" => {
                        let is_export = kind.eq_ignore_ascii_case("@EXPORT");
                        let symbol_kind = words.next().ok_or_else(|| {
                            module_error(line_number, "definition visibility requires PROC or FN")
                        })?;
                        let name = words
                            .next()
                            .ok_or_else(|| {
                                module_error(line_number, "definition visibility requires a name")
                            })?
                            .to_ascii_uppercase();
                        if words.next().is_some() || !is_basic_identifier(&name) {
                            return Err(module_error(
                                line_number,
                                "definition visibility syntax is EXPORT|PRIVATE PROC|FN name",
                            ));
                        }
                        let symbol = match symbol_kind.to_ascii_uppercase().as_str() {
                            "PROC" => name,
                            "FN" => format!("FN:{name}"),
                            _ => {
                                return Err(module_error(
                                    line_number,
                                    "visibility kind must be PROC or FN",
                                ));
                            }
                        };
                        if symbol_kind.eq_ignore_ascii_case("PROC")
                            && exports
                                .iter()
                                .any(|export| export.definition_name == symbol)
                            && !is_export
                        {
                            return Err(module_error(
                                line_number,
                                "an SWI export cannot be declared private",
                            ));
                        }
                        if is_export {
                            if private_symbols.contains(&symbol) || !symbol_exports.insert(symbol) {
                                return Err(module_error(
                                    line_number,
                                    "definition visibility is conflicting or duplicated",
                                ));
                            }
                        } else if symbol_exports.contains(&symbol)
                            || !private_symbols.insert(symbol.clone())
                        {
                            return Err(module_error(
                                line_number,
                                "definition visibility is conflicting or duplicated",
                            ));
                        }
                    }
                    "@CAPABILITY" => {
                        let capability = words.next().ok_or_else(|| {
                            module_error(line_number, "@CAPABILITY requires a capability name")
                        })?;
                        if words.next().is_some() {
                            return Err(module_error(line_number, "@CAPABILITY accepts one name"));
                        }
                        let capability = CapabilityName::new(capability)
                            .map_err(|error| module_error(line_number, &error.to_string()))?;
                        if !requested_capabilities.insert(capability) {
                            return Err(module_error(
                                line_number,
                                "capability is requested more than once",
                            ));
                        }
                    }
                    "@LIFECYCLE" => {
                        let hook = words.next().ok_or_else(|| {
                            module_error(
                                line_number,
                                "@LIFECYCLE requires START, QUIESCE, or FINALISE",
                            )
                        })?;
                        let definition = words
                            .next()
                            .ok_or_else(|| {
                                module_error(line_number, "@LIFECYCLE requires a PROC name")
                            })?
                            .to_ascii_uppercase();
                        if words.next().is_some() {
                            return Err(module_error(
                                line_number,
                                "@LIFECYCLE accepts exactly a hook and PROC name",
                            ));
                        }
                        let slot = match hook.to_ascii_uppercase().as_str() {
                            "START" => &mut lifecycle.start,
                            "QUIESCE" => &mut lifecycle.quiesce,
                            "FINALISE" | "FINALIZE" => &mut lifecycle.finalise,
                            _ => return Err(module_error(line_number, "unknown lifecycle hook")),
                        };
                        if slot.replace(definition).is_some() {
                            return Err(module_error(
                                line_number,
                                "lifecycle hook is declared more than once",
                            ));
                        }
                    }
                    "@REPLACE" => {
                        if replacement_seen {
                            return Err(module_error(
                                line_number,
                                "replacement policy is declared more than once",
                            ));
                        }
                        replacement_seen = true;
                        replacement_policy = match words
                            .next()
                            .unwrap_or_default()
                            .to_ascii_uppercase()
                            .as_str()
                        {
                            "IMMEDIATE" => ReplacementPolicy::CompatibleImmediate,
                            "QUIESCENT" => ReplacementPolicy::Quiescent,
                            "MIGRATING" => ReplacementPolicy::Migrating,
                            "RESTART" | "RESTART_REQUIRED" => ReplacementPolicy::RestartRequired,
                            _ => {
                                return Err(module_error(
                                    line_number,
                                    "replacement policy must be IMMEDIATE, QUIESCENT, MIGRATING, or RESTART",
                                ));
                            }
                        };
                        if words.next().is_some() {
                            return Err(module_error(line_number, "@REPLACE accepts one policy"));
                        }
                    }
                    "@STATE" => {
                        let name = words
                            .next()
                            .ok_or_else(|| {
                                module_error(line_number, "@STATE requires a private variable name")
                            })?
                            .to_ascii_uppercase();
                        let type_name = words
                            .next()
                            .ok_or_else(|| module_error(line_number, "@STATE requires a type"))?;
                        let ty = parse_system_type(type_name)
                            .map_err(|message| module_error(line_number, &message))?;
                        let read_only = match words.next() {
                            None => false,
                            Some(value) if value.eq_ignore_ascii_case("READONLY") => true,
                            Some(_) => {
                                return Err(module_error(
                                    line_number,
                                    "@STATE syntax is name TYPE [READONLY]",
                                ));
                            }
                        };
                        if words.next().is_some() || !is_basic_identifier(&name) {
                            return Err(module_error(
                                line_number,
                                "@STATE syntax is name TYPE [READONLY]",
                            ));
                        }
                        if persistent_state.insert(name.clone(), ty).is_some() {
                            return Err(module_error(
                                line_number,
                                "@STATE name is declared more than once",
                            ));
                        }
                        if read_only {
                            program.readonly_bindings.insert(name.to_ascii_uppercase());
                        }
                    }
                    "@SWI" => {
                        let name = words.next().ok_or_else(|| {
                            module_error(line_number, "@SWI requires a public name")
                        })?;
                        let number = words
                            .next()
                            .ok_or_else(|| module_error(line_number, "@SWI requires a number"))?;
                        let number = parse_number(number)
                            .map_err(|message| module_error(line_number, &message))?;
                        let definition_name = words.next().ok_or_else(|| {
                            module_error(line_number, "@SWI requires a BASIC64 definition name")
                        })?;
                        let attributes = words.next().unwrap_or_default();
                        if words.next().is_some() {
                            return Err(module_error(
                                line_number,
                                "@SWI contract attributes must be one semicolon-delimited field",
                            ));
                        }
                        let contract = parse_contract(attributes)
                            .map_err(|message| module_error(line_number, &message))?;
                        exports.push(SwiExport {
                            number,
                            name: name.into(),
                            definition_name: definition_name.to_ascii_uppercase(),
                            contract,
                        });
                    }
                    _ => unreachable!("metadata marker was prefiltered"),
                }
                program
                    .line_entries
                    .insert(line_number, program.instructions.len());
                program.instructions.push(LocatedStatement {
                    line_number,
                    statement: Statement::NoOp,
                });
                continue;
            }

            if let Some(comment) = comment {
                if comment.trim_start().eq_ignore_ascii_case("@BASIC64") {
                    // The profile marker is processed above; this is unreachable.
                    continue;
                }
            }

            if let Some(builder) = parse_system_type_start(text, line_number)? {
                if executable_seen {
                    return Err(module_error(
                        line_number,
                        "type declarations must precede executable source",
                    ));
                }
                system_type_builder = Some(builder);
                program
                    .line_entries
                    .insert(line_number, program.instructions.len());
                program.instructions.push(LocatedStatement {
                    line_number,
                    statement: Statement::NoOp,
                });
                continue;
            }
            if let Some(handle_name) = parse_handle_declaration(text, line_number)? {
                if executable_seen {
                    return Err(module_error(
                        line_number,
                        "type declarations must precede executable source",
                    ));
                }
                if program
                    .system_types
                    .insert(handle_name, SystemTypeDefinition::Handle)
                    .is_some()
                {
                    return Err(module_error(
                        line_number,
                        "System Profile type is declared more than once",
                    ));
                }
                program
                    .line_entries
                    .insert(line_number, program.instructions.len());
                program.instructions.push(LocatedStatement {
                    line_number,
                    statement: Statement::NoOp,
                });
                continue;
            }

            if !text.trim().is_empty() && source_rem_comment(text).is_none() {
                executable_seen = true;
            }
            let transformed = strip_parameter_type_annotations(
                text,
                line_number,
                &mut program.typed_parameters,
                &mut program.typed_results,
                &mut program.throws_types,
            )?;
            let line = TokenizedBasicLine {
                number: line_number,
                bytes: transformed.into_bytes(),
                line_references: Vec::new(),
            };
            program
                .line_entries
                .insert(line_number, program.instructions.len());
            for statement in parser::parse_line(&line, LexMode::SystemSource)? {
                record_statement(&mut program, line_number, statement);
            }
        }

        if system_type_builder.is_some() {
            return Err(module_error(
                next_line.saturating_sub(10),
                "unterminated System Profile type declaration",
            ));
        }

        if !basic64_seen {
            return Err(module_error(
                0,
                "system module is missing REM @BASIC64 MODE=BASIC64",
            ));
        }
        if !profile_seen {
            return Err(module_error(
                0,
                "system module is missing REM @SYSTEM_PROFILE 0.1",
            ));
        }
        let name =
            module_name.ok_or_else(|| module_error(0, "system module is missing REM @MODULE"))?;
        let version = module_version.expect("module name and version are parsed together");
        program.module_state_types = persistent_state.clone();
        validate_system_type_definitions(&mut program)?;
        validate_program_types(&mut program)?;
        validate_symbol_calls(&program, &symbol_imports)?;
        for (state_name, state_type) in &program.module_state_types {
            if system_type_contains_address(state_type, &program.system_types, &mut BTreeSet::new())
            {
                return Err(module_error(
                    0,
                    &format!(
                        "module state {state_name} cannot retain caller-scoped ADDRESS32 values"
                    ),
                ));
            }
        }
        persistent_state = program.module_state_types.clone();
        if exports.is_empty() && symbol_exports.is_empty() && lifecycle.start.is_none() {
            return Err(module_error(
                0,
                "system module must define a SWI, exported symbol, or startup hook",
            ));
        }

        for export in &exports {
            if !program.procedures.contains_key(&export.definition_name) {
                return Err(module_error(
                    0,
                    &format!(
                        "SWI {} names missing PROC {}",
                        export.name, export.definition_name
                    ),
                ));
            }
        }
        for export in &exports {
            if private_symbols.contains(&export.definition_name) {
                return Err(module_error(
                    0,
                    &format!(
                        "SWI export {} conflicts with a private definition declaration",
                        export.name
                    ),
                ));
            }
            symbol_exports.insert(export.definition_name.clone());
            for register in &export.contract.registers {
                if let RegisterKind::OpaqueHandle { type_name } = &register.kind
                    && !matches!(
                        program.system_types.get(&type_name.to_ascii_uppercase()),
                        Some(SystemTypeDefinition::Handle)
                    )
                {
                    return Err(module_error(
                        0,
                        &format!(
                            "SWI {} contract references undeclared opaque handle type {type_name}",
                            export.name
                        ),
                    ));
                }
            }
        }
        for symbol in &symbol_exports {
            let exists = if let Some(name) = symbol.strip_prefix("FN:") {
                program.functions.contains_key(name)
            } else {
                program.procedures.contains_key(symbol)
            };
            if !exists {
                return Err(module_error(
                    0,
                    &format!("exported BASIC64 symbol {symbol} is not defined"),
                ));
            }
        }
        for symbol in &private_symbols {
            let exists = if let Some(name) = symbol.strip_prefix("FN:") {
                program.functions.contains_key(name)
            } else {
                program.procedures.contains_key(symbol)
            };
            if !exists {
                return Err(module_error(
                    0,
                    &format!("private BASIC64 symbol {symbol} is not defined"),
                ));
            }
        }
        validate_primitive_references(&program, &imports)?;
        for import in &imports {
            if !requested_capabilities.contains(&import.capability) {
                return Err(module_error(
                    0,
                    &format!(
                        "primitive import {} requires explicit @CAPABILITY {}",
                        import.name,
                        import.capability.as_str()
                    ),
                ));
            }
        }
        for hook in [&lifecycle.start, &lifecycle.quiesce, &lifecycle.finalise]
            .into_iter()
            .flatten()
        {
            let Some(definition) = program.procedures.get(hook) else {
                return Err(module_error(
                    0,
                    &format!("lifecycle hook PROC {hook} is not defined"),
                ));
            };
            if !definition.parameters.is_empty() {
                return Err(module_error(
                    0,
                    &format!("lifecycle hook PROC {hook} must not require arguments"),
                ));
            }
        }
        validate_dependencies(&name, &dependencies)?;
        for import in &symbol_imports {
            if !dependencies
                .iter()
                .any(|(dependency, _)| dependency.eq_ignore_ascii_case(&import.module))
            {
                return Err(module_error(
                    0,
                    &format!(
                        "@IMPORT_SYMBOL {}.{} requires @IMPORT_MODULE or @DEPENDS",
                        import.module, import.symbol
                    ),
                ));
            }
        }
        let source_hash = source_digest(source.as_bytes());
        let target_profile = match program.options.target {
            crate::graphics::GraphicsProfile::Hosted => "HOSTED",
            crate::graphics::GraphicsProfile::Agon => "AGON",
        }
        .to_owned();
        let symbol_exports = symbol_exports.into_iter().collect::<Vec<_>>();
        let manifest = ModuleManifest {
            schema_version: 1,
            name,
            version,
            language_profile: "BASIC64-SYSTEM-0.1".into(),
            target_profile: target_profile.clone(),
            dependencies,
            symbol_imports,
            primitive_imports: imports,
            requested_capabilities,
            lifecycle,
            replacement_policy,
            symbol_exports,
            exports,
            source_path: source_path.clone(),
            source_hash: source_hash.clone(),
        };
        manifest
            .encode_v1()
            .map_err(|error| module_error(0, &error.to_string()))?;
        let placeholder_module = allocator.module_id();
        let mut definitions = BTreeMap::new();
        for name in program.procedures.keys() {
            definitions.insert(
                name.clone(),
                DefinitionDescriptor {
                    id: allocator.definition_id(),
                    name: name.clone(),
                    module: placeholder_module,
                    source_path: source_path.clone(),
                    source_hash: source_hash.clone(),
                    language_profile: "BASIC64-SYSTEM-0.1".into(),
                    target_profile: target_profile.clone(),
                },
            );
        }
        for name in program.functions.keys() {
            definitions.insert(
                format!("FN:{name}"),
                DefinitionDescriptor {
                    id: allocator.definition_id(),
                    name: format!("FN {name}"),
                    module: placeholder_module,
                    source_path: source_path.clone(),
                    source_hash: source_hash.clone(),
                    language_profile: "BASIC64-SYSTEM-0.1".into(),
                    target_profile: target_profile.clone(),
                },
            );
        }
        let typed_ir = PortableSystemIr::lower_module(&manifest, &definitions, &program);
        Ok(Self {
            manifest,
            definitions,
            persistent_state,
            workspace: runtime::ModuleWorkspace::default(),
            source_text: source.to_owned(),
            program,
            typed_ir,
        })
    }

    /// Backend-neutral typed module representation, including source maps,
    /// definition contracts, private workspace types, and checked memory ops.
    pub fn typed_ir(&self) -> &PortableSystemIr {
        &self.typed_ir
    }

    pub(crate) fn preserve_module_title(&mut self, title: &str) {
        self.manifest.name = title.to_owned();
        self.typed_ir.module_name = title.to_owned();
        if let Some(manifest) = self.typed_ir.manifest.as_mut() {
            manifest.name = title.to_owned();
        }
    }

    #[cfg(test)]
    pub(crate) fn test_read_workspace_number(&self, name: &str) -> Option<f64> {
        self.workspace.read_number(name)
    }

    pub fn prepare_execution(
        &self,
        backend: SystemIrBackend,
    ) -> Result<SystemIrExecutionPlan, SystemIrLoweringError> {
        self.typed_ir.prepare(backend)
    }

    pub fn validate_primitive_shapes(
        &self,
        primitives: &PrimitiveRegistry,
    ) -> Result<(), RuntimeError> {
        let imports = self
            .manifest
            .primitive_imports
            .iter()
            .map(|import| import.name.as_str())
            .collect::<BTreeSet<_>>();
        for instruction in &self.program.instructions {
            validate_statement_primitive_shapes(
                &instruction.statement,
                &imports,
                primitives,
                instruction.line_number,
            )?;
        }
        Ok(())
    }

    /// Read-only source/type/export metadata for diagnostics and a future live
    /// system browser. It grants no module, primitive, or edit authority.
    pub fn reflection(&self) -> SystemModuleReflection {
        let mut definitions = Vec::with_capacity(self.definitions.len());
        for (key, descriptor) in &self.definitions {
            let (is_function, name) = key
                .strip_prefix("FN:")
                .map(|name| (true, name))
                .unwrap_or((false, key.as_str()));
            let definition = if is_function {
                self.program.functions.get(name)
            } else {
                self.program.procedures.get(name)
            };
            let Some(definition) = definition else {
                continue;
            };
            let source_line = definition
                .entry
                .checked_sub(1)
                .and_then(|entry| self.program.instructions.get(entry))
                .map_or(0, |instruction| instruction.line_number);
            let typed_parameters = self.program.typed_parameters.get(name);
            let parameter_types = definition
                .parameters
                .iter()
                .enumerate()
                .map(|(index, _)| typed_parameters.and_then(|types| types.get(index)).cloned())
                .collect();
            definitions.push(SystemDefinitionReflection {
                descriptor: descriptor.clone(),
                source_line,
                parameters: definition.parameters.clone(),
                parameter_types,
                result_type: is_function
                    .then(|| self.program.typed_results.get(name).cloned())
                    .flatten(),
                throws_type: self.program.throws_types.get(name).cloned(),
                public: self
                    .manifest
                    .symbol_exports
                    .iter()
                    .any(|symbol| symbol.eq_ignore_ascii_case(key)),
            });
        }
        SystemModuleReflection {
            manifest: self.manifest.clone(),
            definitions,
            types: self.program.system_types.clone(),
            private_state: self.persistent_state.clone(),
        }
    }

    /// Returns the retained BASIC64 source block for one procedure or
    /// function. This is intentionally sourced from the parsed module's
    /// retained input, never reopened through HostFS.
    pub(crate) fn definition_source(&self, definition: &str) -> Option<String> {
        let definition = definition.trim();
        let upper = definition.to_ascii_uppercase();
        let normalized = upper
            .strip_prefix("FN:")
            .or_else(|| upper.strip_prefix("FN "))
            .map(|name| format!("FN:{name}"))
            .unwrap_or(upper);
        self.definitions.get(&normalized)?;

        let lines = self.source_text.split_inclusive('\n').collect::<Vec<_>>();
        let start = lines
            .iter()
            .position(|line| source_definition_key(line).as_deref() == Some(normalized.as_str()))?;
        let is_function = normalized.starts_with("FN:");
        let end = if is_function {
            (start + 1..lines.len())
                .find(|index| source_definition_key(lines[*index]).is_some())
                .unwrap_or(lines.len())
        } else {
            (start + 1..lines.len())
                .find(|index| source_line_is_endproc(lines[*index]))
                .map(|index| index + 1)
                .unwrap_or_else(|| {
                    (start + 1..lines.len())
                        .find(|index| source_definition_key(lines[*index]).is_some())
                        .unwrap_or(lines.len())
                })
        };
        if end <= start {
            return None;
        }
        let start_offset = lines[..start].iter().map(|line| line.len()).sum::<usize>();
        let end_offset = start_offset
            + lines[start..end]
                .iter()
                .map(|line| line.len())
                .sum::<usize>();
        self.source_text
            .get(start_offset..end_offset)
            .map(str::to_owned)
    }

    pub(crate) fn invoke(
        &self,
        module_id: ModuleId,
        definition: &DefinitionDescriptor,
        contract: &SwiContract,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        self.typed_ir
            .prepare(SystemIrBackend::Interpreter)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        let program = self
            .typed_ir
            .lower_for_reference_interpreter()
            .map_err(RuntimeError::Program)?;
        runtime::invoke_system_definition(
            program,
            &definition.name,
            module_id,
            contract,
            &self.workspace,
            &self.persistent_state,
            task,
            dispatcher,
            context,
        )
    }

    pub(crate) fn invoke_lifecycle(
        &self,
        hook: &str,
        module_id: ModuleId,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        let definition = match hook.to_ascii_uppercase().as_str() {
            "START" => self.manifest.lifecycle.start.as_ref(),
            "QUIESCE" => self.manifest.lifecycle.quiesce.as_ref(),
            "FINALISE" | "FINALIZE" => self.manifest.lifecycle.finalise.as_ref(),
            _ => None,
        };
        let Some(definition) = definition else {
            return Ok(());
        };
        self.typed_ir
            .prepare(SystemIrBackend::Interpreter)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        let program = self
            .typed_ir
            .lower_for_reference_interpreter()
            .map_err(RuntimeError::Program)?;
        runtime::invoke_system_definition(
            program,
            definition,
            module_id,
            &SwiContract::default(),
            &self.workspace,
            &self.persistent_state,
            task,
            dispatcher,
            &mut SwiContext::default(),
        )
    }

    pub(crate) fn invoke_imported_symbol(
        &self,
        module_id: ModuleId,
        symbol: &str,
        values: Vec<runtime::Value>,
        function: bool,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<Option<runtime::Value>, RuntimeError> {
        let (actual_function, name) = symbol
            .strip_prefix("FN:")
            .map(|name| (true, name.to_ascii_uppercase()))
            .unwrap_or_else(|| (false, symbol.to_ascii_uppercase()));
        if actual_function != function {
            return Err(module_error(
                0,
                "qualified symbol kind does not match call form",
            ));
        }
        if !self
            .manifest
            .symbol_exports
            .iter()
            .any(|export| export.eq_ignore_ascii_case(symbol))
        {
            return Err(module_error(
                0,
                &format!("symbol {symbol} is not exported by {}", self.manifest.name),
            ));
        }
        self.typed_ir
            .prepare(SystemIrBackend::Interpreter)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        let program = self
            .typed_ir
            .lower_for_reference_interpreter()
            .map_err(RuntimeError::Program)?;
        runtime::invoke_imported_system_symbol(
            program,
            &name,
            function,
            values,
            module_id,
            &self.workspace,
            &self.persistent_state,
            task,
            dispatcher,
        )
    }

    /// Runs a lifecycle hook transactionally with respect to private module
    /// workspace. Host effects performed by primitives are not reversible;
    /// hooks must defer irreversible effects until they can succeed.
    pub(crate) fn invoke_lifecycle_transactional(
        &self,
        hook: &str,
        module_id: ModuleId,
        task: &mut Task,
        dispatcher: &mut SwiDispatcher,
    ) -> Result<(), RuntimeError> {
        let snapshot = self.workspace.snapshot();
        match self.invoke_lifecycle(hook, module_id, task, dispatcher) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.workspace.restore(snapshot);
                Err(error)
            }
        }
    }

    pub(crate) fn has_compatible_workspace(&self, other: &Self) -> bool {
        self.persistent_state == other.persistent_state
    }

    pub(crate) fn has_compatible_workspace_schema(&self, other: &Self) -> bool {
        // A retained state slot may name a RECORD/ENUM/FLAGS/ERROR whose
        // layout is stored separately from the slot's SystemType. Requiring
        // the complete named-type table to match keeps immediate replacement
        // out of state migration and rejects otherwise invisible layout edits.
        self.persistent_state == other.persistent_state
            && self.program.system_types == other.program.system_types
    }

    pub(crate) fn has_compatible_public_abi(&self, other: &Self) -> bool {
        if self.manifest.symbol_exports != other.manifest.symbol_exports
            || self.manifest.lifecycle != other.manifest.lifecycle
        {
            return false;
        }
        let left = self.reflection();
        let right = other.reflection();
        let same_definition = |key: &str| {
            let display_name = key
                .strip_prefix("FN:")
                .map(|name| format!("FN {name}"))
                .unwrap_or_else(|| key.to_owned());
            let left_definition = left.definitions.iter().find(|definition| {
                definition
                    .descriptor
                    .name
                    .eq_ignore_ascii_case(&display_name)
            });
            let right_definition = right.definitions.iter().find(|definition| {
                definition
                    .descriptor
                    .name
                    .eq_ignore_ascii_case(&display_name)
            });
            match (left_definition, right_definition) {
                (Some(left), Some(right)) => {
                    left.parameters.len() == right.parameters.len()
                        && left.parameter_types == right.parameter_types
                        && left.result_type == right.result_type
                        && left.throws_type == right.throws_type
                }
                _ => false,
            }
        };
        if !self
            .manifest
            .symbol_exports
            .iter()
            .all(|symbol| same_definition(symbol))
        {
            return false;
        }
        [
            self.manifest.lifecycle.start.as_deref(),
            self.manifest.lifecycle.quiesce.as_deref(),
            self.manifest.lifecycle.finalise.as_deref(),
        ]
        .into_iter()
        .flatten()
        .all(|hook| same_definition(hook))
    }

    pub(crate) fn inherit_workspace(&mut self, other: &Self) -> Result<(), RuntimeError> {
        if !self.has_compatible_workspace(other) {
            return Err(RuntimeError::Program(
                "live replacement changes the private workspace schema".into(),
            ));
        }
        self.workspace = other.workspace.clone();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn workspace_number(&self, name: &str) -> Option<f64> {
        self.workspace.read_number(&name.to_ascii_uppercase())
    }
}

fn is_basic_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphabetic()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'%'))
}

enum SystemTypeBuilder {
    Record(String, Vec<SystemField>),
    Enum(String, SystemType, BTreeMap<String, i64>),
    Flags(String, SystemType, BTreeMap<String, i64>),
    Error(String, Vec<SystemField>),
}

impl SystemTypeBuilder {
    fn name(&self) -> &str {
        match self {
            Self::Record(name, _)
            | Self::Enum(name, _, _)
            | Self::Flags(name, _, _)
            | Self::Error(name, _) => name,
        }
    }

    fn end_keyword(&self) -> &'static str {
        match self {
            Self::Record(..) => "END RECORD",
            Self::Enum(..) => "END ENUM",
            Self::Flags(..) => "END FLAGS",
            Self::Error(..) => "END ERROR",
        }
    }

    fn finish(self) -> SystemTypeDefinition {
        match self {
            Self::Record(_, fields) => SystemTypeDefinition::Record { fields },
            Self::Enum(_, underlying, members) => SystemTypeDefinition::Enum {
                underlying,
                members,
            },
            Self::Flags(_, underlying, members) => SystemTypeDefinition::Flags {
                underlying,
                members,
            },
            Self::Error(_, fields) => SystemTypeDefinition::Error { fields },
        }
    }

    fn add_line(&mut self, text: &str, line: u16) -> Result<(), RuntimeError> {
        let flags = matches!(&*self, Self::Flags(..));
        match self {
            Self::Record(_, fields) | Self::Error(_, fields) => {
                let field = parse_system_field(text, line)?;
                if fields.iter().any(|existing| existing.name == field.name) {
                    return Err(module_error(
                        line,
                        "record field is declared more than once",
                    ));
                }
                fields.push(field);
            }
            Self::Enum(_, _, members) | Self::Flags(_, _, members) => {
                let (name, value) = text
                    .split_once('=')
                    .ok_or_else(|| module_error(line, "enum and flag members use NAME = value"))?;
                let name = name.trim().to_ascii_uppercase();
                if !is_basic_identifier(&name) {
                    return Err(module_error(line, "invalid enum or flag member name"));
                }
                let value = parse_signed_integer(value.trim())
                    .map_err(|message| module_error(line, &message))?;
                if flags {
                    if value < 0 || (value != 0 && (value as u64).count_ones() != 1) {
                        return Err(module_error(
                            line,
                            "flag values must be zero or one distinct bit",
                        ));
                    }
                    if value != 0 && members.values().any(|other| *other == value) {
                        return Err(module_error(line, "flag bits must be distinct"));
                    }
                }
                if members.insert(name, value).is_some() {
                    return Err(module_error(
                        line,
                        "enum or flag member is declared more than once",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn source_line_body(line: &str) -> &str {
    let trimmed = line.trim_start();
    let digit_count = trimmed
        .bytes()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if digit_count == 0 {
        return trimmed;
    }
    trimmed[digit_count..].trim_start()
}

fn source_definition_key(line: &str) -> Option<String> {
    let body = source_line_body(line.trim_end_matches(['\r', '\n']));
    let mut words = body.split_ascii_whitespace();
    if !words.next()?.eq_ignore_ascii_case("DEF") {
        return None;
    }
    let kind = words.next()?;
    let name = words
        .next()?
        .split(['(', ' ', '\t'])
        .next()?
        .trim_end_matches(['%', '$', '!']);
    if name.is_empty() {
        return None;
    }
    if kind.eq_ignore_ascii_case("PROC") {
        Some(name.to_ascii_uppercase())
    } else if kind.eq_ignore_ascii_case("FN") {
        Some(format!("FN:{}", name.to_ascii_uppercase()))
    } else {
        None
    }
}

fn source_line_is_endproc(line: &str) -> bool {
    source_line_body(line.trim_end_matches(['\r', '\n']))
        .split_ascii_whitespace()
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("ENDPROC"))
}

fn parse_system_field(text: &str, line: u16) -> Result<SystemField, RuntimeError> {
    let mut words = text.split_ascii_whitespace();
    let name = words.next().unwrap_or_default().to_ascii_uppercase();
    if !is_basic_identifier(&name) {
        return Err(module_error(line, "invalid record field name"));
    }
    if !words
        .next()
        .is_some_and(|word| word.eq_ignore_ascii_case("AS"))
    {
        return Err(module_error(line, "record fields use NAME AS TYPE"));
    }
    let type_name = words
        .next()
        .ok_or_else(|| module_error(line, "record field has no type"))?;
    let read_only = match words.next() {
        None => false,
        Some(value) if value.eq_ignore_ascii_case("READONLY") => true,
        Some(_) => return Err(module_error(line, "record field modifier must be READONLY")),
    };
    if words.next().is_some() {
        return Err(module_error(line, "record field has extra tokens"));
    }
    Ok(SystemField {
        name,
        value_type: parse_system_type(type_name).map_err(|message| module_error(line, &message))?,
        read_only,
    })
}

fn parse_system_type_start(
    text: &str,
    line: u16,
) -> Result<Option<SystemTypeBuilder>, RuntimeError> {
    let mut words = text.split_ascii_whitespace();
    let kind = words.next().unwrap_or_default().to_ascii_uppercase();
    let name = words.next().unwrap_or_default().to_ascii_uppercase();
    if !matches!(kind.as_str(), "RECORD" | "ENUM" | "FLAGS" | "ERROR") {
        return Ok(None);
    }
    if !is_basic_identifier(&name) {
        return Err(module_error(line, "type declaration requires a valid name"));
    }
    let rest = words.collect::<Vec<_>>();
    match kind.as_str() {
        "RECORD" | "ERROR" if rest.is_empty() => Ok(Some(if kind == "RECORD" {
            SystemTypeBuilder::Record(name, Vec::new())
        } else {
            SystemTypeBuilder::Error(name, Vec::new())
        })),
        "ENUM" | "FLAGS" if rest.len() == 2 && rest[0].eq_ignore_ascii_case("AS") => {
            let underlying =
                parse_system_type(rest[1]).map_err(|message| module_error(line, &message))?;
            if !is_integer_type(&underlying) {
                return Err(module_error(
                    line,
                    "enum and flag underlying types must be integers",
                ));
            }
            Ok(Some(if kind == "ENUM" {
                SystemTypeBuilder::Enum(name, underlying, BTreeMap::new())
            } else {
                SystemTypeBuilder::Flags(name, underlying, BTreeMap::new())
            }))
        }
        _ => Err(module_error(
            line,
            "type declarations use RECORD|ERROR name, ENUM|FLAGS name AS integer-type, or HANDLE name",
        )),
    }
}

fn parse_handle_declaration(text: &str, line: u16) -> Result<Option<String>, RuntimeError> {
    let mut words = text.split_ascii_whitespace();
    let kind = words.next().unwrap_or_default();
    if !kind.eq_ignore_ascii_case("HANDLE") {
        return Ok(None);
    }
    let name = words.next().unwrap_or_default().to_ascii_uppercase();
    if !is_basic_identifier(&name) || words.next().is_some() {
        return Err(module_error(
            line,
            "opaque handle declarations use HANDLE Name",
        ));
    }
    Ok(Some(name))
}

fn is_integer_type(kind: &SystemType) -> bool {
    matches!(
        kind,
        SystemType::Byte
            | SystemType::UInt16
            | SystemType::UInt32
            | SystemType::Int32
            | SystemType::UInt64
            | SystemType::Int64
    )
}

fn validate_system_type_definitions(program: &mut ParsedProgram) -> Result<(), RuntimeError> {
    let type_names = program
        .system_types
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    if type_names.len() != program.system_types.len() {
        return Err(module_error(0, "System Profile type names must be unique"));
    }
    let snapshot = program.system_types.clone();
    for definition in program.system_types.values_mut() {
        match definition {
            SystemTypeDefinition::Record { fields } | SystemTypeDefinition::Error { fields } => {
                let mut seen = BTreeSet::new();
                for field in fields {
                    if !seen.insert(field.name.clone()) {
                        return Err(module_error(0, "record field names must be unique"));
                    }
                    resolve_system_type(&mut field.value_type, &snapshot)?;
                }
            }
            SystemTypeDefinition::Enum {
                underlying,
                members,
            }
            | SystemTypeDefinition::Flags {
                underlying,
                members,
            } => {
                if !is_integer_type(underlying) {
                    return Err(module_error(
                        0,
                        "enum and flags must use an integer underlying type",
                    ));
                }
                if members.is_empty() {
                    return Err(module_error(
                        0,
                        "enum and flags require at least one member",
                    ));
                }
                for value in members.values() {
                    validate_integer_literal(*value, underlying)?;
                }
            }
            SystemTypeDefinition::Handle => {}
        }
    }
    validate_type_cycles(&program.system_types, &type_names)?;
    Ok(())
}

fn system_type_contains_address(
    kind: &SystemType,
    definitions: &BTreeMap<String, SystemTypeDefinition>,
    visited: &mut BTreeSet<String>,
) -> bool {
    match kind {
        SystemType::Address32 => true,
        SystemType::Record(name) | SystemType::Error(name) => {
            if !visited.insert(name.clone()) {
                return false;
            }
            let contains = match definitions.get(name) {
                Some(SystemTypeDefinition::Record { fields })
                | Some(SystemTypeDefinition::Error { fields }) => fields.iter().any(|field| {
                    system_type_contains_address(&field.value_type, definitions, visited)
                }),
                _ => false,
            };
            visited.remove(name);
            contains
        }
        SystemType::Byte
        | SystemType::UInt16
        | SystemType::UInt32
        | SystemType::Int32
        | SystemType::UInt64
        | SystemType::Int64
        | SystemType::String
        | SystemType::Enum(_)
        | SystemType::Flags(_)
        | SystemType::Handle(_) => false,
    }
}

fn validate_type_cycles(
    definitions: &BTreeMap<String, SystemTypeDefinition>,
    names: &BTreeSet<String>,
) -> Result<(), RuntimeError> {
    fn visit(
        name: &str,
        definitions: &BTreeMap<String, SystemTypeDefinition>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
    ) -> Result<(), RuntimeError> {
        if visited.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name.to_owned()) {
            return Err(module_error(
                0,
                &format!("recursive value type {name} has no finite default layout"),
            ));
        }
        if let Some(
            SystemTypeDefinition::Record { fields } | SystemTypeDefinition::Error { fields },
        ) = definitions.get(name)
        {
            for field in fields {
                let child = match &field.value_type {
                    SystemType::Record(child) | SystemType::Error(child) => Some(child.as_str()),
                    _ => None,
                };
                if let Some(child) = child {
                    visit(child, definitions, visiting, visited)?;
                }
            }
        }
        visiting.remove(name);
        visited.insert(name.to_owned());
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for name in names {
        visit(name, definitions, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn validate_integer_literal(value: i64, kind: &SystemType) -> Result<(), RuntimeError> {
    let valid = match kind {
        SystemType::Byte => (0..=u8::MAX.into()).contains(&value),
        SystemType::UInt16 => (0..=u16::MAX.into()).contains(&value),
        SystemType::UInt32 => value >= 0,
        SystemType::Int32 => (i64::from(i32::MIN)..=i64::from(i32::MAX)).contains(&value),
        SystemType::UInt64 => value >= 0,
        SystemType::Int64 => true,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(module_error(
            0,
            "enum member value is outside its underlying type",
        ))
    }
}

fn resolve_system_type(
    kind: &mut SystemType,
    definitions: &BTreeMap<String, SystemTypeDefinition>,
) -> Result<(), RuntimeError> {
    match kind {
        SystemType::Record(name) => {
            let definition = definitions
                .get(name)
                .ok_or_else(|| module_error(0, &format!("unknown System Profile type {name}")))?;
            *kind = match definition {
                SystemTypeDefinition::Record { .. } => SystemType::Record(name.clone()),
                SystemTypeDefinition::Enum { .. } => SystemType::Enum(name.clone()),
                SystemTypeDefinition::Flags { .. } => SystemType::Flags(name.clone()),
                SystemTypeDefinition::Error { .. } => SystemType::Error(name.clone()),
                SystemTypeDefinition::Handle => {
                    return Err(module_error(0, "opaque handles must use HANDLE<Type>"));
                }
            };
        }
        SystemType::Handle(name) => {
            if !matches!(definitions.get(name), Some(SystemTypeDefinition::Handle)) {
                return Err(module_error(
                    0,
                    &format!("unknown opaque handle type {name}"),
                ));
            }
        }
        SystemType::Error(name) => {
            if !matches!(
                definitions.get(name),
                Some(SystemTypeDefinition::Error { .. })
            ) {
                return Err(module_error(
                    0,
                    &format!("{name} is not a structured error type"),
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_program_types(program: &mut ParsedProgram) -> Result<(), RuntimeError> {
    let definitions = program.system_types.clone();
    for types in program.typed_parameters.values_mut() {
        for kind in types {
            resolve_system_type(kind, &definitions)?;
        }
    }
    for kind in program
        .typed_results
        .values_mut()
        .chain(program.module_state_types.values_mut())
    {
        resolve_system_type(kind, &definitions)?;
    }
    for error_name in program.throws_types.values() {
        if !matches!(
            definitions.get(error_name),
            Some(SystemTypeDefinition::Error { .. })
        ) {
            return Err(module_error(
                0,
                &format!("THROWS type {error_name} is not a structured ERROR"),
            ));
        }
    }
    program.readonly_local_bindings.clear();
    let mut routine = None::<String>;
    for instruction in &mut program.instructions {
        match &mut instruction.statement {
            Statement::DefineProcedure(name, _) | Statement::DefineFunction(name, _) => {
                routine = Some(name.clone());
            }
            Statement::LocalReadOnly {
                name, value_type, ..
            } => {
                let Some(current_routine) = routine.as_ref() else {
                    return Err(module_error(
                        instruction.line_number,
                        "LET READONLY is valid only inside a PROC or FN",
                    ));
                };
                resolve_system_type(value_type, &definitions)?;
                let bindings = program
                    .readonly_local_bindings
                    .entry(current_routine.clone())
                    .or_default();
                if bindings.insert(name.clone(), value_type.clone()).is_some() {
                    return Err(module_error(
                        instruction.line_number,
                        "read-only local binding is declared more than once in a PROC",
                    ));
                }
            }
            Statement::EndProcedure | Statement::FunctionReturn(_) => routine = None,
            _ => {}
        }
    }
    let mut routine = None::<String>;
    for instruction in &program.instructions {
        match &instruction.statement {
            Statement::DefineProcedure(name, _) | Statement::DefineFunction(name, _) => {
                routine = Some(name.clone());
            }
            Statement::EndProcedure => routine = None,
            Statement::FunctionReturn(_) => routine = None,
            _ => validate_throw_contract(
                &instruction.statement,
                routine.as_deref(),
                &program.throws_types,
                instruction.line_number,
            )?,
        }
    }
    for binding in &program.readonly_bindings {
        if !program.module_state_types.contains_key(binding) {
            return Err(module_error(0, "read-only bindings must name module state"));
        }
    }
    let error_types = definitions
        .iter()
        .filter_map(|(name, definition)| {
            matches!(definition, SystemTypeDefinition::Error { .. }).then_some(name.as_str())
        })
        .collect::<BTreeSet<_>>();
    let mut try_stack = Vec::new();
    for instruction in &program.instructions {
        match &instruction.statement {
            Statement::Try => try_stack.push(false),
            Statement::Catch { error_type, .. } => {
                if !error_types.contains(error_type.as_str()) {
                    return Err(module_error(
                        instruction.line_number,
                        &format!("CATCH type {error_type} is not a structured ERROR"),
                    ));
                }
                let Some(caught) = try_stack.last_mut() else {
                    return Err(module_error(
                        instruction.line_number,
                        "CATCH has no matching TRY",
                    ));
                };
                if *caught {
                    return Err(module_error(
                        instruction.line_number,
                        "a TRY block may have only one CATCH",
                    ));
                }
                *caught = true;
            }
            Statement::EndTry => {
                if !try_stack.pop().is_some_and(|caught| caught) {
                    return Err(module_error(
                        instruction.line_number,
                        "ENDTRY requires one matching TRY/CATCH",
                    ));
                }
            }
            Statement::Throw { error_type, .. } if !error_types.contains(error_type.as_str()) => {
                return Err(module_error(
                    instruction.line_number,
                    &format!("THROW type {error_type} is not a structured ERROR"),
                ));
            }
            Statement::If(_, then_body, else_body)
                if then_body.iter().chain(else_body).any(|nested| {
                    matches!(
                        nested,
                        Statement::Try | Statement::Catch { .. } | Statement::EndTry
                    )
                }) =>
            {
                return Err(module_error(
                    instruction.line_number,
                    "TRY/CATCH/ENDTRY must use block form, not an inline IF",
                ));
            }
            _ => {}
        }
    }
    if !try_stack.is_empty() {
        return Err(module_error(0, "TRY has no matching CATCH/ENDTRY"));
    }
    if program
        .functions
        .keys()
        .any(|name| !program.typed_results.contains_key(name))
    {
        return Err(module_error(
            0,
            "every BASIC64 FN requires an explicit result type",
        ));
    }
    Ok(())
}

fn validate_throw_contract(
    statement: &Statement,
    routine: Option<&str>,
    throws_types: &std::collections::HashMap<String, String>,
    line: u16,
) -> Result<(), RuntimeError> {
    match statement {
        Statement::Throw { error_type, .. } => {
            let Some(routine) = routine else {
                return Err(module_error(
                    line,
                    "THROW must be inside a declared PROC or FN",
                ));
            };
            if !throws_types
                .get(routine)
                .is_some_and(|declared| declared.eq_ignore_ascii_case(error_type))
            {
                return Err(module_error(
                    line,
                    &format!("{routine} must declare THROWS {error_type}"),
                ));
            }
        }
        Statement::If(_, then_body, else_body) => {
            for nested in then_body.iter().chain(else_body) {
                validate_throw_contract(nested, routine, throws_types, line)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_signed_integer(value: &str) -> Result<i64, String> {
    if let Some(hex) = value.strip_prefix('&') {
        i64::from_str_radix(hex, 16).map_err(|_| format!("invalid integer literal {value}"))
    } else {
        value
            .parse::<i64>()
            .map_err(|_| format!("invalid integer literal {value}"))
    }
}

fn validate_statement_primitive_shapes(
    statement: &Statement,
    imports: &BTreeSet<&str>,
    primitives: &PrimitiveRegistry,
    line: u16,
) -> Result<(), RuntimeError> {
    match statement {
        Statement::PrimitiveCall {
            name,
            arguments,
            results,
        } => {
            if !imports.contains(name.as_str()) {
                return Err(module_error(
                    line,
                    &format!("primitive {name} is used without a declared import"),
                ));
            }
            let descriptor = primitives.get(name).ok_or_else(|| {
                module_error(line, &format!("primitive {name} is not registered"))
            })?;
            if arguments.len() != descriptor.arguments.len()
                || results.len() != descriptor.results.len()
            {
                return Err(module_error(
                    line,
                    &format!("primitive {name} has an argument/result count mismatch"),
                ));
            }
        }
        Statement::If(_, then_body, else_body) => {
            for nested in then_body.iter().chain(else_body) {
                validate_statement_primitive_shapes(nested, imports, primitives, line)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_primitive_references(
    program: &ParsedProgram,
    imports: &[PrimitiveImport],
) -> Result<(), RuntimeError> {
    let imports = imports
        .iter()
        .map(|import| import.name.as_str())
        .collect::<BTreeSet<_>>();
    fn visit(
        statement: &Statement,
        imports: &BTreeSet<&str>,
        line: u16,
    ) -> Result<(), RuntimeError> {
        match statement {
            Statement::PrimitiveCall { name, .. } if !imports.contains(name.as_str()) => {
                Err(module_error(
                    line,
                    &format!("primitive {name} is used without a declared import"),
                ))
            }
            Statement::If(_, then_body, else_body) => {
                for nested in then_body.iter().chain(else_body) {
                    visit(nested, imports, line)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    for instruction in &program.instructions {
        visit(&instruction.statement, &imports, instruction.line_number)?;
    }
    Ok(())
}

fn strip_parameter_type_annotations(
    line: &str,
    line_number: u16,
    typed_parameters: &mut std::collections::HashMap<String, Vec<SystemType>>,
    typed_results: &mut std::collections::HashMap<String, SystemType>,
    throws_types: &mut std::collections::HashMap<String, String>,
) -> Result<String, RuntimeError> {
    let upper = line.trim_start().to_ascii_uppercase();
    if !upper.starts_with("DEF ") {
        return Ok(line.to_owned());
    }
    let Some(open) = line.find('(') else {
        let mut cursor = 0;
        let mut suffix_start = None;
        for word in line.split_ascii_whitespace() {
            if word.eq_ignore_ascii_case("AS") || word.eq_ignore_ascii_case("THROWS") {
                suffix_start = Some(cursor);
                break;
            }
            cursor += word.len();
            while line
                .as_bytes()
                .get(cursor)
                .is_some_and(u8::is_ascii_whitespace)
            {
                cursor += 1;
            }
        }
        let Some(suffix_start) = suffix_start else {
            return Ok(line.to_owned());
        };
        let declaration = line[..suffix_start].trim_end();
        let routine_name = declaration
            .split_ascii_whitespace()
            .last()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let is_function = declaration
            .split_ascii_whitespace()
            .any(|word| word.eq_ignore_ascii_case("FN"));
        let trailer = line[suffix_start..].trim();
        parse_signature_suffix(
            trailer,
            &routine_name,
            is_function,
            line_number,
            typed_results,
            throws_types,
        )?;
        return Ok(if is_function {
            format!("{}()", declaration)
        } else {
            declaration.to_owned()
        });
    };
    let Some(close) = line.find(')') else {
        return Err(module_error(
            line_number,
            "typed procedure signature has no closing parenthesis",
        ));
    };
    if close < open {
        return Err(module_error(
            line_number,
            "malformed typed procedure signature",
        ));
    }
    let declaration = line[..open].trim();
    let routine_name = declaration
        .split_ascii_whitespace()
        .last()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let parameters = line[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|parameter| !parameter.is_empty())
        .collect::<Vec<_>>();
    let has_parameters = !parameters.is_empty();
    let mut names = Vec::new();
    let mut types = Vec::new();
    for parameter in parameters {
        let mut words = parameter.split_ascii_whitespace();
        let name = words
            .next()
            .ok_or_else(|| module_error(line_number, "empty typed parameter"))?;
        let as_keyword = words.next();
        let type_name = words.next();
        if as_keyword.is_none() && type_name.is_none() {
            names.push(name.to_ascii_uppercase());
            types.push(SystemType::Int32);
        } else if !as_keyword.is_some_and(|word| word.eq_ignore_ascii_case("AS"))
            || words.next().is_some()
        {
            return Err(module_error(
                line_number,
                "typed parameters use `name AS TYPE`",
            ));
        } else {
            names.push(name.to_ascii_uppercase());
            types.push(
                parse_system_type(type_name.unwrap_or_default())
                    .map_err(|message| module_error(line_number, &message))?,
            );
        }
    }
    let is_function = declaration
        .split_ascii_whitespace()
        .any(|word| word.eq_ignore_ascii_case("FN"));
    if has_parameters {
        typed_parameters.insert(routine_name.clone(), types);
    }
    let trailer = line[close + 1..].trim();
    let (trailer, inline_result) = if let Some((signature, result)) = trailer.split_once('=') {
        (signature.trim(), Some(result.trim()))
    } else {
        (trailer, None)
    };
    parse_signature_suffix(
        trailer,
        &routine_name,
        is_function,
        line_number,
        typed_results,
        throws_types,
    )?;
    let mut transformed = format!("{}({})", &line[..open], names.join(","));
    if let Some(result) = inline_result {
        transformed.push_str(" = ");
        transformed.push_str(result);
    }
    Ok(transformed)
}

fn parse_signature_suffix(
    trailer: &str,
    routine_name: &str,
    is_function: bool,
    line_number: u16,
    typed_results: &mut std::collections::HashMap<String, SystemType>,
    throws_types: &mut std::collections::HashMap<String, String>,
) -> Result<(), RuntimeError> {
    let mut result_type = None;
    let mut throws_type = None;
    let mut words = trailer.split_ascii_whitespace().peekable();
    while let Some(word) = words.next() {
        if word.eq_ignore_ascii_case("AS") {
            if result_type.is_some() {
                return Err(module_error(
                    line_number,
                    "function result type is declared more than once",
                ));
            }
            let type_name = words
                .next()
                .ok_or_else(|| module_error(line_number, "AS requires a result type"))?;
            result_type = Some(
                parse_system_type(type_name)
                    .map_err(|message| module_error(line_number, &message))?,
            );
        } else if word.eq_ignore_ascii_case("THROWS") {
            if throws_type.is_some() {
                return Err(module_error(
                    line_number,
                    "THROWS is declared more than once",
                ));
            }
            throws_type = Some(
                words
                    .next()
                    .ok_or_else(|| module_error(line_number, "THROWS requires an error type"))?
                    .to_ascii_uppercase(),
            );
        } else {
            return Err(module_error(
                line_number,
                "function signature suffix is AS result-type [THROWS ErrorType]",
            ));
        }
    }
    if is_function {
        typed_results.insert(
            routine_name.to_owned(),
            result_type.ok_or_else(|| {
                module_error(
                    line_number,
                    "BASIC64 functions require an explicit AS result type",
                )
            })?,
        );
    } else if result_type.is_some() {
        return Err(module_error(
            line_number,
            "only FN declarations accept a result type",
        ));
    }
    if let Some(throws_type) = throws_type {
        throws_types.insert(routine_name.to_owned(), throws_type);
    }
    Ok(())
}

pub(crate) fn parse_system_type(name: &str) -> Result<SystemType, String> {
    Ok(match name.to_ascii_uppercase().as_str() {
        "BYTE" | "UINT8" => SystemType::Byte,
        "UINT16" => SystemType::UInt16,
        "UINT32" => SystemType::UInt32,
        "INT32" => SystemType::Int32,
        "UINT64" => SystemType::UInt64,
        "INT64" => SystemType::Int64,
        "ADDRESS32" => SystemType::Address32,
        "STRING" => SystemType::String,
        name if name.starts_with("HANDLE<") && name.ends_with('>') => {
            let inner = &name[7..name.len() - 1];
            if !is_basic_identifier(inner) {
                return Err(format!("invalid opaque handle type {name}"));
            }
            SystemType::Handle(inner.to_ascii_uppercase())
        }
        name if is_basic_identifier(name) => SystemType::Record(name.to_ascii_uppercase()),
        _ => return Err(format!("unknown System Profile type {name}")),
    })
}

fn parse_contract(attributes: &str) -> Result<SwiContract, String> {
    let mut contract = SwiContract::default();
    if attributes.is_empty() {
        return Ok(contract);
    }
    for attribute in attributes.split(';') {
        let Some((key, value)) = attribute.split_once('=') else {
            return Err(format!("invalid SWI contract field {attribute:?}"));
        };
        match key.to_ascii_uppercase().as_str() {
            "REGISTERS" => {
                if value != "NONE" {
                    for register in value.split('|') {
                        let fields = register.split(':').collect::<Vec<_>>();
                        if fields.len() != 3 {
                            return Err(format!("invalid register contract {register:?}"));
                        }
                        let register_number = parse_register(fields[0])?;
                        let kind = parse_register_kind(fields[1])?;
                        let direction = parse_direction(fields[2])?;
                        contract.registers.push(RegisterContract {
                            register: register_number,
                            kind,
                            direction,
                        });
                    }
                }
            }
            "MEMORY" => {
                if value != "NONE" {
                    for memory in value.split('|') {
                        let fields = memory.split(':').collect::<Vec<_>>();
                        if fields.len() != 3 {
                            return Err(format!("invalid logical-memory contract {memory:?}"));
                        }
                        let register = parse_register(fields[0])?;
                        let length = if fields[2].eq_ignore_ascii_case("UNBOUNDED") {
                            None
                        } else {
                            Some(fields[2].parse().map_err(|_| {
                                "memory length must be an unsigned byte count".to_owned()
                            })?)
                        };
                        contract.logical_memory.push(
                            match fields[1].to_ascii_uppercase().as_str() {
                                "READ" => LogicalMemoryContract::Read {
                                    register,
                                    max_bytes: length,
                                },
                                "WRITE" => LogicalMemoryContract::Write {
                                    register,
                                    max_bytes: length,
                                },
                                "READWRITE" => LogicalMemoryContract::ReadWrite {
                                    register,
                                    max_bytes: length,
                                },
                                _ => {
                                    return Err(format!(
                                        "invalid memory access direction {}",
                                        fields[1]
                                    ));
                                }
                            },
                        );
                    }
                }
            }
            "PC" => contract.program_counter = Some(parse_direction(value)?),
            "CARRY" => contract.carry = Some(parse_direction(value)?),
            "BLOCKING" => contract.may_block = parse_bool(value)?,
            "REENTRANT" => contract.may_reenter = parse_bool(value)?,
            "ERROR" => contract.error_transport = value.into(),
            _ => return Err(format!("unknown SWI contract field {key}")),
        }
    }
    Ok(contract)
}

fn parse_register(value: &str) -> Result<u8, String> {
    if value.eq_ignore_ascii_case("PC") {
        return Ok(15);
    }
    value
        .strip_prefix('R')
        .or_else(|| value.strip_prefix('r'))
        .ok_or_else(|| format!("register {value} must be R0..R15 or PC"))?
        .parse::<u8>()
        .map_err(|_| format!("invalid register {value}"))
}

fn parse_register_kind(value: &str) -> Result<RegisterKind, String> {
    Ok(match value.to_ascii_uppercase().as_str() {
        "BYTE" | "U8" => RegisterKind::Unsigned { bits: 8 },
        "U32" | "UINT32" => RegisterKind::Unsigned { bits: 32 },
        "S32" | "INT32" => RegisterKind::Signed { bits: 32 },
        "ADDRESS32" => RegisterKind::LogicalAddress { bits: 32 },
        value if value.starts_with("HANDLE<") && value.ends_with('>') => {
            RegisterKind::OpaqueHandle {
                type_name: value[7..value.len() - 1].to_owned(),
            }
        }
        _ => return Err(format!("unknown register type {value}")),
    })
}

fn parse_direction(value: &str) -> Result<ArgumentDirection, String> {
    match value.to_ascii_uppercase().as_str() {
        "IN" => Ok(ArgumentDirection::In),
        "OUT" => Ok(ArgumentDirection::Out),
        "INOUT" => Ok(ArgumentDirection::InOut),
        _ => Err(format!("unknown direction {value}")),
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.to_ascii_uppercase().as_str() {
        "TRUE" => Ok(true),
        "FALSE" => Ok(false),
        _ => Err(format!("expected TRUE or FALSE, found {value}")),
    }
}

fn parse_number(value: &str) -> Result<u32, String> {
    if let Some(hex) = value.strip_prefix('&') {
        u32::from_str_radix(hex, 16).map_err(|_| format!("invalid hexadecimal SWI number {value}"))
    } else {
        value
            .parse()
            .map_err(|_| format!("invalid SWI number {value}"))
    }
}

fn parse_version(value: &str) -> Result<SemanticVersion, String> {
    let parts = value
        .split('.')
        .map(str::parse::<u16>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| format!("invalid semantic version {value}"))?;
    if parts.len() != 3 {
        return Err(format!(
            "semantic version {value} must have three components"
        ));
    }
    Ok(SemanticVersion::new(parts[0], parts[1], parts[2]))
}

fn source_digest(source: &[u8]) -> String {
    // FNV-1a is a deterministic cache key here, not an integrity/authenticity hash.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in source {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn validate_dependencies(
    module_name: &str,
    dependencies: &[(String, SemanticVersion)],
) -> Result<(), RuntimeError> {
    let mut seen = BTreeSet::new();
    for (dependency, _) in dependencies {
        let canonical = dependency.to_ascii_uppercase();
        if canonical == module_name.to_ascii_uppercase() {
            return Err(module_error(0, "a module cannot depend on itself"));
        }
        if !seen.insert(canonical) {
            return Err(module_error(0, "module dependencies must be unique"));
        }
        if dependency.is_empty()
            || dependency.split('.').any(|part| {
                part.is_empty()
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            })
        {
            return Err(module_error(
                0,
                &format!("invalid dependency module name {dependency}"),
            ));
        }
    }
    Ok(())
}

fn validate_symbol_calls(
    program: &ParsedProgram,
    imports: &[ModuleSymbolImport],
) -> Result<(), RuntimeError> {
    fn imported(imports: &[ModuleSymbolImport], module: &str, symbol: &str) -> bool {
        imports.iter().any(|import| {
            import.module.eq_ignore_ascii_case(module) && import.symbol.eq_ignore_ascii_case(symbol)
        })
    }
    fn expression(
        value: &super::parser::Expr,
        imports: &[ModuleSymbolImport],
        line: u16,
    ) -> Result<(), RuntimeError> {
        if let super::parser::Expr::ImportedFunction { module, name, .. } = value {
            let symbol = format!("FN:{name}");
            if !imported(imports, module, &symbol) {
                return Err(module_error(
                    line,
                    &format!(
                        "qualified FN {module}.{name} requires @IMPORT_SYMBOL {module} FN {name}"
                    ),
                ));
            }
        }
        for child in super::system_ir::expression_children(value) {
            expression(child, imports, line)?;
        }
        Ok(())
    }
    fn statement(
        value: &Statement,
        imports: &[ModuleSymbolImport],
        line: u16,
    ) -> Result<(), RuntimeError> {
        if let Statement::ImportedProcedureCall { module, name, .. } = value
            && !imported(imports, module, name)
        {
            return Err(module_error(
                line,
                &format!(
                    "qualified PROC {module}.{name} requires @IMPORT_SYMBOL {module} PROC {name}"
                ),
            ));
        }
        for operand in super::system_ir::statement_expressions(value) {
            expression(operand, imports, line)?;
        }
        if let Statement::If(_, then_body, else_body) = value {
            for nested in then_body.iter().chain(else_body) {
                statement(nested, imports, line)?;
            }
        }
        Ok(())
    }
    for instruction in &program.instructions {
        statement(&instruction.statement, imports, instruction.line_number)?;
    }
    Ok(())
}

fn module_error(line: u16, message: &str) -> RuntimeError {
    RuntimeError::Program(format!("BASIC64 System Profile line {line}: {message}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_system_module_resolves_swi_exports_and_typed_helper_parameters() {
        let allocator = IdentityAllocator::default();
        let module = SystemModule::parse(
            include_str!("../../modules/Console.bas64"),
            "modules/Console.bas64",
            &allocator,
        )
        .unwrap();
        assert_eq!(module.manifest.name, "Console");
        assert_eq!(module.manifest.version, SemanticVersion::new(1, 0, 0));
        assert_eq!(module.manifest.schema_version, 1);
        assert_eq!(module.manifest.lifecycle.start.as_deref(), Some("START"));
        assert_eq!(
            module.manifest.lifecycle.quiesce.as_deref(),
            Some("QUIESCE")
        );
        assert_eq!(
            module.manifest.lifecycle.finalise.as_deref(),
            Some("FINALISE")
        );
        assert_eq!(
            module.manifest.replacement_policy,
            ReplacementPolicy::CompatibleImmediate
        );
        assert_eq!(module.manifest.exports.len(), 6);
        assert_eq!(
            module
                .manifest
                .exports
                .iter()
                .map(|export| export.number)
                .collect::<Vec<_>>(),
            [14, 0, 1, 2, 3, 4]
        );
        assert_eq!(
            module.program.typed_parameters.get("EMITBYTE"),
            Some(&vec![SystemType::Byte])
        );
        assert!(module.program.procedures.contains_key("EMITBYTE"));
        assert!(module.program.procedures.contains_key("WRITES"));
        let write_s = module
            .manifest
            .exports
            .iter()
            .find(|export| export.name == "OS_WriteS")
            .unwrap();
        assert_eq!(
            write_s.contract.program_counter,
            Some(ArgumentDirection::InOut)
        );
        let write_0 = module
            .manifest
            .exports
            .iter()
            .find(|export| export.name == "OS_Write0")
            .unwrap();
        assert_eq!(
            write_0.contract.logical_memory,
            [LogicalMemoryContract::Read {
                register: 0,
                max_bytes: Some(4096)
            }]
        );
        assert!(matches!(
            write_0.contract.registers[0].kind,
            RegisterKind::LogicalAddress { bits: 32 }
        ));
        let reflection = module.reflection();
        assert_eq!(reflection.manifest.exports.len(), 6);
        assert!(reflection.definitions.iter().any(|definition| {
            definition.descriptor.name == "WRITE0"
                && definition.public
                && definition.source_line > 0
        }));
        assert!(reflection.definitions.iter().any(|definition| {
            definition.descriptor.name == "WRITE0FROM"
                && !definition.public
                && definition.parameter_types == [Some(SystemType::Address32)]
        }));
        let encoded = module.manifest.encode_v1().unwrap();
        assert_eq!(
            ModuleManifest::decode_v1(&encoded).unwrap(),
            module.manifest
        );
    }

    #[test]
    fn console_typed_ir_preserves_identity_checked_addresses_and_backend_boundary() {
        let allocator = IdentityAllocator::default();
        let module = SystemModule::parse(
            include_str!("../../modules/Console.bas64"),
            "modules/Console.bas64",
            &allocator,
        )
        .unwrap();
        let ir = module.typed_ir();
        assert_eq!(ir.module_name, "Console");
        assert_eq!(ir.module_version, SemanticVersion::new(1, 0, 0));
        assert_eq!(ir.source_path, "modules/Console.bas64");
        assert!(!ir.source_hash.is_empty());
        assert!(ir.declared_dependencies.is_empty());
        assert_eq!(ir.manifest.as_ref().unwrap().exports.len(), 6);
        assert_eq!(
            ir.workspace.get("STARTCOUNT%"),
            Some(&SystemIrValueType::System(SystemType::UInt32))
        );
        let write_s_from = ir.definitions.get("WRITESFROM").unwrap();
        assert_eq!(
            write_s_from.parameters[0].value_type,
            SystemIrValueType::System(SystemType::Address32)
        );
        assert!(write_s_from.source.line > 0);
        assert_eq!(write_s_from.visibility, SystemIrVisibility::Private);
        assert_eq!(
            ir.definitions.get("WRITEC").unwrap().visibility,
            SystemIrVisibility::Exported
        );

        let checked_offset = ir.instructions.iter().find_map(|instruction| {
            let SystemIrOpcode::Assign { target, value } = &instruction.operation else {
                return None;
            };
            let SystemIrPlaceKind::Binding { name, .. } = &target.kind else {
                return None;
            };
            (name.eq_ignore_ascii_case("address")
                && matches!(
                    &value.kind,
                    SystemIrExpressionKind::CheckedAddressOffset {
                        operator: SystemIrBinaryOperator::Add,
                        width_bits: 32,
                        owner: AddressOwner::InvokingTask,
                    }
                ))
            .then_some(instruction)
        });
        let checked_offset = checked_offset.unwrap();

        let checked_read = ir.instructions.iter().find_map(|instruction| {
            let SystemIrOpcode::Assign { value, .. } = &instruction.operation else {
                return None;
            };
            matches!(
                &value.kind,
                SystemIrExpressionKind::CheckedLogicalMemoryRead {
                    width: SystemIrMemoryWidth::Byte,
                    owner: AddressOwner::InvokingTask,
                }
            )
            .then_some((instruction, value))
        });
        let (read_instruction, checked_read) = checked_read.unwrap();
        assert_eq!(read_instruction.source.path, "modules/Console.bas64");
        assert!(read_instruction.source.line > 0);
        assert_eq!(
            checked_read.children[0].value_type,
            SystemIrValueType::System(SystemType::Address32)
        );
        assert!(matches!(
            &checked_read.children[0].kind,
            SystemIrExpressionKind::LoadBinding {
                storage: SystemIrStorage::Parameter,
                ..
            }
        ));

        let plan = module
            .prepare_execution(SystemIrBackend::Interpreter)
            .unwrap();
        assert_eq!(plan.source_hash, ir.source_hash);
        assert_eq!(plan.instruction_count, ir.instructions.len());
        assert!(
            module
                .prepare_execution(SystemIrBackend::HybridJit)
                .unwrap_err()
                .source
                .line
                > 0
        );
        for backend in [SystemIrBackend::StrictJit, SystemIrBackend::Aot] {
            assert!(module.prepare_execution(backend).is_err());
        }
        let reference = ir.lower_for_reference_interpreter().unwrap();
        assert_eq!(
            reference.instructions.len(),
            module.program.instructions.len()
        );
        assert_eq!(
            reference
                .instructions
                .iter()
                .map(|instruction| (instruction.line_number, instruction.statement.clone()))
                .collect::<Vec<_>>(),
            module
                .program
                .instructions
                .iter()
                .map(|instruction| (instruction.line_number, instruction.statement.clone()))
                .collect::<Vec<_>>(),
            "the portable IR must contain enough data to reconstruct executable statements",
        );
        assert!(
            reference
                .instructions
                .iter()
                .any(|instruction| instruction.line_number == checked_offset.source.line)
        );
    }

    #[test]
    fn typed_ir_keeps_enum_members_and_flags_operations_nominal() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE IrTypes 1.0.0\nREM @SWI IrTypes_Test &101 Entry\nFLAGS Access AS UINT32\n    Read = 1\n    Write = 2\nEND FLAGS\nENUM Result AS UINT32\n    Ok = 0\nEND ENUM\nREM @STATE FLAGSRESULT Access\nREM @STATE ENUMRESULT Result\nDEF PROC Entry\n    FLAGSRESULT = Access.Read OR Access.Write\n    ENUMRESULT = Result.Ok\nENDPROC\n";
        let module = SystemModule::parse(
            source,
            "modules/IrTypes.bas64",
            &IdentityAllocator::default(),
        )
        .unwrap();
        let flags = module
            .typed_ir()
            .instructions
            .iter()
            .find_map(|instruction| {
                let SystemIrOpcode::Assign { target, value } = &instruction.operation else {
                    return None;
                };
                let SystemIrPlaceKind::Binding { name, .. } = &target.kind else {
                    return None;
                };
                name.eq_ignore_ascii_case("FLAGSRESULT").then_some(value)
            })
            .unwrap();
        assert_eq!(
            flags.value_type,
            SystemIrValueType::System(SystemType::Flags("ACCESS".into()))
        );
        assert!(matches!(
            &flags.kind,
            SystemIrExpressionKind::FlagsCombine {
                operator: SystemIrBinaryOperator::Or,
                left_type,
                right_type,
            } if left_type == "ACCESS" && right_type == "ACCESS"
        ));
        assert!(matches!(
            &flags.children[0].kind,
            SystemIrExpressionKind::FlagsConstant { type_name, member }
                if type_name == "ACCESS" && member == "READ"
        ));
        let enum_value = module
            .typed_ir()
            .instructions
            .iter()
            .find_map(|instruction| {
                let SystemIrOpcode::Assign { target, value } = &instruction.operation else {
                    return None;
                };
                let SystemIrPlaceKind::Binding { name, .. } = &target.kind else {
                    return None;
                };
                name.eq_ignore_ascii_case("ENUMRESULT").then_some(value)
            })
            .unwrap();
        assert_eq!(
            enum_value.value_type,
            SystemIrValueType::System(SystemType::Enum("RESULT".into()))
        );
        assert!(matches!(
            &enum_value.kind,
            SystemIrExpressionKind::EnumConstant { type_name, member }
                if type_name == "RESULT" && member == "OK"
        ));
    }

    #[test]
    fn separately_parsed_modules_can_keep_same_named_private_procedures() {
        let allocator = IdentityAllocator::default();
        let source = |module: &str, swi: &str, number: u32| {
            format!(
                "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE {module} 1.0.0\nREM @SWI {swi} &{number:X} Entry\nDEF PROC Entry\n    PROC Helper\nENDPROC\nDEF PROC Helper\nENDPROC\n"
            )
        };
        let first = SystemModule::parse(
            &source("First", "First_Service", 0x100),
            "modules/First.bas64",
            &allocator,
        )
        .unwrap();
        let second = SystemModule::parse(
            &source("Second", "Second_Service", 0x101),
            "modules/Second.bas64",
            &allocator,
        )
        .unwrap();

        assert!(first.program.procedures.contains_key("HELPER"));
        assert!(second.program.procedures.contains_key("HELPER"));
        assert_ne!(
            first.definitions["HELPER"].id,
            second.definitions["HELPER"].id
        );
        assert_eq!(first.manifest.exports[0].definition_name, "ENTRY");
        assert_eq!(second.manifest.exports[0].definition_name, "ENTRY");
    }

    #[test]
    fn a_system_module_cannot_use_an_undeclared_primitive() {
        let allocator = IdentityAllocator::default();
        let source = include_str!("../../modules/Console.bas64")
            .replace("REM @IMPORT Host.Console.ReadByteStatus ConsoleInput\n", "");
        let error =
            SystemModule::parse(&source, "modules/bad-console.bas64", &allocator).unwrap_err();
        assert!(error.to_string().contains("without a declared import"));
    }

    #[test]
    fn classic_source_keeps_primitive_as_an_ordinary_identifier() {
        let parsed = parser::parse_source("PRIMITIVE%=7\nEND\n").unwrap();
        assert!(parsed.procedures.is_empty());
        assert!(
            parsed
                .instructions
                .iter()
                .any(|instruction| matches!(instruction.statement, Statement::Assign(_, _)))
        );
    }

    #[test]
    fn classic_and_hybrid_keep_system_profile_keywords_as_identifiers() {
        for mode in ["CLASSIC", "HYBRID"] {
            let source = format!("REM @BASIC64 MODE={mode}\nTRY%=1\nCATCH%=2\nEND\n");
            let parsed = parser::parse_source(&source).unwrap();
            assert_eq!(
                parsed.options.mode,
                if mode == "CLASSIC" {
                    BasicLanguageMode::Classic
                } else {
                    BasicLanguageMode::Hybrid
                }
            );
            assert!(
                parsed
                    .instructions
                    .iter()
                    .any(|instruction| matches!(instruction.statement, Statement::Assign(_, _)))
            );
            assert!(!parsed.instructions.iter().any(|instruction| matches!(
                instruction.statement,
                Statement::Try
                    | Statement::Catch { .. }
                    | Statement::EndTry
                    | Statement::Throw { .. }
            )));
        }
    }

    #[test]
    fn dependency_directive_spellings_have_the_same_manifest_semantics() {
        let allocator = IdentityAllocator::default();
        let template = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Consumer 1.0.0\nREM @SWI Consumer_Test &101 Entry\nDEF PROC Entry\nENDPROC\n";
        let import_module = SystemModule::parse(
            &template.replace("REM @SWI", "REM @IMPORT_MODULE Runtime 1.2.3\nREM @SWI"),
            "modules/Consumer.bas64",
            &allocator,
        )
        .unwrap();
        let depends = SystemModule::parse(
            &template.replace("REM @SWI", "REM @DEPENDS Runtime 1.2.3\nREM @SWI"),
            "modules/Consumer.bas64",
            &allocator,
        )
        .unwrap();

        assert_eq!(
            import_module.manifest.dependencies,
            depends.manifest.dependencies
        );
        assert_eq!(
            import_module.manifest.dependencies,
            [("Runtime".to_owned(), SemanticVersion::new(1, 2, 3))]
        );

        let duplicate = template.replace(
            "REM @SWI",
            "REM @IMPORT_MODULE Runtime 1.2.3\nREM @DEPENDS Runtime 1.2.3\nREM @SWI",
        );
        assert!(SystemModule::parse(&duplicate, "modules/Consumer.bas64", &allocator).is_err());
    }

    #[test]
    fn module_symbol_visibility_and_imports_are_resolved_during_linking() {
        let allocator = IdentityAllocator::default();
        let provider_source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE FileSwitch 1.0.0\nREM @SWI FileSwitch_Test &100 Entry\nREM @EXPORT PROC Open\nREM @PRIVATE PROC Hidden\nDEF PROC Entry\nENDPROC\nDEF PROC Open\nENDPROC\nDEF PROC Hidden\nENDPROC\n";
        let consumer_source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Console 1.0.0\nREM @IMPORT_MODULE FileSwitch 1.0.0\nREM @IMPORT_SYMBOL FileSwitch PROC Open\nREM @SWI Console_Test &101 Entry\nDEF PROC Entry\nENDPROC\n";
        let provider =
            SystemModule::parse(provider_source, "modules/FileSwitch.bas64", &allocator).unwrap();
        let consumer =
            SystemModule::parse(consumer_source, "modules/Console.bas64", &allocator).unwrap();
        assert!(
            provider
                .manifest
                .symbol_exports
                .contains(&"OPEN".to_owned())
        );
        assert!(
            !provider
                .manifest
                .symbol_exports
                .contains(&"HIDDEN".to_owned())
        );
        assert_eq!(
            consumer.manifest.symbol_imports,
            [ModuleSymbolImport {
                module: "FileSwitch".into(),
                symbol: "OPEN".into()
            }]
        );

        let mut registry = crate::ricochet::ModuleRegistry::new();
        let provider_id = registry
            .stage_module(provider.manifest.clone(), provider.definitions.clone())
            .unwrap();
        let consumer_id = registry
            .stage_module(consumer.manifest.clone(), consumer.definitions.clone())
            .unwrap();
        registry.link_module(provider_id, BTreeSet::new()).unwrap();
        registry.link_module(consumer_id, BTreeSet::new()).unwrap();

        let hidden_import = consumer_source
            .replace("@MODULE Console", "@MODULE HiddenConsumer")
            .replace("PROC Open", "PROC Hidden");
        let hidden =
            SystemModule::parse(&hidden_import, "modules/HiddenConsumer.bas64", &allocator)
                .unwrap();
        let hidden_id = registry
            .stage_module(hidden.manifest.clone(), hidden.definitions.clone())
            .unwrap();
        assert!(matches!(
            registry.link_module(hidden_id, BTreeSet::new()),
            Err(crate::ricochet::RegistryError::MissingSymbol { symbol, .. }) if symbol == "HIDDEN"
        ));
    }

    #[test]
    fn handle_signatures_require_a_declared_opaque_handle_type() {
        let allocator = IdentityAllocator::default();
        let source =
            include_str!("../../modules/Console.bas64").replace("AS BYTE", "AS HANDLE<Task>");
        let error =
            SystemModule::parse(&source, "modules/unsupported-type.bas64", &allocator).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown opaque handle type TASK")
        );
    }

    #[test]
    fn throwing_routines_require_a_matching_declared_error_contract() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Errors 1.0.0\nREM @SWI Errors_Test &100 Entry\nERROR FileError\n    Code AS UINT32 READONLY\n    Message AS STRING READONLY\nEND ERROR\nDEF PROC Entry\nENDPROC\nDEF PROC Fail\n    THROW FileError, 1, \"failure\"\nENDPROC\n";
        let error = SystemModule::parse(
            source,
            "modules/Errors.bad-throws.bas64",
            &IdentityAllocator::default(),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("FAIL must declare THROWS FILEERROR")
        );
    }

    #[test]
    fn typed_function_parameters_are_checked_by_the_interpreter() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE TypedFunctions 1.0.0\nREM @SWI Typed_Test &100 Entry REGISTERS=R0:U32:INOUT\nDEF PROC Entry\n    R0% = FN Check(300)\nENDPROC\nDEF FN Check(byteValue% AS BYTE) AS BYTE\n=byteValue%\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module = SystemModule::parse(
            source,
            "modules/TypedFunctions.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let module_id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(module_id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module_id]).unwrap();
        registry.start_module(module_id, true).unwrap();

        let export = &module.manifest.exports[0];
        let definition = &module.definitions[&export.definition_name];
        let (input_sender, input_receiver) = std::sync::mpsc::channel();
        drop(input_sender);
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let mut task = Task::new(8);
        let mut context = SwiContext::default();
        let error = module
            .invoke(
                module_id,
                definition,
                &export.contract,
                &mut task,
                &mut dispatcher,
                &mut context,
            )
            .unwrap_err();
        assert!(error.to_string().contains("System Profile type Byte"));
    }

    #[test]
    fn records_flags_typed_results_readonly_and_structured_errors_execute() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE FileSwitch 1.0.0\nREM @SWI FileSwitch_Test &100 Entry\nERROR FileError\n    Code AS UINT32 READONLY\n    Message AS STRING READONLY\nEND ERROR\nRECORD FileInfo\n    LoadAddress AS UINT64 READONLY\n    Access AS FileAccess\nEND RECORD\nENUM FileReason AS UINT32\n    Load = 0\n    NotFound = 73\nEND ENUM\nFLAGS FileAccess AS UINT32\n    Read = 1\n    Write = 2\nEND FLAGS\nHANDLE FileHandle\nREM @STATE INFO FileInfo\nREM @STATE ACCESS FileAccess\nREM @STATE REASON FileReason\nREM @STATE RESULTACCESS FileAccess\nREM @STATE RESULT% UINT64\nREM @STATE ROERROR% UINT32\nREM @STATE THROWERR% UINT32\nREM @STATE FNERROR% UINT32\nREM @STATE BOOT% UINT32 READONLY\nDEF PROC Entry\n    BOOT% = 1\n    INFO = FN FileInfo()\n    TRY\n        INFO.LoadAddress = 99\n    CATCH fault AS FileError\n        ROERROR% = fault.Code\n    ENDTRY\n    INFO.Access = FileAccess.Read OR FileAccess.Write\n    RESULTACCESS = FN CopyAccess(INFO.Access)\n    RESULT% = FN ReadLoadAddress(INFO)\n    REASON = FileReason.NotFound\n    TRY\n        PROC Fail\n    CATCH fault AS FileError\n        THROWERR% = fault.Code\n    ENDTRY\n    TRY\n        FNERROR% = FN Failing()\n    CATCH fault AS FileError\n        FNERROR% = fault.Code\n    ENDTRY\nENDPROC\nDEF FN CopyAccess(access AS FileAccess) AS FileAccess\n=access\nDEF FN ReadLoadAddress(info AS FileInfo) AS UINT64\n=info.LoadAddress\nDEF FN Failing() AS UINT32 THROWS FileError\n=1 DIV 0\nDEF PROC Fail THROWS FileError\n    THROW FileError, 73, \"file not found\"\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module =
            SystemModule::parse(source, "modules/FileSwitch.bas64", &registry.allocator()).unwrap();
        assert!(matches!(
            module.program.system_types.get("FILEINFO"),
            Some(SystemTypeDefinition::Record { .. })
        ));
        assert!(matches!(
            module.program.system_types.get("FILEACCESS"),
            Some(SystemTypeDefinition::Flags { .. })
        ));
        assert!(matches!(
            module.program.system_types.get("FILEREASON"),
            Some(SystemTypeDefinition::Enum { .. })
        ));
        assert!(matches!(
            module.program.system_types.get("FILEHANDLE"),
            Some(SystemTypeDefinition::Handle)
        ));
        assert_eq!(
            module.program.typed_results.get("READLOADADDRESS"),
            Some(&SystemType::UInt64)
        );
        assert_eq!(module.program.throws_types.get("READLOADADDRESS"), None);
        assert_eq!(
            module.program.throws_types.get("FAILING"),
            Some(&"FILEERROR".to_owned())
        );

        let id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[id]).unwrap();
        registry.start_module(id, true).unwrap();
        let export = &module.manifest.exports[0];
        let definition = &module.definitions[&export.definition_name];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let mut task = Task::new(9);
        module
            .invoke(
                id,
                definition,
                &export.contract,
                &mut task,
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap();
        assert_eq!(module.workspace_number("ROERROR%"), Some(1.0));
        assert_eq!(module.workspace_number("THROWERR%"), Some(73.0));
        assert_eq!(module.workspace_number("FNERROR%"), Some(1.0));
        assert_eq!(module.workspace_number("RESULT%"), Some(0.0));
        assert_eq!(module.workspace_number("RESULTACCESS"), Some(3.0));
        assert_eq!(module.workspace_number("REASON"), Some(73.0));

        let error = module
            .invoke(
                id,
                definition,
                &export.contract,
                &mut task,
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("read-only module binding"));
        assert_eq!(module.workspace_number("BOOT%"), Some(1.0));
    }

    #[test]
    fn uint64_and_int64_literals_arithmetic_and_comparisons_remain_exact() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE ExactIntegers 1.0.0\nREM @SWI Exact_Test &100 Entry\nREM @STATE U UINT64\nREM @STATE I INT64\nREM @STATE CHECK% UINT32\nREM @STATE LOOPS% UINT32\nDEF PROC Entry\n    U = 18446744073709551615\n    I = -9223372036854775808\n    IF U = 18446744073709551615 THEN CHECK% = CHECK% + 1\n    IF I = -9223372036854775808 THEN CHECK% = CHECK% + 2\n    IF U THEN CHECK% = CHECK% + 4\n    IF ABS(U) = 18446744073709551615 THEN CHECK% = CHECK% + 8\n    IF INT(U) = 18446744073709551615 THEN CHECK% = CHECK% + 16\n    IF STR$(U) = \"18446744073709551615\" THEN CHECK% = CHECK% + 32\n    U = U - 1\n    IF U = 18446744073709551614 THEN CHECK% = CHECK% + 64\n    I = I + 1\n    IF I = -9223372036854775807 THEN CHECK% = CHECK% + 128\n    FOR U = 9007199254740992 TO 9007199254740994\n        LOOPS% = LOOPS% + 1\n    NEXT U\n    IF LOOPS% = 3 THEN CHECK% = CHECK% + 256\n    FOR I = -9223372036854775807 TO -9223372036854775805\n        LOOPS% = LOOPS% + 1\n    NEXT I\n    IF LOOPS% = 6 THEN CHECK% = CHECK% + 512\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module =
            SystemModule::parse(source, "modules/ExactIntegers.bas64", &registry.allocator())
                .unwrap();
        let module_id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(module_id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module_id]).unwrap();
        registry.start_module(module_id, true).unwrap();
        let export = &module.manifest.exports[0];
        let definition = &module.definitions[&export.definition_name];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let mut task = Task::new(32);
        module
            .invoke(
                module_id,
                definition,
                &export.contract,
                &mut task,
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap();
        assert_eq!(module.workspace.read_number("CHECK%"), Some(1023.0));
        assert_eq!(module.workspace.read_number("LOOPS%"), Some(6.0));
    }

    #[test]
    fn uint64_arithmetic_overflow_is_checked_instead_of_rounding() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE IntegerOverflow 1.0.0\nREM @SWI Integer_Test &100 Entry\nREM @STATE U UINT64\nDEF PROC Entry\n    U = 18446744073709551615\n    U = U + 1\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module = SystemModule::parse(
            source,
            "modules/IntegerOverflow.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let module_id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(module_id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module_id]).unwrap();
        registry.start_module(module_id, true).unwrap();
        let export = &module.manifest.exports[0];
        let definition = &module.definitions[&export.definition_name];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let mut task = Task::new(33);
        let error = module
            .invoke(
                module_id,
                definition,
                &export.contract,
                &mut task,
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("UINT64 arithmetic overflowed"));
    }

    #[test]
    fn int64_arithmetic_underflow_is_checked_instead_of_wrapping() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE SignedIntegerUnderflow 1.0.0\nREM @SWI SignedInteger_Test &100 Entry\nREM @STATE I INT64\nDEF PROC Entry\n    I = -9223372036854775808\n    I = I - 1\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module = SystemModule::parse(
            source,
            "modules/SignedIntegerUnderflow.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let module_id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(module_id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module_id]).unwrap();
        registry.start_module(module_id, true).unwrap();
        let export = &module.manifest.exports[0];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let error = module
            .invoke(
                module_id,
                &module.definitions[&export.definition_name],
                &export.contract,
                &mut Task::new(39),
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("INT64 arithmetic overflowed"));
    }

    #[test]
    fn local_readonly_binding_is_typed_scoped_and_immutable() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE ReadonlyLocal 1.0.0\nREM @SWI Readonly_Test &100 Entry\nREM @STATE RESULT% UINT32\nDEF PROC Entry\n    PROC InitLimit\n    limit = 0\n    RESULT% = 1\nENDPROC\nDEF PROC InitLimit\n    LET READONLY limit AS UINT64 = 18446744073709551615\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module =
            SystemModule::parse(source, "modules/ReadonlyLocal.bas64", &registry.allocator())
                .unwrap();
        let module_id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(module_id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module_id]).unwrap();
        registry.start_module(module_id, true).unwrap();
        let export = &module.manifest.exports[0];
        let definition = &module.definitions[&export.definition_name];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let mut task = Task::new(34);
        module
            .invoke(
                module_id,
                definition,
                &export.contract,
                &mut task,
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap();
        assert_eq!(module.workspace.read_number("RESULT%"), Some(1.0));

        let illegal = source.replace(
            "    LET READONLY limit AS UINT64 = 18446744073709551615\nENDPROC",
            "    LET READONLY limit AS UINT64 = 18446744073709551615\n    limit = 0\nENDPROC",
        );
        let invalid = SystemModule::parse(
            &illegal,
            "modules/ReadonlyLocal.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let invalid_export = &invalid.manifest.exports[0];
        let invalid_definition = &invalid.definitions[&invalid_export.definition_name];
        let error = invalid
            .invoke(
                module_id,
                invalid_definition,
                &invalid_export.contract,
                &mut task,
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("read-only local binding"));

        let slice_illegal = source.replace(
            "    LET READONLY limit AS UINT64 = 18446744073709551615\nENDPROC",
            "    LET READONLY limit AS UINT64 = 18446744073709551615\n    LET READONLY phrase$ AS STRING = \"safe\"\n    MID$(phrase$, 1, 1) = \"N\"\nENDPROC",
        );
        let invalid_slice = SystemModule::parse(
            &slice_illegal,
            "modules/ReadonlyLocal.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let invalid_slice_export = &invalid_slice.manifest.exports[0];
        let error = invalid_slice
            .invoke(
                module_id,
                &invalid_slice.definitions[&invalid_slice_export.definition_name],
                &invalid_slice_export.contract,
                &mut Task::new(37),
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("read-only local binding"));
    }

    #[test]
    fn readonly_local_record_cannot_be_mutated_through_a_field_place() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE ReadonlyRecord 1.0.0\nREM @STATE SOURCE FileInfo\nREM @SWI Readonly_RecordTest &100 Entry\nRECORD FileInfo\n    Length AS UINT32\nEND RECORD\nDEF PROC Entry\n    LET READONLY localInfo AS FileInfo = SOURCE\n    localInfo.Length = 42\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module = SystemModule::parse(
            source,
            "modules/ReadonlyRecord.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let module_id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(module_id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module_id]).unwrap();
        registry.start_module(module_id, true).unwrap();
        let export = &module.manifest.exports[0];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let error = module
            .invoke(
                module_id,
                &module.definitions[&export.definition_name],
                &export.contract,
                &mut Task::new(38),
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("read-only local binding"));
    }

    #[test]
    fn local_readonly_binding_is_available_in_function_bodies() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE ReadonlyFunction 1.0.0\nREM @SWI Readonly_FunctionTest &100 Entry\nREM @STATE RESULT UINT64\nDEF PROC Entry\n    RESULT = FN Twice(21) + FN Twice(20)\nENDPROC\nDEF FN Twice(amount AS UINT64) AS UINT64\n    LET READONLY incremented AS UINT64 = amount + 1\n=incremented + amount\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module = SystemModule::parse(
            source,
            "modules/ReadonlyFunction.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let module_id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(module_id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module_id]).unwrap();
        registry.start_module(module_id, true).unwrap();
        let export = &module.manifest.exports[0];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        module
            .invoke(
                module_id,
                &module.definitions[&export.definition_name],
                &export.contract,
                &mut Task::new(35),
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap();
        assert_eq!(module.workspace.read_number("RESULT"), Some(84.0));

        let illegal = source.replace(
            "    LET READONLY incremented AS UINT64 = amount + 1\n=incremented + amount\n",
            "    LET READONLY incremented AS UINT64 = amount + 1\n    incremented = 0\n=incremented + amount\n",
        );
        let invalid = SystemModule::parse(
            &illegal,
            "modules/ReadonlyFunction.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let invalid_export = &invalid.manifest.exports[0];
        let error = invalid
            .invoke(
                module_id,
                &invalid.definitions[&invalid_export.definition_name],
                &invalid_export.contract,
                &mut Task::new(36),
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("read-only local binding"));
    }

    #[test]
    fn caught_errors_restore_nested_procedure_bindings_before_resuming() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE FileSwitch 1.0.0\nREM @SWI FileSwitch_UnwindTest &100 Entry\nERROR FileError\n    Code AS UINT32 READONLY\n    Message AS STRING READONLY\nEND ERROR\nREM @STATE BASE% UINT32\nREM @STATE AFTER% UINT32\nDEF PROC Entry\n    BASE% = 7\n    TRY\n        PROC Fail(BASE%)\n    CATCH failure AS FileError\n        AFTER% = BASE%\n    ENDTRY\nENDPROC\nDEF PROC Fail(base% AS UINT32) THROWS FileError\n    base% = 99\n    THROW FileError, 73, \"not found\"\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module = SystemModule::parse(
            source,
            "modules/FileSwitch.unwind.bas64",
            &registry.allocator(),
        )
        .unwrap();
        let id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[id]).unwrap();
        registry.start_module(id, true).unwrap();
        let export = &module.manifest.exports[0];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let mut task = Task::new(10);
        module
            .invoke(
                id,
                &module.definitions[&export.definition_name],
                &export.contract,
                &mut task,
                &mut dispatcher,
                &mut SwiContext::default(),
            )
            .unwrap();
        assert_eq!(module.workspace_number("BASE%"), Some(7.0));
        assert_eq!(module.workspace_number("AFTER%"), Some(7.0));
    }

    #[test]
    fn opaque_handles_round_trip_by_type_and_cannot_be_used_as_numbers() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Wimp 1.0.0\nREM @SWI Wimp_HandleTest &100 Entry REGISTERS=R0:HANDLE<WindowHandle>:INOUT|R1:U32:OUT\nHANDLE WindowHandle\nERROR TypeError\n    Code AS UINT32 READONLY\n    Message AS STRING READONLY\nEND ERROR\nREM @STATE CAUGHT% UINT32\nDEF PROC Entry\n    TRY\n        R1% = R0%\n    CATCH failure AS TypeError\n        CAUGHT% = failure.Code\n    ENDTRY\n    R0% = FN Echo(R0%)\nENDPROC\nDEF FN Echo(window AS HANDLE<WindowHandle>) AS HANDLE<WindowHandle>\n=window\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module =
            SystemModule::parse(source, "modules/Wimp.handles.bas64", &registry.allocator())
                .unwrap();
        let id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[id]).unwrap();
        registry.start_module(id, true).unwrap();
        let export = &module.manifest.exports[0];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let mut task = Task::new(11);
        let mut context = SwiContext::default();
        context.registers[0] = 0x1234_5678;
        module
            .invoke(
                id,
                &module.definitions[&export.definition_name],
                &export.contract,
                &mut task,
                &mut dispatcher,
                &mut context,
            )
            .unwrap();
        assert_eq!(context.registers[0], 0x1234_5678);
        assert_eq!(context.registers[1], 0);
        assert_eq!(module.workspace_number("CAUGHT%"), Some(1.0));
    }

    #[test]
    fn lifecycle_hooks_share_private_workspace_across_interpreted_calls() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Stateful 1.0.0\nREM @STATE STARTS% UINT32\nREM @STATE INVOKES% UINT32\nREM @STATE QUIESCES% UINT32\nREM @STATE FINALS% UINT32\nREM @LIFECYCLE START Start\nREM @LIFECYCLE QUIESCE Quiesce\nREM @LIFECYCLE FINALISE Finalise\nREM @SWI Stateful_Test &100 Entry\nDEF PROC Start\n    STARTS% = STARTS% + 1\nENDPROC\nDEF PROC Quiesce\n    QUIESCES% = QUIESCES% + 1\nENDPROC\nDEF PROC Finalise\n    FINALS% = FINALS% + 1\nENDPROC\nDEF PROC Entry\n    INVOKES% = INVOKES% + 1\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module =
            SystemModule::parse(source, "modules/Stateful.bas64", &registry.allocator()).unwrap();
        let id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[id]).unwrap();
        registry.begin_module_start(id).unwrap();
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let mut task = Task::new(99);
        module
            .invoke_lifecycle("START", id, &mut task, &mut dispatcher)
            .unwrap();
        registry.complete_module_start(id).unwrap();
        let (owner, _lease) = registry.acquire_swi(0x100).unwrap();
        let mut context = SwiContext::default();
        module
            .invoke(
                id,
                &module.definitions[&owner.definition_name],
                &owner.contract,
                &mut task,
                &mut dispatcher,
                &mut context,
            )
            .unwrap();
        module
            .invoke(
                id,
                &module.definitions[&owner.definition_name],
                &owner.contract,
                &mut task,
                &mut dispatcher,
                &mut context,
            )
            .unwrap();
        drop(_lease);
        registry.quiesce_module(id).unwrap();
        module
            .invoke_lifecycle("QUIESCE", id, &mut task, &mut dispatcher)
            .unwrap();
        module
            .invoke_lifecycle("FINALISE", id, &mut task, &mut dispatcher)
            .unwrap();
        assert_eq!(module.workspace_number("STARTS%"), Some(1.0));
        assert_eq!(module.workspace_number("INVOKES%"), Some(2.0));
        assert_eq!(module.workspace_number("QUIESCES%"), Some(1.0));
        assert_eq!(module.workspace_number("FINALS%"), Some(1.0));
        registry.retire_module(id).unwrap();
    }

    #[test]
    fn failing_basic64_start_rolls_back_all_published_exports_and_workspace_updates() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE StartFailure 1.0.0\nREM @STATE STARTS% UINT32\nREM @LIFECYCLE START Start\nREM @SWI StartFailure_First &100 First\nREM @SWI StartFailure_Second &101 Second\nERROR StartError\n    Code AS UINT32 READONLY\n    Message AS STRING READONLY\nEND ERROR\nDEF PROC Start THROWS StartError\n    STARTS% = STARTS% + 1\n    THROW StartError, 99, \"startup failed\"\nENDPROC\nDEF PROC First\nENDPROC\nDEF PROC Second\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module =
            SystemModule::parse(source, "modules/StartFailure.bas64", &registry.allocator())
                .unwrap();
        let id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[id]).unwrap();
        registry.begin_module_start(id).unwrap();
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));
        let error = module
            .invoke_lifecycle("START", id, &mut Task::new(100), &mut dispatcher)
            .unwrap_err();
        assert!(error.to_string().contains("startup failed"));
        registry.fail_module_start(id, error.to_string()).unwrap();
        assert_eq!(
            registry.module_state(id),
            Some(crate::ricochet::ModuleState::Linked)
        );
        assert_eq!(registry.registered_swi_count(), 0);
        assert!(registry.acquire_swi(0x100).is_none());
        assert!(registry.acquire_swi(0x101).is_none());
        assert_eq!(module.workspace_number("STARTS%"), None);
    }

    #[test]
    fn logical_addresses_remain_typed_and_are_resolved_in_the_invoking_task() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE AddressTest 1.0.0\nREM @SWI Address_Test &100 Entry REGISTERS=R0:ADDRESS32:INOUT|R1:U32:OUT\nERROR AddressError\n    Code AS UINT32 READONLY\n    Message AS STRING READONLY\nEND ERROR\nREM @STATE ERRORCODE% UINT32\nDEF PROC Entry\n    PROC ReadByte(R0%)\n    TRY\n        R1% = R0% * 2\n    CATCH failure AS AddressError\n        ERRORCODE% = failure.Code\n    ENDTRY\nENDPROC\nDEF PROC ReadByte(pointer AS ADDRESS32)\n    R1% = ?pointer\n    pointer = pointer + 1\n    R0% = pointer\nENDPROC\n";
        let mut registry = crate::ricochet::ModuleRegistry::new();
        let module =
            SystemModule::parse(source, "modules/AddressTest.bas64", &registry.allocator())
                .unwrap();
        let id = registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        registry.link_module(id, BTreeSet::new()).unwrap();
        registry.publish_modules(&[id]).unwrap();
        registry.start_module(id, true).unwrap();
        let export = &module.manifest.exports[0];
        let definition = &module.definitions[&export.definition_name];
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(crate::host::HostConsole::windowed(input_receiver));

        let mut first_task = Task::new(501);
        first_task.memory.write_byte(0x2200, 17).unwrap();
        let mut first_context = SwiContext::default();
        first_context.registers[0] = 0x2200;
        module
            .invoke(
                id,
                definition,
                &export.contract,
                &mut first_task,
                &mut dispatcher,
                &mut first_context,
            )
            .unwrap();
        assert_eq!(first_context.registers[0], 0x2201);
        assert_eq!(first_context.registers[1], 17);
        assert_eq!(module.workspace_number("ERRORCODE%"), Some(1.0));

        let mut second_task = Task::new(502);
        second_task.memory.write_byte(0x2200, 29).unwrap();
        let mut second_context = SwiContext::default();
        second_context.registers[0] = 0x2200;
        module
            .invoke(
                id,
                definition,
                &export.contract,
                &mut second_task,
                &mut dispatcher,
                &mut second_context,
            )
            .unwrap();
        assert_eq!(second_context.registers[1], 29);
        assert_eq!(second_task.memory.read_byte(0x2201).unwrap(), 0);
    }

    #[test]
    fn module_workspace_rejects_caller_scoped_addresses_in_direct_and_nested_state() {
        let direct = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE BadAddressState 1.0.0\nREM @SWI Test &100 Entry\nREM @STATE BUFFER ADDRESS32\nDEF PROC Entry\nENDPROC\n";
        let nested = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE BadNestedAddressState 1.0.0\nREM @SWI Test &100 Entry\nRECORD PointerRecord\n    Buffer AS ADDRESS32\nEND RECORD\nREM @STATE PTR PointerRecord\nDEF PROC Entry\nENDPROC\n";
        for source in [direct, nested] {
            let error = SystemModule::parse(
                source,
                "modules/BadAddressState.bas64",
                &IdentityAllocator::default(),
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("cannot retain caller-scoped ADDRESS32 values")
            );
        }
    }
}
