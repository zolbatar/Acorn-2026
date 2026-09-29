//! Versioned identities, module publication, and capability checked primitive
//! imports for Trellis.
//!
//! This module contains substrate machinery. It deliberately has no public SWI
//! names or implementations; SWI exports and their BASIC64 source are supplied
//! by module manifests.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    ops::Deref,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

#[derive(Debug, Default)]
pub struct IdentityAllocator {
    next: AtomicU64,
}

macro_rules! opaque_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            pub fn diagnostic_value(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, concat!(stringify!($name), "({})"), self.0)
            }
        }
    };
}

opaque_id!(ModuleId);
opaque_id!(ModuleInstanceId);
opaque_id!(DefinitionId);
opaque_id!(GenerationId);
opaque_id!(SwiEntryId);
opaque_id!(CapabilityId);
opaque_id!(PrimitiveId);

static NEXT_MODULE_AUTHORITY: AtomicU64 = AtomicU64::new(1);

/// Unforgeable crate-internal authority for module-management operations.
/// Guest BASIC64 values and logical addresses cannot construct this token.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleManagementAuthority(u64);

impl IdentityAllocator {
    fn allocate(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed).saturating_add(1)
    }

    pub fn module_id(&self) -> ModuleId {
        ModuleId(self.allocate())
    }

    pub fn module_instance_id(&self) -> ModuleInstanceId {
        ModuleInstanceId(self.allocate())
    }

    pub fn definition_id(&self) -> DefinitionId {
        DefinitionId(self.allocate())
    }

    pub fn generation_id(&self) -> GenerationId {
        GenerationId(self.allocate())
    }

    pub fn swi_entry_id(&self) -> SwiEntryId {
        SwiEntryId(self.allocate())
    }

    pub fn capability_id(&self) -> CapabilityId {
        CapabilityId(self.allocate())
    }

    pub fn primitive_id(&self) -> PrimitiveId {
        PrimitiveId(self.allocate())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
}

impl SemanticVersion {
    pub const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CapabilityName(String);

impl CapabilityName {
    pub fn new(name: impl Into<String>) -> Result<Self, RegistryError> {
        let name = name.into();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(RegistryError::InvalidCapabilityName(name));
        }
        Ok(Self(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegisterKind {
    Unsigned { bits: u8 },
    Signed { bits: u8 },
    LogicalAddress { bits: u8 },
    OpaqueHandle { type_name: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisterContract {
    pub register: u8,
    pub kind: RegisterKind,
    pub direction: ArgumentDirection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgumentDirection {
    In,
    Out,
    InOut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogicalMemoryContract {
    Read {
        register: u8,
        max_bytes: Option<u32>,
    },
    Write {
        register: u8,
        max_bytes: Option<u32>,
    },
    ReadWrite {
        register: u8,
        max_bytes: Option<u32>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwiContract {
    pub registers: Vec<RegisterContract>,
    pub logical_memory: Vec<LogicalMemoryContract>,
    pub program_counter: Option<ArgumentDirection>,
    pub carry: Option<ArgumentDirection>,
    pub may_block: bool,
    pub may_reenter: bool,
    pub error_transport: String,
}

impl Default for SwiContract {
    fn default() -> Self {
        Self {
            registers: Vec::new(),
            logical_memory: Vec::new(),
            program_counter: None,
            carry: None,
            may_block: false,
            may_reenter: true,
            error_transport: "RuntimeResult".into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimitiveDescriptor {
    pub id: PrimitiveId,
    pub name: String,
    pub capability: CapabilityName,
    pub arguments: Vec<RegisterKind>,
    pub results: Vec<RegisterKind>,
    pub blocking: bool,
    pub reentrant: bool,
    pub memory_rules: Vec<LogicalMemoryContract>,
    /// Required runtime authority for each opaque resource-handle argument.
    /// Handle-typed primitive arguments without an entry here cannot link.
    pub resource_requirements: Vec<ResourceHandleRequirement>,
    pub failure_contract: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ResourceRight {
    Read,
    Write,
    Control,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceHandleRequirement {
    pub argument_register: u8,
    pub type_name: String,
    pub right: ResourceRight,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimitiveImport {
    pub name: String,
    pub capability: CapabilityName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleSymbolImport {
    pub module: String,
    /// `PROC:Name` or `FN:Name` in canonical display form.
    pub symbol: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwiExport {
    pub number: u32,
    pub name: String,
    pub definition_name: String,
    pub contract: SwiContract,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleManifest {
    pub schema_version: u16,
    pub name: String,
    pub version: SemanticVersion,
    pub language_profile: String,
    pub target_profile: String,
    pub dependencies: Vec<(String, SemanticVersion)>,
    pub symbol_imports: Vec<ModuleSymbolImport>,
    pub primitive_imports: Vec<PrimitiveImport>,
    pub requested_capabilities: BTreeSet<CapabilityName>,
    pub lifecycle: ModuleLifecycle,
    pub replacement_policy: ReplacementPolicy,
    /// Public BASIC64 symbols. SWI definitions are included automatically.
    pub symbol_exports: Vec<String>,
    pub exports: Vec<SwiExport>,
    pub source_path: String,
    pub source_hash: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModuleLifecycle {
    pub start: Option<String>,
    pub quiesce: Option<String>,
    pub finalise: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ReplacementPolicy {
    #[default]
    CompatibleImmediate,
    Quiescent,
    Migrating,
    RestartRequired,
}

impl ModuleManifest {
    /// Deterministic, versioned wire form. Strings are percent-escaped UTF-8;
    /// indexed arrays make ordering and empty collections unambiguous.
    pub fn encode_v1(&self) -> Result<String, RegistryError> {
        validate_manifest_shape(self)?;
        let mut fields = vec![
            ("schema".to_owned(), "1".to_owned()),
            ("name".to_owned(), encode_text(&self.name)),
            ("version".to_owned(), encode_version(self.version)),
            (
                "profile.language".to_owned(),
                encode_text(&self.language_profile),
            ),
            (
                "profile.target".to_owned(),
                encode_text(&self.target_profile),
            ),
            ("source_path".to_owned(), encode_text(&self.source_path)),
            ("source_hash".to_owned(), encode_text(&self.source_hash)),
            (
                "replacement".to_owned(),
                self.replacement_policy.as_str().to_owned(),
            ),
            (
                "lifecycle.start".to_owned(),
                encode_optional(&self.lifecycle.start),
            ),
            (
                "lifecycle.quiesce".to_owned(),
                encode_optional(&self.lifecycle.quiesce),
            ),
            (
                "lifecycle.finalise".to_owned(),
                encode_optional(&self.lifecycle.finalise),
            ),
            (
                "dependency.count".to_owned(),
                self.dependencies.len().to_string(),
            ),
            (
                "symbol_import.count".to_owned(),
                self.symbol_imports.len().to_string(),
            ),
            (
                "import.count".to_owned(),
                self.primitive_imports.len().to_string(),
            ),
            (
                "capability.count".to_owned(),
                self.requested_capabilities.len().to_string(),
            ),
            ("export.count".to_owned(), self.exports.len().to_string()),
            (
                "symbol_export.count".to_owned(),
                self.symbol_exports.len().to_string(),
            ),
        ];
        for (index, (name, version)) in self.dependencies.iter().enumerate() {
            fields.push((
                format!("dependency.{index}"),
                format!("{}|{}", encode_text(name), encode_version(*version)),
            ));
        }
        for (index, import) in self.symbol_imports.iter().enumerate() {
            fields.push((
                format!("symbol_import.{index}"),
                format!(
                    "{}|{}",
                    encode_text(&import.module),
                    encode_text(&import.symbol)
                ),
            ));
        }
        for (index, import) in self.primitive_imports.iter().enumerate() {
            fields.push((
                format!("import.{index}"),
                format!(
                    "{}|{}",
                    encode_text(&import.name),
                    encode_text(import.capability.as_str())
                ),
            ));
        }
        for (index, capability) in self.requested_capabilities.iter().enumerate() {
            fields.push((
                format!("capability.{index}"),
                encode_text(capability.as_str()),
            ));
        }
        for (index, symbol) in self.symbol_exports.iter().enumerate() {
            fields.push((format!("symbol_export.{index}"), encode_text(symbol)));
        }
        for (index, export) in self.exports.iter().enumerate() {
            let prefix = format!("export.{index}.");
            fields.push((format!("{prefix}number"), export.number.to_string()));
            fields.push((format!("{prefix}name"), encode_text(&export.name)));
            fields.push((
                format!("{prefix}definition"),
                encode_text(&export.definition_name),
            ));
            fields.push((
                format!("{prefix}register.count"),
                export.contract.registers.len().to_string(),
            ));
            for (register_index, register) in export.contract.registers.iter().enumerate() {
                let kind = encode_register_kind(&register.kind);
                fields.push((
                    format!("{prefix}register.{register_index}"),
                    format!(
                        "{}|{}|{}",
                        register.register,
                        kind,
                        direction_name(register.direction)
                    ),
                ));
            }
            fields.push((
                format!("{prefix}memory.count"),
                export.contract.logical_memory.len().to_string(),
            ));
            for (memory_index, memory) in export.contract.logical_memory.iter().enumerate() {
                let (register, access, max_bytes) = match memory {
                    LogicalMemoryContract::Read {
                        register,
                        max_bytes,
                    } => (*register, "READ", max_bytes),
                    LogicalMemoryContract::Write {
                        register,
                        max_bytes,
                    } => (*register, "WRITE", max_bytes),
                    LogicalMemoryContract::ReadWrite {
                        register,
                        max_bytes,
                    } => (*register, "READWRITE", max_bytes),
                };
                fields.push((
                    format!("{prefix}memory.{memory_index}"),
                    format!(
                        "{register}|{access}|{}",
                        max_bytes.map_or_else(|| "*".to_owned(), |value| value.to_string())
                    ),
                ));
            }
            fields.push((
                format!("{prefix}pc"),
                export
                    .contract
                    .program_counter
                    .map(direction_name)
                    .unwrap_or("-")
                    .to_owned(),
            ));
            fields.push((
                format!("{prefix}carry"),
                export
                    .contract
                    .carry
                    .map(direction_name)
                    .unwrap_or("-")
                    .to_owned(),
            ));
            fields.push((
                format!("{prefix}blocking"),
                export.contract.may_block.to_string(),
            ));
            fields.push((
                format!("{prefix}reentrant"),
                export.contract.may_reenter.to_string(),
            ));
            fields.push((
                format!("{prefix}error"),
                encode_text(&export.contract.error_transport),
            ));
        }
        let mut output = String::from("TRELLIS-MANIFEST\t1\n");
        for (key, value) in fields {
            output.push_str(&key);
            output.push('\t');
            output.push_str(&value);
            output.push('\n');
        }
        Ok(output)
    }

    /// Parses the deterministic schema emitted by `encode_v1`, rejecting
    /// unknown, duplicate, missing, or malformed fields rather than guessing.
    pub fn decode_v1(input: &str) -> Result<Self, RegistryError> {
        let mut lines = input.lines();
        if lines.next() != Some("TRELLIS-MANIFEST\t1") {
            return Err(RegistryError::InvalidManifest(
                "missing TRELLIS-MANIFEST v1 header".into(),
            ));
        }
        let mut fields = BTreeMap::new();
        for line in lines {
            let (key, value) = line.split_once('\t').ok_or_else(|| {
                RegistryError::InvalidManifest("manifest row must be key<TAB>value".into())
            })?;
            if key.is_empty() || fields.insert(key.to_owned(), value.to_owned()).is_some() {
                return Err(RegistryError::InvalidManifest(format!(
                    "duplicate or empty manifest key {key:?}"
                )));
            }
        }
        let schema = take_field(&mut fields, "schema")?
            .parse::<u16>()
            .map_err(|_| RegistryError::InvalidManifest("invalid schema version".into()))?;
        if schema != 1 {
            return Err(RegistryError::UnsupportedManifestSchema(schema));
        }
        let name = decode_text(&take_field(&mut fields, "name")?)?;
        validate_module_name(&name)?;
        let version = decode_version(&take_field(&mut fields, "version")?)?;
        let language_profile = decode_text(&take_field(&mut fields, "profile.language")?)?;
        let target_profile = decode_text(&take_field(&mut fields, "profile.target")?)?;
        let source_path = decode_text(&take_field(&mut fields, "source_path")?)?;
        let source_hash = decode_text(&take_field(&mut fields, "source_hash")?)?;
        let replacement_policy =
            ReplacementPolicy::parse(&take_field(&mut fields, "replacement")?)?;
        let lifecycle = ModuleLifecycle {
            start: decode_optional(&take_field(&mut fields, "lifecycle.start")?)?,
            quiesce: decode_optional(&take_field(&mut fields, "lifecycle.quiesce")?)?,
            finalise: decode_optional(&take_field(&mut fields, "lifecycle.finalise")?)?,
        };
        let dependency_count = parse_count(&mut fields, "dependency.count")?;
        let symbol_import_count = parse_count(&mut fields, "symbol_import.count")?;
        let import_count = parse_count(&mut fields, "import.count")?;
        let capability_count = parse_count(&mut fields, "capability.count")?;
        let symbol_export_count = parse_count(&mut fields, "symbol_export.count")?;
        let export_count = parse_count(&mut fields, "export.count")?;
        let mut dependencies = Vec::with_capacity(dependency_count);
        for index in 0..dependency_count {
            let value = take_field(&mut fields, &format!("dependency.{index}"))?;
            let (name, version) = value.split_once('|').ok_or_else(|| {
                RegistryError::InvalidManifest("dependency must contain name and version".into())
            })?;
            dependencies.push((decode_text(name)?, decode_version(version)?));
        }
        let mut symbol_imports = Vec::with_capacity(symbol_import_count);
        for index in 0..symbol_import_count {
            let value = take_field(&mut fields, &format!("symbol_import.{index}"))?;
            let (module, symbol) = value.split_once('|').ok_or_else(|| {
                RegistryError::InvalidManifest(
                    "symbol import must contain module and symbol".into(),
                )
            })?;
            symbol_imports.push(ModuleSymbolImport {
                module: decode_text(module)?,
                symbol: decode_text(symbol)?,
            });
        }
        let mut primitive_imports = Vec::with_capacity(import_count);
        for index in 0..import_count {
            let value = take_field(&mut fields, &format!("import.{index}"))?;
            let (name, capability) = value.split_once('|').ok_or_else(|| {
                RegistryError::InvalidManifest(
                    "primitive import must contain name and capability".into(),
                )
            })?;
            primitive_imports.push(PrimitiveImport {
                name: decode_text(name)?,
                capability: CapabilityName::new(decode_text(capability)?)?,
            });
        }
        let mut requested_capabilities = BTreeSet::new();
        for index in 0..capability_count {
            if !requested_capabilities.insert(CapabilityName::new(decode_text(&take_field(
                &mut fields,
                &format!("capability.{index}"),
            )?)?)?) {
                return Err(RegistryError::InvalidManifest(
                    "duplicate requested capability".into(),
                ));
            }
        }
        let mut symbol_exports = Vec::with_capacity(symbol_export_count);
        for index in 0..symbol_export_count {
            symbol_exports.push(decode_text(&take_field(
                &mut fields,
                &format!("symbol_export.{index}"),
            )?)?);
        }
        let mut exports = Vec::with_capacity(export_count);
        for index in 0..export_count {
            let prefix = format!("export.{index}.");
            let number = take_field(&mut fields, &format!("{prefix}number"))?
                .parse::<u32>()
                .map_err(|_| RegistryError::InvalidManifest("invalid SWI number".into()))?;
            let name = decode_text(&take_field(&mut fields, &format!("{prefix}name"))?)?;
            let definition_name =
                decode_text(&take_field(&mut fields, &format!("{prefix}definition"))?)?;
            let register_count = parse_count(&mut fields, &format!("{prefix}register.count"))?;
            let mut registers = Vec::with_capacity(register_count);
            for item in 0..register_count {
                let fields = take_field(&mut fields, &format!("{prefix}register.{item}"))?;
                let mut parts = fields.split('|');
                let register = parse_u8(parts.next(), "register number")?;
                let kind = decode_register_kind(parts.next().ok_or_else(|| {
                    RegistryError::InvalidManifest("missing register type".into())
                })?)?;
                let direction = parse_direction_field(parts.next())?;
                if parts.next().is_some() {
                    return Err(RegistryError::InvalidManifest(
                        "too many register fields".into(),
                    ));
                }
                registers.push(RegisterContract {
                    register,
                    kind,
                    direction,
                });
            }
            let memory_count = parse_count(&mut fields, &format!("{prefix}memory.count"))?;
            let mut logical_memory = Vec::with_capacity(memory_count);
            for item in 0..memory_count {
                let entry = take_field(&mut fields, &format!("{prefix}memory.{item}"))?;
                let mut parts = entry.split('|');
                let register = parse_u8(parts.next(), "memory register")?;
                let access = parts.next().ok_or_else(|| {
                    RegistryError::InvalidManifest("missing memory access".into())
                })?;
                let limit = parts
                    .next()
                    .ok_or_else(|| RegistryError::InvalidManifest("missing memory bound".into()))?;
                let max_bytes = if limit == "*" {
                    None
                } else {
                    Some(limit.parse::<u32>().map_err(|_| {
                        RegistryError::InvalidManifest("invalid memory bound".into())
                    })?)
                };
                if parts.next().is_some() {
                    return Err(RegistryError::InvalidManifest(
                        "too many memory contract fields".into(),
                    ));
                }
                logical_memory.push(match access {
                    "READ" => LogicalMemoryContract::Read {
                        register,
                        max_bytes,
                    },
                    "WRITE" => LogicalMemoryContract::Write {
                        register,
                        max_bytes,
                    },
                    "READWRITE" => LogicalMemoryContract::ReadWrite {
                        register,
                        max_bytes,
                    },
                    _ => {
                        return Err(RegistryError::InvalidManifest(format!(
                            "invalid memory direction {access}"
                        )));
                    }
                });
            }
            let pc = parse_optional_direction(&take_field(&mut fields, &format!("{prefix}pc"))?)?;
            let carry =
                parse_optional_direction(&take_field(&mut fields, &format!("{prefix}carry"))?)?;
            let may_block =
                parse_bool_field(&take_field(&mut fields, &format!("{prefix}blocking"))?)?;
            let may_reenter =
                parse_bool_field(&take_field(&mut fields, &format!("{prefix}reentrant"))?)?;
            let error_transport =
                decode_text(&take_field(&mut fields, &format!("{prefix}error"))?)?;
            exports.push(SwiExport {
                number,
                name,
                definition_name,
                contract: SwiContract {
                    registers,
                    logical_memory,
                    program_counter: pc,
                    carry,
                    may_block,
                    may_reenter,
                    error_transport,
                },
            });
        }
        if let Some(key) = fields.keys().next() {
            return Err(RegistryError::InvalidManifest(format!(
                "unknown manifest field {key}"
            )));
        }
        let manifest = Self {
            schema_version: 1,
            name,
            version,
            language_profile,
            target_profile,
            dependencies,
            symbol_imports,
            primitive_imports,
            requested_capabilities,
            lifecycle,
            replacement_policy,
            symbol_exports,
            exports,
            source_path,
            source_hash,
        };
        validate_manifest_shape(&manifest)?;
        Ok(manifest)
    }
}

impl ReplacementPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::CompatibleImmediate => "compatible-immediate",
            Self::Quiescent => "quiescent",
            Self::Migrating => "migrating",
            Self::RestartRequired => "restart-required",
        }
    }
    fn parse(value: &str) -> Result<Self, RegistryError> {
        match value {
            "compatible-immediate" => Ok(Self::CompatibleImmediate),
            "quiescent" => Ok(Self::Quiescent),
            "migrating" => Ok(Self::Migrating),
            "restart-required" => Ok(Self::RestartRequired),
            _ => Err(RegistryError::InvalidManifest(format!(
                "unknown replacement policy {value}"
            ))),
        }
    }
}

fn encode_text(text: &str) -> String {
    let mut output = String::new();
    for byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'_' | b'-' | b'.' | b'/' | b':' | b' ')
        {
            output.push(char::from(*byte));
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}
fn decode_text(text: &str) -> Result<String, RegistryError> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let digits = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| RegistryError::InvalidManifest("truncated percent escape".into()))?;
            let hex = std::str::from_utf8(digits)
                .map_err(|_| RegistryError::InvalidManifest("invalid percent escape".into()))?;
            decoded
                .push(u8::from_str_radix(hex, 16).map_err(|_| {
                    RegistryError::InvalidManifest("invalid percent escape".into())
                })?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| RegistryError::InvalidManifest("manifest string is not UTF-8".into()))
}
fn encode_optional(value: &Option<String>) -> String {
    value
        .as_deref()
        .map(encode_text)
        .unwrap_or_else(|| "-".into())
}
fn decode_optional(value: &str) -> Result<Option<String>, RegistryError> {
    if value == "-" {
        Ok(None)
    } else {
        Ok(Some(decode_text(value)?))
    }
}
fn encode_version(value: SemanticVersion) -> String {
    format!("{}.{}.{}", value.major, value.minor, value.patch)
}
fn decode_version(value: &str) -> Result<SemanticVersion, RegistryError> {
    let parts = value
        .split('.')
        .map(str::parse::<u16>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RegistryError::InvalidManifest("invalid semantic version".into()))?;
    if parts.len() != 3 {
        return Err(RegistryError::InvalidManifest(
            "semantic version must have three components".into(),
        ));
    }
    Ok(SemanticVersion::new(parts[0], parts[1], parts[2]))
}
fn take_field(fields: &mut BTreeMap<String, String>, key: &str) -> Result<String, RegistryError> {
    fields
        .remove(key)
        .ok_or_else(|| RegistryError::InvalidManifest(format!("missing manifest field {key}")))
}
fn parse_count(fields: &mut BTreeMap<String, String>, key: &str) -> Result<usize, RegistryError> {
    let count = take_field(fields, key)?
        .parse::<usize>()
        .map_err(|_| RegistryError::InvalidManifest(format!("invalid count in {key}")))?;
    if count > 4096 {
        return Err(RegistryError::InvalidManifest(format!(
            "count in {key} exceeds 4096"
        )));
    }
    Ok(count)
}
fn direction_name(value: ArgumentDirection) -> &'static str {
    match value {
        ArgumentDirection::In => "IN",
        ArgumentDirection::Out => "OUT",
        ArgumentDirection::InOut => "INOUT",
    }
}
fn parse_direction_field(value: Option<&str>) -> Result<ArgumentDirection, RegistryError> {
    match value {
        Some("IN") => Ok(ArgumentDirection::In),
        Some("OUT") => Ok(ArgumentDirection::Out),
        Some("INOUT") => Ok(ArgumentDirection::InOut),
        _ => Err(RegistryError::InvalidManifest(
            "invalid argument direction".into(),
        )),
    }
}
fn parse_optional_direction(value: &str) -> Result<Option<ArgumentDirection>, RegistryError> {
    if value == "-" {
        Ok(None)
    } else {
        Ok(Some(parse_direction_field(Some(value))?))
    }
}
fn encode_register_kind(kind: &RegisterKind) -> String {
    match kind {
        RegisterKind::Unsigned { bits } => format!("U{bits}"),
        RegisterKind::Signed { bits } => format!("S{bits}"),
        RegisterKind::LogicalAddress { bits } => format!("A{bits}"),
        RegisterKind::OpaqueHandle { type_name } => format!("H{}", encode_text(type_name)),
    }
}
fn decode_register_kind(value: &str) -> Result<RegisterKind, RegistryError> {
    let number = |prefix: char| {
        value
            .strip_prefix(prefix)
            .and_then(|v| v.parse::<u8>().ok())
    };
    if let Some(bits) = number('U') {
        Ok(RegisterKind::Unsigned { bits })
    } else if let Some(bits) = number('S') {
        Ok(RegisterKind::Signed { bits })
    } else if let Some(bits) = number('A') {
        Ok(RegisterKind::LogicalAddress { bits })
    } else if let Some(name) = value.strip_prefix('H') {
        Ok(RegisterKind::OpaqueHandle {
            type_name: decode_text(name)?,
        })
    } else {
        Err(RegistryError::InvalidManifest(
            "invalid register type".into(),
        ))
    }
}
fn parse_u8(value: Option<&str>, name: &str) -> Result<u8, RegistryError> {
    value
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|value| *value < 16)
        .ok_or_else(|| RegistryError::InvalidManifest(format!("invalid {name}")))
}
fn parse_bool_field(value: &str) -> Result<bool, RegistryError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(RegistryError::InvalidManifest("invalid boolean".into())),
    }
}
fn validate_manifest_shape(manifest: &ModuleManifest) -> Result<(), RegistryError> {
    if manifest.schema_version != 1 {
        return Err(RegistryError::UnsupportedManifestSchema(
            manifest.schema_version,
        ));
    }
    validate_module_name(&manifest.name)?;
    if manifest.source_path.is_empty() || manifest.source_hash.is_empty() {
        return Err(RegistryError::InvalidManifest(
            "source path and source hash are required".into(),
        ));
    }
    if manifest.language_profile.trim().is_empty() {
        return Err(RegistryError::InvalidManifest(
            "language profile is required".into(),
        ));
    }
    if !matches!(manifest.target_profile.as_str(), "HOSTED" | "AGON") {
        return Err(RegistryError::InvalidManifest(format!(
            "unsupported module target profile {}",
            manifest.target_profile
        )));
    }
    if manifest
        .language_profile
        .bytes()
        .any(|byte| byte.is_ascii_control())
    {
        return Err(RegistryError::InvalidManifest(
            "language profile contains a control character".into(),
        ));
    }
    if manifest
        .source_path
        .bytes()
        .any(|byte| byte.is_ascii_control())
        || manifest.source_path.starts_with('/')
        || manifest.source_path.contains('\\')
        || manifest
            .source_path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(RegistryError::InvalidManifest(
            "source path must be a normalized relative package path".into(),
        ));
    }
    if manifest
        .source_hash
        .bytes()
        .any(|byte| byte.is_ascii_control())
    {
        return Err(RegistryError::InvalidManifest(
            "source hash contains a control character".into(),
        ));
    }

    let mut lifecycle_names = BTreeSet::new();
    for hook in [
        manifest.lifecycle.start.as_deref(),
        manifest.lifecycle.quiesce.as_deref(),
        manifest.lifecycle.finalise.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if !is_module_symbol_identifier(hook) || !lifecycle_names.insert(hook.to_ascii_uppercase())
        {
            return Err(RegistryError::InvalidManifest(format!(
                "invalid or reused lifecycle procedure {hook:?}"
            )));
        }
    }

    let mut dependencies = BTreeSet::new();
    for (name, _) in &manifest.dependencies {
        validate_module_name(name)?;
        let canonical = canonical_module_name(name);
        if canonical == canonical_module_name(&manifest.name) || !dependencies.insert(canonical) {
            return Err(RegistryError::InvalidManifest(format!(
                "self or duplicate dependency {name}"
            )));
        }
    }

    let mut symbol_imports = BTreeSet::new();
    for import in &manifest.symbol_imports {
        validate_module_name(&import.module)?;
        if !dependencies.contains(&canonical_module_name(&import.module)) {
            return Err(RegistryError::InvalidManifest(format!(
                "symbol import from {} lacks a declared module dependency",
                import.module
            )));
        }
        validate_symbol_name(&import.symbol)?;
        if !symbol_imports.insert((
            canonical_module_name(&import.module),
            import.symbol.to_ascii_uppercase(),
        )) {
            return Err(RegistryError::InvalidManifest(format!(
                "duplicate imported symbol {}.{}",
                import.module, import.symbol
            )));
        }
    }

    let mut numbers = BTreeSet::new();
    let mut names = BTreeSet::new();
    for export in &manifest.exports {
        if !numbers.insert(export.number) {
            return Err(RegistryError::DuplicateSwiNumber(export.number));
        }
        if export.name.trim().is_empty() || !names.insert(canonical_swi_name(&export.name)) {
            return Err(RegistryError::DuplicateSwiName(export.name.clone()));
        }
        if export.definition_name.is_empty() || export.contract.error_transport.is_empty() {
            return Err(RegistryError::InvalidManifest(format!(
                "SWI {} is missing a definition or error contract",
                export.name
            )));
        }
        let mut registers = BTreeSet::new();
        for register in &export.contract.registers {
            if register.register >= 16 || !registers.insert(register.register) {
                return Err(RegistryError::InvalidManifest(format!(
                    "SWI {} has an invalid or duplicate register contract",
                    export.name
                )));
            }
            match &register.kind {
                RegisterKind::Unsigned { bits } | RegisterKind::Signed { bits }
                    if !(1..=32).contains(bits) =>
                {
                    return Err(RegistryError::InvalidManifest(format!(
                        "SWI {} has invalid register width {bits}",
                        export.name
                    )));
                }
                RegisterKind::LogicalAddress { bits } if *bits != 32 => {
                    return Err(RegistryError::InvalidManifest(format!(
                        "SWI {} logical addresses must use the 32-bit caller address space",
                        export.name
                    )));
                }
                RegisterKind::OpaqueHandle { type_name } => validate_module_name(type_name)?,
                _ => {}
            }
        }
        if export.contract.program_counter.is_some() && registers.contains(&15) {
            return Err(RegistryError::InvalidManifest(format!(
                "SWI {} describes PC both as a register and as a program-counter field",
                export.name
            )));
        }
        for memory in &export.contract.logical_memory {
            let (register, direction) = match memory {
                LogicalMemoryContract::Read { register, .. } => (*register, ArgumentDirection::In),
                LogicalMemoryContract::Write { register, .. } => {
                    (*register, ArgumentDirection::Out)
                }
                LogicalMemoryContract::ReadWrite { register, .. } => {
                    (*register, ArgumentDirection::InOut)
                }
            };
            if register >= 16 {
                return Err(RegistryError::InvalidManifest(format!(
                    "SWI {} has invalid logical-memory register R{register}",
                    export.name
                )));
            }
            let declared_direction = if register == 15 {
                export.contract.program_counter
            } else {
                export
                    .contract
                    .registers
                    .iter()
                    .find(|entry| entry.register == register)
                    .map(|entry| entry.direction)
            };
            let declared_kind = if register == 15 {
                true
            } else {
                export
                    .contract
                    .registers
                    .iter()
                    .find(|entry| entry.register == register)
                    .is_some_and(|entry| {
                        matches!(
                            &entry.kind,
                            RegisterKind::Unsigned { bits: 32 }
                                | RegisterKind::LogicalAddress { bits: 32 }
                        )
                    })
            };
            let direction_ok = match (direction, declared_direction) {
                (ArgumentDirection::In, Some(ArgumentDirection::In | ArgumentDirection::InOut)) => {
                    true
                }
                (
                    ArgumentDirection::Out,
                    Some(ArgumentDirection::Out | ArgumentDirection::InOut),
                ) => true,
                (ArgumentDirection::InOut, Some(ArgumentDirection::InOut)) => true,
                _ => false,
            };
            if !declared_kind || !direction_ok {
                return Err(RegistryError::InvalidManifest(format!(
                    "SWI {} logical-memory register R{register} must be a U32/ADDRESS32 pointer with a compatible direction",
                    export.name
                )));
            }
        }
    }

    let mut symbols = BTreeSet::new();
    for symbol in &manifest.symbol_exports {
        validate_symbol_name(symbol)?;
        if !symbols.insert(symbol.to_ascii_uppercase()) {
            return Err(RegistryError::InvalidManifest(format!(
                "duplicate exported symbol {symbol:?}"
            )));
        }
    }
    let mut primitive_imports = BTreeSet::new();
    for import in &manifest.primitive_imports {
        if import.name.is_empty()
            || !primitive_imports.insert(canonical_primitive_name(&import.name))
        {
            return Err(RegistryError::InvalidManifest(format!(
                "empty or duplicate primitive import {:?}",
                import.name
            )));
        }
        if !manifest.requested_capabilities.contains(&import.capability) {
            return Err(RegistryError::CapabilityNotGranted {
                module: manifest.name.clone(),
                capability: import.capability.as_str().into(),
            });
        }
    }
    Ok(())
}

fn is_module_symbol_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'%'))
}

fn validate_symbol_name(symbol: &str) -> Result<(), RegistryError> {
    let name = symbol.strip_prefix("FN:").unwrap_or(symbol);
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'%'))
    {
        return Err(RegistryError::InvalidManifest(format!(
            "invalid BASIC64 symbol {symbol:?}"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefinitionDescriptor {
    pub id: DefinitionId,
    pub name: String,
    pub module: ModuleId,
    pub source_path: String,
    pub source_hash: String,
    pub language_profile: String,
    pub target_profile: String,
}

pub const RUNTIME_ABI_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvocationBackend {
    Interpreter,
    Jit,
    Aot,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct DependencyFingerprint {
    pub module: String,
    pub version: SemanticVersion,
    pub source_path: String,
    pub source_hash: String,
    pub language_profile: String,
    pub target_profile: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DerivedTargetIdentity {
    pub definition: DefinitionId,
    pub generation: GenerationId,
    pub module_version: SemanticVersion,
    pub source_path: String,
    pub source_hash: String,
    pub dependencies: Vec<DependencyFingerprint>,
    pub language_profile: String,
    pub target_profile: String,
    pub runtime_abi: u32,
}

/// One version-guarded invocation decision shared by interpreted and future
/// derived targets. System modules currently produce only the interpreter
/// variant; JIT/AOT requests fail explicitly until their module ABI is ready.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvocationPlan {
    pub backend: InvocationBackend,
    pub identity: DerivedTargetIdentity,
}

/// Derived code is always optional and addressed by source, dependency,
/// generation, and runtime identity. The interpreter remains the fallback.
#[derive(Debug, Default)]
pub struct DerivedTargetCache<T> {
    targets: HashMap<(DefinitionId, GenerationId), (DerivedTargetIdentity, Arc<T>)>,
}

impl<T> DerivedTargetCache<T> {
    pub fn insert(&mut self, identity: DerivedTargetIdentity, target: Arc<T>) {
        self.targets.insert(
            (identity.definition, identity.generation),
            (identity, target),
        );
    }

    pub fn get(&self, identity: &DerivedTargetIdentity) -> Option<Arc<T>> {
        let (stored_identity, target) = self
            .targets
            .get(&(identity.definition, identity.generation))?;
        (stored_identity == identity && identity.runtime_abi == RUNTIME_ABI_VERSION)
            .then(|| Arc::clone(target))
    }

    /// Drop reusable targets that depend on a replaced source module. Any
    /// already-entered invocation may retain its own `Arc` until it returns.
    pub fn invalidate_dependency(&mut self, module: &str, current: &DependencyFingerprint) {
        self.targets.retain(|_, (identity, _)| {
            !identity.dependencies.iter().any(|dependency| {
                dependency.module.eq_ignore_ascii_case(module) && dependency != current
            })
        });
    }

    pub fn invalidate_definition(&mut self, definition: DefinitionId) {
        self.targets
            .retain(|(target_definition, _), _| *target_definition != definition);
    }

    pub fn len(&self) -> usize {
        self.targets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleState {
    Validated,
    Linked,
    Published,
    Starting,
    Active,
    Quiescing,
    Retired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RegistryError {
    InvalidCapabilityName(String),
    InvalidModuleName(String),
    DuplicateModule(String),
    UnknownModule(ModuleId),
    UnknownModuleName(String),
    InvalidState {
        module: String,
        state: ModuleState,
    },
    MissingPrimitive(String),
    DuplicatePrimitive(String),
    CapabilityNotGranted {
        module: String,
        capability: String,
    },
    CapabilityMismatch {
        primitive: String,
        expected: String,
        declared: String,
    },
    MissingDefinition {
        module: String,
        definition: String,
    },
    DuplicateSwiNumber(u32),
    DuplicateSwiName(String),
    AlreadyPublishedNumber(u32),
    AlreadyPublishedName(String),
    MissingDependency {
        module: String,
        dependency: String,
    },
    MissingSymbol {
        module: String,
        dependency: String,
        symbol: String,
    },
    ModuleBusy {
        module: String,
        active_calls: usize,
    },
    PrimitiveNotImported {
        module: String,
        primitive: String,
    },
    MissingResourceRight {
        primitive: String,
        argument_register: u8,
    },
    IncompatibleContract,
    FailedStart(String),
    UnsupportedInvocationBackend(InvocationBackend),
    UnsupportedManifestSchema(u16),
    InvalidManifest(String),
    ReplacementRequiresPolicy(ReplacementPolicy),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapabilityName(name) => {
                write!(formatter, "invalid capability name {name:?}")
            }
            Self::InvalidModuleName(name) => write!(formatter, "invalid module name {name:?}"),
            Self::DuplicateModule(name) => write!(formatter, "module {name} is already loaded"),
            Self::UnknownModule(id) => write!(formatter, "unknown module {id}"),
            Self::UnknownModuleName(name) => write!(formatter, "unknown module {name}"),
            Self::InvalidState { module, state } => {
                write!(formatter, "module {module} is in state {state:?}")
            }
            Self::MissingPrimitive(name) => write!(formatter, "primitive {name} is not registered"),
            Self::DuplicatePrimitive(name) => {
                write!(formatter, "primitive {name} is already registered")
            }
            Self::CapabilityNotGranted { module, capability } => write!(
                formatter,
                "module {module} was not granted capability {capability}"
            ),
            Self::CapabilityMismatch {
                primitive,
                expected,
                declared,
            } => write!(
                formatter,
                "primitive {primitive} requires {expected}, but import declares {declared}"
            ),
            Self::MissingDefinition { module, definition } => {
                write!(formatter, "module {module} does not define {definition}")
            }
            Self::DuplicateSwiNumber(number) => write!(
                formatter,
                "SWI number &{number:X} is exported more than once"
            ),
            Self::DuplicateSwiName(name) => {
                write!(formatter, "SWI name {name} is exported more than once")
            }
            Self::AlreadyPublishedNumber(number) => {
                write!(formatter, "SWI number &{number:X} already has an owner")
            }
            Self::AlreadyPublishedName(name) => {
                write!(formatter, "SWI name {name} already has an owner")
            }
            Self::MissingDependency { module, dependency } => write!(
                formatter,
                "module {module} requires unavailable module {dependency}"
            ),
            Self::MissingSymbol {
                module,
                dependency,
                symbol,
            } => write!(
                formatter,
                "module {module} imports {dependency}.{symbol}, but that symbol is not exported"
            ),
            Self::ModuleBusy {
                module,
                active_calls,
            } => write!(
                formatter,
                "module {module} still has {active_calls} active call(s)"
            ),
            Self::PrimitiveNotImported { module, primitive } => write!(
                formatter,
                "module {module} did not import primitive {primitive}"
            ),
            Self::MissingResourceRight {
                primitive,
                argument_register,
            } => write!(
                formatter,
                "primitive {primitive} has no authority requirement for handle argument R{argument_register}"
            ),
            Self::IncompatibleContract => {
                formatter.write_str("replacement has an incompatible public SWI contract")
            }
            Self::FailedStart(message) => write!(formatter, "module start failed: {message}"),
            Self::UnsupportedInvocationBackend(backend) => write!(
                formatter,
                "BASIC64 system module invocation backend {backend:?} is not implemented; use the interpreter"
            ),
            Self::UnsupportedManifestSchema(version) => {
                write!(formatter, "unsupported module manifest schema {version}")
            }
            Self::InvalidManifest(message) => {
                write!(formatter, "invalid module manifest: {message}")
            }
            Self::ReplacementRequiresPolicy(policy) => write!(
                formatter,
                "live replacement requires compatible-immediate policy, found {policy:?}"
            ),
        }
    }
}

impl std::error::Error for RegistryError {}

#[derive(Clone, Debug)]
pub struct PrimitiveRegistry {
    allocator: Arc<IdentityAllocator>,
    by_name: BTreeMap<String, PrimitiveDescriptor>,
}

impl PrimitiveRegistry {
    pub fn new(allocator: Arc<IdentityAllocator>) -> Self {
        Self {
            allocator,
            by_name: BTreeMap::new(),
        }
    }

    pub fn register(
        &mut self,
        name: impl Into<String>,
        capability: CapabilityName,
        arguments: Vec<RegisterKind>,
        results: Vec<RegisterKind>,
        blocking: bool,
        reentrant: bool,
        memory_rules: Vec<LogicalMemoryContract>,
        failure_contract: impl Into<String>,
    ) -> Result<PrimitiveId, RegistryError> {
        let name: String = name.into();
        let name = canonical_primitive_name(&name);
        if self.by_name.contains_key(&name) {
            return Err(RegistryError::DuplicatePrimitive(name));
        }
        let id = self.allocator.primitive_id();
        self.by_name.insert(
            name.clone(),
            PrimitiveDescriptor {
                id,
                name,
                capability,
                arguments,
                results,
                blocking,
                reentrant,
                memory_rules,
                resource_requirements: Vec::new(),
                failure_contract: failure_contract.into(),
            },
        );
        Ok(id)
    }

    pub fn get(&self, name: &str) -> Option<&PrimitiveDescriptor> {
        self.by_name.get(&canonical_primitive_name(name))
    }

    pub fn require_resource_right(
        &mut self,
        primitive_name: &str,
        argument_register: u8,
        right: ResourceRight,
    ) -> Result<(), RegistryError> {
        let name = canonical_primitive_name(primitive_name);
        let primitive = self
            .by_name
            .get_mut(&name)
            .ok_or_else(|| RegistryError::MissingPrimitive(name.clone()))?;
        let Some(RegisterKind::OpaqueHandle { type_name }) =
            primitive.arguments.get(usize::from(argument_register))
        else {
            return Err(RegistryError::InvalidManifest(format!(
                "primitive {name} resource rights must target an opaque-handle argument"
            )));
        };
        if primitive
            .resource_requirements
            .iter()
            .any(|requirement| requirement.argument_register == argument_register)
        {
            return Err(RegistryError::InvalidManifest(format!(
                "primitive {name} declares handle rights for R{argument_register} more than once"
            )));
        }
        primitive
            .resource_requirements
            .push(ResourceHandleRequirement {
                argument_register,
                type_name: type_name.clone(),
                right,
            });
        Ok(())
    }
}

struct DefinitionGeneration<T> {
    id: GenerationId,
    number: u64,
    value: Arc<T>,
    active_calls: AtomicUsize,
    retired: AtomicBool,
}

impl<T> fmt::Debug for DefinitionGeneration<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DefinitionGeneration")
            .field("id", &self.id)
            .field("number", &self.number)
            .field("active_calls", &self.active_calls.load(Ordering::Acquire))
            .field("retired", &self.retired.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

struct CellState<T> {
    current: Arc<DefinitionGeneration<T>>,
    retired: Vec<Weak<DefinitionGeneration<T>>>,
}

/// Stable call target whose current executable definition can be replaced.
/// `InvocationLease` retains a retired generation until its caller exits.
pub struct VersionedDefinitionCell<T> {
    id: SwiEntryId,
    contract: SwiContract,
    allocator: Arc<IdentityAllocator>,
    next_number: AtomicU64,
    state: Mutex<CellState<T>>,
}

impl<T> fmt::Debug for VersionedDefinitionCell<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VersionedDefinitionCell")
            .field("id", &self.id)
            .field("contract", &self.contract)
            .field("current_generation", &self.current_generation_id())
            .field("retired_generations", &self.retired_generation_count())
            .finish()
    }
}

impl<T> VersionedDefinitionCell<T> {
    pub fn new(
        id: SwiEntryId,
        contract: SwiContract,
        initial: T,
        allocator: Arc<IdentityAllocator>,
    ) -> Self {
        let generation = Arc::new(DefinitionGeneration {
            id: allocator.generation_id(),
            number: 1,
            value: Arc::new(initial),
            active_calls: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
        });
        Self {
            id,
            contract,
            allocator,
            next_number: AtomicU64::new(2),
            state: Mutex::new(CellState {
                current: generation,
                retired: Vec::new(),
            }),
        }
    }

    pub fn id(&self) -> SwiEntryId {
        self.id
    }

    pub fn contract(&self) -> &SwiContract {
        &self.contract
    }

    pub fn current_value(&self) -> Arc<T> {
        Arc::clone(
            &self
                .state
                .lock()
                .expect("definition cell poisoned")
                .current
                .value,
        )
    }

    pub fn acquire(&self) -> InvocationLease<T> {
        let state = self.state.lock().expect("definition cell poisoned");
        let generation = Arc::clone(&state.current);
        generation.active_calls.fetch_add(1, Ordering::AcqRel);
        InvocationLease { generation }
    }

    pub fn replace(
        &self,
        contract: &SwiContract,
        replacement: T,
    ) -> Result<GenerationId, RegistryError> {
        if contract != &self.contract {
            return Err(RegistryError::IncompatibleContract);
        }
        let mut state = self.state.lock().expect("definition cell poisoned");
        let number = self.next_number.fetch_add(1, Ordering::Relaxed);
        let next = Arc::new(DefinitionGeneration {
            id: self.allocator.generation_id(),
            number,
            value: Arc::new(replacement),
            active_calls: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
        });
        let old = std::mem::replace(&mut state.current, next);
        old.retired.store(true, Ordering::Release);
        state.retired.push(Arc::downgrade(&old));
        let id = state.current.id;
        Self::prune_retired(&mut state.retired);
        Ok(id)
    }

    pub fn current_generation_id(&self) -> GenerationId {
        self.state
            .lock()
            .expect("definition cell poisoned")
            .current
            .id
    }

    pub fn current_generation_number(&self) -> u64 {
        self.state
            .lock()
            .expect("definition cell poisoned")
            .current
            .number
    }

    pub fn retired_generation_count(&self) -> usize {
        let mut state = self.state.lock().expect("definition cell poisoned");
        Self::prune_retired(&mut state.retired);
        state.retired.len()
    }

    pub fn active_call_count(&self) -> usize {
        let mut state = self.state.lock().expect("definition cell poisoned");
        Self::prune_retired(&mut state.retired);
        state.current.active_calls.load(Ordering::Acquire)
            + state
                .retired
                .iter()
                .filter_map(Weak::upgrade)
                .map(|generation| generation.active_calls.load(Ordering::Acquire))
                .sum::<usize>()
    }

    pub fn collect_retired(&self) {
        let mut state = self.state.lock().expect("definition cell poisoned");
        Self::prune_retired(&mut state.retired);
    }

    fn prune_retired(retired: &mut Vec<Weak<DefinitionGeneration<T>>>) {
        retired.retain(|generation| {
            generation
                .upgrade()
                .is_some_and(|generation| generation.active_calls.load(Ordering::Acquire) != 0)
        });
    }
}

pub struct InvocationLease<T> {
    generation: Arc<DefinitionGeneration<T>>,
}

impl<T> InvocationLease<T> {
    pub fn generation_id(&self) -> GenerationId {
        self.generation.id
    }
    pub fn generation_number(&self) -> u64 {
        self.generation.number
    }
    pub fn active_calls(&self) -> usize {
        self.generation.active_calls.load(Ordering::Acquire)
    }
    pub fn retired(&self) -> bool {
        self.generation.retired.load(Ordering::Acquire)
    }
}

impl<T> Deref for InvocationLease<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.generation.value
    }
}

impl<T> Drop for InvocationLease<T> {
    fn drop(&mut self) {
        self.generation.active_calls.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug)]
pub struct ModuleRecord {
    pub id: ModuleId,
    pub instance_id: ModuleInstanceId,
    pub manifest: ModuleManifest,
    pub state: ModuleState,
    pub definitions: BTreeMap<String, DefinitionDescriptor>,
    pub resolved_primitives: BTreeMap<String, PrimitiveId>,
    pub granted_capabilities: BTreeSet<CapabilityName>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SwiOwnership {
    pub number: u32,
    pub name: String,
    pub module: ModuleId,
    pub module_name: String,
    pub definition: DefinitionId,
    pub definition_name: String,
    pub generation: GenerationId,
    pub generation_number: u64,
    pub source_path: String,
    pub contract: SwiContract,
}

struct PublishedSwi {
    number: u32,
    name: String,
    module: ModuleId,
    module_name: String,
    definition: DefinitionDescriptor,
    contract: SwiContract,
    cell: Arc<VersionedDefinitionCell<DefinitionDescriptor>>,
}

impl fmt::Debug for PublishedSwi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedSwi")
            .field("number", &self.number)
            .field("name", &self.name)
            .field("module", &self.module)
            .field("definition", &self.definition)
            .field("cell", &self.cell)
            .finish()
    }
}

/// Runtime module table with separate validation, linking, publication, and
/// start transitions. It exposes read-only inspection records to diagnostics.
pub struct ModuleRegistry {
    allocator: Arc<IdentityAllocator>,
    pub primitives: PrimitiveRegistry,
    modules: BTreeMap<ModuleId, ModuleRecord>,
    by_name: BTreeMap<String, ModuleId>,
    by_swi_number: HashMap<u32, PublishedSwi>,
    by_swi_name: HashMap<String, u32>,
}

impl fmt::Debug for ModuleRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ModuleRegistry")
            .field("module_count", &self.modules.len())
            .field("published_swi_count", &self.by_swi_number.len())
            .finish()
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleRegistry {
    pub fn new() -> Self {
        let allocator = Arc::new(IdentityAllocator::default());
        Self {
            primitives: PrimitiveRegistry::new(Arc::clone(&allocator)),
            allocator,
            modules: BTreeMap::new(),
            by_name: BTreeMap::new(),
            by_swi_number: HashMap::new(),
            by_swi_name: HashMap::new(),
        }
    }

    pub fn allocator(&self) -> Arc<IdentityAllocator> {
        Arc::clone(&self.allocator)
    }

    pub(crate) fn issue_module_management_authority(&self) -> ModuleManagementAuthority {
        ModuleManagementAuthority(NEXT_MODULE_AUTHORITY.fetch_add(1, Ordering::Relaxed))
    }

    pub(crate) fn accepts_module_management_authority(
        &self,
        expected: &ModuleManagementAuthority,
        supplied: &ModuleManagementAuthority,
    ) -> bool {
        expected == supplied
    }

    pub fn stage_module(
        &mut self,
        manifest: ModuleManifest,
        definitions: impl IntoIterator<Item = (String, DefinitionDescriptor)>,
    ) -> Result<ModuleId, RegistryError> {
        validate_manifest_shape(&manifest)?;
        let canonical_name = canonical_module_name(&manifest.name);
        if self.by_name.contains_key(&canonical_name) {
            return Err(RegistryError::DuplicateModule(manifest.name));
        }
        let definitions = definitions
            .into_iter()
            .map(|(name, definition)| (name, definition))
            .collect::<BTreeMap<_, _>>();
        let mut definition_ids = BTreeSet::new();
        for (key, definition) in &definitions {
            let expected_name = key
                .strip_prefix("FN:")
                .map(|name| format!("FN {name}"))
                .unwrap_or_else(|| key.clone());
            if !definition_ids.insert(definition.id)
                || !definition.name.eq_ignore_ascii_case(&expected_name)
                || definition.source_path != manifest.source_path
                || definition.source_hash != manifest.source_hash
                || definition.language_profile != manifest.language_profile
                || !definition
                    .target_profile
                    .eq_ignore_ascii_case(&manifest.target_profile)
            {
                return Err(RegistryError::InvalidManifest(format!(
                    "definition {key} identity does not match module source/profile metadata"
                )));
            }
        }
        for export in &manifest.exports {
            if !definitions.contains_key(&export.definition_name) {
                return Err(RegistryError::MissingDefinition {
                    module: manifest.name.clone(),
                    definition: export.definition_name.clone(),
                });
            }
        }
        for symbol in &manifest.symbol_exports {
            if !definitions.contains_key(symbol) {
                return Err(RegistryError::MissingDefinition {
                    module: manifest.name.clone(),
                    definition: symbol.clone(),
                });
            }
        }
        let mut numbers = BTreeSet::new();
        let mut names = BTreeSet::new();
        for export in &manifest.exports {
            if !numbers.insert(export.number) {
                return Err(RegistryError::DuplicateSwiNumber(export.number));
            }
            let name = canonical_swi_name(&export.name);
            if !names.insert(name.clone()) {
                return Err(RegistryError::DuplicateSwiName(name));
            }
        }
        let id = self.allocator.module_id();
        let instance_id = self.allocator.module_instance_id();
        let definitions = definitions
            .into_iter()
            .map(|(name, mut definition)| {
                definition.module = id;
                (name, definition)
            })
            .collect();
        let record = ModuleRecord {
            id,
            instance_id,
            manifest: manifest.clone(),
            state: ModuleState::Validated,
            definitions,
            resolved_primitives: BTreeMap::new(),
            granted_capabilities: BTreeSet::new(),
        };
        self.by_name.insert(canonical_name, id);
        self.modules.insert(id, record);
        Ok(id)
    }

    pub fn link_module(
        &mut self,
        id: ModuleId,
        granted: BTreeSet<CapabilityName>,
    ) -> Result<(), RegistryError> {
        let record = self
            .modules
            .get(&id)
            .ok_or(RegistryError::UnknownModule(id))?;
        if record.state != ModuleState::Validated {
            return Err(RegistryError::InvalidState {
                module: record.manifest.name.clone(),
                state: record.state,
            });
        }
        for (name, version) in &record.manifest.dependencies {
            let dependency_id =
                self.by_name
                    .get(&canonical_module_name(name))
                    .ok_or_else(|| RegistryError::MissingDependency {
                        module: record.manifest.name.clone(),
                        dependency: name.clone(),
                    })?;
            let dependency = self
                .modules
                .get(dependency_id)
                .expect("name table points to module");
            if dependency.manifest.version < *version || dependency.state == ModuleState::Retired {
                return Err(RegistryError::MissingDependency {
                    module: record.manifest.name.clone(),
                    dependency: name.clone(),
                });
            }
        }
        for import in &record.manifest.symbol_imports {
            let dependency_id = self
                .by_name
                .get(&canonical_module_name(&import.module))
                .ok_or_else(|| RegistryError::MissingDependency {
                    module: record.manifest.name.clone(),
                    dependency: import.module.clone(),
                })?;
            let dependency = self
                .modules
                .get(dependency_id)
                .expect("name table points to module");
            if !dependency
                .manifest
                .symbol_exports
                .iter()
                .any(|symbol| symbol.eq_ignore_ascii_case(&import.symbol))
            {
                return Err(RegistryError::MissingSymbol {
                    module: record.manifest.name.clone(),
                    dependency: import.module.clone(),
                    symbol: import.symbol.clone(),
                });
            }
        }
        for capability in &record.manifest.requested_capabilities {
            if !granted.contains(capability) {
                return Err(RegistryError::CapabilityNotGranted {
                    module: record.manifest.name.clone(),
                    capability: capability.as_str().into(),
                });
            }
        }
        let mut resolved = BTreeMap::new();
        for import in &record.manifest.primitive_imports {
            let primitive = self
                .primitives
                .get(&import.name)
                .ok_or_else(|| RegistryError::MissingPrimitive(import.name.clone()))?;
            for (index, kind) in primitive.arguments.iter().enumerate() {
                if let RegisterKind::OpaqueHandle { type_name } = kind {
                    let index = u8::try_from(index).unwrap_or(u8::MAX);
                    if !primitive.resource_requirements.iter().any(|requirement| {
                        requirement.argument_register == index
                            && requirement.type_name.eq_ignore_ascii_case(type_name)
                    }) {
                        return Err(RegistryError::MissingResourceRight {
                            primitive: primitive.name.clone(),
                            argument_register: index,
                        });
                    }
                }
            }
            if primitive.capability != import.capability {
                return Err(RegistryError::CapabilityMismatch {
                    primitive: import.name.clone(),
                    expected: primitive.capability.as_str().into(),
                    declared: import.capability.as_str().into(),
                });
            }
            if !granted.contains(&primitive.capability) {
                return Err(RegistryError::CapabilityNotGranted {
                    module: record.manifest.name.clone(),
                    capability: primitive.capability.as_str().into(),
                });
            }
            resolved.insert(canonical_primitive_name(&import.name), primitive.id);
        }
        let record = self
            .modules
            .get_mut(&id)
            .expect("module was validated above");
        record.resolved_primitives = resolved;
        record.granted_capabilities = granted;
        record.state = ModuleState::Linked;
        Ok(())
    }

    /// Publishes all requested modules as one transaction. No SWI table entry
    /// changes unless every conflict and lifecycle precondition has passed.
    pub fn publish_modules(&mut self, ids: &[ModuleId]) -> Result<(), RegistryError> {
        let mut numbers = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut pending = Vec::new();
        for id in ids {
            let record = self
                .modules
                .get(id)
                .ok_or(RegistryError::UnknownModule(*id))?;
            if record.state != ModuleState::Linked {
                return Err(RegistryError::InvalidState {
                    module: record.manifest.name.clone(),
                    state: record.state,
                });
            }
            for export in &record.manifest.exports {
                let name = canonical_swi_name(&export.name);
                if self.by_swi_number.contains_key(&export.number) {
                    return Err(RegistryError::AlreadyPublishedNumber(export.number));
                }
                if self.by_swi_name.contains_key(&name) {
                    return Err(RegistryError::AlreadyPublishedName(name));
                }
                if !numbers.insert(export.number) {
                    return Err(RegistryError::DuplicateSwiNumber(export.number));
                }
                if !names.insert(name.clone()) {
                    return Err(RegistryError::DuplicateSwiName(name));
                }
                let definition = record
                    .definitions
                    .get(&export.definition_name)
                    .expect("stage_module validated definitions")
                    .clone();
                let cell = Arc::new(VersionedDefinitionCell::new(
                    self.allocator.swi_entry_id(),
                    export.contract.clone(),
                    definition.clone(),
                    Arc::clone(&self.allocator),
                ));
                pending.push((
                    record.manifest.name.clone(),
                    *id,
                    export.clone(),
                    definition,
                    cell,
                ));
            }
        }
        for (module_name, module_id, export, definition, cell) in pending {
            let name = canonical_swi_name(&export.name);
            self.by_swi_name.insert(name, export.number);
            self.by_swi_number.insert(
                export.number,
                PublishedSwi {
                    number: export.number,
                    name: export.name,
                    module: module_id,
                    module_name,
                    definition,
                    contract: export.contract,
                    cell,
                },
            );
        }
        for id in ids {
            self.modules
                .get_mut(id)
                .expect("preflight checked module")
                .state = ModuleState::Published;
        }
        Ok(())
    }

    pub fn begin_module_start(&mut self, id: ModuleId) -> Result<(), RegistryError> {
        let record = self
            .modules
            .get_mut(&id)
            .ok_or(RegistryError::UnknownModule(id))?;
        if record.state != ModuleState::Published {
            return Err(RegistryError::InvalidState {
                module: record.manifest.name.clone(),
                state: record.state,
            });
        }
        record.state = ModuleState::Starting;
        Ok(())
    }

    pub fn complete_module_start(&mut self, id: ModuleId) -> Result<(), RegistryError> {
        let record = self
            .modules
            .get_mut(&id)
            .ok_or(RegistryError::UnknownModule(id))?;
        if record.state != ModuleState::Starting {
            return Err(RegistryError::InvalidState {
                module: record.manifest.name.clone(),
                state: record.state,
            });
        }
        record.state = ModuleState::Active;
        Ok(())
    }

    /// Rolls back a failed start as a unit: every export belonging to this
    /// module is removed before it can be observed by a caller. The linked
    /// module may be published again after the start fault has been corrected.
    pub fn fail_module_start(
        &mut self,
        id: ModuleId,
        message: impl Into<String>,
    ) -> Result<(), RegistryError> {
        let record = self
            .modules
            .get(&id)
            .ok_or(RegistryError::UnknownModule(id))?;
        if record.state != ModuleState::Starting {
            return Err(RegistryError::InvalidState {
                module: record.manifest.name.clone(),
                state: record.state,
            });
        }
        self.remove_module_exports(id);
        self.modules
            .get_mut(&id)
            .expect("module checked above")
            .state = ModuleState::Linked;
        let _ = message.into();
        Ok(())
    }

    pub fn start_module(
        &mut self,
        id: ModuleId,
        start_succeeds: bool,
    ) -> Result<(), RegistryError> {
        self.begin_module_start(id)?;
        if !start_succeeds {
            let name = self
                .modules
                .get(&id)
                .expect("module entered starting")
                .manifest
                .name
                .clone();
            self.fail_module_start(id, name.clone())?;
            return Err(RegistryError::FailedStart(name));
        }
        self.complete_module_start(id)
    }

    fn remove_module_exports(&mut self, id: ModuleId) {
        let numbers = self
            .by_swi_number
            .iter()
            .filter_map(|(number, published)| (published.module == id).then_some(*number))
            .collect::<Vec<_>>();
        for number in numbers {
            if let Some(published) = self.by_swi_number.remove(&number) {
                self.by_swi_name
                    .remove(&canonical_swi_name(&published.name));
            }
        }
    }

    /// Stops admitting new SWI calls while allowing already acquired leases to
    /// finish against their retained definition generation.
    pub fn quiesce_module(&mut self, id: ModuleId) -> Result<(), RegistryError> {
        let record = self
            .modules
            .get_mut(&id)
            .ok_or(RegistryError::UnknownModule(id))?;
        if record.state != ModuleState::Active {
            return Err(RegistryError::InvalidState {
                module: record.manifest.name.clone(),
                state: record.state,
            });
        }
        record.state = ModuleState::Quiescing;
        Ok(())
    }

    /// Reopens admission after a failed quiesce hook. The caller is
    /// responsible for rolling back that hook's workspace transaction first.
    pub fn rollback_module_quiesce(&mut self, id: ModuleId) -> Result<(), RegistryError> {
        let record = self
            .modules
            .get_mut(&id)
            .ok_or(RegistryError::UnknownModule(id))?;
        if record.state != ModuleState::Quiescing {
            return Err(RegistryError::InvalidState {
                module: record.manifest.name.clone(),
                state: record.state,
            });
        }
        record.state = ModuleState::Active;
        Ok(())
    }

    /// Removes a quiesced module's public entries after every active invocation
    /// has released its lease. Retained Rust references are not a guest-visible
    /// concept in this prototype and are therefore not tracked separately.
    pub fn retire_module(&mut self, id: ModuleId) -> Result<(), RegistryError> {
        let record = self
            .modules
            .get(&id)
            .ok_or(RegistryError::UnknownModule(id))?;
        if record.state != ModuleState::Quiescing {
            return Err(RegistryError::InvalidState {
                module: record.manifest.name.clone(),
                state: record.state,
            });
        }
        let module_name = record.manifest.name.clone();
        let active_calls = self
            .by_swi_number
            .values()
            .filter(|published| published.module == id)
            .map(|published| published.cell.active_call_count())
            .sum::<usize>();
        if active_calls != 0 {
            return Err(RegistryError::ModuleBusy {
                module: module_name,
                active_calls,
            });
        }

        let numbers = self
            .by_swi_number
            .iter()
            .filter_map(|(number, published)| (published.module == id).then_some(*number))
            .collect::<Vec<_>>();
        for number in numbers {
            if let Some(published) = self.by_swi_number.remove(&number) {
                self.by_swi_name
                    .remove(&canonical_swi_name(&published.name));
            }
        }
        self.modules
            .get_mut(&id)
            .expect("module was checked above")
            .state = ModuleState::Retired;
        Ok(())
    }

    pub fn module_active_call_count(&self, id: ModuleId) -> Result<usize, RegistryError> {
        self.modules
            .get(&id)
            .ok_or(RegistryError::UnknownModule(id))?;
        Ok(self
            .by_swi_number
            .values()
            .filter(|published| published.module == id)
            .map(|published| published.cell.active_call_count())
            .sum())
    }

    pub fn module(&self, id: ModuleId) -> Option<&ModuleRecord> {
        self.modules.get(&id)
    }

    pub fn module_named(&self, name: &str) -> Option<&ModuleRecord> {
        self.by_name
            .get(&canonical_module_name(name))
            .and_then(|id| self.modules.get(id))
    }

    pub fn module_state(&self, id: ModuleId) -> Option<ModuleState> {
        self.module(id).map(|module| module.state)
    }

    pub fn acquire_swi(
        &self,
        number: u32,
    ) -> Option<(SwiOwnership, InvocationLease<DefinitionDescriptor>)> {
        let published = self.by_swi_number.get(&number)?;
        if self.modules.get(&published.module)?.state != ModuleState::Active {
            return None;
        }
        let lease = published.cell.acquire();
        let ownership = SwiOwnership {
            number: published.number,
            name: published.name.clone(),
            module: published.module,
            module_name: published.module_name.clone(),
            definition: lease.id,
            definition_name: lease.name.clone(),
            generation: lease.generation_id(),
            generation_number: lease.generation_number(),
            source_path: lease.source_path.clone(),
            contract: published.contract.clone(),
        };
        Some((ownership, lease))
    }

    pub fn swi_number(&self, name: &str) -> Option<u32> {
        self.by_swi_name.get(&canonical_swi_name(name)).copied()
    }

    pub(crate) fn replace_swi_definition(
        &mut self,
        number: u32,
        contract: &SwiContract,
        mut replacement: DefinitionDescriptor,
    ) -> Result<GenerationId, RegistryError> {
        let published = self
            .by_swi_number
            .get(&number)
            .ok_or(RegistryError::UnknownModuleName(format!("SWI &{number:X}")))?;
        replacement.module = published.module;
        let module_id = published.module;
        let policy = self
            .modules
            .get(&module_id)
            .expect("published export has a module")
            .manifest
            .replacement_policy;
        if policy != ReplacementPolicy::CompatibleImmediate {
            return Err(RegistryError::ReplacementRequiresPolicy(policy));
        }
        let source_path = replacement.source_path.clone();
        let source_hash = replacement.source_hash.clone();
        let generation = published.cell.replace(contract, replacement)?;
        let manifest = &mut self
            .modules
            .get_mut(&module_id)
            .expect("published export has a module")
            .manifest;
        manifest.source_path = source_path;
        manifest.source_hash = source_hash;
        Ok(generation)
    }

    pub fn current_swi_definition(&self, number: u32) -> Option<DefinitionDescriptor> {
        let published = self.by_swi_number.get(&number)?;
        Some((*published.cell.current_value()).clone())
    }

    pub fn derived_target_identity(&self, number: u32) -> Option<DerivedTargetIdentity> {
        let published = self.by_swi_number.get(&number)?;
        let definition = published.cell.current_value();
        let module = self.modules.get(&published.module)?;
        let mut seen = BTreeSet::from([published.module]);
        let mut dependencies = Vec::new();
        self.collect_dependency_fingerprints(published.module, &mut seen, &mut dependencies)?;
        dependencies.sort();
        Some(DerivedTargetIdentity {
            definition: definition.id,
            generation: published.cell.current_generation_id(),
            module_version: module.manifest.version,
            source_path: definition.source_path.clone(),
            source_hash: definition.source_hash.clone(),
            dependencies,
            language_profile: definition.language_profile.clone(),
            target_profile: definition.target_profile.clone(),
            runtime_abi: RUNTIME_ABI_VERSION,
        })
    }

    fn collect_dependency_fingerprints(
        &self,
        module_id: ModuleId,
        seen: &mut BTreeSet<ModuleId>,
        output: &mut Vec<DependencyFingerprint>,
    ) -> Option<()> {
        let module = self.modules.get(&module_id)?;
        for (name, _) in &module.manifest.dependencies {
            let dependency_id = *self.by_name.get(&canonical_module_name(name))?;
            if !seen.insert(dependency_id) {
                continue;
            }
            let dependency = self.modules.get(&dependency_id)?;
            output.push(DependencyFingerprint {
                module: dependency.manifest.name.clone(),
                version: dependency.manifest.version,
                source_path: dependency.manifest.source_path.clone(),
                source_hash: dependency.manifest.source_hash.clone(),
                language_profile: dependency.manifest.language_profile.clone(),
                target_profile: dependency.manifest.target_profile.clone(),
            });
            self.collect_dependency_fingerprints(dependency_id, seen, output)?;
        }
        Some(())
    }

    /// Selects the backend for one current versioned SWI definition. Every
    /// backend receives the same source/dependency/generation identity; until
    /// module JIT/AOT targets are integrated, only the reference interpreter
    /// can be selected.
    pub fn invocation_plan(
        &self,
        number: u32,
        backend: InvocationBackend,
    ) -> Result<InvocationPlan, RegistryError> {
        let identity = self
            .derived_target_identity(number)
            .ok_or_else(|| RegistryError::UnknownModuleName(format!("SWI &{number:X}")))?;
        if backend != InvocationBackend::Interpreter {
            return Err(RegistryError::UnsupportedInvocationBackend(backend));
        }
        Ok(InvocationPlan { backend, identity })
    }

    pub fn swi_entry_id(&self, number: u32) -> Option<SwiEntryId> {
        self.by_swi_number
            .get(&number)
            .map(|published| published.cell.id())
    }

    pub fn swi_contract(&self, number: u32) -> Option<&SwiContract> {
        self.by_swi_number
            .get(&number)
            .map(|published| &published.contract)
    }

    pub fn swi_module_name(&self, number: u32) -> Option<&str> {
        self.by_swi_number
            .get(&number)
            .map(|published| published.module_name.as_str())
    }

    pub fn swi_retired_generation_count(&self, number: u32) -> Option<usize> {
        let cell = self.by_swi_number.get(&number)?.cell.as_ref();
        cell.collect_retired();
        Some(cell.retired_generation_count())
    }

    pub fn registered_swi_count(&self) -> usize {
        self.by_swi_number.len()
    }

    pub fn authorized_primitive(
        &self,
        module_id: ModuleId,
        primitive_name: &str,
    ) -> Result<PrimitiveDescriptor, RegistryError> {
        let module = self
            .modules
            .get(&module_id)
            .ok_or(RegistryError::UnknownModule(module_id))?;
        if !matches!(
            module.state,
            ModuleState::Starting | ModuleState::Active | ModuleState::Quiescing
        ) {
            return Err(RegistryError::InvalidState {
                module: module.manifest.name.clone(),
                state: module.state,
            });
        }
        let primitive_name = canonical_primitive_name(primitive_name);
        let primitive_id = module
            .resolved_primitives
            .get(&primitive_name)
            .ok_or_else(|| RegistryError::PrimitiveNotImported {
                module: module.manifest.name.clone(),
                primitive: primitive_name.clone(),
            })?;
        let primitive = self
            .primitives
            .get(&primitive_name)
            .ok_or_else(|| RegistryError::MissingPrimitive(primitive_name.clone()))?;
        if primitive.id != *primitive_id
            || !module.granted_capabilities.contains(&primitive.capability)
        {
            return Err(RegistryError::CapabilityNotGranted {
                module: module.manifest.name.clone(),
                capability: primitive.capability.as_str().into(),
            });
        }
        Ok(primitive.clone())
    }
}

fn canonical_swi_name(name: &str) -> String {
    name.to_ascii_uppercase()
}

fn canonical_module_name(name: &str) -> String {
    name.to_ascii_uppercase()
}

fn canonical_primitive_name(name: &str) -> String {
    name.to_ascii_uppercase()
}

fn validate_module_name(name: &str) -> Result<(), RegistryError> {
    let mut parts = name.split('.');
    if name.is_empty()
        || parts.any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
    {
        return Err(RegistryError::InvalidModuleName(name.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn cap(name: &str) -> CapabilityName {
        CapabilityName::new(name).unwrap()
    }

    fn definition(module: ModuleId, name: &str) -> DefinitionDescriptor {
        DefinitionDescriptor {
            id: DefinitionId(0),
            name: name.into(),
            module,
            source_path: "modules/test.bas64".into(),
            source_hash: "sha256:test".into(),
            language_profile: "BASIC64-SYSTEM-0.1".into(),
            target_profile: "HOSTED".into(),
        }
    }

    fn manifest(
        name: &str,
        exports: Vec<SwiExport>,
        imports: Vec<PrimitiveImport>,
        requested: &[&str],
    ) -> ModuleManifest {
        ModuleManifest {
            schema_version: 1,
            name: name.into(),
            version: SemanticVersion::new(1, 0, 0),
            language_profile: "BASIC64-SYSTEM-0.1".into(),
            target_profile: "HOSTED".into(),
            dependencies: Vec::new(),
            symbol_imports: Vec::new(),
            primitive_imports: imports,
            requested_capabilities: requested.iter().map(|name| cap(name)).collect(),
            lifecycle: ModuleLifecycle::default(),
            replacement_policy: ReplacementPolicy::CompatibleImmediate,
            symbol_exports: Vec::new(),
            exports,
            source_path: "modules/test.bas64".into(),
            source_hash: "sha256:test".into(),
        }
    }

    fn export(number: u32, name: &str, definition: &str) -> SwiExport {
        SwiExport {
            number,
            name: name.into(),
            definition_name: definition.into(),
            contract: SwiContract::default(),
        }
    }

    #[test]
    fn opaque_identities_are_stable_across_moves_and_distinct_by_kind() {
        let allocator = IdentityAllocator::default();
        let module = allocator.module_id();
        let definition = allocator.definition_id();
        let moved = module;
        assert_eq!(module, moved);
        assert_ne!(module.diagnostic_value(), definition.diagnostic_value());
        assert!(module.to_string().starts_with("ModuleId("));
    }

    #[test]
    fn manifest_v1_rejects_malformed_lifecycle_source_and_memory_contracts() {
        let mut value = manifest(
            "Console",
            vec![export(0, "OS_WriteC", "WriteC")],
            Vec::new(),
            &[],
        );
        value.lifecycle.start = Some("../invalid".into());
        assert!(matches!(
            value.encode_v1(),
            Err(RegistryError::InvalidManifest(_))
        ));

        for invalid_path in [
            "modules/../outside.bas64",
            "/modules/outside.bas64",
            "modules\\outside.bas64",
            "modules//outside.bas64",
            "modules/./outside.bas64",
        ] {
            let mut value = manifest(
                "Console",
                vec![export(0, "OS_WriteC", "WriteC")],
                Vec::new(),
                &[],
            );
            value.source_path = invalid_path.into();
            assert!(matches!(
                value.encode_v1(),
                Err(RegistryError::InvalidManifest(_))
            ));
        }

        let mut value = manifest(
            "Console",
            vec![export(0, "OS_WriteC", "WriteC")],
            Vec::new(),
            &[],
        );
        value.exports[0].contract.registers.push(RegisterContract {
            register: 0,
            kind: RegisterKind::Unsigned { bits: 32 },
            direction: ArgumentDirection::In,
        });
        value.exports[0]
            .contract
            .logical_memory
            .push(LogicalMemoryContract::Write {
                register: 0,
                max_bytes: Some(32),
            });
        assert!(matches!(
            value.encode_v1(),
            Err(RegistryError::InvalidManifest(_))
        ));

        let mut value = manifest(
            "Console",
            vec![export(0, "OS_WriteC", "WriteC")],
            Vec::new(),
            &[],
        );
        value.exports[0].contract.registers.push(RegisterContract {
            register: 0,
            kind: RegisterKind::LogicalAddress { bits: 24 },
            direction: ArgumentDirection::InOut,
        });
        assert!(matches!(
            value.encode_v1(),
            Err(RegistryError::InvalidManifest(_))
        ));
    }

    #[test]
    fn dependency_replacement_invalidates_derived_targets_but_active_targets_can_finish() {
        let dependency_v1 = DependencyFingerprint {
            module: "Console".into(),
            version: SemanticVersion::new(1, 0, 0),
            source_path: "modules/Console.bas64".into(),
            source_hash: "fnv1a64:old".into(),
            language_profile: "BASIC64-SYSTEM-0.1".into(),
            target_profile: "HOSTED".into(),
        };
        let identity = DerivedTargetIdentity {
            definition: DefinitionId(7),
            generation: GenerationId(9),
            module_version: SemanticVersion::new(1, 0, 0),
            source_path: "modules/consumer.bas64".into(),
            source_hash: "fnv1a64:consumer".into(),
            dependencies: vec![dependency_v1],
            language_profile: "BASIC64-SYSTEM-0.1".into(),
            target_profile: "HOSTED".into(),
            runtime_abi: RUNTIME_ABI_VERSION,
        };
        let mut cache = DerivedTargetCache::default();
        cache.insert(identity.clone(), Arc::new(vec![1, 2, 3]));
        let active_target = cache.get(&identity).unwrap();
        cache.invalidate_dependency(
            "Console",
            &DependencyFingerprint {
                module: "Console".into(),
                version: SemanticVersion::new(1, 0, 0),
                source_path: "modules/Console.bas64".into(),
                source_hash: "fnv1a64:new".into(),
                language_profile: "BASIC64-SYSTEM-0.1".into(),
                target_profile: "HOSTED".into(),
            },
        );
        assert!(cache.is_empty());
        assert_eq!(&*active_target, &[1, 2, 3]);
    }

    #[test]
    fn derived_identity_includes_transitive_module_sources() {
        let mut registry = ModuleRegistry::new();
        let mut runtime_manifest = manifest("Runtime", Vec::new(), Vec::new(), &[]);
        runtime_manifest.source_path = "modules/Runtime.bas64".into();
        runtime_manifest.source_hash = "runtime-v1".into();
        let runtime = registry.stage_module(runtime_manifest, Vec::new()).unwrap();
        registry.link_module(runtime, BTreeSet::new()).unwrap();

        let mut library_manifest = manifest("Library", Vec::new(), Vec::new(), &[]);
        library_manifest.source_path = "modules/Library.bas64".into();
        library_manifest.source_hash = "library-v1".into();
        library_manifest
            .dependencies
            .push(("Runtime".into(), SemanticVersion::new(1, 0, 0)));
        let library = registry.stage_module(library_manifest, Vec::new()).unwrap();
        registry.link_module(library, BTreeSet::new()).unwrap();

        let mut consumer_manifest = manifest(
            "Consumer",
            vec![export(0x90, "Consumer_Test", "Entry")],
            Vec::new(),
            &[],
        );
        consumer_manifest.source_path = "modules/Consumer.bas64".into();
        consumer_manifest.source_hash = "consumer-v1".into();
        consumer_manifest
            .dependencies
            .push(("Library".into(), SemanticVersion::new(1, 0, 0)));
        let consumer_id = registry.allocator.module_id();
        let mut consumer_definition = definition(consumer_id, "Entry");
        consumer_definition.source_path = consumer_manifest.source_path.clone();
        consumer_definition.source_hash = consumer_manifest.source_hash.clone();
        let consumer = registry
            .stage_module(consumer_manifest, [("Entry".into(), consumer_definition)])
            .unwrap();
        registry.link_module(consumer, BTreeSet::new()).unwrap();
        registry.publish_modules(&[consumer]).unwrap();
        registry.start_module(consumer, true).unwrap();

        let identity = registry.derived_target_identity(0x90).unwrap();
        assert_eq!(identity.module_version, SemanticVersion::new(1, 0, 0));
        assert_eq!(
            identity
                .dependencies
                .iter()
                .map(|item| item.module.as_str())
                .collect::<Vec<_>>(),
            ["Library", "Runtime"]
        );
        let runtime_identity = identity
            .dependencies
            .iter()
            .find(|item| item.module == "Runtime")
            .unwrap();
        assert_eq!(runtime_identity.source_path, "modules/Runtime.bas64");
        let mut cache = DerivedTargetCache::default();
        cache.insert(identity, Arc::new(vec![4, 5, 6]));
        cache.invalidate_dependency(
            "Runtime",
            &DependencyFingerprint {
                module: "Runtime".into(),
                version: SemanticVersion::new(1, 0, 0),
                source_path: "modules/Runtime.bas64".into(),
                source_hash: "runtime-v2".into(),
                language_profile: "BASIC64-SYSTEM-0.1".into(),
                target_profile: "HOSTED".into(),
            },
        );
        assert!(cache.is_empty());
    }

    #[test]
    fn invocation_plan_uses_one_source_identity_and_rejects_unimplemented_compilers() {
        let mut registry = ModuleRegistry::new();
        let module = registry
            .stage_module(
                manifest(
                    "Console",
                    vec![export(0, "OS_WriteC", "WriteC")],
                    Vec::new(),
                    &[],
                ),
                [("WriteC".into(), definition(ModuleId(0), "WriteC"))],
            )
            .unwrap();
        registry.link_module(module, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module]).unwrap();
        registry.start_module(module, true).unwrap();

        let plan = registry
            .invocation_plan(0, InvocationBackend::Interpreter)
            .unwrap();
        assert_eq!(plan.backend, InvocationBackend::Interpreter);
        assert_eq!(plan.identity.source_path, "modules/test.bas64");
        assert_eq!(plan.identity.source_hash, "sha256:test");
        assert_eq!(plan.identity.language_profile, "BASIC64-SYSTEM-0.1");
        assert_eq!(plan.identity.target_profile, "HOSTED");
        assert!(matches!(
            registry.invocation_plan(0, InvocationBackend::Jit),
            Err(RegistryError::UnsupportedInvocationBackend(
                InvocationBackend::Jit
            ))
        ));
        assert!(matches!(
            registry.invocation_plan(0, InvocationBackend::Aot),
            Err(RegistryError::UnsupportedInvocationBackend(
                InvocationBackend::Aot
            ))
        ));
    }

    #[test]
    fn staging_rejects_definition_target_identity_that_disagrees_with_manifest() {
        let mut registry = ModuleRegistry::new();
        let mut write = definition(ModuleId(0), "WriteC");
        write.target_profile = "AGON".into();
        let error = registry
            .stage_module(
                manifest(
                    "Console",
                    vec![export(0, "OS_WriteC", "WriteC")],
                    Vec::new(),
                    &[],
                ),
                [("WriteC".into(), write)],
            )
            .unwrap_err();
        assert!(error.to_string().contains("source/profile metadata"));

        let mut mismatched_source = definition(ModuleId(0), "WriteC");
        mismatched_source.source_hash = "sha256:forged".into();
        let error = registry
            .stage_module(
                manifest(
                    "Console2",
                    vec![export(1, "OS_WriteC2", "WriteC")],
                    Vec::new(),
                    &[],
                ),
                [("WriteC".into(), mismatched_source)],
            )
            .unwrap_err();
        assert!(error.to_string().contains("source/profile metadata"));
    }

    #[test]
    fn module_names_are_case_insensitive_but_keep_their_declared_spelling() {
        let mut registry = ModuleRegistry::new();
        let first = registry
            .stage_module(
                manifest("Console", Vec::new(), Vec::new(), &[]),
                std::iter::empty(),
            )
            .unwrap();
        assert_eq!(registry.module_named("console").unwrap().id, first);
        assert_eq!(registry.module(first).unwrap().manifest.name, "Console");

        let duplicate = registry.stage_module(
            manifest("console", Vec::new(), Vec::new(), &[]),
            std::iter::empty(),
        );
        assert!(
            matches!(duplicate, Err(RegistryError::DuplicateModule(name)) if name == "console")
        );
    }

    #[test]
    fn active_call_keeps_old_generation_until_return_and_new_calls_see_replacement() {
        let allocator = Arc::new(IdentityAllocator::default());
        let cell = VersionedDefinitionCell::new(
            allocator.swi_entry_id(),
            SwiContract::default(),
            "old",
            Arc::clone(&allocator),
        );
        let old = cell.acquire();
        let old_id = old.generation_id();
        let new_id = cell.replace(&SwiContract::default(), "new").unwrap();
        assert!(old.retired());
        assert_eq!(*old, "old");
        assert_eq!(cell.retired_generation_count(), 1);
        let new = cell.acquire();
        assert_eq!(*new, "new");
        assert_eq!(new.generation_id(), new_id);
        drop(old);
        cell.collect_retired();
        assert_eq!(cell.retired_generation_count(), 0);
        assert_ne!(old_id, new_id);
    }

    #[test]
    fn concurrent_replacement_retains_the_old_generation_until_its_invocation_returns() {
        let allocator = Arc::new(IdentityAllocator::default());
        let cell = Arc::new(VersionedDefinitionCell::new(
            allocator.swi_entry_id(),
            SwiContract::default(),
            String::from("old"),
            allocator,
        ));
        let (acquired_sender, acquired_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let worker_cell = Arc::clone(&cell);
        let worker = std::thread::spawn(move || {
            let invocation = worker_cell.acquire();
            acquired_sender.send(invocation.generation_id()).unwrap();
            release_receiver.recv().unwrap();
            assert!(invocation.retired());
            assert_eq!(&*invocation, "old");
        });

        let old_generation = acquired_receiver.recv().unwrap();
        let new_generation = cell
            .replace(&SwiContract::default(), String::from("new"))
            .unwrap();
        assert_ne!(old_generation, new_generation);
        assert_eq!(cell.retired_generation_count(), 1);
        assert_eq!(&*cell.acquire(), "new");

        release_sender.send(()).unwrap();
        worker.join().unwrap();
        cell.collect_retired();
        assert_eq!(cell.retired_generation_count(), 0);
    }

    #[test]
    fn rejects_contract_change_without_disturbing_current_definition() {
        let allocator = Arc::new(IdentityAllocator::default());
        let contract = SwiContract::default();
        let cell = VersionedDefinitionCell::new(
            allocator.swi_entry_id(),
            contract.clone(),
            "old",
            allocator,
        );
        let mut incompatible = contract;
        incompatible.may_block = true;
        assert_eq!(
            cell.replace(&incompatible, "bad"),
            Err(RegistryError::IncompatibleContract)
        );
        assert_eq!(*cell.acquire(), "old");
        assert_eq!(cell.current_generation_number(), 1);
    }

    #[test]
    fn primitive_linking_requires_matching_declared_and_granted_capabilities() {
        let mut registry = ModuleRegistry::new();
        registry
            .primitives
            .register(
                "Host.Console.WriteByte",
                cap("ConsoleOutput"),
                vec![RegisterKind::Unsigned { bits: 8 }],
                Vec::new(),
                false,
                true,
                Vec::new(),
                "RuntimeResult",
            )
            .unwrap();
        assert!(matches!(
            registry.primitives.register("host.console.writebyte", cap("ConsoleOutput"), vec![RegisterKind::Unsigned { bits: 8 }], Vec::new(), false, true, Vec::new(), "RuntimeResult"),
            Err(RegistryError::DuplicatePrimitive(name)) if name == "HOST.CONSOLE.WRITEBYTE"
        ));
        let import = PrimitiveImport {
            name: "Host.Console.WriteByte".into(),
            capability: cap("ConsoleOutput"),
        };
        let manifest = manifest(
            "Console",
            vec![export(0, "OS_WriteC", "WriteC")],
            vec![import],
            &["ConsoleOutput"],
        );
        let module = registry
            .stage_module(
                manifest,
                [("WriteC".into(), definition(ModuleId(0), "WriteC"))],
            )
            .unwrap();
        assert!(matches!(
            registry.link_module(module, BTreeSet::new()),
            Err(RegistryError::CapabilityNotGranted { .. })
        ));
        registry
            .link_module(module, [cap("ConsoleOutput")].into_iter().collect())
            .unwrap();
        let record = registry.module(module).unwrap();
        assert_eq!(record.resolved_primitives.len(), 1);
        assert_eq!(record.state, ModuleState::Linked);
    }

    #[test]
    fn failed_set_publication_leaves_every_swi_invisible() {
        let mut registry = ModuleRegistry::new();
        let first = registry
            .stage_module(
                manifest(
                    "First",
                    vec![export(0, "OS_WriteC", "WriteC")],
                    Vec::new(),
                    &[],
                ),
                [("WriteC".into(), definition(ModuleId(0), "WriteC"))],
            )
            .unwrap();
        let second = registry
            .stage_module(
                manifest(
                    "Second",
                    vec![export(0, "OS_ReadC", "ReadC")],
                    Vec::new(),
                    &[],
                ),
                [("ReadC".into(), definition(ModuleId(0), "ReadC"))],
            )
            .unwrap();
        registry.link_module(first, BTreeSet::new()).unwrap();
        registry.link_module(second, BTreeSet::new()).unwrap();
        assert!(matches!(
            registry.publish_modules(&[first, second]),
            Err(RegistryError::DuplicateSwiNumber(0))
        ));
        assert_eq!(registry.registered_swi_count(), 0);
        assert_eq!(registry.module_state(first), Some(ModuleState::Linked));
        assert_eq!(registry.module_state(second), Some(ModuleState::Linked));
    }

    #[test]
    fn active_swi_inspection_traces_owner_definition_and_generation() {
        let mut registry = ModuleRegistry::new();
        let module = registry
            .stage_module(
                manifest(
                    "Console",
                    vec![export(0, "OS_WriteC", "WriteC")],
                    Vec::new(),
                    &[],
                ),
                [("WriteC".into(), definition(ModuleId(0), "WriteC"))],
            )
            .unwrap();
        registry.link_module(module, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module]).unwrap();
        assert!(registry.acquire_swi(0).is_none());
        registry.start_module(module, true).unwrap();
        let (owner, lease) = registry.acquire_swi(0).unwrap();
        assert_eq!(owner.module, module);
        assert_eq!(owner.module_name, "Console");
        assert_eq!(owner.name, "OS_WriteC");
        assert_eq!(owner.definition_name, "WriteC");
        assert_eq!(owner.generation, lease.generation_id());
        assert_eq!(owner.source_path, "modules/test.bas64");
    }

    #[test]
    fn module_private_definition_names_are_scoped_to_each_module() {
        let mut registry = ModuleRegistry::new();
        let allocator = registry.allocator();
        let mut first_entry = definition(ModuleId(0), "FirstEntry");
        first_entry.id = allocator.definition_id();
        let mut first_helper = definition(ModuleId(0), "Helper");
        first_helper.id = allocator.definition_id();
        let first = registry
            .stage_module(
                manifest(
                    "First",
                    vec![export(0x100, "First_Service", "FirstEntry")],
                    Vec::new(),
                    &[],
                ),
                [
                    ("FirstEntry".into(), first_entry),
                    ("Helper".into(), first_helper),
                ],
            )
            .unwrap();

        let mut second_entry = definition(ModuleId(0), "SecondEntry");
        second_entry.id = allocator.definition_id();
        let mut second_helper = definition(ModuleId(0), "Helper");
        second_helper.id = allocator.definition_id();
        let second = registry
            .stage_module(
                manifest(
                    "Second",
                    vec![export(0x101, "Second_Service", "SecondEntry")],
                    Vec::new(),
                    &[],
                ),
                [
                    ("SecondEntry".into(), second_entry),
                    ("Helper".into(), second_helper),
                ],
            )
            .unwrap();

        registry.link_module(first, BTreeSet::new()).unwrap();
        registry.link_module(second, BTreeSet::new()).unwrap();
        registry.publish_modules(&[first, second]).unwrap();
        registry.start_module(first, true).unwrap();
        registry.start_module(second, true).unwrap();

        let first_helper = &registry.module(first).unwrap().definitions["Helper"];
        let second_helper = &registry.module(second).unwrap().definitions["Helper"];
        assert_ne!(first_helper.id, second_helper.id);
        assert_eq!(first_helper.name, second_helper.name);
        assert_eq!(registry.acquire_swi(0x100).unwrap().0.module, first);
        assert_eq!(registry.acquire_swi(0x101).unwrap().0.module, second);
    }

    #[test]
    fn module_symbol_imports_link_only_to_declared_exports_and_roundtrip_in_manifest() {
        let mut registry = ModuleRegistry::new();
        let mut provider = manifest("FileSwitch", Vec::new(), Vec::new(), &[]);
        provider.symbol_exports.push("OPEN".into());
        let provider_id = registry
            .stage_module(
                provider.clone(),
                [("OPEN".into(), definition(ModuleId(0), "OPEN"))],
            )
            .unwrap();
        registry.link_module(provider_id, BTreeSet::new()).unwrap();

        let mut consumer = manifest("Console", Vec::new(), Vec::new(), &[]);
        consumer
            .dependencies
            .push(("FileSwitch".into(), SemanticVersion::new(1, 0, 0)));
        consumer.symbol_imports.push(ModuleSymbolImport {
            module: "FileSwitch".into(),
            symbol: "OPEN".into(),
        });
        let wire = consumer.encode_v1().unwrap();
        assert_eq!(ModuleManifest::decode_v1(&wire).unwrap(), consumer);
        assert!(ModuleManifest::decode_v1(&format!("{wire}unknown.field\t1\n")).is_err());
        assert!(ModuleManifest::decode_v1(&format!("{wire}name\tConsole\n")).is_err());
        let consumer_id = registry.stage_module(consumer.clone(), []).unwrap();
        registry.link_module(consumer_id, BTreeSet::new()).unwrap();

        let mut hidden = manifest("HiddenConsumer", Vec::new(), Vec::new(), &[]);
        hidden
            .dependencies
            .push(("FileSwitch".into(), SemanticVersion::new(1, 0, 0)));
        hidden.symbol_imports.push(ModuleSymbolImport {
            module: "FileSwitch".into(),
            symbol: "PRIVATE".into(),
        });
        let hidden_id = registry.stage_module(hidden, []).unwrap();
        assert!(matches!(
            registry.link_module(hidden_id, BTreeSet::new()),
            Err(RegistryError::MissingSymbol { module, dependency, symbol })
                if module == "HiddenConsumer" && dependency == "FileSwitch" && symbol == "PRIVATE"
        ));
    }

    #[test]
    fn failed_start_rolls_back_the_whole_export_set_before_retry() {
        let mut registry = ModuleRegistry::new();
        let module = registry
            .stage_module(
                manifest(
                    "Console",
                    vec![export(0, "OS_WriteC", "WriteC")],
                    Vec::new(),
                    &[],
                ),
                [("WriteC".into(), definition(ModuleId(0), "WriteC"))],
            )
            .unwrap();
        registry.link_module(module, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module]).unwrap();
        assert!(registry.start_module(module, false).is_err());
        assert_eq!(registry.module_state(module), Some(ModuleState::Linked));
        assert!(registry.acquire_swi(0).is_none());
        assert_eq!(registry.registered_swi_count(), 0);
        registry.publish_modules(&[module]).unwrap();
        registry.start_module(module, true).unwrap();
        assert!(registry.acquire_swi(0).is_some());
    }

    #[test]
    fn quiescing_blocks_new_calls_and_retirement_waits_for_active_leases() {
        let mut registry = ModuleRegistry::new();
        let module = registry
            .stage_module(
                manifest(
                    "Console",
                    vec![export(0, "OS_WriteC", "WriteC")],
                    Vec::new(),
                    &[],
                ),
                [("WriteC".into(), definition(ModuleId(0), "WriteC"))],
            )
            .unwrap();
        registry.link_module(module, BTreeSet::new()).unwrap();
        registry.publish_modules(&[module]).unwrap();
        registry.start_module(module, true).unwrap();

        let entry_cell = registry.swi_entry_id(0).unwrap();
        let (_, active_call) = registry.acquire_swi(0).unwrap();
        registry.quiesce_module(module).unwrap();
        assert_eq!(registry.module_state(module), Some(ModuleState::Quiescing));
        assert!(registry.acquire_swi(0).is_none());
        assert_eq!(
            registry.retire_module(module),
            Err(RegistryError::ModuleBusy {
                module: "Console".into(),
                active_calls: 1,
            })
        );
        assert_eq!(registry.swi_entry_id(0), Some(entry_cell));
        assert!(!active_call.retired());

        drop(active_call);
        registry.retire_module(module).unwrap();
        assert_eq!(registry.module_state(module), Some(ModuleState::Retired));
        assert_eq!(registry.swi_entry_id(0), None);
        assert_eq!(registry.swi_number("OS_WriteC"), None);
    }
}
