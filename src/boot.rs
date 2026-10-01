//! Deterministic Ricochet boot capsule construction and verification.
//!
//! The executable embeds the visible module source with `include_str!` and
//! assembles the canonical capsule from that source at process startup. There
//! is deliberately no checked-in opaque archive that can become stale when a
//! BASIC64 source file changes.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

use crate::{
    basic_compat::system_profile::SystemModule,
    ricochet::{CapabilityName, IdentityAllocator, ModuleManifest},
};

pub const RUNTIME_ABI_VERSION: u32 = 1;
const FORMAT_VERSION: u16 = 1;
const MAGIC: &[u8; 8] = b"TRBOOT01";
const MAX_CAPSULE_BYTES: usize = 32 * 1024 * 1024;
const MAX_MODULES: usize = 64;
const MAX_MODULE_SOURCE_BYTES: usize = 4 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const MAX_GRANTS_PER_MODULE: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BootCapsuleError {
    Invalid(String),
    Corrupt { expected: u32, actual: u32 },
    IncompatibleRuntimeAbi { capsule: u32, runtime: u32 },
}

impl std::fmt::Display for BootCapsuleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(formatter, "{message}"),
            Self::Corrupt { expected, actual } => write!(
                formatter,
                "capsule integrity check failed (stored {expected:08X}, calculated {actual:08X})"
            ),
            Self::IncompatibleRuntimeAbi { capsule, runtime } => write!(
                formatter,
                "capsule runtime ABI {capsule} is incompatible with runtime ABI {runtime}"
            ),
        }
    }
}

impl std::error::Error for BootCapsuleError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootModuleInput<'a> {
    pub source_path: &'a str,
    pub source: &'a str,
    /// Host-approved grants for this exact embedded/selected module. These
    /// are not inferred from imports and are serialized into the capsule.
    pub grants: &'a [&'a str],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBootModule {
    pub source_path: String,
    pub source: String,
    pub manifest: ModuleManifest,
    pub grants: BTreeSet<CapabilityName>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootCapsule {
    pub runtime_abi: u32,
    pub modules: Vec<VerifiedBootModule>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BootStage {
    CapsuleBuild,
    CapsuleValidation,
    ModuleValidation,
    Linking,
    Publication,
    Start,
}

impl BootStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CapsuleBuild => "capsule build",
            Self::CapsuleValidation => "capsule validation",
            Self::ModuleValidation => "module validation",
            Self::Linking => "module linking",
            Self::Publication => "initial publication",
            Self::Start => "foundation start",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootFailure {
    pub stage: BootStage,
    pub module: Option<String>,
    pub definition: Option<String>,
    pub cause_type: String,
    pub cause_code: u32,
    pub cause_message: String,
    pub capsule_abi: Option<u32>,
    pub runtime_abi: u32,
    pub diagnostic_log: Vec<String>,
}

impl BootFailure {
    pub fn from_capsule(stage: BootStage, error: impl std::fmt::Display) -> Self {
        Self {
            stage,
            module: None,
            definition: None,
            cause_type: "BootCapsuleError".into(),
            cause_code: 1,
            cause_message: error.to_string(),
            capsule_abi: None,
            runtime_abi: RUNTIME_ABI_VERSION,
            diagnostic_log: Vec::new(),
        }
    }

    pub fn summary(&self) -> String {
        let module = self.module.as_deref().unwrap_or("<none>");
        let definition = self.definition.as_deref().unwrap_or("<none>");
        let capsule = self
            .capsule_abi
            .map_or_else(|| "unknown".to_owned(), |abi| abi.to_string());
        format!(
            "stage={} module={} definition={} cause={}(&{:08X}): {}; capsule ABI={} runtime ABI={}",
            self.stage.as_str(),
            module,
            definition,
            self.cause_type,
            self.cause_code,
            self.cause_message,
            capsule,
            self.runtime_abi
        )
    }

    pub fn from_runtime(
        stage: BootStage,
        module: Option<String>,
        definition: Option<String>,
        error: &crate::error::RuntimeError,
        capsule_abi: Option<u32>,
    ) -> Self {
        let (cause_type, cause_code, cause_message) = match error {
            crate::error::RuntimeError::Structured {
                type_name,
                code,
                message,
            } => (type_name.clone(), *code, message.clone()),
            crate::error::RuntimeError::StandardErrorBlock { code, message } => {
                ("OSError".into(), *code, message.clone())
            }
            crate::error::RuntimeError::InvalidSwi(number) => (
                "InvalidSwi".into(),
                *number,
                format!("unsupported SWI &{number:X}"),
            ),
            crate::error::RuntimeError::EndOfInput => (
                "EndOfInput".into(),
                1,
                "input ended during module start".into(),
            ),
            crate::error::RuntimeError::Io(error) => ("HostIoError".into(), 1, error.to_string()),
            crate::error::RuntimeError::Memory(error) => {
                ("LogicalMemoryError".into(), 1, error.to_string())
            }
            crate::error::RuntimeError::Program(message) => {
                ("ModuleStartError".into(), 1, message.clone())
            }
        };
        Self {
            stage,
            module,
            definition,
            cause_type,
            cause_code,
            cause_message,
            capsule_abi,
            runtime_abi: RUNTIME_ABI_VERSION,
            diagnostic_log: Vec::new(),
        }
    }

    pub fn host_capsule_read(path: &str, error: impl std::fmt::Display) -> Self {
        let mut failure = Self::from_capsule(
            BootStage::CapsuleValidation,
            format!("could not read selected capsule {path}: {error}"),
        );
        failure.cause_type = "NativeCapsuleReadError".into();
        failure.cause_code = 2;
        failure
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    RetryEmbedded,
    SelectCapsule(String),
    Exit,
    Invalid,
}

pub fn parse_recovery_action(input: &str) -> RecoveryAction {
    let input = input.trim();
    if input.eq_ignore_ascii_case("R") || input.eq_ignore_ascii_case("RETRY") {
        RecoveryAction::RetryEmbedded
    } else if input.eq_ignore_ascii_case("Q") || input.eq_ignore_ascii_case("QUIT") {
        RecoveryAction::Exit
    } else if input.eq_ignore_ascii_case("A") || input.eq_ignore_ascii_case("ALTERNATE") {
        RecoveryAction::SelectCapsule(String::new())
    } else if let Some(path) = input
        .strip_prefix("A ")
        .or_else(|| input.strip_prefix("a "))
        .or_else(|| input.strip_prefix("--capsule "))
    {
        let path = path.trim();
        if path.is_empty() {
            RecoveryAction::Invalid
        } else {
            RecoveryAction::SelectCapsule(path.to_owned())
        }
    } else {
        RecoveryAction::Invalid
    }
}

impl BootCapsule {
    pub fn build(
        runtime_abi: u32,
        inputs: &[BootModuleInput<'_>],
    ) -> Result<Vec<u8>, BootCapsuleError> {
        if inputs.is_empty() || inputs.len() > MAX_MODULES {
            return Err(BootCapsuleError::Invalid(format!(
                "capsule module count must be between 1 and {MAX_MODULES}"
            )));
        }
        let allocator = IdentityAllocator::default();
        let mut modules = Vec::with_capacity(inputs.len());
        for input in inputs {
            if input.source_path.is_empty()
                || input.source_path.starts_with('/')
                || input
                    .source_path
                    .split('/')
                    .any(|part| part.is_empty() || part == "." || part == "..")
            {
                return Err(BootCapsuleError::Invalid(format!(
                    "invalid capsule source path {:?}",
                    input.source_path
                )));
            }
            if input.source.len() > MAX_MODULE_SOURCE_BYTES {
                return Err(BootCapsuleError::Invalid(format!(
                    "module source {} exceeds {MAX_MODULE_SOURCE_BYTES} bytes",
                    input.source_path
                )));
            }
            let parsed = SystemModule::parse(input.source, input.source_path, &allocator).map_err(
                |error| {
                    BootCapsuleError::Invalid(format!(
                        "module {} does not parse: {error}",
                        input.source_path
                    ))
                },
            )?;
            let mut grants = BTreeSet::new();
            for grant in input.grants {
                let capability = CapabilityName::new(*grant)
                    .map_err(|error| BootCapsuleError::Invalid(error.to_string()))?;
                if !grants.insert(capability) {
                    return Err(BootCapsuleError::Invalid(format!(
                        "duplicate host capability grant {grant} for {}",
                        parsed.manifest.name
                    )));
                }
            }
            let manifest = parsed.manifest;
            for grant in &grants {
                if !manifest.requested_capabilities.contains(grant) {
                    return Err(BootCapsuleError::Invalid(format!(
                        "capsule grants unrequested capability {} to module {}",
                        grant.as_str(),
                        manifest.name
                    )));
                }
            }
            for import in &manifest.primitive_imports {
                if !grants.contains(&import.capability) {
                    return Err(BootCapsuleError::Invalid(format!(
                        "module {} imports {} without capsule grant {}",
                        manifest.name,
                        import.name,
                        import.capability.as_str()
                    )));
                }
            }
            let encoded_manifest = manifest
                .encode_v1()
                .map_err(|error| BootCapsuleError::Invalid(error.to_string()))?;
            if encoded_manifest.len() > MAX_MANIFEST_BYTES {
                return Err(BootCapsuleError::Invalid(format!(
                    "manifest for {} exceeds {MAX_MANIFEST_BYTES} bytes",
                    manifest.name
                )));
            }
            modules.push(VerifiedBootModule {
                source_path: input.source_path.to_owned(),
                source: input.source.to_owned(),
                manifest,
                grants,
            });
        }
        validate_module_set(&modules)?;
        modules.sort_by_key(|module| module.manifest.name.to_ascii_lowercase());
        encode_capsule(runtime_abi, &modules)
    }

    pub fn decode(bytes: &[u8], runtime_abi: u32) -> Result<Self, BootCapsuleError> {
        if bytes.len() < 8 + 2 + 4 + 2 + 4 || bytes.len() > MAX_CAPSULE_BYTES {
            return Err(BootCapsuleError::Invalid(
                "capsule length is outside the supported bounds".into(),
            ));
        }
        let body_length = bytes.len() - 4;
        let stored_crc = u32::from_be_bytes(
            bytes[body_length..]
                .try_into()
                .expect("the capsule footer is exactly four bytes"),
        );
        let actual_crc = crc32(&bytes[..body_length]);
        if stored_crc != actual_crc {
            return Err(BootCapsuleError::Corrupt {
                expected: stored_crc,
                actual: actual_crc,
            });
        }

        let mut cursor = Cursor::new(&bytes[..body_length]);
        if cursor.take(8)? != MAGIC {
            return Err(BootCapsuleError::Invalid(
                "capsule magic does not match".into(),
            ));
        }
        let format = cursor.u16()?;
        if format != FORMAT_VERSION {
            return Err(BootCapsuleError::Invalid(format!(
                "unsupported capsule format {format}"
            )));
        }
        let capsule_abi = cursor.u32()?;
        if capsule_abi != runtime_abi {
            return Err(BootCapsuleError::IncompatibleRuntimeAbi {
                capsule: capsule_abi,
                runtime: runtime_abi,
            });
        }
        let module_count = usize::from(cursor.u16()?);
        if module_count == 0 || module_count > MAX_MODULES {
            return Err(BootCapsuleError::Invalid(format!(
                "capsule module count {module_count} is invalid"
            )));
        }

        let allocator = IdentityAllocator::default();
        let mut modules = Vec::with_capacity(module_count);
        for _ in 0..module_count {
            let path = cursor.string_u16(4096)?;
            let manifest_text = cursor.string_u32(MAX_MANIFEST_BYTES)?;
            let source = cursor.string_u32(MAX_MODULE_SOURCE_BYTES)?;
            let grant_count = usize::from(cursor.u16()?);
            if grant_count > MAX_GRANTS_PER_MODULE {
                return Err(BootCapsuleError::Invalid(format!(
                    "module {path} declares too many grants"
                )));
            }
            let mut grants = BTreeSet::new();
            for _ in 0..grant_count {
                let name = cursor.string_u16(128)?;
                let capability = CapabilityName::new(name.clone())
                    .map_err(|error| BootCapsuleError::Invalid(error.to_string()))?;
                if !grants.insert(capability) {
                    return Err(BootCapsuleError::Invalid(format!(
                        "module {path} repeats capability grant {name}"
                    )));
                }
            }
            let manifest = ModuleManifest::decode_v1(&manifest_text)
                .map_err(|error| BootCapsuleError::Invalid(error.to_string()))?;
            if manifest.encode_v1().map_err(|error| {
                BootCapsuleError::Invalid(format!("manifest canonicalization failed: {error}"))
            })? != manifest_text
            {
                return Err(BootCapsuleError::Invalid(format!(
                    "module {} manifest is not canonical",
                    manifest.name
                )));
            }
            if manifest.source_path != path {
                return Err(BootCapsuleError::Invalid(format!(
                    "module {} manifest path does not match capsule record",
                    manifest.name
                )));
            }
            for grant in &grants {
                if !manifest.requested_capabilities.contains(grant) {
                    return Err(BootCapsuleError::Invalid(format!(
                        "capsule grants unrequested capability {} to module {}",
                        grant.as_str(),
                        manifest.name
                    )));
                }
            }
            let parsed =
                SystemModule::parse(&source, path.clone(), &allocator).map_err(|error| {
                    BootCapsuleError::Invalid(format!(
                        "module {} source validation failed: {error}",
                        manifest.name
                    ))
                })?;
            if parsed.manifest != manifest {
                return Err(BootCapsuleError::Invalid(format!(
                    "module {} source and canonical manifest disagree",
                    manifest.name
                )));
            }
            for import in &manifest.primitive_imports {
                if !grants.contains(&import.capability) {
                    return Err(BootCapsuleError::Invalid(format!(
                        "module {} imports {} without capsule grant {}",
                        manifest.name,
                        import.name,
                        import.capability.as_str()
                    )));
                }
            }
            modules.push(VerifiedBootModule {
                source_path: path,
                source,
                manifest,
                grants,
            });
        }
        if !cursor.is_empty() {
            return Err(BootCapsuleError::Invalid(
                "capsule contains trailing data".into(),
            ));
        }
        validate_module_set(&modules)?;
        let canonical = encode_capsule(capsule_abi, &modules)?;
        if canonical != bytes {
            return Err(BootCapsuleError::Invalid(
                "capsule records are not in canonical order or encoding".into(),
            ));
        }
        Ok(Self {
            runtime_abi: capsule_abi,
            modules,
        })
    }

    /// Stable dependency order for lifecycle hooks. Dependencies start before
    /// their consumers; names break ties deterministically.
    pub fn start_order(&self) -> Vec<usize> {
        let mut by_name = BTreeMap::new();
        for (index, module) in self.modules.iter().enumerate() {
            by_name.insert(module.manifest.name.to_ascii_lowercase(), index);
        }
        let mut order = Vec::with_capacity(self.modules.len());
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut roots = (0..self.modules.len()).collect::<Vec<_>>();
        roots.sort_by_key(|index| self.modules[*index].manifest.name.to_ascii_lowercase());
        for index in roots {
            append_start_order(
                index,
                &self.modules,
                &by_name,
                &mut visiting,
                &mut visited,
                &mut order,
            );
        }
        order
    }
}

/// Rebuilds the embedded capsule from the checked-in, human-readable modules.
/// The `include_str!` dependencies make Cargo rebuild the executable whenever
/// either source changes; capsule bytes are never stored as a stale archive.
pub fn embedded_capsule_bytes() -> Result<&'static [u8], BootCapsuleError> {
    static EMBEDDED: OnceLock<Result<Vec<u8>, BootCapsuleError>> = OnceLock::new();
    let result = EMBEDDED.get_or_init(|| {
        let inputs = [
            BootModuleInput {
                source_path: "modules/Boot.bas64",
                source: include_str!("../modules/Boot.bas64"),
                grants: &[],
            },
            BootModuleInput {
                source_path: "modules/Error.bas64",
                source: include_str!("../modules/Error.bas64"),
                grants: &["ErrorDispatch"],
            },
            BootModuleInput {
                source_path: "modules/Memory.bas64",
                source: include_str!("../modules/Memory.bas64"),
                grants: &["RuntimeErrors", "TaskMemory"],
            },
            BootModuleInput {
                source_path: "modules/ModuleManager.bas64",
                source: include_str!("../modules/ModuleManager.bas64"),
                grants: &["ModuleIntrospection", "ModuleManagement", "RuntimeErrors"],
            },
            BootModuleInput {
                source_path: "modules/TaskManager.bas64",
                source: include_str!("../modules/TaskManager.bas64"),
                grants: &["RuntimeErrors", "TaskQuery"],
            },
            BootModuleInput {
                source_path: "modules/RicochetCommands.bas64",
                source: include_str!("../modules/RicochetCommands.bas64"),
                grants: &[
                    "ConfigurationStoreRead",
                    "ConfigurationStoreWrite",
                    "CommandRegistry",
                    "CommandScripts",
                    "ExecInput",
                    "RuntimeErrors",
                    "TaskMemory",
                ],
            },
            BootModuleInput {
                source_path: "modules/Console.bas64",
                source: include_str!("../modules/Console.bas64"),
                grants: &[
                    "ConsoleInput",
                    "ConsoleOutput",
                    "RuntimeErrors",
                    "GraphicsVduStream",
                ],
            },
            BootModuleInput {
                source_path: "modules/Mos.bas64",
                source: include_str!("../modules/Mos.bas64"),
                grants: &["MosInput", "MosClock", "TaskMemory", "RuntimeErrors"],
            },
            BootModuleInput {
                source_path: "modules/FileSwitch.bas64",
                source: include_str!("../modules/FileSwitch.bas64"),
                grants: &[
                    "FileSystem",
                    "RuntimeErrors",
                    "SystemVariableStore",
                    "TaskMemory",
                ],
            },
            BootModuleInput {
                source_path: "modules/Graphics.bas64",
                source: include_str!("../modules/Graphics.bas64"),
                grants: &["GraphicsRaster", "RuntimeErrors"],
            },
            BootModuleInput {
                source_path: "modules/ColourTrans.bas64",
                source: include_str!("../modules/ColourTrans.bas64"),
                grants: &["GraphicsRaster"],
            },
            BootModuleInput {
                source_path: "modules/DesktopServices.bas64",
                source: include_str!("../modules/DesktopServices.bas64"),
                grants: &["FileSystem", "WimpSystemMenu"],
            },
            BootModuleInput {
                source_path: "modules/DisplayManager.bas64",
                source: include_str!("../modules/DisplayManager.bas64"),
                grants: &["DisplaySettings"],
            },
            BootModuleInput {
                source_path: "modules/Wimp.bas64",
                source: include_str!("../modules/Wimp.bas64"),
                grants: &["RuntimeErrors", "WimpTaskLifecycle", "WimpWindowState"],
            },
            BootModuleInput {
                source_path: "modules/System.bas64",
                source: include_str!("../modules/System.bas64"),
                grants: &["StartupPolicy", "SystemQueries", "SystemVariableStore"],
            },
        ];
        BootCapsule::build(RUNTIME_ABI_VERSION, &inputs)
    });
    result.as_ref().map(Vec::as_slice).map_err(Clone::clone)
}

fn validate_module_set(modules: &[VerifiedBootModule]) -> Result<(), BootCapsuleError> {
    let mut by_name = BTreeMap::new();
    let mut paths = BTreeSet::new();
    let mut swi_numbers = BTreeSet::new();
    let mut swi_names = BTreeSet::new();
    for (index, module) in modules.iter().enumerate() {
        if by_name
            .insert(module.manifest.name.to_ascii_lowercase(), index)
            .is_some()
        {
            return Err(BootCapsuleError::Invalid(format!(
                "capsule repeats module {}",
                module.manifest.name
            )));
        }
        if !paths.insert(module.source_path.to_ascii_lowercase()) {
            return Err(BootCapsuleError::Invalid(format!(
                "capsule repeats source path {}",
                module.source_path
            )));
        }
        for export in &module.manifest.exports {
            if !swi_numbers.insert(export.number) {
                return Err(BootCapsuleError::Invalid(format!(
                    "duplicate SWI number &{:X} in boot capsule",
                    export.number
                )));
            }
            if !swi_names.insert(export.name.to_ascii_lowercase()) {
                return Err(BootCapsuleError::Invalid(format!(
                    "duplicate SWI name {} in boot capsule",
                    export.name
                )));
            }
        }
    }
    for module in modules {
        for (dependency, required_version) in &module.manifest.dependencies {
            let Some(index) = by_name.get(&dependency.to_ascii_lowercase()) else {
                return Err(BootCapsuleError::Invalid(format!(
                    "module {} has unresolved dependency {dependency}",
                    module.manifest.name
                )));
            };
            let provider = &modules[*index].manifest;
            if provider.version < *required_version {
                return Err(BootCapsuleError::Invalid(format!(
                    "module {} requires {} version {}.{}.{} but capsule provides {}.{}.{}",
                    module.manifest.name,
                    provider.name,
                    required_version.major,
                    required_version.minor,
                    required_version.patch,
                    provider.version.major,
                    provider.version.minor,
                    provider.version.patch
                )));
            }
        }
        for import in &module.manifest.symbol_imports {
            let Some(index) = by_name.get(&import.module.to_ascii_lowercase()) else {
                return Err(BootCapsuleError::Invalid(format!(
                    "module {} has unresolved symbol provider {}",
                    module.manifest.name, import.module
                )));
            };
            let provider = &modules[*index].manifest;
            if !provider
                .symbol_exports
                .iter()
                .any(|symbol| symbol.eq_ignore_ascii_case(&import.symbol))
            {
                return Err(BootCapsuleError::Invalid(format!(
                    "module {} imports {}.{} which is not exported",
                    module.manifest.name, import.module, import.symbol
                )));
            }
        }
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for module in modules {
        visit_dependencies(
            &module.manifest.name.to_ascii_lowercase(),
            modules,
            &by_name,
            &mut visiting,
            &mut visited,
        )?;
    }
    Ok(())
}

fn append_start_order(
    index: usize,
    modules: &[VerifiedBootModule],
    by_name: &BTreeMap<String, usize>,
    visiting: &mut BTreeSet<usize>,
    visited: &mut BTreeSet<usize>,
    output: &mut Vec<usize>,
) {
    if visited.contains(&index) || !visiting.insert(index) {
        return;
    }
    for (dependency, _) in &modules[index].manifest.dependencies {
        if let Some(dependency_index) = by_name.get(&dependency.to_ascii_lowercase()) {
            append_start_order(
                *dependency_index,
                modules,
                by_name,
                visiting,
                visited,
                output,
            );
        }
    }
    visiting.remove(&index);
    visited.insert(index);
    output.push(index);
}

fn visit_dependencies(
    name: &str,
    modules: &[VerifiedBootModule],
    by_name: &BTreeMap<String, usize>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
) -> Result<(), BootCapsuleError> {
    if visited.contains(name) {
        return Ok(());
    }
    if !visiting.insert(name.to_owned()) {
        return Err(BootCapsuleError::Invalid(format!(
            "module dependency cycle includes {name}"
        )));
    }
    let index = by_name[name];
    for (dependency, _) in &modules[index].manifest.dependencies {
        visit_dependencies(
            &dependency.to_ascii_lowercase(),
            modules,
            by_name,
            visiting,
            visited,
        )?;
    }
    visiting.remove(name);
    visited.insert(name.to_owned());
    Ok(())
}

fn encode_capsule(
    runtime_abi: u32,
    modules: &[VerifiedBootModule],
) -> Result<Vec<u8>, BootCapsuleError> {
    let mut ordered = modules.to_vec();
    ordered.sort_by_key(|module| module.manifest.name.to_ascii_lowercase());
    let mut output = Vec::new();
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&FORMAT_VERSION.to_be_bytes());
    output.extend_from_slice(&runtime_abi.to_be_bytes());
    output.extend_from_slice(
        &u16::try_from(ordered.len())
            .map_err(|_| BootCapsuleError::Invalid("too many modules".into()))?
            .to_be_bytes(),
    );
    for module in ordered {
        let manifest = module
            .manifest
            .encode_v1()
            .map_err(|error| BootCapsuleError::Invalid(error.to_string()))?;
        push_u16_string(&mut output, &module.source_path)?;
        push_u32_string(&mut output, &manifest)?;
        push_u32_string(&mut output, &module.source)?;
        output.extend_from_slice(
            &u16::try_from(module.grants.len())
                .map_err(|_| BootCapsuleError::Invalid("too many module grants".into()))?
                .to_be_bytes(),
        );
        for grant in module.grants {
            push_u16_string(&mut output, grant.as_str())?;
        }
        if output.len() > MAX_CAPSULE_BYTES - 4 {
            return Err(BootCapsuleError::Invalid(
                "capsule exceeds the supported size limit".into(),
            ));
        }
    }
    let checksum = crc32(&output);
    output.extend_from_slice(&checksum.to_be_bytes());
    Ok(output)
}

fn push_u16_string(output: &mut Vec<u8>, value: &str) -> Result<(), BootCapsuleError> {
    let length = u16::try_from(value.len())
        .map_err(|_| BootCapsuleError::Invalid("capsule string is too large".into()))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn push_u32_string(output: &mut Vec<u8>, value: &str) -> Result<(), BootCapsuleError> {
    let length = u32::try_from(value.len())
        .map_err(|_| BootCapsuleError::Invalid("capsule record is too large".into()))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], BootCapsuleError> {
        let end = self.offset.checked_add(count).ok_or_else(|| {
            BootCapsuleError::Invalid("capsule length arithmetic overflow".into())
        })?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| BootCapsuleError::Invalid("capsule is truncated".into()))?;
        self.offset = end;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, BootCapsuleError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("fixed width"),
        ))
    }

    fn u32(&mut self) -> Result<u32, BootCapsuleError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("fixed width"),
        ))
    }

    fn string_u16(&mut self, maximum: usize) -> Result<String, BootCapsuleError> {
        let length = usize::from(self.u16()?);
        self.string(length, maximum)
    }

    fn string_u32(&mut self, maximum: usize) -> Result<String, BootCapsuleError> {
        let length = usize::try_from(self.u32()?)
            .map_err(|_| BootCapsuleError::Invalid("capsule length overflow".into()))?;
        self.string(length, maximum)
    }

    fn string(&mut self, length: usize, maximum: usize) -> Result<String, BootCapsuleError> {
        if length > maximum {
            return Err(BootCapsuleError::Invalid(format!(
                "capsule string exceeds its {maximum}-byte limit"
            )));
        }
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| BootCapsuleError::Invalid("capsule text is not UTF-8".into()))
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        BootCapsule, BootModuleInput, RUNTIME_ABI_VERSION, RecoveryAction, embedded_capsule_bytes,
        parse_recovery_action,
    };

    const LEAF: &str = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Leaf 1.0.0\nREM @LIFECYCLE START Begin\nREM @EXPORT PROC Ping\nREM @PRIVATE PROC Begin\nDEF PROC Begin\nENDPROC\nDEF PROC Ping\nENDPROC\n";
    const ROOT: &str = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Root 1.0.0\nREM @IMPORT_MODULE Leaf 1.0.0\nREM @IMPORT_SYMBOL Leaf PROC Ping\nREM @LIFECYCLE START Begin\nREM @EXPORT PROC Ping\nREM @PRIVATE PROC Begin\nDEF PROC Begin\nENDPROC\nDEF PROC Ping\nENDPROC\n";

    #[test]
    fn capsule_bytes_are_deterministic_and_keep_visible_source() {
        let inputs = [
            BootModuleInput {
                source_path: "modules/Root.bas64",
                source: ROOT,
                grants: &[],
            },
            BootModuleInput {
                source_path: "modules/Leaf.bas64",
                source: LEAF,
                grants: &[],
            },
        ];
        let first = BootCapsule::build(RUNTIME_ABI_VERSION, &inputs).unwrap();
        let second = BootCapsule::build(RUNTIME_ABI_VERSION, &inputs).unwrap();
        assert_eq!(first, second);
        let parsed = BootCapsule::decode(&first, RUNTIME_ABI_VERSION).unwrap();
        assert_eq!(parsed.modules[0].manifest.name, "Leaf");
        assert!(parsed.modules.iter().any(|module| module.source == ROOT));
    }

    #[test]
    fn embedded_foundation_capsule_has_all_owners_and_startup_authority_on_system() {
        let capsule =
            BootCapsule::decode(embedded_capsule_bytes().unwrap(), RUNTIME_ABI_VERSION).unwrap();
        let module_names = capsule
            .modules
            .iter()
            .map(|module| module.manifest.name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            module_names,
            BTreeSet::from([
                "Boot",
                "Console",
                "ColourTrans",
                "DesktopServices",
                "DisplayManager",
                "Error",
                "FileSwitch",
                "Graphics",
                "Memory",
                "ModuleManager",
                "Mos",
                "Wimp",
                "System",
                "TaskManager",
                "RicochetCommands",
            ])
        );
        let system = capsule
            .modules
            .iter()
            .find(|module| module.manifest.name == "System")
            .unwrap();
        let boot = capsule
            .modules
            .iter()
            .find(|module| module.manifest.name == "Boot")
            .unwrap();
        assert!(
            system
                .grants
                .contains(&crate::ricochet::CapabilityName::new("StartupPolicy").unwrap())
        );
        assert!(
            system
                .grants
                .contains(&crate::ricochet::CapabilityName::new("SystemQueries").unwrap())
        );
        assert!(boot.grants.is_empty());
        for (module_name, capability) in [
            ("Console", "ConsoleInput"),
            ("Console", "ConsoleOutput"),
            ("Console", "GraphicsVduStream"),
            ("Console", "RuntimeErrors"),
            ("Mos", "MosInput"),
            ("Mos", "MosClock"),
            ("Mos", "TaskMemory"),
            ("Mos", "RuntimeErrors"),
            ("Graphics", "GraphicsRaster"),
            ("Graphics", "RuntimeErrors"),
            ("ColourTrans", "GraphicsRaster"),
            ("DesktopServices", "FileSystem"),
            ("DesktopServices", "WimpSystemMenu"),
            ("DisplayManager", "DisplaySettings"),
            ("Wimp", "RuntimeErrors"),
            ("Wimp", "WimpTaskLifecycle"),
            ("Wimp", "WimpWindowState"),
            ("FileSwitch", "TaskMemory"),
            ("System", "SystemQueries"),
            ("System", "SystemVariableStore"),
            ("Error", "ErrorDispatch"),
            ("Memory", "TaskMemory"),
            ("Memory", "RuntimeErrors"),
            ("ModuleManager", "ModuleIntrospection"),
            ("ModuleManager", "ModuleManagement"),
            ("ModuleManager", "RuntimeErrors"),
            ("TaskManager", "TaskQuery"),
            ("TaskManager", "RuntimeErrors"),
            ("RicochetCommands", "CommandRegistry"),
            ("RicochetCommands", "ConfigurationStoreRead"),
            ("RicochetCommands", "ConfigurationStoreWrite"),
            ("RicochetCommands", "TaskMemory"),
            ("RicochetCommands", "RuntimeErrors"),
        ] {
            let module = capsule
                .modules
                .iter()
                .find(|module| module.manifest.name == module_name)
                .unwrap();
            assert!(
                module
                    .grants
                    .contains(&crate::ricochet::CapabilityName::new(capability).unwrap())
            );
        }
        let boot_dependencies = boot
            .manifest
            .dependencies
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            boot_dependencies,
            BTreeSet::from([
                "Console",
                "Error",
                "Memory",
                "ModuleManager",
                "Mos",
                "System",
                "TaskManager",
                "RicochetCommands",
            ])
        );

        let owned_exports = capsule
            .modules
            .iter()
            .flat_map(|module| {
                module.manifest.exports.iter().map(|export| {
                    (
                        module.manifest.name.to_ascii_uppercase(),
                        export.name.to_ascii_uppercase(),
                    )
                })
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(owned_exports.len(), 40);
        for (owner, swi) in [
            ("ERROR", "OS_GENERATEERROR"),
            ("MEMORY", "OS_CHANGEDYNAMICAREA"),
            ("MEMORY", "OS_DYNAMICAREA"),
            ("MODULEMANAGER", "RICOCHET_MODULEINFO"),
            ("TASKMANAGER", "RICOCHET_TASKINFO"),
            ("MODULEMANAGER", "RICOCHET_MODULELOOKUP"),
            ("MODULEMANAGER", "RICOCHET_SWIINFO"),
            ("MODULEMANAGER", "RICOCHET_MODULEEXPORT"),
            ("MODULEMANAGER", "RICOCHET_DEFINITIONSOURCE"),
            ("MODULEMANAGER", "OS_MODULE"),
            ("RICOCHETCOMMANDS", "OS_CLI"),
            ("MOS", "OS_BYTE"),
            ("MOS", "OS_WORD"),
            ("GRAPHICS", "OS_PLOT"),
            ("GRAPHICS", "OS_READPOINT"),
            ("FILESWITCH", "OS_GBPB"),
            ("FILESWITCH", "OS_FILE"),
            ("FILESWITCH", "OS_ARGS"),
            ("FILESWITCH", "OS_BGET"),
            ("FILESWITCH", "OS_BPUT"),
            ("FILESWITCH", "OS_FIND"),
            ("SYSTEM", "OS_READMONOTONICTIME"),
            ("SYSTEM", "OS_READVARVAL"),
            ("SYSTEM", "OS_SETVARVAL"),
            ("SYSTEM", "OS_SWINUMBERTOSTRING"),
            ("SYSTEM", "OS_SWINUMBERFROMSTRING"),
        ] {
            assert!(owned_exports.contains(&(owner.into(), swi.into())));
        }

        let order = capsule
            .start_order()
            .into_iter()
            .map(|index| capsule.modules[index].manifest.name.as_str())
            .collect::<Vec<_>>();
        assert!(
            order.iter().position(|name| *name == "Console").unwrap()
                < order.iter().position(|name| *name == "Boot").unwrap()
        );
        assert!(
            order.iter().position(|name| *name == "System").unwrap()
                < order.iter().position(|name| *name == "Boot").unwrap()
        );
        assert!(
            order.iter().position(|name| *name == "Mos").unwrap()
                < order.iter().position(|name| *name == "Boot").unwrap()
        );
    }

    #[test]
    fn capsule_rejects_corruption_abi_mismatch_and_dependency_cycles() {
        let original = embedded_capsule_bytes().unwrap();
        let mut corrupted = original.to_vec();
        corrupted[20] ^= 0x80;
        assert!(BootCapsule::decode(&corrupted, RUNTIME_ABI_VERSION).is_err());
        let incompatible = BootCapsule::decode(original, RUNTIME_ABI_VERSION + 1).unwrap_err();
        assert!(incompatible.to_string().contains("incompatible"));

        let first = LEAF.replace(
            "REM @MODULE Leaf 1.0.0",
            "REM @MODULE Leaf 1.0.0\nREM @IMPORT_MODULE Root 1.0.0",
        );
        let inputs = [
            BootModuleInput {
                source_path: "modules/Leaf.bas64",
                source: &first,
                grants: &[],
            },
            BootModuleInput {
                source_path: "modules/Root.bas64",
                source: ROOT,
                grants: &[],
            },
        ];
        assert!(
            BootCapsule::build(RUNTIME_ABI_VERSION, &inputs)
                .unwrap_err()
                .to_string()
                .contains("cycle")
        );
    }

    #[test]
    fn capsule_rejects_duplicate_swi_owners_and_unresolved_dependencies() {
        let first = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE First 1.0.0\nREM @SWI First_Service &500 Entry\nDEF PROC Entry\nENDPROC\n";
        let second = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Second 1.0.0\nREM @SWI Second_Service &500 Entry\nDEF PROC Entry\nENDPROC\n";
        let duplicate = [
            BootModuleInput {
                source_path: "modules/First.bas64",
                source: first,
                grants: &[],
            },
            BootModuleInput {
                source_path: "modules/Second.bas64",
                source: second,
                grants: &[],
            },
        ];
        assert!(
            BootCapsule::build(RUNTIME_ABI_VERSION, &duplicate)
                .unwrap_err()
                .to_string()
                .contains("duplicate SWI number")
        );

        let missing = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Root 1.0.0\nREM @IMPORT_MODULE Missing 1.0.0\nREM @LIFECYCLE START Begin\nREM @PRIVATE PROC Begin\nDEF PROC Begin\nENDPROC\n";
        let unresolved = [BootModuleInput {
            source_path: "modules/Root.bas64",
            source: missing,
            grants: &[],
        }];
        assert!(
            BootCapsule::build(RUNTIME_ABI_VERSION, &unresolved)
                .unwrap_err()
                .to_string()
                .contains("unresolved dependency")
        );
    }

    #[test]
    fn recovery_actions_are_restricted_to_retry_alternate_or_exit() {
        assert_eq!(
            parse_recovery_action("retry"),
            RecoveryAction::RetryEmbedded
        );
        assert_eq!(
            parse_recovery_action("A /tmp/boot.cap"),
            RecoveryAction::SelectCapsule("/tmp/boot.cap".into())
        );
        assert_eq!(parse_recovery_action("quit"), RecoveryAction::Exit);
        assert_eq!(parse_recovery_action("HELP"), RecoveryAction::Invalid);
        assert_eq!(parse_recovery_action("CAT"), RecoveryAction::Invalid);
    }
}
