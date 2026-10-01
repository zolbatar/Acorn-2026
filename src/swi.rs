use crate::system_variables::{
    MAX_NAME_BYTES, MAX_VALUE_BYTES, SystemVariable, SystemVariableStore, SystemVariableType,
    buffer_error, limit_error, name_error, selector_matches_name, validate_selector,
};
use crate::{
    basic_compat::{BasicLaunchOptions, system_profile::SystemModule},
    boot::{
        BootCapsule, BootFailure, BootStage, RUNTIME_ABI_VERSION, RecoveryAction,
        embedded_capsule_bytes, parse_recovery_action,
    },
    configure::{BasicConfiguration, BasicLanguageMode, ConfigureStore, StartupLanguage},
    display::{DesktopResolution, DisplayColour, DisplaySettings},
    error::RuntimeError,
    filesystem::{
        FILETYPE_BASIC, FILETYPE_BASIC64, FILETYPE_TEXT, FileMetadata, HostFileSystem, OpenFile,
    },
    graphics::{
        GraphicsProfile, GraphicsService, GraphicsSnapshot, GraphicsWindow, TextEncoding,
        TextRenderingProfile,
    },
    host::HostConsole,
    memory::{ExecInputRead, GUEST_MEMORY_BASE, SystemVariableReadCursor, Task},
    ricochet::{
        ActiveCommand, ArgumentDirection, CapabilityName, CommandHandler, DefinitionId,
        DependencyFingerprint, DerivedTargetCache, InvocationBackend, LogicalMemoryContract,
        ModuleId, ModuleManagementAuthority, ModuleRegistry, ModuleState, RegisterContract,
        RegisterKind, ResourceRight, SwiContract, same_replacement_manifest,
    },
};
use std::{
    collections::{BTreeSet, HashMap},
    io::{Read, Seek, SeekFrom, Write},
    sync::mpsc::Sender,
    time::{Duration, Instant},
};

use std::sync::Arc;

mod mos;
pub(crate) use mos::MosClock;

fn same_export_contracts(
    left: &[crate::ricochet::SwiExport],
    right: &[crate::ricochet::SwiExport],
) -> bool {
    left.len() == right.len()
        && left.iter().all(|export| {
            right.iter().any(|candidate| {
                candidate.number == export.number
                    && candidate.name.eq_ignore_ascii_case(&export.name)
                    && candidate.contract == export.contract
            })
        })
}

use crate::wimp::{
    DesktopTaskKind, WIMP_CLOSE_DOWN, WIMP_CLOSE_WINDOW, WIMP_CREATE_ICON, WIMP_CREATE_ICON_EX,
    WIMP_CREATE_MENU, WIMP_CREATE_WINDOW, WIMP_DELETE_ICON, WIMP_FORCE_REDRAW,
    WIMP_GET_POINTER_INFO, WIMP_GET_RECTANGLE, WIMP_GET_WINDOW_STATE, WIMP_INITIALISE,
    WIMP_OPEN_WINDOW, WIMP_POLL, WIMP_REDRAW_WINDOW, WIMP_SET_EXTENT, WIMP_SET_ICON_STATE,
    WIMP_START_TASK, WIMP_UPDATE_WINDOW, WimpServer, WorkArea,
};

const DISPLAY_BATCH_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const INKEY_POLL_INTERVAL: Duration = Duration::from_millis(8);
const MAX_EXTENDED_MODE_PIXELS: u64 = 4_194_304;
const MAX_TASK_GRAPHICS_PIXELS: u64 = 8_388_608;
const MAX_TASK_DEFAULT_GRAPHICS_CONTEXTS: usize = 64;

pub const OS_WRITE_C: u32 = 0x00;
pub const OS_WRITE_S: u32 = 0x01;
pub const OS_WRITE_0: u32 = 0x02;
pub const OS_NEW_LINE: u32 = 0x03;
pub const OS_READ_C: u32 = 0x04;
pub const OS_CLI: u32 = 0x05;
pub const OS_BYTE: u32 = 0x06;
pub const OS_WORD: u32 = 0x07;

fn clock_from_chunks(context: &SwiContext) -> u64 {
    u64::from(context.registers[R0] & 0xFFFF)
        | (u64::from(context.registers[R1] & 0xFFFF) << 16)
        | (u64::from(context.registers[R2] & 0xFF) << 32)
}
pub const OS_FILE: u32 = 0x08;
pub const OS_FS_CONTROL: u32 = 0x29;
pub const OS_ARGS: u32 = 0x09;
pub const OS_BGET: u32 = 0x0A;
pub const OS_BPUT: u32 = 0x0B;
pub const OS_GBPB: u32 = 0x0C;
pub const OS_FIND: u32 = 0x0D;
pub const OS_READ_LINE: u32 = 0x0E;
pub const OS_READ_VAR_VAL: u32 = 0x23;
pub const OS_SET_VAR_VAL: u32 = 0x24;
pub const OS_SWI_NUMBER_TO_STRING: u32 = 0x38;
pub const OS_SWI_NUMBER_FROM_STRING: u32 = 0x39;
pub const OS_READ_MONOTONIC_TIME: u32 = 0x42;
pub const OS_CHANGE_DYNAMIC_AREA: u32 = 0x2A;
pub const OS_GENERATE_ERROR: u32 = 0x2B;
pub const OS_FSCONTROL: u32 = 0x29;
pub const OS_READ_POINT: u32 = 0x32;
pub const OS_DYNAMIC_AREA: u32 = 0x66;
pub const OS_PLOT: u32 = 0x45;
pub const RICOCHET_MODULE_INFO: u32 = 0x4FF10;
pub const RICOCHET_TASK_INFO: u32 = 0x4FF11;
pub const RICOCHET_MODULE_LOOKUP: u32 = 0x4FF12;
pub const RICOCHET_SWI_INFO: u32 = 0x4FF13;
pub const RICOCHET_MODULE_EXPORT: u32 = 0x4FF14;
pub const RICOCHET_DEFINITION_SOURCE: u32 = 0x4FF15;
pub use crate::wimp::{
    WIMP_CLOSE_DOWN as WIMP_CLOSE_DOWN_SWI, WIMP_CLOSE_WINDOW as WIMP_CLOSE_WINDOW_SWI,
    WIMP_CREATE_ICON as WIMP_CREATE_ICON_SWI, WIMP_CREATE_ICON_EX as WIMP_CREATE_ICON_EX_SWI,
    WIMP_CREATE_MENU as WIMP_CREATE_MENU_SWI, WIMP_CREATE_WINDOW as WIMP_CREATE_WINDOW_SWI,
    WIMP_DELETE_ICON as WIMP_DELETE_ICON_SWI, WIMP_FORCE_REDRAW as WIMP_FORCE_REDRAW_SWI,
    WIMP_GET_POINTER_INFO as WIMP_GET_POINTER_INFO_SWI,
    WIMP_GET_RECTANGLE as WIMP_GET_RECTANGLE_SWI,
    WIMP_GET_WINDOW_STATE as WIMP_GET_WINDOW_STATE_SWI, WIMP_INITIALISE as WIMP_INITIALISE_SWI,
    WIMP_OPEN_WINDOW as WIMP_OPEN_WINDOW_SWI, WIMP_POLL as WIMP_POLL_SWI,
    WIMP_REDRAW_WINDOW as WIMP_REDRAW_WINDOW_SWI, WIMP_SET_EXTENT as WIMP_SET_EXTENT_SWI,
    WIMP_SET_ICON_STATE as WIMP_SET_ICON_STATE_SWI, WIMP_START_TASK as WIMP_START_TASK_SWI,
    WIMP_UPDATE_WINDOW as WIMP_UPDATE_WINDOW_SWI,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DisplayEvent {
    WriteByte {
        task_id: u64,
        window_handle: Option<u32>,
        byte: u8,
    },
    Plot {
        task_id: u64,
        window_handle: Option<u32>,
        code: u8,
        x: i32,
        y: i32,
    },
    GraphicsSnapshot {
        task_id: u64,
        window_handle: Option<u32>,
        snapshot: GraphicsSnapshot,
    },
    DesktopStarted,
    DesktopChanged,
    RuntimeExited,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SwiDispatchRoute {
    ModuleOwned {
        number: u32,
        name: String,
        module: String,
        definition: String,
        generation: u64,
        backend: InvocationBackend,
    },
    ModuleOwnedNamed {
        name: String,
        module: String,
        definition: String,
        definition_id: u64,
        source_hash: String,
    },
    TransitionalRust {
        number: u32,
    },
    TransitionalNamed {
        name: String,
    },
}

const R0: usize = 0;
const R1: usize = 1;
const R2: usize = 2;
const R3: usize = 3;
const R4: usize = 4;
const R5: usize = 5;
const R6: usize = 6;
const R7: usize = 7;
#[cfg(test)]
const R8: usize = 8;
const SWI_X_BIT: u32 = 1 << 17;
const SWI_UNKNOWN_ERROR_CODE: u32 = 1;
#[cfg(test)]
const RICOCHET_DISPLAY_ABI_VERSION: u32 = 1;
#[cfg(test)]
const RICOCHET_DISPLAY_QUERY: u32 = 0;
#[cfg(test)]
const RICOCHET_DISPLAY_APPLY: u32 = 1;
const SYSTEM_SWI_NAME_MAX_BYTES: usize = 128;
const SYSTEM_SWI_NAME_MAX_BYTES_U32: u32 = 128;
const HOST_FS_NUMBER: u32 = 1;
const HOST_FS_CONTROL_BLOCK: u32 = GUEST_MEMORY_BASE;
const GUEST_ADDRESS_MASK: u32 = 0x3FFF_FFFF;
const READ_LINE_ECHO_ONLY_BUFFERED: u32 = 1 << 31;
const READ_LINE_ECHO_R4: u32 = 1 << 30;
const MAX_CLI_BYTES: usize = 256;
const MAX_OBEY_SCRIPT_BYTES: usize = 64 * 1024;
// BASIC64 command handlers use recursive interpreter calls for nested OBEY.
// Keep the hosted bound conservative to avoid exhausting the native stack.
const MAX_OBEY_NESTING: usize = 8;
const MAX_OBEY_LINES: usize = 4096;
const MAX_OBEY_LINE_BYTES: usize = MAX_CLI_BYTES - 1;
const MAX_EXEC_SOURCE_BYTES: usize = 64 * 1024;
const MAX_EXEC_LINES: usize = 4096;
const MAX_EXEC_LINE_BYTES: usize = MAX_CLI_BYTES - 1;
const MAX_STRING_BYTES: usize = 4096;
const CONFIG_SERVICE_OK: u32 = 0;
const CONFIG_SERVICE_NOT_FOUND: u32 = 1;
const CONFIG_SERVICE_ERROR: u32 = 2;
const CONFIG_SERVICE_BUFFER_ERROR: u32 = 3;
const CONFIG_SERVICE_DENIED: u32 = 4;
const CONFIG_VALUE_BUFFER_MAX: usize = 512;
const CONFIG_ERROR_BUFFER_MAX: usize = 512;
const OUTPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x1000;
#[cfg(test)]
const CLI_STRING_BUFFER: u32 = GUEST_MEMORY_BASE + 0x3000;

/// Return the one- or two-byte physical line terminator at `offset`.
/// CRLF is one boundary everywhere in the Exec source contract.
fn exec_line_terminator_len(bytes: &[u8], offset: usize) -> Option<usize> {
    match bytes.get(offset) {
        Some(b'\r') if bytes.get(offset + 1) == Some(&b'\n') => Some(2),
        Some(b'\r' | b'\n') => Some(1),
        _ => None,
    }
}

/// Return the 1-based physical line containing a byte offset in an Exec source.
fn exec_source_line_at(bytes: &[u8], byte_offset: usize) -> u32 {
    let mut offset = 0;
    let mut line = 1_u32;
    let end = byte_offset.min(bytes.len());
    while offset < end {
        if let Some(terminator_len) = exec_line_terminator_len(bytes, offset) {
            line = line.saturating_add(1);
            offset = offset.saturating_add(terminator_len);
        } else {
            offset += 1;
        }
    }
    line
}

fn validate_boot_grants(
    module_name: &str,
    grants: &BTreeSet<CapabilityName>,
) -> Result<(), String> {
    let permitted = if module_name.eq_ignore_ascii_case("Console") {
        [
            "ConsoleInput",
            "ConsoleOutput",
            "RuntimeErrors",
            "GraphicsVduStream",
        ]
        .as_slice()
    } else if module_name.eq_ignore_ascii_case("Mos") {
        ["MosInput", "MosClock", "TaskMemory", "RuntimeErrors"].as_slice()
    } else if module_name.eq_ignore_ascii_case("Graphics") {
        ["GraphicsRaster", "RuntimeErrors"].as_slice()
    } else if module_name.eq_ignore_ascii_case("ColourTrans") {
        ["GraphicsRaster"].as_slice()
    } else if module_name.eq_ignore_ascii_case("DesktopServices") {
        ["FileSystem", "WimpSystemMenu"].as_slice()
    } else if module_name.eq_ignore_ascii_case("DisplayManager") {
        ["DisplaySettings"].as_slice()
    } else if module_name.eq_ignore_ascii_case("Wimp") {
        ["RuntimeErrors", "WimpTaskLifecycle", "WimpWindowState"].as_slice()
    } else if module_name.eq_ignore_ascii_case("FileSwitch") {
        [
            "FileSystem",
            "RuntimeErrors",
            "SystemVariableStore",
            "TaskMemory",
        ]
        .as_slice()
    } else if module_name.eq_ignore_ascii_case("Boot") {
        [].as_slice()
    } else if module_name.eq_ignore_ascii_case("System") {
        ["StartupPolicy", "SystemQueries", "SystemVariableStore"].as_slice()
    } else if module_name.eq_ignore_ascii_case("Error") {
        ["ErrorDispatch"].as_slice()
    } else if module_name.eq_ignore_ascii_case("Memory") {
        ["RuntimeErrors", "TaskMemory"].as_slice()
    } else if module_name.eq_ignore_ascii_case("ModuleManager") {
        ["ModuleIntrospection", "ModuleManagement", "RuntimeErrors"].as_slice()
    } else if module_name.eq_ignore_ascii_case("RicochetCommands") {
        [
            "CommandRegistry",
            "ConfigurationStoreRead",
            "ConfigurationStoreWrite",
            "CommandScripts",
            "ExecInput",
            "RuntimeErrors",
            "TaskMemory",
        ]
        .as_slice()
    } else if module_name.eq_ignore_ascii_case("TaskManager") {
        ["RuntimeErrors", "TaskQuery"].as_slice()
    } else {
        [].as_slice()
    };
    for grant in grants {
        if !permitted
            .iter()
            .any(|name| grant.as_str().eq_ignore_ascii_case(name))
        {
            return Err(format!(
                "host boot policy does not grant {} to module {module_name}",
                grant.as_str()
            ));
        }
    }
    Ok(())
}

fn read_capsule_abi(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 14 {
        None
    } else {
        Some(u32::from_be_bytes(bytes[10..14].try_into().ok()?))
    }
}

fn work_area_to_graphics_clip(
    work: WorkArea,
    extent: WorkArea,
    logical_height: i32,
) -> GraphicsWindow {
    let left = i64::from(work.min_x) - i64::from(extent.min_x);
    let right = i64::from(work.max_x) - 1 - i64::from(extent.min_x);
    let bottom = i64::from(logical_height) + i64::from(work.min_y) - i64::from(extent.max_y);
    let top = i64::from(logical_height) + i64::from(work.max_y) - 1 - i64::from(extent.max_y);
    GraphicsWindow {
        left: left.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        bottom: bottom.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        right: right.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        top: top.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    }
}

fn write_display_query(wimp: &WimpServer, context: &mut SwiContext) {
    let settings = wimp.display_settings();
    let metrics = wimp.desktop_metrics();
    let (width, height) = metrics.pixel_size();
    let (host_width, host_height) = metrics.host_pixel_size();
    context.registers[R2] = settings.resolution.id();
    context.registers[R3] = settings.colour.id();
    context.registers[R4] = width;
    context.registers[R5] = height;
    context.registers[R6] = host_width;
    context.registers[R7] = host_height;
}

fn normalize_swi_error(error: RuntimeError) -> RuntimeError {
    match error {
        RuntimeError::InvalidSwi(number) => RuntimeError::Structured {
            type_name: "UnknownSwi".into(),
            code: SWI_UNKNOWN_ERROR_CODE,
            message: format!("no such SWI &{number:X}"),
        },
        RuntimeError::StandardErrorBlock { code, message } => RuntimeError::Structured {
            type_name: "OSError".into(),
            code,
            message,
        },
        error => error,
    }
}

fn named_colourtrans_contract(name: &str) -> SwiContract {
    let registers = match name {
        "COLOURTRANS_CONVERTHSVTORGB" => vec![
            RegisterContract {
                register: R0 as u8,
                kind: RegisterKind::Signed { bits: 32 },
                direction: ArgumentDirection::InOut,
            },
            RegisterContract {
                register: R1 as u8,
                kind: RegisterKind::Unsigned { bits: 32 },
                direction: ArgumentDirection::InOut,
            },
            RegisterContract {
                register: R2 as u8,
                kind: RegisterKind::Unsigned { bits: 32 },
                direction: ArgumentDirection::InOut,
            },
        ],
        "COLOURTRANS_SETGCOL" => vec![RegisterContract {
            register: R0 as u8,
            kind: RegisterKind::Unsigned { bits: 32 },
            direction: ArgumentDirection::In,
        }],
        _ => Vec::new(),
    };
    SwiContract {
        registers,
        logical_memory: Vec::new(),
        program_counter: None,
        carry: None,
        may_block: false,
        may_reenter: false,
        error_transport: "RuntimeResult".into(),
    }
}

fn named_project_service_contract(name: &str) -> SwiContract {
    let count = match name {
        "RICOCHET_DESKTOP" => 5,
        "RICOCHET_DISPLAY" => 9,
        _ => 0,
    };
    SwiContract {
        registers: (0..count)
            .map(|register| RegisterContract {
                register: register as u8,
                kind: RegisterKind::Unsigned { bits: 32 },
                direction: ArgumentDirection::InOut,
            })
            .collect(),
        logical_memory: Vec::new(),
        program_counter: None,
        carry: None,
        may_block: false,
        may_reenter: false,
        error_transport: "RuntimeResult".into(),
    }
}

fn module_service_error(type_name: &str, code: u32, message: impl Into<String>) -> RuntimeError {
    RuntimeError::Structured {
        type_name: type_name.into(),
        code,
        message: message.into(),
    }
}

fn normalize_definition_selector(definition: &str) -> String {
    let definition = definition.trim().to_ascii_uppercase();
    let (is_function, name) = definition
        .strip_prefix("FN:")
        .or_else(|| definition.strip_prefix("FN "))
        .map(|name| (true, name))
        .unwrap_or((false, definition.as_str()));
    if is_function {
        format!("FN:{name}")
    } else {
        name.to_owned()
    }
}

fn write_c_string_with_capacity(
    memory: &mut crate::memory::GuestMemory,
    address: u32,
    capacity: u32,
    value: &str,
    label: &str,
) -> Result<u32, RuntimeError> {
    let required = value
        .len()
        .checked_add(1)
        .ok_or(crate::memory::MemoryError::AddressOverflow)?;
    let capacity = usize::try_from(capacity).unwrap_or(usize::MAX);
    if capacity < required || capacity > 128 {
        return Err(module_service_error(
            "ModuleInfoBufferError",
            u32::try_from(required).unwrap_or(u32::MAX),
            format!(
                "{label} needs {required} bytes, caller supplied {} (maximum buffer is 128)",
                capacity
            ),
        ));
    }
    let mut terminated = Vec::with_capacity(required);
    terminated.extend_from_slice(value.as_bytes());
    terminated.push(0);
    memory.write_bytes(address, &terminated)?;
    Ok(u32::try_from(value.len()).unwrap_or(u32::MAX))
}

fn write_swi_name_with_capacity(
    memory: &mut crate::memory::GuestMemory,
    address: u32,
    capacity: u32,
    value: &str,
) -> Result<u32, RuntimeError> {
    let required = value
        .len()
        .checked_add(1)
        .ok_or(crate::memory::MemoryError::AddressOverflow)?;
    let capacity = usize::try_from(capacity).unwrap_or(usize::MAX);
    if capacity < required || capacity > SYSTEM_SWI_NAME_MAX_BYTES {
        return Err(module_service_error(
            "SwiNameBufferError",
            u32::try_from(required).unwrap_or(u32::MAX),
            format!(
                "SWI name needs {required} bytes, caller supplied {capacity} (maximum buffer is {SYSTEM_SWI_NAME_MAX_BYTES})"
            ),
        ));
    }
    let mut terminated = Vec::with_capacity(required);
    terminated.extend_from_slice(value.as_bytes());
    terminated.push(0);
    memory.write_bytes(address, &terminated)?;
    Ok(u32::try_from(value.len()).unwrap_or(u32::MAX))
}

fn read_control_terminated_bytes(
    memory: &crate::memory::GuestMemory,
    address: u32,
    maximum: usize,
) -> Result<Vec<u8>, RuntimeError> {
    let mut result = Vec::new();
    for offset in 0..maximum {
        let current = address
            .checked_add(
                u32::try_from(offset).map_err(|_| crate::memory::MemoryError::AddressOverflow)?,
            )
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = memory.read_byte(current)?;
        if byte <= b' ' {
            return Ok(result);
        }
        result.push(byte);
    }
    Err(module_service_error(
        "SwiNameInputError",
        1,
        format!("SWI name has no control/space terminator within {maximum} bytes"),
    ))
}

fn read_system_variable_selector(
    memory: &crate::memory::GuestMemory,
    address: u32,
) -> Result<String, RuntimeError> {
    let bytes = memory
        .read_c_string(address, MAX_NAME_BYTES + 2)
        .map_err(|error| match error {
            crate::memory::MemoryError::MissingNullTerminator(_) => {
                name_error("variable name/pattern is too long or not NUL-terminated")
            }
            other => RuntimeError::from(other),
        })?;
    let selector = String::from_utf8(bytes)
        .map_err(|_| name_error("variable names/patterns must be visible ASCII"))?;
    validate_selector(&selector)?;
    Ok(selector)
}

fn is_system_variable_not_found(error: &RuntimeError) -> bool {
    matches!(
        error,
        RuntimeError::Structured { type_name, code: 2, .. }
            if type_name == "SystemVariableNotFound"
    )
}

fn error_block_contents(error: &RuntimeError) -> (u32, String) {
    match error {
        RuntimeError::Structured {
            type_name: _,
            code,
            message,
        } => (*code, message.clone()),
        RuntimeError::StandardErrorBlock { code, message } => (*code, message.clone()),
        RuntimeError::InvalidSwi(number) => (*number, format!("no such SWI &{number:X}")),
        RuntimeError::EndOfInput => (3, "end of input".into()),
        RuntimeError::Memory(error) => (5, error.to_string()),
        RuntimeError::Io(error) => (4, error.to_string()),
        RuntimeError::Program(message) => (1, message.clone()),
    }
}

fn return_x_form_error(
    result: Result<(), RuntimeError>,
    task: &mut Task,
    context: &mut SwiContext,
) -> Result<(), RuntimeError> {
    match result {
        Ok(()) => {
            context.overflow = false;
            Ok(())
        }
        Err(RuntimeError::StandardErrorBlock { .. }) => {
            // XOS_GenerateError is the special historical X form: the caller
            // supplied the error block in R0, and the SWI returns with V set.
            // Unlike errors raised by other services, it does not need to copy
            // an error into the dispatcher's reserved scratch block.
            context.overflow = true;
            Ok(())
        }
        Err(error) => {
            let error = normalize_swi_error(error);
            let (code, message) = error_block_contents(&error);
            context.registers[R0] = task.memory.write_swi_error_block(code, &message);
            context.overflow = true;
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SwiContext {
    pub registers: [u32; 16],
    /// For OS_WriteS, this is the caller's saved return address: the first
    /// byte immediately following the SWI instruction.
    pub pc: u32,
    pub carry: bool,
    pub negative: bool,
    pub zero: bool,
    /// ARM V/overflow flag. In the hosted dispatcher, X-form errors return a
    /// caller-scoped standard error block in R0 and set this flag.
    pub overflow: bool,
}

impl SwiContext {
    pub fn returned_flags(&self) -> u32 {
        (u32::from(self.negative) << 3)
            | (u32::from(self.zero) << 2)
            | (u32::from(self.carry) << 1)
            | u32::from(self.overflow)
    }
}

struct ManagedResourceHandle {
    type_name: String,
    owner_task: u64,
    rights: BTreeSet<ResourceRight>,
    bytes: Vec<u8>,
}

struct ObeyScriptFrame {
    handle: u32,
    source_path: String,
    obey_directory: String,
    arguments: String,
    bytes: Vec<u8>,
    buffer_number: u32,
    buffer_base: u32,
    offset: usize,
    next_line: u32,
    current_line: Option<u32>,
}

#[derive(Default)]
struct ObeyScriptSession {
    frames: Vec<ObeyScriptFrame>,
    total_bytes: usize,
    total_lines: usize,
}

pub struct SwiDispatcher {
    mos: mos::MosState,
    configure: ConfigureStore,
    console: HostConsole,
    graphics: GraphicsService,
    modern_shell_console: bool,
    task_default_graphics: HashMap<u64, GraphicsService>,
    window_graphics: HashMap<u32, GraphicsService>,
    active_graphics_window: Option<u32>,
    file_system: HostFileSystem,
    quit_requested: bool,
    desktop_requested: bool,
    display_events: Option<Sender<DisplayEvent>>,
    display_task_id: u64,
    wimp: Option<Arc<WimpServer>>,
    desktop_service: Option<Arc<WimpServer>>,
    display_batch_active: bool,
    last_display_batch_publish: Option<Instant>,
    last_inkey_poll: Instant,
    active_module: Option<ModuleId>,
    active_command_context: Option<CommandExecutionContext>,
    suppress_command_key_polling: bool,
    module_registry: ModuleRegistry,
    module_management_authority: ModuleManagementAuthority,
    module_programs: HashMap<DefinitionId, Arc<SystemModule>>,
    foundation_module_ids: BTreeSet<ModuleId>,
    resource_handles: HashMap<u32, ManagedResourceHandle>,
    next_resource_handle: u32,
    derived_targets: DerivedTargetCache<Vec<u8>>,
    last_dispatch_route: Option<SwiDispatchRoute>,
    module_dispatch_count: u64,
    transitional_dispatch_count: u64,
    startup_target: Option<BootStartupTarget>,
    boot_failure: Option<BootFailure>,
    system_variables: SystemVariableStore,
    obey_scripts: HashMap<u64, ObeyScriptSession>,
    next_obey_handle: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BootStartupTarget {
    MosPrompt,
    Desktop,
}

/// Typed caller-side provenance for one command dispatch. The current source
/// is interactive OS_CLI; `source_path`/`source_line` reserve explicit slots
/// for future Obey/Exec routing without implementing those facilities here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandExecutionContext {
    pub caller_task: u64,
    pub command: String,
    pub module: String,
    pub module_version: crate::ricochet::SemanticVersion,
    pub module_source_hash: String,
    pub definition_id: Option<u64>,
    pub raw_arguments: String,
    pub origin: CommandInvocationOrigin,
    pub source_path: Option<String>,
    pub source_line: Option<u32>,
    pub depth: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandInvocationOrigin {
    InteractiveCli,
    NestedCli,
}

/// Trusted host-side control plane for loading and replacing BASIC64 system
/// definitions. It is deliberately not exposed as a guest SWI or BASIC value.
pub struct Basic64ModuleManager<'a> {
    dispatcher: &'a mut SwiDispatcher,
    authority: ModuleManagementAuthority,
}

impl Basic64ModuleManager<'_> {
    pub fn replace_swi_definition(
        &mut self,
        number: u32,
        source: &str,
        source_path: &str,
    ) -> Result<crate::ricochet::GenerationId, RuntimeError> {
        self.dispatcher
            .replace_basic64_swi(&self.authority, number, source, source_path)
    }

    pub fn quiesce(&mut self, module_name: &str, task: &mut Task) -> Result<(), RuntimeError> {
        self.dispatcher
            .quiesce_basic64_module(&self.authority, module_name, task)
    }

    pub fn retire(&mut self, module_name: &str, task: &mut Task) -> Result<(), RuntimeError> {
        self.dispatcher
            .retire_basic64_module(&self.authority, module_name, task)
    }
}

impl SwiDispatcher {
    pub fn new(console: HostConsole) -> Self {
        Self::with_display_events(console, None, 1, None)
    }

    fn invoke_registered_command(
        &mut self,
        active: ActiveCommand,
        arguments: String,
        task: &mut Task,
        _context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let previous = self.active_command_context.take();
        let depth = previous
            .as_ref()
            .map_or(0, |parent| parent.depth.saturating_add(1));
        if depth >= 64 {
            self.active_command_context = previous;
            return Err(RuntimeError::Program(
                "command nesting exceeds the hosted limit of 64".into(),
            ));
        }
        let origin = if previous.is_some() {
            CommandInvocationOrigin::NestedCli
        } else {
            CommandInvocationOrigin::InteractiveCli
        };
        let definition_id = active
            .definition
            .as_ref()
            .map(|definition| definition.id.diagnostic_value());
        let command_name = active.command.name.clone();
        let previous_key_poll_suppression = self.suppress_command_key_polling;
        let script_source = self
            .obey_scripts
            .get(&task.id)
            .and_then(|session| session.frames.last())
            .and_then(|frame| {
                frame
                    .current_line
                    .map(|line| (frame.source_path.clone(), line))
            });
        let exec_source = task
            .exec_input_provenance()
            .map(|(path, line)| (path.to_owned(), line));
        // BASIC64 command-policy procedures can run for many interpreter
        // steps (notably *HELP), but must not opportunistically consume the
        // user's next terminal character while formatting a response. A
        // Rust BRIDGE may launch an interactive BASIC program, so that path
        // explicitly restores normal key polling for its duration.
        self.suppress_command_key_polling =
            matches!(&active.command.handler, CommandHandler::Basic64Proc(_));
        self.active_command_context = Some(CommandExecutionContext {
            caller_task: task.id,
            command: active.command.name.clone(),
            module: active.module_name.clone(),
            module_version: active.module_version,
            module_source_hash: active.source_hash.clone(),
            definition_id,
            raw_arguments: arguments.clone(),
            origin,
            source_path: script_source
                .as_ref()
                .map(|(path, _)| path.clone())
                .or_else(|| exec_source.as_ref().map(|(path, _)| path.clone()))
                .or_else(|| {
                    previous
                        .as_ref()
                        .and_then(|parent| parent.source_path.clone())
                }),
            source_line: script_source
                .as_ref()
                .map(|(_, line)| *line)
                .or_else(|| exec_source.as_ref().map(|(_, line)| *line))
                .or_else(|| previous.as_ref().and_then(|parent| parent.source_line)),
            depth,
        });

        let mut result = match active.command.handler {
            CommandHandler::Basic64Proc(handler) => match active.definition {
                Some(definition) => match self.module_programs.get(&definition.id).cloned() {
                    Some(program) => self.with_module_execution(active.module, |dispatcher| {
                        program.invoke_command_handler(
                            active.module,
                            &handler,
                            command_name.clone(),
                            arguments,
                            task,
                            dispatcher,
                        )
                    }),
                    None => Err(RuntimeError::Program(format!(
                        "command handler source for {} is not retained",
                        active.command.name
                    ))),
                },
                None => Err(RuntimeError::Program(format!(
                    "active command {} has no resolved BASIC64 definition",
                    active.command.name
                ))),
            },
            CommandHandler::RustBridge => {
                self.execute_cli_command(task, &active.command.name, &arguments)
            }
        };
        if script_source.is_none()
            && let (Err(error), Some((path, line))) = (&result, &exec_source)
            && !matches!(
                error,
                RuntimeError::Structured { type_name, .. }
                    if type_name == "ObeySourceError" || type_name == "ExecInputSourceError"
            )
        {
            result = Err(exec_input_source_error(path, *line, error.to_string()));
        }
        self.active_command_context = previous;
        self.suppress_command_key_polling = previous_key_poll_suppression;
        result
    }

    pub(crate) fn call_module_primitive(
        &mut self,
        name: &str,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        self.invoke_module_primitive(name, task, context)
    }

    pub(crate) fn module_primitive_signature(
        &self,
        name: &str,
    ) -> Result<crate::ricochet::PrimitiveDescriptor, RuntimeError> {
        let module_id = self.active_module.ok_or_else(|| {
            RuntimeError::Program(format!(
                "primitive {name} was inspected outside a BASIC64 module invocation"
            ))
        })?;
        self.module_registry
            .authorized_primitive(module_id, name)
            .map_err(|error| RuntimeError::Program(error.to_string()))
    }

    /// Trusted host-side registration path for managed byte resources. The
    /// returned value is an opaque guest identity, never a host pointer.
    pub(crate) fn register_resource_buffer(
        &mut self,
        type_name: &str,
        owner_task: u64,
        bytes: Vec<u8>,
        rights: impl IntoIterator<Item = ResourceRight>,
    ) -> Result<u32, RuntimeError> {
        let handle = self.next_resource_handle;
        self.next_resource_handle = handle
            .checked_add(1)
            .ok_or_else(|| RuntimeError::Program("resource handle space exhausted".into()))?;
        self.resource_handles.insert(
            handle,
            ManagedResourceHandle {
                type_name: type_name.to_ascii_uppercase(),
                owner_task,
                rights: rights.into_iter().collect(),
                bytes,
            },
        );
        Ok(handle)
    }

    fn validate_resource_handle_access(
        &self,
        handle: u32,
        type_name: &str,
        task_id: u64,
        right: ResourceRight,
    ) -> Result<(), RuntimeError> {
        let resource = self.resource_handles.get(&handle).ok_or_else(|| {
            RuntimeError::Program("opaque resource handle is not registered".into())
        })?;
        if !resource.type_name.eq_ignore_ascii_case(type_name) {
            return Err(RuntimeError::Program(format!(
                "resource handle has type {}, expected {type_name}",
                resource.type_name
            )));
        }
        if resource.owner_task != task_id {
            return Err(RuntimeError::Program(
                "resource handle belongs to a different caller task".into(),
            ));
        }
        if !resource.rights.contains(&right) {
            return Err(RuntimeError::Program(format!(
                "resource handle lacks {right:?} authority"
            )));
        }
        Ok(())
    }

    fn read_resource_byte(
        &self,
        handle: u32,
        type_name: &str,
        task_id: u64,
        offset: u32,
    ) -> Result<u8, RuntimeError> {
        self.validate_resource_handle_access(handle, type_name, task_id, ResourceRight::Read)?;
        self.resource_handles[&handle]
            .bytes
            .get(offset as usize)
            .copied()
            .ok_or_else(|| RuntimeError::Program("resource offset is out of range".into()))
    }

    pub(crate) fn resolve_imported_basic64_symbol(
        &self,
        provider_name: &str,
        symbol: &str,
    ) -> Result<(ModuleId, String, Arc<SystemModule>), RuntimeError> {
        let caller_id = self.active_module.ok_or_else(|| {
            RuntimeError::Program(
                "qualified module symbol was invoked outside a BASIC64 module".into(),
            )
        })?;
        let caller = self
            .module_registry
            .module(caller_id)
            .ok_or_else(|| RuntimeError::Program("calling module has been unloaded".into()))?;
        if !caller.manifest.symbol_imports.iter().any(|import| {
            import.module.eq_ignore_ascii_case(provider_name)
                && import.symbol.eq_ignore_ascii_case(symbol)
        }) {
            return Err(RuntimeError::Program(format!(
                "module {} has no linked import for {}.{}",
                caller.manifest.name, provider_name, symbol
            )));
        }
        let provider = self
            .module_registry
            .module_named(provider_name)
            .ok_or_else(|| RuntimeError::Program(format!("unknown module {provider_name}")))?;
        if provider.state != ModuleState::Active {
            return Err(RuntimeError::Program(format!(
                "imported module {} is not active",
                provider.manifest.name
            )));
        }
        if !provider
            .manifest
            .symbol_exports
            .iter()
            .any(|export| export.eq_ignore_ascii_case(symbol))
        {
            return Err(RuntimeError::Program(format!(
                "module {} does not export {symbol}",
                provider.manifest.name
            )));
        }
        let definition = provider.definitions.get(symbol).ok_or_else(|| {
            RuntimeError::Program(format!(
                "module {} has no definition for {symbol}",
                provider.manifest.name
            ))
        })?;
        let source = self
            .module_programs
            .get(&definition.id)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::Program(format!(
                    "exported symbol {provider_name}.{symbol} has no retained BASIC64 source"
                ))
            })?;
        Ok((provider.id, symbol.to_owned(), source))
    }

    pub(crate) fn with_module_execution<T>(
        &mut self,
        module: ModuleId,
        execute: impl FnOnce(&mut Self) -> Result<T, RuntimeError>,
    ) -> Result<T, RuntimeError> {
        let previous = self.active_module.replace(module);
        let result = execute(self);
        self.active_module = previous;
        result
    }

    pub fn last_dispatch_route(&self) -> Option<&SwiDispatchRoute> {
        self.last_dispatch_route.as_ref()
    }

    pub fn module_dispatch_count(&self) -> u64 {
        self.module_dispatch_count
    }

    pub fn transitional_dispatch_count(&self) -> u64 {
        self.transitional_dispatch_count
    }

    pub fn module_registry(&self) -> &ModuleRegistry {
        &self.module_registry
    }

    /// Borrows a dispatcher into the trusted, host-side module management
    /// surface. BASIC code has no path to construct or request this manager.
    pub fn basic64_module_manager(&mut self) -> Basic64ModuleManager<'_> {
        Basic64ModuleManager {
            authority: self.module_management_authority.clone(),
            dispatcher: self,
        }
    }

    #[cfg(test)]
    pub(crate) fn module_management_authority(&self) -> &ModuleManagementAuthority {
        &self.module_management_authority
    }

    fn check_module_management_authority(
        &self,
        authority: &ModuleManagementAuthority,
    ) -> Result<(), RuntimeError> {
        if self
            .module_registry
            .accepts_module_management_authority(&self.module_management_authority, authority)
        {
            Ok(())
        } else {
            Err(RuntimeError::Program(
                "module operation requires this dispatcher’s module-management authority".into(),
            ))
        }
    }

    fn collect_retired_module_programs(&mut self) {
        let retained = self.module_registry.retained_definition_ids();
        self.module_programs
            .retain(|definition_id, _| retained.contains(definition_id));
    }

    /// Internal module-manager lifecycle path. No guest SWI exposes this
    /// authority; the token is issued only to trusted Rust management code.
    pub fn quiesce_basic64_module(
        &mut self,
        authority: &ModuleManagementAuthority,
        module_name: &str,
        task: &mut Task,
    ) -> Result<(), RuntimeError> {
        self.check_module_management_authority(authority)?;
        let record = self
            .module_registry
            .module_named(module_name)
            .ok_or_else(|| RuntimeError::Program(format!("unknown module {module_name}")))?;
        let module_id = record.id;
        let source_path = record.manifest.source_path.as_str();
        let source_hash = record.manifest.source_hash.as_str();
        let program = self
            .module_programs
            .values()
            .find(|program| {
                program.manifest.name.eq_ignore_ascii_case(module_name)
                    && program.manifest.source_path == source_path
                    && program.manifest.source_hash == source_hash
            })
            .cloned()
            .ok_or_else(|| RuntimeError::Program("module has no retained BASIC64 source".into()))?;
        self.module_registry
            .quiesce_module(module_id)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        let active_calls = match self.module_registry.module_active_call_count(module_id) {
            Ok(active_calls) => active_calls,
            Err(error) => {
                let _ = self.module_registry.rollback_module_quiesce(module_id);
                return Err(RuntimeError::Program(error.to_string()));
            }
        };
        if active_calls != 0 {
            self.module_registry
                .rollback_module_quiesce(module_id)
                .map_err(|error| RuntimeError::Program(error.to_string()))?;
            return Err(RuntimeError::Program(format!(
                "module {module_name} still has {active_calls} active call(s)"
            )));
        }
        if let Err(error) = program.invoke_lifecycle_transactional("QUIESCE", module_id, task, self)
        {
            self.module_registry
                .rollback_module_quiesce(module_id)
                .map_err(|rollback| {
                    RuntimeError::Program(format!(
                        "quiesce failed: {error}; restoring active state also failed: {rollback}"
                    ))
                })?;
            return Err(error);
        }
        Ok(())
    }

    /// Runs Finalise only after all active SWI generations have drained, then
    /// releases the module's exports and retained workspace.
    pub fn retire_basic64_module(
        &mut self,
        authority: &ModuleManagementAuthority,
        module_name: &str,
        task: &mut Task,
    ) -> Result<(), RuntimeError> {
        self.check_module_management_authority(authority)?;
        let record = self
            .module_registry
            .module_named(module_name)
            .ok_or_else(|| RuntimeError::Program(format!("unknown module {module_name}")))?;
        if record.manifest.name.eq_ignore_ascii_case("Mos") {
            return Err(RuntimeError::Program(format!(
                "MOS SWI owner {} cannot be retired",
                record.manifest.name
            )));
        }
        if record.state != ModuleState::Quiescing {
            return Err(RuntimeError::Program(format!(
                "module {module_name} must be quiesced before Finalise"
            )));
        }
        let module_id = record.id;
        let program_ids = self
            .module_programs
            .iter()
            .filter_map(|(definition_id, program)| {
                program
                    .manifest
                    .name
                    .eq_ignore_ascii_case(module_name)
                    .then_some(*definition_id)
            })
            .collect::<Vec<_>>();
        let source_path = record.manifest.source_path.as_str();
        let source_hash = record.manifest.source_hash.as_str();
        let program = program_ids
            .iter()
            .filter_map(|definition_id| self.module_programs.get(definition_id))
            .find(|program| {
                program.manifest.source_path == source_path
                    && program.manifest.source_hash == source_hash
            })
            .cloned()
            .ok_or_else(|| RuntimeError::Program("module has no retained BASIC64 source".into()))?;
        let active_calls = self
            .module_registry
            .module_active_call_count(module_id)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        if active_calls != 0 {
            return Err(RuntimeError::Program(format!(
                "module {module_name} still has {active_calls} active call(s)"
            )));
        }
        program.invoke_lifecycle_transactional("FINALISE", module_id, task, self)?;
        self.module_registry
            .retire_module(module_id)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        for definition_id in program_ids {
            self.module_programs.remove(&definition_id);
        }
        Ok(())
    }

    /// Replaces one published BASIC64 definition while preserving its SWI
    /// entry-cell identity and public register/memory contract.
    pub fn replace_basic64_swi(
        &mut self,
        authority: &ModuleManagementAuthority,
        number: u32,
        source: &str,
        source_path: impl Into<String>,
    ) -> Result<crate::ricochet::GenerationId, RuntimeError> {
        self.check_module_management_authority(authority)?;
        let source_path = source_path.into();
        let owner = self
            .module_registry
            .swi_module_name(number)
            .ok_or_else(|| RuntimeError::InvalidSwi(number))?
            .to_owned();
        let existing_contract = self
            .module_registry
            .swi_contract(number)
            .cloned()
            .ok_or_else(|| RuntimeError::InvalidSwi(number))?;
        let mut module =
            SystemModule::parse(source, source_path, &self.module_registry.allocator())?;
        if !module.manifest.name.eq_ignore_ascii_case(&owner) {
            return Err(RuntimeError::Program(format!(
                "replacement module {} does not own SWI {owner}",
                module.manifest.name
            )));
        }
        let current_module = self
            .module_registry
            .module_named(&owner)
            .ok_or_else(|| RuntimeError::Program(format!("owning module {owner} disappeared")))?;
        let old_manifest = current_module.manifest.clone();
        let old_definition = self
            .module_registry
            .current_swi_definition(number)
            .ok_or_else(|| RuntimeError::InvalidSwi(number))?;
        let old_program = self
            .module_programs
            .get(&old_definition.id)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::Program("active definition has no retained module source".into())
            })?;
        if module.manifest.version != old_manifest.version
            || module.manifest.language_profile != old_manifest.language_profile
            || module.manifest.target_profile != old_manifest.target_profile
            || module.manifest.dependencies != old_manifest.dependencies
            || module.manifest.symbol_imports != old_manifest.symbol_imports
            || module.manifest.primitive_imports != old_manifest.primitive_imports
            || module.manifest.requested_capabilities != old_manifest.requested_capabilities
            || module.manifest.lifecycle != old_manifest.lifecycle
            || module.manifest.replacement_policy != old_manifest.replacement_policy
            || module.manifest.symbol_exports != old_manifest.symbol_exports
            || !same_export_contracts(&module.manifest.exports, &old_manifest.exports)
        {
            return Err(RuntimeError::Program(
                "replacement changes module version, dependencies, capabilities, lifecycle, policy, or the public export set".into(),
            ));
        }
        module.inherit_workspace(&old_program)?;
        module.validate_primitive_shapes(&self.module_registry.primitives)?;
        let export = module
            .manifest
            .exports
            .iter()
            .find(|export| export.number == number)
            .ok_or_else(|| {
                RuntimeError::Program(format!(
                    "replacement module {} does not export SWI &{number:X}",
                    module.manifest.name
                ))
            })?;
        if export.contract != existing_contract {
            return Err(RuntimeError::Program(
                "replacement changes the published SWI contract".into(),
            ));
        }
        let mut replacement = module
            .definitions
            .get(&export.definition_name)
            .cloned()
            .ok_or_else(|| {
                RuntimeError::Program(format!(
                    "replacement has no BASIC64 definition {}",
                    export.definition_name
                ))
            })?;
        let generation = self
            .module_registry
            .replace_swi_definition(number, &existing_contract, replacement.clone())
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        self.derived_targets
            .invalidate_definition(old_definition.id);
        self.derived_targets.invalidate_dependency(
            &owner,
            &DependencyFingerprint {
                module: owner.to_owned(),
                version: module.manifest.version,
                source_path: module.manifest.source_path.clone(),
                source_hash: module.manifest.source_hash.clone(),
                language_profile: module.manifest.language_profile.clone(),
                target_profile: module.manifest.target_profile.clone(),
            },
        );
        replacement.module = self
            .module_registry
            .module_named(&owner)
            .map(|module| module.id)
            .ok_or_else(|| {
                RuntimeError::Program(format!(
                    "owning module {owner} disappeared during replacement"
                ))
            })?;
        self.module_programs
            .insert(replacement.id, Arc::new(module));
        self.collect_retired_module_programs();
        Ok(generation)
    }

    pub fn windowed(console: HostConsole, display_events: Sender<DisplayEvent>) -> Self {
        Self::with_display_events(console, Some(display_events), 1, None)
    }

    pub(crate) fn windowed_with_desktop(
        console: HostConsole,
        display_events: Sender<DisplayEvent>,
        wimp: Arc<WimpServer>,
    ) -> Self {
        let mut dispatcher = Self::with_display_events(console, Some(display_events), 1, None);
        let configure = wimp
            .configure_store()
            .unwrap_or_else(|| dispatcher.configure.clone());
        dispatcher.configure = configure.clone();
        wimp.bind_configure_store(configure);
        dispatcher.desktop_service = Some(wimp);
        dispatcher
    }

    pub(crate) fn desktop_task(
        console: HostConsole,
        display_events: Sender<DisplayEvent>,
        task_id: u64,
        wimp: Arc<WimpServer>,
    ) -> Self {
        Self::with_display_events(console, Some(display_events), task_id, Some(wimp))
    }

    fn with_display_events(
        console: HostConsole,
        display_events: Option<Sender<DisplayEvent>>,
        display_task_id: u64,
        wimp: Option<Arc<WimpServer>>,
    ) -> Self {
        let module_registry = ModuleRegistry::new();
        let module_management_authority = module_registry.issue_module_management_authority();
        let mut dispatcher = Self {
            mos: mos::MosState::default(),
            configure: ConfigureStore::default(),
            console,
            graphics: GraphicsService::default(),
            modern_shell_console: false,
            task_default_graphics: HashMap::new(),
            window_graphics: HashMap::new(),
            active_graphics_window: None,
            file_system: HostFileSystem::demo_default(),
            quit_requested: false,
            desktop_requested: false,
            display_events,
            display_task_id,
            wimp,
            desktop_service: None,
            display_batch_active: false,
            last_display_batch_publish: None,
            last_inkey_poll: Instant::now(),
            active_module: None,
            active_command_context: None,
            suppress_command_key_polling: false,
            module_registry,
            module_management_authority,
            module_programs: HashMap::new(),
            foundation_module_ids: BTreeSet::new(),
            resource_handles: HashMap::new(),
            next_resource_handle: 1,
            derived_targets: DerivedTargetCache::default(),
            last_dispatch_route: None,
            module_dispatch_count: 0,
            transitional_dispatch_count: 0,
            startup_target: None,
            boot_failure: None,
            system_variables: SystemVariableStore::default(),
            obey_scripts: HashMap::new(),
            next_obey_handle: 1,
        };
        if let Some(path) = std::env::var_os("RICOCHET_BOOT_CAPSULE")
            .or_else(|| std::env::var_os("ACORN_BOOT_CAPSULE"))
        {
            let path = std::path::PathBuf::from(path);
            let display_path = path.display().to_string();
            match std::fs::read(&path) {
                Ok(bytes) => {
                    if let Err(failure) = dispatcher.bootstrap_capsule(&bytes) {
                        dispatcher.boot_failure = Some(failure);
                    }
                }
                Err(error) => {
                    dispatcher.reset_boot_registry();
                    dispatcher.boot_failure =
                        Some(BootFailure::host_capsule_read(&display_path, error));
                }
            }
        } else {
            match embedded_capsule_bytes() {
                Ok(bytes) => {
                    if let Err(failure) = dispatcher.bootstrap_capsule(bytes) {
                        dispatcher.boot_failure = Some(failure);
                    }
                }
                Err(error) => {
                    dispatcher.reset_boot_registry();
                    dispatcher.boot_failure =
                        Some(BootFailure::from_capsule(BootStage::CapsuleBuild, error));
                }
            }
        }
        dispatcher
    }

    fn reset_boot_registry(&mut self) {
        self.module_registry = ModuleRegistry::new();
        self.module_management_authority = self.module_registry.issue_module_management_authority();
        self.module_programs.clear();
        self.foundation_module_ids.clear();
        self.active_module = None;
        self.startup_target = None;
        self.register_boot_primitives();
    }

    fn register_boot_primitives(&mut self) {
        let byte = RegisterKind::Unsigned { bits: 8 };
        let boolean = RegisterKind::Unsigned { bits: 1 };
        let status = RegisterKind::Unsigned { bits: 32 };
        let address = RegisterKind::LogicalAddress { bits: 32 };
        let s32 = RegisterKind::Signed { bits: 32 };
        self.module_registry
            .primitives
            .register(
                "Host.Resource.ReadByte",
                CapabilityName::new("ResourceRead").expect("static capability is valid"),
                vec![
                    RegisterKind::OpaqueHandle {
                        type_name: "BufferHandle".into(),
                    },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![byte.clone()],
                false,
                true,
                Vec::new(),
                "InvalidHandleOrRange",
            )
            .expect("embedded resource primitive names are unique");
        self.module_registry
            .primitives
            .require_resource_right("Host.Resource.ReadByte", 0, ResourceRight::Read)
            .expect("resource byte reads declare read authority");
        self.module_registry
            .primitives
            .register(
                "Host.Graphics.AcceptByte",
                CapabilityName::new("GraphicsVduStream").expect("static capability is valid"),
                vec![byte.clone()],
                vec![byte.clone(), boolean.clone()],
                false,
                true,
                Vec::new(),
                "RuntimeResult",
            )
            .expect("embedded Console primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Graphics.Plot",
                CapabilityName::new("GraphicsRaster").expect("static capability is valid"),
                vec![status.clone(), status.clone(), status.clone()],
                Vec::new(),
                false,
                true,
                Vec::new(),
                "GraphicsError",
            )
            .expect("embedded Graphics primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Graphics.SetPackedRgb",
                CapabilityName::new("GraphicsRaster").expect("static capability is valid"),
                vec![status.clone()],
                Vec::new(),
                false,
                true,
                Vec::new(),
                "RuntimeResult",
            )
            .expect("embedded ColourTrans primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Graphics.ReadPoint",
                CapabilityName::new("GraphicsRaster").expect("static capability is valid"),
                vec![status.clone(), status.clone()],
                vec![
                    status.clone(),
                    status.clone(),
                    status.clone(),
                    status.clone(),
                ],
                false,
                true,
                Vec::new(),
                "GraphicsError",
            )
            .expect("embedded Graphics primitive names are unique");
        let desktop_file_system =
            CapabilityName::new("FileSystem").expect("static capability is valid");
        let desktop_menu =
            CapabilityName::new("WimpSystemMenu").expect("static capability is valid");
        for (name, capability, arguments, results, memory_rules, error_type) in [
            (
                "Host.Desktop.ReadCatalogueEntry",
                desktop_file_system.clone(),
                vec![
                    status.clone(),
                    status.clone(),
                    status.clone(),
                    status.clone(),
                ],
                vec![
                    status.clone(),
                    status.clone(),
                    status.clone(),
                    status.clone(),
                ],
                vec![
                    LogicalMemoryContract::Read {
                        register: 0,
                        max_bytes: Some(MAX_STRING_BYTES as u32),
                    },
                    LogicalMemoryContract::Write {
                        register: 2,
                        max_bytes: Some(MAX_STRING_BYTES as u32),
                    },
                ],
                "DesktopCatalogueOrCallerBufferError",
            ),
            (
                "Host.Desktop.WriteVolumeName",
                desktop_file_system.clone(),
                vec![status.clone(), status.clone()],
                vec![status.clone()],
                vec![LogicalMemoryContract::Write {
                    register: 0,
                    max_bytes: Some(MAX_STRING_BYTES as u32),
                }],
                "DesktopVolumeNameOrCallerBufferError",
            ),
            (
                "Host.Desktop.ReadModificationTime",
                desktop_file_system,
                vec![status.clone(), status.clone()],
                vec![status.clone(), status.clone()],
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(MAX_STRING_BYTES as u32),
                }],
                "DesktopCatalogueError",
            ),
            (
                "Host.Desktop.RegisterSystemMenu",
                desktop_menu,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "DesktopMenuRegistrationError",
            ),
            (
                "Host.Display.Query",
                CapabilityName::new("DisplaySettings").expect("static capability is valid"),
                Vec::new(),
                vec![status.clone(); 6],
                Vec::new(),
                "DisplayQueryError",
            ),
            (
                "Host.Display.Apply",
                CapabilityName::new("DisplaySettings").expect("static capability is valid"),
                vec![status.clone(), status.clone()],
                vec![status.clone(); 7],
                Vec::new(),
                "DisplayApplyOrAuthorizationError",
            ),
            (
                "Host.Display.RequireConfigurationWrite",
                CapabilityName::new("DisplaySettings").expect("static capability is valid"),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "DisplayConfigurationAuthorizationError",
            ),
        ] {
            self.module_registry
                .primitives
                .register(
                    name,
                    capability,
                    arguments,
                    results,
                    false,
                    true,
                    memory_rules,
                    error_type,
                )
                .expect("embedded Desktop and Display primitives are unique");
        }
        let wimp_lifecycle = CapabilityName::new("WimpTaskLifecycle")
            .expect("static Wimp lifecycle capability is valid");
        let wimp_windows =
            CapabilityName::new("WimpWindowState").expect("static Wimp window capability is valid");
        for (name, arguments, results, error_type) in [
            (
                "Host.Wimp.OpenWindow",
                vec![
                    status.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                ],
                Vec::new(),
                "WimpWindowError",
            ),
            (
                "Host.Wimp.CloseWindow",
                vec![status.clone()],
                Vec::new(),
                "WimpWindowError",
            ),
            (
                "Host.Wimp.ReadWindowState",
                vec![status.clone()],
                vec![
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                ],
                "WimpWindowError",
            ),
            (
                "Host.Wimp.SetExtent",
                vec![
                    status.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                    s32.clone(),
                ],
                Vec::new(),
                "WimpWindowError",
            ),
        ] {
            self.module_registry
                .primitives
                .register(
                    name,
                    wimp_windows.clone(),
                    arguments,
                    results,
                    false,
                    true,
                    Vec::new(),
                    error_type,
                )
                .expect("embedded Wimp window primitive names are unique");
        }
        for (name, arguments, results, memory_rules, error_type) in [
            (
                "Host.Wimp.ValidateInitialiseInputs",
                vec![status.clone(), address.clone(), address.clone()],
                Vec::new(),
                Vec::new(),
                "CheckedWimpInitialiseInputError",
            ),
            (
                "Host.Wimp.RegisterTask",
                Vec::new(),
                vec![status.clone(), status.clone()],
                Vec::new(),
                "WimpTaskRegistrationError",
            ),
            (
                "Host.Wimp.CloseTask",
                vec![status.clone()],
                Vec::new(),
                Vec::new(),
                "WimpTaskOwnershipError",
            ),
            (
                "Host.Wimp.ReadStartTaskByte",
                vec![address.clone(), status.clone()],
                vec![byte.clone()],
                Vec::new(),
                "CheckedWimpStartTaskCommandError",
            ),
            (
                "Host.Wimp.QueueCommandsTask",
                Vec::new(),
                vec![status.clone()],
                Vec::new(),
                "WimpTaskLaunchError",
            ),
            (
                "Host.Wimp.QueueBasicWindowTask",
                Vec::new(),
                vec![status.clone()],
                Vec::new(),
                "WimpTaskLaunchError",
            ),
            (
                "Host.Wimp.QueueBasicFileTask",
                vec![address.clone(), status.clone(), status.clone()],
                vec![status.clone()],
                Vec::new(),
                "CheckedWimpTaskPathOrLaunchError",
            ),
        ] {
            self.module_registry
                .primitives
                .register(
                    name,
                    wimp_lifecycle.clone(),
                    arguments,
                    results,
                    false,
                    true,
                    memory_rules,
                    error_type,
                )
                .expect("embedded Wimp lifecycle primitive names are unique");
        }
        self.module_registry
            .primitives
            .register(
                "Host.Console.ReadByteStatus",
                CapabilityName::new("ConsoleInput").expect("static capability is valid"),
                Vec::new(),
                vec![byte.clone(), status],
                true,
                false,
                Vec::new(),
                "RuntimeResult<ByteOrEndOfInput>",
            )
            .expect("embedded Console primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.MosInput.InsertKey",
                CapabilityName::new("MosInput").expect("static capability is valid"),
                vec![byte.clone()],
                vec![boolean.clone()],
                false,
                true,
                Vec::new(),
                "KeyboardBufferFull",
            )
            .expect("embedded MOS input primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.MosInput.FlushKeyboard",
                CapabilityName::new("MosInput").expect("static capability is valid"),
                Vec::new(),
                Vec::new(),
                false,
                true,
                Vec::new(),
                "KeyboardInputError",
            )
            .expect("embedded MOS input primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.MosInput.ReadTimed",
                CapabilityName::new("MosInput").expect("static capability is valid"),
                vec![RegisterKind::Unsigned { bits: 16 }],
                vec![byte.clone(), byte.clone()],
                true,
                false,
                Vec::new(),
                "TimedKeyboardInputError",
            )
            .expect("embedded MOS input primitive names are unique");
        for (name, capability, inputs, outputs) in [
            (
                "Host.Clock.ReadSystemChunks",
                "MosClock",
                Vec::new(),
                vec![
                    RegisterKind::Unsigned { bits: 16 },
                    RegisterKind::Unsigned { bits: 16 },
                    byte.clone(),
                ],
            ),
            (
                "Host.Clock.WriteSystemChunks",
                "MosClock",
                vec![
                    RegisterKind::Unsigned { bits: 16 },
                    RegisterKind::Unsigned { bits: 16 },
                    byte.clone(),
                ],
                Vec::new(),
            ),
            (
                "Host.Clock.ReadIntervalChunks",
                "MosClock",
                Vec::new(),
                vec![
                    RegisterKind::Unsigned { bits: 16 },
                    RegisterKind::Unsigned { bits: 16 },
                    byte.clone(),
                ],
            ),
            (
                "Host.Clock.WriteIntervalChunks",
                "MosClock",
                vec![
                    RegisterKind::Unsigned { bits: 16 },
                    RegisterKind::Unsigned { bits: 16 },
                    byte.clone(),
                ],
                Vec::new(),
            ),
        ] {
            self.module_registry
                .primitives
                .register(
                    name,
                    CapabilityName::new(capability).expect("static capability is valid"),
                    inputs,
                    outputs,
                    false,
                    true,
                    Vec::new(),
                    "ClockServiceError",
                )
                .expect("embedded MOS clock primitive names are unique");
        }
        self.module_registry
            .primitives
            .register(
                "Host.Memory.ReadFiveBytes",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                vec![address.clone()],
                vec![byte.clone(); 5],
                false,
                true,
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(5),
                }],
                "CheckedCallerMemoryError",
            )
            .expect("embedded MOS memory primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Memory.WriteFiveBytes",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                vec![
                    address.clone(),
                    byte.clone(),
                    byte.clone(),
                    byte.clone(),
                    byte.clone(),
                    byte.clone(),
                ],
                Vec::new(),
                false,
                true,
                vec![LogicalMemoryContract::Write {
                    register: 0,
                    max_bytes: Some(5),
                }],
                "CheckedCallerMemoryError",
            )
            .expect("embedded MOS memory primitive names are unique");
        let file_u32 = RegisterKind::Unsigned { bits: 32 };
        let file_s32 = RegisterKind::Signed { bits: 32 };
        let file_system = CapabilityName::new("FileSystem").expect("static capability is valid");
        for (name, arguments, results, memory_rules, failure) in [
            (
                "Host.FileChannel.Open",
                vec![file_u32.clone(), file_u32.clone()],
                vec![file_u32.clone(), file_u32.clone()],
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(MAX_STRING_BYTES as u32),
                }],
                "CheckedGuestPathOrOpenError",
            ),
            (
                "Host.FileChannel.Close",
                vec![file_u32.clone()],
                Vec::new(),
                Vec::new(),
                "TaskOwnedFileHandleError",
            ),
            (
                "Host.FileChannel.ReadByte",
                vec![file_u32.clone()],
                vec![byte.clone(), boolean.clone()],
                Vec::new(),
                "TaskOwnedFileReadError",
            ),
            (
                "Host.FileChannel.WriteByte",
                vec![file_u32.clone(), byte.clone()],
                Vec::new(),
                Vec::new(),
                "TaskOwnedFileWriteError",
            ),
            (
                "Host.FileChannel.ReadPosition",
                vec![file_u32.clone()],
                vec![file_s32.clone()],
                Vec::new(),
                "TaskOwnedFilePositionError",
            ),
            (
                "Host.FileChannel.SetPosition",
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                Vec::new(),
                "TaskOwnedFilePositionError",
            ),
            (
                "Host.FileChannel.ReadExtent",
                vec![file_u32.clone()],
                vec![file_s32.clone()],
                Vec::new(),
                "TaskOwnedFileExtentError",
            ),
            (
                "Host.FileChannel.SetExtent",
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                Vec::new(),
                "TaskOwnedFileExtentError",
            ),
            (
                "Host.FileChannel.CanonicalNameLength",
                vec![file_u32.clone()],
                vec![file_u32.clone()],
                Vec::new(),
                "TaskOwnedFileNameError",
            ),
            (
                "Host.FileChannel.Args7SpareBytes",
                vec![file_u32.clone(), file_u32.clone()],
                vec![RegisterKind::Signed { bits: 32 }],
                Vec::new(),
                "FileSwitchArgsArithmeticError",
            ),
            (
                "Host.FileChannel.WriteCanonicalName",
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                vec![LogicalMemoryContract::Write {
                    register: 1,
                    max_bytes: Some(MAX_STRING_BYTES as u32),
                }],
                "CheckedCallerMemoryOrFileNameError",
            ),
            (
                "Host.FileChannel.ValidateTransferSpan",
                vec![file_s32.clone(), file_s32.clone()],
                Vec::new(),
                Vec::new(),
                "CheckedCallerMemoryOrTransferBoundError",
            ),
            (
                "Host.FileChannel.ValidateDirectoryInfoAddress",
                vec![file_s32.clone()],
                Vec::new(),
                Vec::new(),
                "CheckedCallerMemoryAlignmentError",
            ),
            (
                "Host.FileChannel.ValidateTransferRange",
                vec![
                    file_u32.clone(),
                    file_s32.clone(),
                    file_s32.clone(),
                    boolean.clone(),
                ],
                vec![boolean.clone(), file_s32.clone()],
                Vec::new(),
                "TaskOwnedFileTransferError",
            ),
            (
                "Host.FileChannel.ValidateTransferRangeRaw",
                vec![
                    file_u32.clone(),
                    file_u32.clone(),
                    file_s32.clone(),
                    boolean.clone(),
                ],
                vec![boolean.clone(), file_s32.clone()],
                Vec::new(),
                "TaskOwnedFileTransferError",
            ),
            (
                "Host.FileChannel.ReadTransfer",
                vec![file_u32.clone(), file_s32.clone(), file_s32.clone()],
                vec![file_u32.clone()],
                Vec::new(),
                "TaskOwnedFileReadOrMemoryError",
            ),
            (
                "Host.FileChannel.WriteTransfer",
                vec![file_u32.clone(), file_s32.clone(), file_s32.clone()],
                Vec::new(),
                Vec::new(),
                "TaskOwnedFileWriteOrMemoryError",
            ),
            (
                "Host.FileChannel.FixedNameLength",
                vec![byte.clone()],
                vec![file_u32.clone()],
                Vec::new(),
                "TaskFileNameError",
            ),
            (
                "Host.FileChannel.FixedNameByte",
                vec![byte.clone(), file_u32.clone()],
                vec![byte.clone()],
                Vec::new(),
                "TaskFileNameError",
            ),
            (
                "Host.FileChannel.OpenDirectorySnapshot",
                vec![file_s32.clone(), file_s32.clone()],
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                "CheckedGuestPathOrCatalogueError",
            ),
            (
                "Host.FileChannel.ReadDirectoryEntry",
                vec![file_u32.clone(), file_u32.clone()],
                vec![file_u32.clone(); 2],
                Vec::new(),
                "TaskDirectorySnapshotError",
            ),
            (
                "Host.FileChannel.NormalizeDirectoryStart",
                vec![file_u32.clone(), file_s32.clone()],
                vec![file_u32.clone()],
                Vec::new(),
                "TaskDirectorySnapshotError",
            ),
            (
                "Host.FileChannel.ReadDirectoryFieldByte",
                vec![
                    file_u32.clone(),
                    file_u32.clone(),
                    byte.clone(),
                    byte.clone(),
                ],
                vec![byte.clone()],
                Vec::new(),
                "TaskDirectorySnapshotError",
            ),
            (
                "Host.FileChannel.ReadDirectoryNameByte",
                vec![file_u32.clone(), file_u32.clone(), file_u32.clone()],
                vec![byte.clone()],
                Vec::new(),
                "TaskDirectorySnapshotError",
            ),
            (
                "Host.FileChannel.WriteCallerByte",
                vec![file_s32.clone(), byte.clone()],
                Vec::new(),
                Vec::new(),
                "CheckedCallerMemoryError",
            ),
        ] {
            self.module_registry
                .primitives
                .register(
                    name,
                    file_system.clone(),
                    arguments,
                    results,
                    false,
                    true,
                    memory_rules,
                    failure,
                )
                .expect("embedded FileSwitch primitive names are unique");
        }
        let file_path_contract = |register| LogicalMemoryContract::Read {
            register,
            max_bytes: Some(MAX_STRING_BYTES as u32),
        };
        for (name, arguments, results, memory_rules, failure) in [
            (
                "Host.FileObject.Catalogue",
                vec![file_u32.clone()],
                vec![file_u32.clone(); 5],
                vec![file_path_contract(0)],
                "CheckedGuestPathOrFileCatalogueError",
            ),
            (
                "Host.FileObject.SaveBlock",
                vec![file_u32.clone(); 7],
                Vec::new(),
                vec![file_path_contract(0)],
                "CheckedGuestPathMemoryOrFileSaveError",
            ),
            (
                "Host.FileObject.LoadBlock",
                vec![file_u32.clone(), file_u32.clone(), file_u32.clone()],
                vec![file_u32.clone(); 4],
                vec![file_path_contract(0)],
                "CheckedGuestPathMemoryOrFileLoadError",
            ),
            (
                "Host.FileObject.SetMetadata",
                vec![file_u32.clone(); 6],
                Vec::new(),
                vec![file_path_contract(0)],
                "CheckedGuestPathOrFileMetadataError",
            ),
            (
                "Host.FileObject.CreateEmpty",
                vec![file_u32.clone(); 5],
                Vec::new(),
                vec![file_path_contract(0)],
                "CheckedGuestPathOrFileCreateError",
            ),
            (
                "Host.FileObject.CreateDirectory",
                vec![file_u32.clone()],
                Vec::new(),
                vec![file_path_contract(0)],
                "CheckedGuestPathOrDirectoryCreateError",
            ),
            (
                "Host.FileObject.DeleteFile",
                vec![file_u32.clone()],
                Vec::new(),
                vec![file_path_contract(0)],
                "CheckedGuestPathOrFileDeleteError",
            ),
            (
                "Host.FileObject.DeleteDirectory",
                vec![file_u32.clone()],
                Vec::new(),
                vec![file_path_contract(0)],
                "CheckedGuestPathOrDirectoryDeleteError",
            ),
            (
                "Host.FileObject.InvalidSaveRange",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "InvalidOSFileSaveRange",
            ),
            (
                "Host.FileObject.CatalogueCandidate",
                vec![file_u32.clone(); 4],
                vec![file_u32.clone(); 6],
                vec![LogicalMemoryContract::Read {
                    register: 1,
                    max_bytes: Some(MAX_STRING_BYTES as u32),
                }],
                "CheckedGuestPathOrFileCatalogueError",
            ),
            (
                "Host.FileObject.LoadCandidate",
                vec![file_u32.clone(); 6],
                vec![file_u32.clone(); 4],
                vec![LogicalMemoryContract::Read {
                    register: 1,
                    max_bytes: Some(MAX_STRING_BYTES as u32),
                }],
                "CheckedGuestPathMemoryOrFileLoadError",
            ),
            (
                "Host.FileObject.SearchPathNotFound",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "FileSearchPathNotFound",
            ),
        ] {
            self.module_registry
                .primitives
                .register(
                    name,
                    file_system.clone(),
                    arguments,
                    results,
                    false,
                    true,
                    memory_rules,
                    failure,
                )
                .expect("embedded FileSwitch object primitives are unique");
        }
        let fs_read_path = |register| LogicalMemoryContract::Read {
            register,
            max_bytes: Some(MAX_STRING_BYTES as u32),
        };
        let fs_address = RegisterKind::LogicalAddress { bits: 32 };
        for (name, arguments, results, memory_rules, failure) in [
            (
                "Host.FileSwitch.SetDirectory",
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                vec![fs_read_path(1)],
                "FileSwitchDirectoryError",
            ),
            (
                "Host.FileSwitch.UnsetDirectory",
                vec![file_u32.clone()],
                Vec::new(),
                Vec::new(),
                "FileSwitchDirectoryError",
            ),
            (
                "Host.FileSwitch.SwapDirectories",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "FileSwitchDirectoryError",
            ),
            (
                "Host.FileSwitch.SetTemporaryFromPrefix",
                vec![file_u32.clone()],
                vec![file_u32.clone(), file_u32.clone(), file_u32.clone()],
                vec![fs_read_path(0)],
                "FileSwitchSelectionError",
            ),
            (
                "Host.FileSwitch.ReadPathVariable",
                vec![file_u32.clone(), fs_address.clone()],
                vec![boolean.clone(), file_u32.clone()],
                vec![
                    fs_read_path(0),
                    LogicalMemoryContract::Write {
                        register: 1,
                        max_bytes: Some(256),
                    },
                ],
                "CheckedPathVariableOrCallerMemoryError",
            ),
            (
                "Host.FileSwitch.ReadPathSpecification",
                vec![file_u32.clone(), fs_address.clone()],
                vec![file_u32.clone()],
                vec![
                    fs_read_path(0),
                    LogicalMemoryContract::Write {
                        register: 1,
                        max_bytes: Some(256),
                    },
                ],
                "CheckedPathSpecificationOrCallerMemoryError",
            ),
            (
                "Host.FileSwitch.ReadPathName",
                vec![file_u32.clone(), fs_address.clone()],
                vec![file_u32.clone()],
                vec![
                    fs_read_path(0),
                    LogicalMemoryContract::Write {
                        register: 1,
                        max_bytes: Some(MAX_STRING_BYTES as u32),
                    },
                ],
                "CheckedGuestPathOrCallerMemoryError",
            ),
            (
                "Host.FileSwitch.CanonicalPathExists",
                vec![fs_address.clone()],
                vec![boolean.clone()],
                vec![fs_read_path(0)],
                "CheckedGuestPathError",
            ),
            (
                "Host.FileSwitch.RestoreTemporary",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "FileSwitchSelectionError",
            ),
            (
                "Host.FileSwitch.ProbeFileSystemNumber",
                vec![file_u32.clone()],
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                "FileSwitchSelectionError",
            ),
            (
                "Host.FileSwitch.ProbeFileSystemName",
                vec![file_u32.clone(), byte.clone()],
                vec![file_u32.clone(), file_u32.clone(), file_u32.clone()],
                vec![fs_read_path(0)],
                "FileSwitchSelectionError",
            ),
            (
                "Host.FileSwitch.SelectFileSystem",
                vec![file_u32.clone()],
                Vec::new(),
                Vec::new(),
                "FileSwitchSelectionError",
            ),
            (
                "Host.FileSwitch.ClearFileSystemSelection",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "FileSwitchSelectionError",
            ),
            (
                "Host.FileSwitch.CloseAllChannels",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                "TaskOwnedFileHandleError",
            ),
            (
                "Host.FileSwitch.RenameObject",
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                vec![fs_read_path(0), fs_read_path(1)],
                "CheckedGuestPathOrRenameError",
            ),
            (
                "Host.FileSwitch.SetVolumeName",
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                vec![fs_read_path(0), fs_read_path(1)],
                "CheckedGuestPathOrVolumeNameError",
            ),
            (
                "Host.FileSwitch.FileTypeText",
                vec![file_u32.clone()],
                vec![file_u32.clone(), file_u32.clone()],
                Vec::new(),
                "FileSwitchTypeError",
            ),
            (
                "Host.FileSwitch.ParseFileType",
                vec![file_u32.clone()],
                vec![file_u32.clone()],
                vec![fs_read_path(0)],
                "FileSwitchTypeError",
            ),
            (
                "Host.FileSwitch.FileSystemNameLength",
                vec![file_u32.clone()],
                vec![file_u32.clone()],
                Vec::new(),
                "FileSwitchNameError",
            ),
            (
                "Host.FileSwitch.FileSystemNameByte",
                vec![file_u32.clone(), file_u32.clone()],
                vec![byte.clone()],
                Vec::new(),
                "FileSwitchNameError",
            ),
            (
                "Host.FileSwitch.VolumeNameWriteLength",
                vec![file_u32.clone(), file_u32.clone()],
                vec![file_u32.clone()],
                Vec::new(),
                "FileSwitchNameError",
            ),
            (
                "Host.FileSwitch.CanonicalPathLength",
                vec![fs_address.clone()],
                vec![file_u32.clone()],
                vec![fs_read_path(0)],
                "CheckedGuestPathError",
            ),
            (
                "Host.FileSwitch.CanonicalPathByte",
                vec![fs_address.clone(), file_u32.clone()],
                vec![byte.clone()],
                vec![fs_read_path(0)],
                "CheckedGuestPathError",
            ),
            (
                "Host.FileSwitch.CanonicalCapacityFits",
                vec![file_s32.clone(), file_u32.clone()],
                vec![boolean.clone()],
                Vec::new(),
                "FileSwitchArgsArithmeticError",
            ),
            (
                "Host.FileSwitch.CanonicalSpareBytes",
                vec![file_s32.clone(), file_u32.clone()],
                vec![file_s32.clone()],
                Vec::new(),
                "FileSwitchArgsArithmeticError",
            ),
            (
                "Host.FileSwitch.WriteCanonicalPath",
                vec![fs_address.clone(), file_u32.clone(), file_u32.clone()],
                Vec::new(),
                vec![
                    fs_read_path(0),
                    LogicalMemoryContract::Write {
                        register: 1,
                        max_bytes: Some(MAX_STRING_BYTES as u32),
                    },
                ],
                "CheckedCallerMemoryOrFileNameError",
            ),
            (
                "Host.FileSwitch.OpenCatalogueSnapshot",
                vec![fs_address.clone(), fs_address.clone()],
                vec![file_u32.clone(), file_u32.clone()],
                vec![fs_read_path(0), fs_read_path(1)],
                "CheckedGuestPathOrCatalogueError",
            ),
        ] {
            self.module_registry
                .primitives
                .register(
                    name,
                    file_system.clone(),
                    arguments,
                    results,
                    false,
                    true,
                    memory_rules,
                    failure,
                )
                .expect("embedded FileSwitch policy primitives are unique");
        }
        self.module_registry
            .primitives
            .register(
                "Host.Console.SoftwareEcho",
                CapabilityName::new("ConsoleInput").expect("static capability is valid"),
                Vec::new(),
                vec![boolean.clone()],
                false,
                true,
                Vec::new(),
                "Boolean",
            )
            .expect("embedded Console primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Console.WriteByte",
                CapabilityName::new("ConsoleOutput").expect("static capability is valid"),
                vec![byte.clone()],
                Vec::new(),
                false,
                true,
                Vec::new(),
                "RuntimeResult",
            )
            .expect("embedded Console primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Runtime.EndOfInput",
                CapabilityName::new("RuntimeErrors").expect("static capability is valid"),
                Vec::new(),
                Vec::new(),
                false,
                true,
                Vec::new(),
                "RuntimeError::EndOfInput",
            )
            .expect("embedded Console primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Runtime.MissingTerminator",
                CapabilityName::new("RuntimeErrors").expect("static capability is valid"),
                vec![RegisterKind::LogicalAddress { bits: 32 }],
                Vec::new(),
                false,
                true,
                Vec::new(),
                "RuntimeError::MissingNullTerminator",
            )
            .expect("embedded Console primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Configuration.ReadStartupLanguage",
                CapabilityName::new("StartupPolicy").expect("static capability is valid"),
                Vec::new(),
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                false,
                true,
                Vec::new(),
                "RuntimeResult<StartupLanguage>",
            )
            .expect("embedded Boot primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Configuration.ReadValue",
                CapabilityName::new("ConfigurationStoreRead").expect("static capability is valid"),
                vec![
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                false,
                true,
                vec![
                    LogicalMemoryContract::Read {
                        register: 0,
                        max_bytes: Some(128),
                    },
                    LogicalMemoryContract::Write {
                        register: 1,
                        max_bytes: Some(512),
                    },
                    LogicalMemoryContract::Write {
                        register: 3,
                        max_bytes: Some(512),
                    },
                ],
                "CheckedCallerMemoryOrConfigurationReadError",
            )
            .expect("embedded configuration primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Configuration.WriteValue",
                CapabilityName::new("ConfigurationStoreWrite").expect("static capability is valid"),
                vec![
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                false,
                true,
                vec![
                    LogicalMemoryContract::Read {
                        register: 0,
                        max_bytes: Some(128),
                    },
                    LogicalMemoryContract::Read {
                        register: 1,
                        max_bytes: Some(256),
                    },
                    LogicalMemoryContract::Write {
                        register: 2,
                        max_bytes: Some(512),
                    },
                ],
                "CheckedCallerMemoryOrConfigurationWriteError",
            )
            .expect("embedded configuration primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Configuration.ReplaceAll",
                CapabilityName::new("ConfigurationStoreWrite").expect("static capability is valid"),
                vec![
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                false,
                true,
                vec![
                    LogicalMemoryContract::Read {
                        register: 0,
                        max_bytes: Some(512),
                    },
                    LogicalMemoryContract::Write {
                        register: 1,
                        max_bytes: Some(512),
                    },
                ],
                "CheckedCallerMemoryOrConfigurationReplaceError",
            )
            .expect("embedded configuration primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Boot.RequestMos",
                CapabilityName::new("StartupPolicy").expect("static capability is valid"),
                Vec::new(),
                Vec::new(),
                false,
                true,
                Vec::new(),
                "RuntimeResult",
            )
            .expect("embedded Boot primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Boot.RequestDesktop",
                CapabilityName::new("StartupPolicy").expect("static capability is valid"),
                Vec::new(),
                Vec::new(),
                false,
                true,
                Vec::new(),
                "RuntimeResult",
            )
            .expect("embedded Boot primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.System.ReadMonotonicTime",
                CapabilityName::new("SystemQueries").expect("static capability is valid"),
                Vec::new(),
                vec![RegisterKind::Unsigned { bits: 32 }],
                false,
                true,
                Vec::new(),
                "MonotonicCentiseconds",
            )
            .expect("embedded System primitive names are unique");
        let variable_address = RegisterKind::LogicalAddress { bits: 32 };
        let variable_length = RegisterKind::Signed { bits: 32 };
        let variable_type = RegisterKind::Unsigned { bits: 32 };
        self.module_registry
            .primitives
            .register(
                "Host.SystemVariables.Read",
                CapabilityName::new("SystemVariableStore").expect("static capability is valid"),
                vec![
                    variable_address.clone(),
                    variable_address.clone(),
                    variable_length.clone(),
                    variable_type.clone(),
                    variable_type.clone(),
                ],
                vec![
                    variable_address.clone(),
                    variable_address.clone(),
                    variable_length,
                    variable_type.clone(),
                    variable_type,
                ],
                false,
                true,
                Vec::new(),
                "CheckedCallerBuffersOrSystemVariableError",
            )
            .expect("embedded system-variable primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.SystemVariables.Write",
                CapabilityName::new("SystemVariableStore").expect("static capability is valid"),
                vec![
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Signed { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Signed { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                false,
                false,
                Vec::new(),
                "TaskAuthorizationDeniedOrCheckedCallerBufferOrSystemVariableError",
            )
            .expect("embedded system-variable primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.System.SwiNumberToString",
                CapabilityName::new("SystemQueries").expect("static capability is valid"),
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![RegisterKind::Unsigned { bits: 32 }],
                false,
                true,
                vec![LogicalMemoryContract::Write {
                    register: 1,
                    max_bytes: Some(SYSTEM_SWI_NAME_MAX_BYTES_U32),
                }],
                "ManifestSwiIdentityAndCheckedCallerBuffer",
            )
            .expect("embedded System primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.System.SwiNumberFromString",
                CapabilityName::new("SystemQueries").expect("static capability is valid"),
                vec![RegisterKind::LogicalAddress { bits: 32 }],
                vec![RegisterKind::Unsigned { bits: 32 }],
                false,
                true,
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(SYSTEM_SWI_NAME_MAX_BYTES_U32),
                }],
                "ManifestSwiIdentityAndCheckedCallerString",
            )
            .expect("embedded System primitive names are unique");

        self.module_registry
            .primitives
            .register(
                "Host.Error.RaiseErrorBlock",
                CapabilityName::new("ErrorDispatch").expect("static capability is valid"),
                vec![RegisterKind::LogicalAddress { bits: 32 }],
                Vec::new(),
                false,
                true,
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(256),
                }],
                "StructuredOSFailure",
            )
            .expect("embedded Error primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Runtime.UnsupportedServiceReason",
                CapabilityName::new("RuntimeErrors").expect("static capability is valid"),
                vec![RegisterKind::Unsigned { bits: 32 }; 2],
                Vec::new(),
                false,
                true,
                Vec::new(),
                "StructuredUnsupportedReason",
            )
            .expect("foundation runtime error primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.ReadInfo",
                CapabilityName::new("ModuleIntrospection").expect("static capability is valid"),
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![RegisterKind::Unsigned { bits: 32 }; 6],
                false,
                true,
                vec![LogicalMemoryContract::Write {
                    register: 1,
                    max_bytes: Some(128),
                }],
                "CheckedCallerMemoryOrModuleInfoError",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.LoadSource",
                CapabilityName::new("ModuleManagement").expect("static capability is valid"),
                vec![RegisterKind::LogicalAddress { bits: 32 }],
                Vec::new(),
                true,
                false,
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(256),
                }],
                "ModuleLoadOrStartError",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.Unload",
                CapabilityName::new("ModuleManagement").expect("static capability is valid"),
                vec![RegisterKind::LogicalAddress { bits: 32 }],
                Vec::new(),
                true,
                false,
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(128),
                }],
                "ModuleQuiesceOrFinaliseError",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.LookupModule",
                CapabilityName::new("ModuleIntrospection").expect("static capability is valid"),
                vec![RegisterKind::LogicalAddress { bits: 32 }],
                vec![RegisterKind::Unsigned { bits: 32 }; 6],
                false,
                true,
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(128),
                }],
                "CheckedCallerMemoryOrModuleLookupError",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.LookupSwi",
                CapabilityName::new("ModuleIntrospection").expect("static capability is valid"),
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                false,
                true,
                vec![
                    LogicalMemoryContract::Write {
                        register: 1,
                        max_bytes: Some(512),
                    },
                    LogicalMemoryContract::Write {
                        register: 3,
                        max_bytes: Some(128),
                    },
                    LogicalMemoryContract::Write {
                        register: 5,
                        max_bytes: Some(128),
                    },
                ],
                "CheckedCallerMemoryOrSwiIdentityError",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.LookupModuleExport",
                CapabilityName::new("ModuleIntrospection").expect("static capability is valid"),
                vec![
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![RegisterKind::Unsigned { bits: 32 }; 4],
                false,
                true,
                vec![
                    LogicalMemoryContract::Read {
                        register: 0,
                        max_bytes: Some(128),
                    },
                    LogicalMemoryContract::Write {
                        register: 2,
                        max_bytes: Some(128),
                    },
                    LogicalMemoryContract::Write {
                        register: 4,
                        max_bytes: Some(128),
                    },
                ],
                "CheckedCallerMemoryOrModuleExportError",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.ReadDefinitionSource",
                CapabilityName::new("ModuleIntrospection").expect("static capability is valid"),
                vec![
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![RegisterKind::Unsigned { bits: 32 }; 6],
                false,
                true,
                vec![
                    LogicalMemoryContract::Read {
                        register: 0,
                        max_bytes: Some(128),
                    },
                    LogicalMemoryContract::Write {
                        register: 2,
                        max_bytes: Some(1025),
                    },
                    LogicalMemoryContract::Write {
                        register: 4,
                        max_bytes: Some(128),
                    },
                ],
                "CheckedCallerMemoryOrDefinitionSourceError",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.AuthorizeSourceRead",
                CapabilityName::new("ModuleIntrospection").expect("static capability is valid"),
                Vec::new(),
                Vec::new(),
                false,
                true,
                Vec::new(),
                "TaskAuthorizationDenied",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ModuleManager.AuthorizeManagement",
                CapabilityName::new("ModuleManagement").expect("static capability is valid"),
                Vec::new(),
                Vec::new(),
                false,
                true,
                Vec::new(),
                "TaskAuthorizationDenied",
            )
            .expect("embedded ModuleManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.CommandRegistry.ReadEntry",
                CapabilityName::new("CommandRegistry").expect("static capability is valid"),
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                ],
                vec![RegisterKind::Unsigned { bits: 32 }; 2],
                false,
                true,
                vec![LogicalMemoryContract::Write {
                    register: 1,
                    max_bytes: Some(1024),
                }],
                "CommandRegistryBufferError",
            )
            .expect("embedded command-registry primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.CommandRegistry.Invoke",
                CapabilityName::new("CommandRegistry").expect("static capability is valid"),
                vec![RegisterKind::LogicalAddress { bits: 32 }; 5],
                Vec::new(),
                true,
                false,
                vec![
                    LogicalMemoryContract::Read {
                        register: 0,
                        max_bytes: Some(33),
                    },
                    LogicalMemoryContract::Read {
                        register: 1,
                        max_bytes: Some(65),
                    },
                    LogicalMemoryContract::Read {
                        register: 2,
                        max_bytes: Some(17),
                    },
                    LogicalMemoryContract::Read {
                        register: 3,
                        max_bytes: Some(65),
                    },
                    LogicalMemoryContract::Read {
                        register: 4,
                        max_bytes: Some(MAX_CLI_BYTES as u32),
                    },
                ],
                "CommandNotFoundOrHandlerError",
            )
            .expect("embedded command-registry primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.CommandScripts.Open",
                CapabilityName::new("CommandScripts").expect("static capability is valid"),
                vec![
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                ],
                vec![RegisterKind::Unsigned { bits: 32 }],
                false,
                false,
                vec![
                    LogicalMemoryContract::Read {
                        register: 0,
                        max_bytes: Some(MAX_CLI_BYTES as u32),
                    },
                    LogicalMemoryContract::Read {
                        register: 1,
                        max_bytes: Some(MAX_CLI_BYTES as u32),
                    },
                ],
                "ObeyFileOrBoundedScriptError",
            )
            .expect("embedded command-script primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.CommandScripts.ReadLine",
                CapabilityName::new("CommandScripts").expect("static capability is valid"),
                vec![RegisterKind::Unsigned { bits: 32 }],
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                ],
                false,
                false,
                Vec::new(),
                "ObeyLineOrBoundedScriptError",
            )
            .expect("embedded command-script primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.CommandScripts.Close",
                CapabilityName::new("CommandScripts").expect("static capability is valid"),
                vec![RegisterKind::Unsigned { bits: 32 }],
                Vec::new(),
                false,
                false,
                Vec::new(),
                "ObeyContextError",
            )
            .expect("embedded command-script primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.ExecInput.Replace",
                CapabilityName::new("ExecInput").expect("static capability is valid"),
                vec![RegisterKind::LogicalAddress { bits: 32 }],
                Vec::new(),
                false,
                false,
                vec![LogicalMemoryContract::Read {
                    register: 0,
                    max_bytes: Some(MAX_CLI_BYTES as u32),
                }],
                "ExecPathOrBoundedInputError",
            )
            .expect("embedded Exec input primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Task.ReadIdentity",
                CapabilityName::new("TaskQuery").expect("static capability is valid"),
                Vec::new(),
                vec![RegisterKind::Unsigned { bits: 32 }; 3],
                false,
                true,
                Vec::new(),
                "CallerTaskIdentity",
            )
            .expect("embedded TaskManager primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Memory.CreateDynamicArea",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                ],
                vec![RegisterKind::Unsigned { bits: 32 }; 3],
                false,
                true,
                vec![LogicalMemoryContract::Read {
                    register: 7,
                    max_bytes: Some(128),
                }],
                "DynamicAreaError",
            )
            .expect("embedded Memory primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Memory.RemoveDynamicArea",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                vec![RegisterKind::Unsigned { bits: 32 }],
                Vec::new(),
                false,
                true,
                Vec::new(),
                "DynamicAreaError",
            )
            .expect("embedded Memory primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Memory.AcquireCommandScratch",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                Vec::new(),
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                ],
                false,
                true,
                Vec::new(),
                "DynamicAreaError",
            )
            .expect("embedded Memory primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Memory.ReleaseCommandScratch",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                vec![RegisterKind::Unsigned { bits: 32 }],
                Vec::new(),
                false,
                true,
                Vec::new(),
                "DynamicAreaError",
            )
            .expect("embedded Memory primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Memory.ReadDynamicArea",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                vec![RegisterKind::Unsigned { bits: 32 }],
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::LogicalAddress { bits: 32 },
                ],
                false,
                true,
                Vec::new(),
                "DynamicAreaError",
            )
            .expect("embedded Memory primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Memory.NextDynamicArea",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                vec![RegisterKind::Unsigned { bits: 32 }],
                vec![RegisterKind::Unsigned { bits: 32 }],
                false,
                true,
                Vec::new(),
                "DynamicAreaError",
            )
            .expect("embedded Memory primitive names are unique");
        self.module_registry
            .primitives
            .register(
                "Host.Memory.ChangeDynamicArea",
                CapabilityName::new("TaskMemory").expect("static capability is valid"),
                vec![
                    RegisterKind::Unsigned { bits: 32 },
                    RegisterKind::Signed { bits: 32 },
                ],
                vec![RegisterKind::Unsigned { bits: 32 }],
                false,
                true,
                Vec::new(),
                "DynamicAreaError",
            )
            .expect("embedded Memory primitive names are unique");
    }

    fn bootstrap_capsule(&mut self, bytes: &[u8]) -> Result<(), BootFailure> {
        self.reset_boot_registry();
        let result = self.bootstrap_capsule_inner(bytes);
        if result.is_err() {
            // Capsule publication is an initial-namespace transaction. If any
            // start hook fails, discard every foundation export and workspace
            // before exposing the restricted native recovery surface.
            self.reset_boot_registry();
        } else {
            self.boot_failure = None;
        }
        result
    }

    fn bootstrap_capsule_inner(&mut self, bytes: &[u8]) -> Result<(), BootFailure> {
        let capsule = BootCapsule::decode(bytes, RUNTIME_ABI_VERSION).map_err(|error| {
            let mut failure = BootFailure::from_capsule(BootStage::CapsuleValidation, error);
            failure.capsule_abi = read_capsule_abi(bytes);
            failure
                .diagnostic_log
                .push(format!("capsule byte length: {}", bytes.len()));
            failure
        })?;
        for required in [
            "Console",
            "System",
            "ModuleManager",
            "Error",
            "TaskManager",
            "RicochetCommands",
            "Memory",
            "Boot",
            "Mos",
        ] {
            if !capsule
                .modules
                .iter()
                .any(|module| module.manifest.name.eq_ignore_ascii_case(required))
            {
                let mut failure = BootFailure::from_capsule(
                    BootStage::ModuleValidation,
                    format!("boot capsule is missing required {required} foundation module"),
                );
                failure.module = Some(required.into());
                failure.capsule_abi = Some(capsule.runtime_abi);
                return Err(failure);
            }
        }
        if self.module_registry.registered_swi_count() != 0 {
            return Err(BootFailure::from_capsule(
                BootStage::Publication,
                "initial SWI table was not empty before capsule linking",
            ));
        }

        let ids_by_name = self.link_capsule_unpublished(&capsule)?;

        let module_ids = capsule
            .modules
            .iter()
            .map(|module| ids_by_name[&module.manifest.name.to_ascii_lowercase()])
            .collect::<Vec<_>>();
        self.foundation_module_ids = module_ids.iter().copied().collect();
        self.module_registry
            .publish_modules(&module_ids)
            .map_err(|error| {
                let mut failure = BootFailure::from_capsule(BootStage::Publication, error);
                failure.capsule_abi = Some(capsule.runtime_abi);
                failure
            })?;

        for index in capsule.start_order() {
            let module_record = &capsule.modules[index];
            let module_id = ids_by_name[&module_record.manifest.name.to_ascii_lowercase()];
            self.module_registry
                .begin_module_start(module_id)
                .map_err(|error| {
                    let mut failure = BootFailure::from_capsule(BootStage::Start, error);
                    failure.module = Some(module_record.manifest.name.clone());
                    failure.capsule_abi = Some(capsule.runtime_abi);
                    failure
                })?;
            let module = self
                .module_programs
                .values()
                .find(|module| {
                    module
                        .manifest
                        .name
                        .eq_ignore_ascii_case(&module_record.manifest.name)
                })
                .cloned()
                .expect("boot module has a retained source program");
            if let Err(error) =
                module.invoke_lifecycle_transactional("START", module_id, &mut Task::new(0), self)
            {
                let mut failure = BootFailure::from_runtime(
                    BootStage::Start,
                    Some(module_record.manifest.name.clone()),
                    module_record.manifest.lifecycle.start.clone(),
                    &error,
                    Some(capsule.runtime_abi),
                );
                failure.diagnostic_log.push(format!(
                    "module exports: {}",
                    module_record
                        .manifest
                        .exports
                        .iter()
                        .map(|export| format!("{} (&{:X})", export.name, export.number))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                return Err(failure);
            }
            self.module_registry
                .complete_module_start(module_id)
                .map_err(|error| {
                    let mut failure = BootFailure::from_capsule(BootStage::Start, error);
                    failure.module = Some(module_record.manifest.name.clone());
                    failure.definition = module_record.manifest.lifecycle.start.clone();
                    failure.capsule_abi = Some(capsule.runtime_abi);
                    failure
                })?;
        }
        if self.startup_target.is_none() {
            let mut failure = BootFailure::from_capsule(
                BootStage::Start,
                "Boot.Start did not request either the MOS command environment or desktop",
            );
            failure.module = Some("Boot".into());
            failure.definition = capsule
                .modules
                .iter()
                .find(|module| module.manifest.name.eq_ignore_ascii_case("Boot"))
                .and_then(|module| module.manifest.lifecycle.start.clone());
            failure.capsule_abi = Some(capsule.runtime_abi);
            return Err(failure);
        }
        Ok(())
    }

    fn link_capsule_unpublished(
        &mut self,
        capsule: &BootCapsule,
    ) -> Result<HashMap<String, ModuleId>, BootFailure> {
        if self.module_registry.registered_swi_count() != 0 {
            return Err(BootFailure::from_capsule(
                BootStage::Publication,
                "native loader requires an empty public SWI table",
            ));
        }
        let allocator = self.module_registry.allocator();
        let mut ids_by_name = HashMap::new();
        for module_record in &capsule.modules {
            let module = SystemModule::parse(
                &module_record.source,
                module_record.source_path.clone(),
                &allocator,
            )
            .map_err(|error| {
                let mut failure = BootFailure::from_capsule(BootStage::ModuleValidation, error);
                failure.module = Some(module_record.manifest.name.clone());
                failure.capsule_abi = Some(capsule.runtime_abi);
                failure
            })?;
            if module.manifest != module_record.manifest {
                let mut failure = BootFailure::from_capsule(
                    BootStage::ModuleValidation,
                    "source-derived manifest differs from capsule manifest",
                );
                failure.module = Some(module_record.manifest.name.clone());
                failure.capsule_abi = Some(capsule.runtime_abi);
                return Err(failure);
            }
            module
                .validate_primitive_shapes(&self.module_registry.primitives)
                .map_err(|error| {
                    let mut failure = BootFailure::from_runtime(
                        BootStage::ModuleValidation,
                        Some(module_record.manifest.name.clone()),
                        None,
                        &error,
                        Some(capsule.runtime_abi),
                    );
                    failure
                        .diagnostic_log
                        .push(format!("source: {}", module_record.source_path));
                    failure
                })?;

            // Grants in a capsule are still constrained by the host's reviewed
            // boot policy; a selected capsule cannot invent authority.
            validate_boot_grants(module_record.manifest.name.as_str(), &module_record.grants)
                .map_err(|message| {
                    let mut failure =
                        BootFailure::from_capsule(BootStage::ModuleValidation, message);
                    failure.module = Some(module_record.manifest.name.clone());
                    failure.capsule_abi = Some(capsule.runtime_abi);
                    failure
                })?;
            let module_id = self
                .module_registry
                .stage_module(module.manifest.clone(), module.definitions.clone())
                .map_err(|error| {
                    let mut failure = BootFailure::from_capsule(BootStage::ModuleValidation, error);
                    failure.module = Some(module_record.manifest.name.clone());
                    failure.capsule_abi = Some(capsule.runtime_abi);
                    failure
                })?;
            ids_by_name.insert(module.manifest.name.to_ascii_lowercase(), module_id);
            for definition in self
                .module_registry
                .module(module_id)
                .expect("staged boot module exists")
                .definitions
                .values()
            {
                self.module_programs
                    .insert(definition.id, Arc::new(module.clone()));
            }
        }

        for module_record in &capsule.modules {
            let module_id = ids_by_name[&module_record.manifest.name.to_ascii_lowercase()];
            self.module_registry
                .link_module(module_id, module_record.grants.clone())
                .map_err(|error| {
                    let mut failure = BootFailure::from_capsule(BootStage::Linking, error);
                    failure.module = Some(module_record.manifest.name.clone());
                    failure.capsule_abi = Some(capsule.runtime_abi);
                    failure.diagnostic_log.push(format!(
                        "primitive imports: {}",
                        module_record
                            .manifest
                            .primitive_imports
                            .iter()
                            .map(|import| import.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                    failure
                })?;
        }
        Ok(ids_by_name)
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    fn obey_source(&self, task_id: u64) -> Option<(String, u32)> {
        self.obey_scripts
            .get(&task_id)
            .and_then(|session| session.frames.last())
            .and_then(|frame| {
                frame
                    .current_line
                    .map(|line| (frame.source_path.clone(), line))
            })
    }

    fn obey_directory(&self, task_id: u64) -> Option<&str> {
        self.obey_scripts
            .get(&task_id)
            .and_then(|session| session.frames.last())
            .map(|frame| frame.obey_directory.as_str())
    }

    fn unwind_obey_scripts(&mut self, task: &mut Task, depth: usize) {
        let Some(session) = self.obey_scripts.get_mut(&task.id) else {
            return;
        };
        let keep_depth = depth.min(session.frames.len());
        let frames = session.frames.drain(keep_depth..).collect::<Vec<_>>();
        for frame in frames {
            let _ = task.memory.remove_dynamic_area(frame.buffer_number);
        }
        session.total_bytes = session.frames.iter().map(|frame| frame.bytes.len()).sum();
        if session.frames.is_empty() {
            self.obey_scripts.remove(&task.id);
        }
    }

    pub fn desktop_requested(&self) -> bool {
        self.desktop_requested
    }

    pub fn graphics(&self) -> &GraphicsService {
        self.current_graphics()
    }

    fn current_graphics(&self) -> &GraphicsService {
        self.active_graphics_window
            .and_then(|handle| self.window_graphics.get(&handle))
            .unwrap_or(&self.graphics)
    }

    fn current_graphics_mut(&mut self) -> &mut GraphicsService {
        if let Some(handle) = self.active_graphics_window {
            if let Some(graphics) = self.window_graphics.get_mut(&handle) {
                return graphics;
            }
        }
        &mut self.graphics
    }

    fn ensure_graphics_pixel_budget(
        &self,
        target_window: Option<u32>,
        replacement_pixels: u64,
    ) -> Result<(), RuntimeError> {
        let mut total = if target_window.is_none() {
            replacement_pixels
        } else {
            self.graphics.mode_pixel_count()
        };
        total = total.saturating_add(
            self.task_default_graphics
                .values()
                .map(GraphicsService::mode_pixel_count)
                .sum::<u64>(),
        );
        for (handle, graphics) in &self.window_graphics {
            total = total.saturating_add(if Some(*handle) == target_window {
                replacement_pixels
            } else {
                graphics.mode_pixel_count()
            });
        }
        if let Some(handle) = target_window {
            if !self.window_graphics.contains_key(&handle) {
                total = total.saturating_add(replacement_pixels);
            }
        }
        if total > MAX_TASK_GRAPHICS_PIXELS {
            return Err(RuntimeError::Program(
                "hosted Wimp graphics surface budget for this task is exhausted".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn load_basic_configuration(&self) -> Result<BasicConfiguration, RuntimeError> {
        self.effective_configure_store()
            .load()
            .map_err(RuntimeError::Program)
    }

    fn configuration_read_value(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let option = read_guest_string(task, context.registers[R0])?;
        let value_address = context.registers[R1];
        let value_capacity = context.registers[R2] as usize;
        let error_address = context.registers[R3];
        let error_capacity = context.registers[R4] as usize;
        validate_configuration_error_capacity(error_capacity)?;
        let store = self.effective_configure_store();

        match store.load() {
            Err(error) => {
                write_configuration_message(task, error_address, error_capacity, &error)?;
                context.registers[R0] = CONFIG_SERVICE_ERROR;
            }
            Ok(configuration) => {
                let Some((_, value)) = configuration.status_value(&option) else {
                    context.registers[R0] = CONFIG_SERVICE_NOT_FOUND;
                    return Ok(());
                };
                if value_capacity == 0 || value_capacity > CONFIG_VALUE_BUFFER_MAX {
                    write_configuration_message(
                        task,
                        error_address,
                        error_capacity,
                        "value buffer capacity must be between 1 and 512 bytes",
                    )?;
                    context.registers[R0] = CONFIG_SERVICE_BUFFER_ERROR;
                } else if value.len() + 1 > value_capacity {
                    write_configuration_message(
                        task,
                        error_address,
                        error_capacity,
                        "configuration value does not fit the caller buffer",
                    )?;
                    context.registers[R0] = CONFIG_SERVICE_BUFFER_ERROR;
                } else {
                    write_guest_string(task, value_address, &value)?;
                    context.registers[R0] = CONFIG_SERVICE_OK;
                }
            }
        }
        context.registers[R1] = store.recovery_code();
        Ok(())
    }

    fn configuration_write_value(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let error_address = context.registers[R2];
        let error_capacity = context.registers[R3] as usize;
        validate_configuration_error_capacity(error_capacity)?;
        if let Err(error) = task.require_configuration_write() {
            write_configuration_message(task, error_address, error_capacity, &error.to_string())?;
            context.registers[R0] = CONFIG_SERVICE_DENIED;
            context.registers[R1] = 0;
            return Ok(());
        }

        let option = read_guest_string(task, context.registers[R0])?;
        let value = read_guest_string(task, context.registers[R1])?;
        let store = self.effective_configure_store();
        match store.set_with_recovery(&option, &value) {
            Ok((_, recovery_copy_created)) => {
                context.registers[R0] = CONFIG_SERVICE_OK;
                context.registers[R1] = u32::from(recovery_copy_created);
            }
            Err(error) => {
                write_configuration_message(task, error_address, error_capacity, &error)?;
                context.registers[R0] = CONFIG_SERVICE_ERROR;
                context.registers[R1] = 0;
            }
        }
        Ok(())
    }

    fn configuration_replace_all(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let error_address = context.registers[R1];
        let error_capacity = context.registers[R2] as usize;
        validate_configuration_error_capacity(error_capacity)?;
        if let Err(error) = task.require_configuration_write() {
            write_configuration_message(task, error_address, error_capacity, &error.to_string())?;
            context.registers[R0] = CONFIG_SERVICE_DENIED;
            context.registers[R1] = 0;
            return Ok(());
        }

        let payload = read_guest_string(task, context.registers[R0])?;
        let store = self.effective_configure_store();
        match store.replace_from_payload_with_recovery(&payload) {
            Ok((_, recovery_copy_created)) => {
                context.registers[R0] = CONFIG_SERVICE_OK;
                context.registers[R1] = u32::from(recovery_copy_created);
            }
            Err(error) => {
                write_configuration_message(task, error_address, error_capacity, &error)?;
                context.registers[R0] = CONFIG_SERVICE_ERROR;
                context.registers[R1] = 0;
            }
        }
        Ok(())
    }

    pub(crate) fn desktop_is_configured_for_startup(&self) -> bool {
        self.desktop_service.is_some() && self.startup_target == Some(BootStartupTarget::Desktop)
    }

    pub(crate) fn has_boot_failure(&self) -> bool {
        self.boot_failure.is_some()
    }

    /// Runs the restricted native recovery interface before any public SWI is
    /// made available to the task. The only host-file operation is an
    /// operator-selected capsule read; no normal guest path or CLI exists here.
    pub(crate) fn recover_boot(&mut self) -> Result<bool, RuntimeError> {
        while let Some(failure) = self.boot_failure.clone() {
            self.native_recovery_write(b"Ricochet native recovery\n\r")?;
            self.native_recovery_write(failure.summary().as_bytes())?;
            self.native_recovery_write(b"\n\r")?;
            for line in &failure.diagnostic_log {
                self.native_recovery_write(b"  ")?;
                self.native_recovery_write(line.as_bytes())?;
                self.native_recovery_write(b"\n\r")?;
            }
            self.native_recovery_write(
                b"R retry embedded capsule; A <path> select alternate capsule; Q exit\n\r> ",
            )?;
            let Some(input) = self.native_recovery_read_line()? else {
                return Ok(false);
            };
            let action = parse_recovery_action(&input);
            let attempt = match action {
                RecoveryAction::Exit => return Ok(false),
                RecoveryAction::RetryEmbedded => match embedded_capsule_bytes() {
                    Ok(bytes) => self.bootstrap_capsule(bytes),
                    Err(error) => Err(BootFailure::from_capsule(BootStage::CapsuleBuild, error)),
                },
                RecoveryAction::SelectCapsule(path) if path.is_empty() => {
                    self.native_recovery_write(b"alternate capsule path: ")?;
                    let Some(path) = self.native_recovery_read_line()? else {
                        return Ok(false);
                    };
                    self.try_alternate_capsule(path.trim())
                }
                RecoveryAction::SelectCapsule(path) => self.try_alternate_capsule(path.trim()),
                RecoveryAction::Invalid => {
                    self.native_recovery_write(
                        b"Recovery accepts only R, A <capsule path>, or Q.\n\r",
                    )?;
                    continue;
                }
            };
            match attempt {
                Ok(()) => {
                    self.boot_failure = None;
                    return Ok(true);
                }
                Err(failure) => {
                    self.boot_failure = Some(failure);
                }
            }
        }
        Ok(true)
    }

    fn try_alternate_capsule(&mut self, path: &str) -> Result<(), BootFailure> {
        if path.is_empty() || path.len() > 4096 {
            return Err(BootFailure::host_capsule_read(
                path,
                "capsule path must contain 1–4096 bytes",
            ));
        }
        let bytes =
            std::fs::read(path).map_err(|error| BootFailure::host_capsule_read(path, error))?;
        self.bootstrap_capsule(&bytes)
    }

    fn native_recovery_write(&mut self, bytes: &[u8]) -> Result<(), RuntimeError> {
        for byte in bytes {
            self.console.write_byte(*byte)?;
            if self.display_events.is_some() {
                self.publish_display_event(DisplayEvent::WriteByte {
                    task_id: self.display_task_id,
                    window_handle: None,
                    byte: *byte,
                });
            }
        }
        self.console.flush().map_err(RuntimeError::from)
    }

    fn native_recovery_read_line(&mut self) -> Result<Option<String>, RuntimeError> {
        let mut bytes = Vec::new();
        loop {
            let Some(byte) = self.console.read_byte()? else {
                return if bytes.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
                };
            };
            match byte {
                b'\r' | b'\n' => {
                    self.native_recovery_write(b"\n\r")?;
                    return Ok(Some(String::from_utf8_lossy(&bytes).into_owned()));
                }
                8 | 127 => {
                    if bytes.pop().is_some() {
                        self.native_recovery_write(b"\x08 \x08")?;
                    }
                }
                byte if byte.is_ascii_graphic() || byte == b' ' => {
                    if bytes.len() == 4096 {
                        self.native_recovery_write(b"\x07")?;
                    } else {
                        bytes.push(byte);
                        self.native_recovery_write(&[byte])?;
                    }
                }
                _ => {}
            }
        }
    }

    /// Materializes the startup request selected by the BASIC64 Boot module.
    /// Rust does not read or interpret the saved Language value here.
    pub(crate) fn enter_boot_startup(&mut self) -> Result<bool, RuntimeError> {
        if self.startup_target != Some(BootStartupTarget::Desktop) {
            return Ok(false);
        }
        if self.desktop_service.is_none() {
            // --stdio deliberately remains the MOS recovery interface, even
            // when a saved graphical startup preference is present.
            return Ok(false);
        }
        self.begin_desktop()?;
        Ok(true)
    }

    fn effective_configure_store(&self) -> ConfigureStore {
        self.wimp
            .as_ref()
            .and_then(|wimp| wimp.configure_store())
            .unwrap_or_else(|| self.configure.clone())
    }

    #[cfg(test)]
    pub(crate) fn set_configure_store_for_test(&mut self, configure: ConfigureStore) {
        self.configure = configure.clone();
        if let Some(wimp) = self.wimp.as_ref().or(self.desktop_service.as_ref()) {
            wimp.bind_configure_store(configure);
        }
        let Some(record) = self.module_registry.module_named("Boot") else {
            return;
        };
        let module_id = record.id;
        let source_path = record.manifest.source_path.clone();
        let source_hash = record.manifest.source_hash.clone();
        let Some(program) = self
            .module_programs
            .values()
            .find(|program| {
                program.manifest.name.eq_ignore_ascii_case("Boot")
                    && program.manifest.source_path == source_path
                    && program.manifest.source_hash == source_hash
            })
            .cloned()
        else {
            return;
        };
        if let Err(error) =
            program.invoke_lifecycle_transactional("START", module_id, &mut Task::new(0), self)
        {
            self.boot_failure = Some(BootFailure::from_runtime(
                BootStage::Start,
                Some("Boot".into()),
                Some("START".into()),
                &error,
                Some(RUNTIME_ABI_VERSION),
            ));
        }
    }

    #[cfg(test)]
    pub(crate) fn set_file_system_for_test(&mut self, file_system: HostFileSystem) {
        self.file_system = file_system;
    }

    pub(crate) fn set_display_profiles(
        &mut self,
        graphics_profile: GraphicsProfile,
        text_profile: TextRenderingProfile,
        encoding: TextEncoding,
    ) -> Result<(), RuntimeError> {
        let previous = self.current_graphics().snapshot().clone();
        self.current_graphics_mut().set_display_profiles(
            graphics_profile,
            text_profile,
            encoding,
        )?;
        for graphics in self.task_default_graphics.values_mut() {
            graphics.set_display_profiles(graphics_profile, text_profile, encoding)?;
        }
        let snapshot = self.current_graphics().snapshot().clone();
        if snapshot != previous {
            self.publish_snapshot(snapshot);
        }
        Ok(())
    }

    pub(crate) fn can_keep_modern_shell_for_basic_console(
        &self,
        graphics_profile: GraphicsProfile,
        text_profile: TextRenderingProfile,
        encoding: TextEncoding,
    ) -> bool {
        self.modern_shell_console
            && self.active_graphics_window.is_none()
            && graphics_profile == self.graphics.snapshot().mode.profile
            && text_profile == TextRenderingProfile::Modern
            && encoding == TextEncoding::Utf8
    }

    pub(crate) fn set_basic_console_display_profiles(
        &mut self,
        graphics_profile: GraphicsProfile,
        text_profile: TextRenderingProfile,
        encoding: TextEncoding,
    ) -> Result<(), RuntimeError> {
        if self.can_keep_modern_shell_for_basic_console(graphics_profile, text_profile, encoding) {
            // The immediate BASIC prompt is already running with these
            // semantics. Leaving the responsive shell surface in place keeps
            // its transcript and host-derived grid across each submitted line.
            return Ok(());
        }
        self.set_display_profiles(graphics_profile, text_profile, encoding)
    }

    /// Initialize a Runtime-owned MOS shell. Generic SWI dispatchers retain
    /// their historical Classic default; the interactive host shell opts in.
    pub(crate) fn initialize_mos_shell_console(&mut self) {
        let previous = self.graphics.snapshot().clone();
        if self.graphics.set_modern_shell_console().is_ok() {
            self.modern_shell_console = true;
            self.sync_modern_shell_grid_from_wimp();
            let snapshot = self.graphics.snapshot().clone();
            if snapshot != previous {
                self.publish_snapshot(snapshot);
            }
        }
    }

    fn sync_modern_shell_grid_from_wimp(&mut self) -> bool {
        if self.active_graphics_window.is_some() || !self.graphics.snapshot().modern_shell_console {
            return false;
        }
        let size_osu = self
            .wimp
            .as_ref()
            .and_then(|wimp| {
                wimp.console_work_area(self.display_task_id).map(|area| {
                    (
                        area.max_x.saturating_sub(area.min_x),
                        area.max_y.saturating_sub(area.min_y),
                    )
                })
            })
            .or_else(|| {
                self.desktop_service.as_ref().map(|wimp| {
                    let metrics = wimp.desktop_metrics();
                    let (host_width, host_height) = metrics.host_pixel_size();
                    (
                        host_width.saturating_mul(2).min(i32::MAX as u32) as i32,
                        host_height.saturating_mul(2).min(i32::MAX as u32) as i32,
                    )
                })
            });
        let Some((width_osu, height_osu)) = size_osu else {
            return false;
        };
        let size = crate::graphics::modern_shell_grid_for_area(width_osu, height_osu);
        self.graphics.set_modern_shell_text_grid(size.0, size.1)
    }

    fn save_mos_shell_for_guest(&self) -> Option<GraphicsService> {
        (self.modern_shell_console && self.active_graphics_window.is_none())
            .then(|| self.graphics.detached_copy())
    }

    fn restore_mos_shell_after_guest(&mut self, saved: Option<GraphicsService>) {
        if let Some(shell) = saved {
            let finished = self.graphics.snapshot().clone();
            let saved_snapshot = shell.snapshot();
            if finished.text_profile == TextRenderingProfile::Modern
                && finished.mode == saved_snapshot.mode
            {
                // Keep successful modern program output in the interactive
                // shell when its display contract still matches, then restore
                // the host-owned console canvas and UTF-8 input policy.
                let previous = self.graphics.snapshot().clone();
                if self.graphics.set_modern_shell_console().is_ok() {
                    let snapshot = self.graphics.snapshot().clone();
                    if snapshot != previous {
                        self.publish_snapshot(snapshot);
                    }
                }
            } else {
                // Classic bitmap or incompatible target/mode output lived in
                // the transient guest view. Restore the detached shell grid
                // instead of reinterpreting those bytes with scalable text.
                self.graphics = shell;
                self.publish_snapshot(self.graphics.snapshot().clone());
            }
        }
    }

    pub(crate) fn with_mos_shell_suspended<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let shell = self.save_mos_shell_for_guest();
        let result = operation(self);
        self.restore_mos_shell_after_guest(shell);
        result
    }

    pub(crate) fn poll_key(&mut self, task: &Task) -> Option<u8> {
        if self.suppress_command_key_polling || task.has_exec_input() {
            return None;
        }
        if self.last_inkey_poll.elapsed() < INKEY_POLL_INTERVAL {
            return None;
        }
        self.last_inkey_poll = Instant::now();
        self.mos
            .input
            .pop_front()
            .or_else(|| self.console.try_read_byte())
    }

    pub(crate) fn set_mode_from_block(
        &mut self,
        task: &Task,
        address: u32,
    ) -> Result<(), RuntimeError> {
        let read_word = |offset: u32| -> Result<i32, RuntimeError> {
            let word_address = address
                .checked_add(offset)
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
            let bytes = [
                task.memory.read_byte(word_address)?,
                task.memory.read_byte(word_address + 1)?,
                task.memory.read_byte(word_address + 2)?,
                task.memory.read_byte(word_address + 3)?,
            ];
            Ok(i32::from_le_bytes(bytes))
        };
        if read_word(0)? != 1 || read_word(36)? != -1 {
            return Err(RuntimeError::Program(
                "unsupported extended mode block format".into(),
            ));
        }
        let width = read_word(4)?;
        let height = read_word(8)?;
        let depth = read_word(12)?;
        let y_eigenfactor = read_word(24)?;
        let x_eigenfactor = read_word(32)?;
        if depth != 5 {
            return Err(RuntimeError::Program(
                "the hosted extended mode currently supports 32-bit colour".into(),
            ));
        }
        if width <= 0
            || height <= 0
            || u64::try_from(width).unwrap_or(u64::MAX) * u64::try_from(height).unwrap_or(u64::MAX)
                > MAX_EXTENDED_MODE_PIXELS
            || !(0..=4).contains(&x_eigenfactor)
            || !(0..=4).contains(&y_eigenfactor)
        {
            return Err(RuntimeError::Program(
                "extended mode dimensions or eigenfactors are outside the hosted profile".into(),
            ));
        }
        self.ensure_graphics_pixel_budget(
            self.active_graphics_window,
            u64::from(width as u32) * u64::from(height as u32),
        )?;
        self.current_graphics_mut().set_extended_mode(
            width as u32,
            height as u32,
            x_eigenfactor as u8,
            y_eigenfactor as u8,
        )?;
        // MODE blocks bypass VDU 22: publish the new grid and shared raster
        // before any subsequent plotting, just as a numbered mode does.
        self.publish_snapshot(self.current_graphics().snapshot().clone());
        Ok(())
    }

    fn dispatch_module_owned_swi(
        &mut self,
        number: u32,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Option<Result<(), RuntimeError>> {
        let (ownership, definition) = match self.module_registry.acquire_swi(number) {
            Some(acquired) => acquired,
            None if self.module_registry.swi_entry_id(number).is_some() => {
                return Some(Err(RuntimeError::Program(format!(
                    "SWI &{number:X} is published but its owning module is not active"
                ))));
            }
            None => return None,
        };
        let invocation = match self
            .module_registry
            .invocation_plan(number, InvocationBackend::Interpreter)
        {
            Ok(plan)
                if plan.identity.definition == definition.id
                    && plan.identity.generation == ownership.generation =>
            {
                plan
            }
            Ok(_) => {
                return Some(Err(RuntimeError::Program(
                    "interpreter target identity no longer matches the acquired definition generation".into(),
                )));
            }
            Err(error) => return Some(Err(RuntimeError::Program(error.to_string()))),
        };
        self.module_dispatch_count = self.module_dispatch_count.saturating_add(1);
        self.last_dispatch_route = Some(SwiDispatchRoute::ModuleOwned {
            number,
            name: ownership.name.clone(),
            module: ownership.module_name.clone(),
            definition: ownership.definition_name.clone(),
            generation: ownership.generation_number,
            backend: invocation.backend,
        });
        let Some(module) = self.module_programs.get(&definition.id).cloned() else {
            return Some(Err(RuntimeError::Program(format!(
                "module definition {} has no retained BASIC64 source",
                definition.id
            ))));
        };
        let previous_key_poll_suppression = self.suppress_command_key_polling;
        if number == OS_CLI || number == OS_READ_LINE {
            // OS_CLI owns command parsing/dispatch, and OS_ReadLine may block
            // while consuming the same input queue. Keep either complete
            // module-owned call frame isolated from opportunistic INKEY
            // polling so a long line cannot lose characters to the scheduler.
            // A Rust BRIDGE can explicitly re-enable polling while it runs an
            // interactive guest program.
            self.suppress_command_key_polling = true;
        }
        let result = module.invoke(
            ownership.module,
            &definition,
            &ownership.contract,
            task,
            self,
            context,
        );
        self.suppress_command_key_polling = previous_key_poll_suppression;
        if number == WIMP_CLOSE_DOWN && result.is_ok() {
            self.release_closed_wimp_graphics();
        }
        self.last_dispatch_route = Some(SwiDispatchRoute::ModuleOwned {
            number,
            name: ownership.name,
            module: ownership.module_name,
            definition: ownership.definition_name,
            generation: ownership.generation_number,
            backend: invocation.backend,
        });
        drop(definition);
        self.collect_retired_module_programs();
        Some(result)
    }

    fn release_closed_wimp_graphics(&mut self) {
        let Some(wimp) = &self.wimp else {
            return;
        };
        let live_handles = wimp
            .desktop_windows()
            .into_iter()
            .map(|window| window.handle)
            .collect::<std::collections::HashSet<_>>();
        self.window_graphics
            .retain(|handle, _| live_handles.contains(handle));
        if self
            .active_graphics_window
            .is_some_and(|handle| !live_handles.contains(&handle))
        {
            self.active_graphics_window = None;
        }
    }

    fn record_transitional_numeric_dispatch(&mut self, number: u32) {
        self.transitional_dispatch_count = self.transitional_dispatch_count.saturating_add(1);
        self.last_dispatch_route = Some(SwiDispatchRoute::TransitionalRust { number });
    }

    fn invoke_module_primitive(
        &mut self,
        name: &str,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let module_id = self.active_module.ok_or_else(|| {
            RuntimeError::Program(format!(
                "primitive {name} was called outside a BASIC64 module invocation"
            ))
        })?;
        let descriptor = self
            .module_registry
            .authorized_primitive(module_id, name)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        if descriptor.arguments.len() > 10 {
            return Err(RuntimeError::Program(format!(
                "primitive {name} exceeds the checked register argument limit"
            )));
        }
        for (index, kind) in descriptor.arguments.iter().enumerate() {
            let value = context.registers[index];
            let valid = match kind {
                RegisterKind::Unsigned { bits } if *bits < 32 => value < (1_u32 << bits),
                RegisterKind::Unsigned { .. } | RegisterKind::LogicalAddress { .. } => true,
                RegisterKind::Signed { bits: 32 } => true,
                RegisterKind::Signed { bits } if *bits < 32 => {
                    let signed = value as i32;
                    let bound = 1_i32 << (*bits - 1);
                    (-bound..bound).contains(&signed)
                }
                RegisterKind::Signed { .. } => true,
                RegisterKind::OpaqueHandle { type_name } => {
                    let index = u8::try_from(index).unwrap_or(u8::MAX);
                    let requirement = descriptor
                        .resource_requirements
                        .iter()
                        .find(|requirement| requirement.argument_register == index)
                        .ok_or_else(|| {
                            RuntimeError::Program(format!(
                                "primitive {name} has no resource-right contract for R{index}"
                            ))
                        })?;
                    if !requirement.type_name.eq_ignore_ascii_case(type_name) {
                        return Err(RuntimeError::Program(format!(
                            "primitive {name} has inconsistent resource handle type metadata"
                        )));
                    }
                    self.validate_resource_handle_access(
                        value,
                        type_name,
                        task.id,
                        requirement.right,
                    )?;
                    true
                }
            };
            if !valid {
                return Err(RuntimeError::Program(format!(
                    "primitive {name} argument R{index} violates {kind:?}"
                )));
            }
        }
        match name.to_ascii_uppercase().as_str() {
            "HOST.WIMP.OPENWINDOW" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program("Wimp window service is unavailable".into())
                })?;
                wimp.open_window_values(
                    task,
                    context.registers[R0],
                    WorkArea {
                        min_x: context.registers[R1] as i32,
                        min_y: context.registers[R2] as i32,
                        max_x: context.registers[R3] as i32,
                        max_y: context.registers[R4] as i32,
                    },
                    context.registers[R5] as i32,
                    context.registers[R6] as i32,
                    context.registers[R7] as i32,
                )
            }
            "HOST.WIMP.CLOSEWINDOW" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program("Wimp window service is unavailable".into())
                })?;
                wimp.close_window_handle(task, context.registers[R0])
            }
            "HOST.WIMP.READWINDOWSTATE" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program("Wimp window service is unavailable".into())
                })?;
                let values = wimp.window_state_values(task, context.registers[R0])?;
                context.registers[R0] = values[1];
                context.registers[R1] = values[2];
                context.registers[R2] = values[3];
                context.registers[R3] = values[4];
                context.registers[R4] = values[5];
                context.registers[R5] = values[6];
                context.registers[R6] = values[7];
                context.registers[R7] = values[8];
                Ok(())
            }
            "HOST.WIMP.SETEXTENT" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program("Wimp window service is unavailable".into())
                })?;
                wimp.set_extent_values(
                    task,
                    context.registers[R0],
                    WorkArea {
                        min_x: context.registers[R1] as i32,
                        min_y: context.registers[R2] as i32,
                        max_x: context.registers[R3] as i32,
                        max_y: context.registers[R4] as i32,
                    },
                )
            }
            "HOST.WIMP.VALIDATEINITIALISEINPUTS" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program(
                        "Wimp task lifecycle requires the hosted Wimp service".into(),
                    )
                })?;
                wimp.validate_initialise_inputs(
                    task,
                    context.registers[R0],
                    context.registers[R1],
                    context.registers[R2],
                )
            }
            "HOST.WIMP.REGISTERTASK" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program(
                        "Wimp task lifecycle requires the hosted Wimp service".into(),
                    )
                })?;
                let (version, handle) = wimp.register_task(task)?;
                context.registers[R0] = version;
                context.registers[R1] = handle;
                Ok(())
            }
            "HOST.WIMP.CLOSETASK" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program(
                        "Wimp task lifecycle requires the hosted Wimp service".into(),
                    )
                })?;
                wimp.close_task_for_caller(task, context.registers[R0])
            }
            "HOST.WIMP.READSTARTTASKBYTE" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program(
                        "Wimp task lifecycle requires the hosted Wimp service".into(),
                    )
                })?;
                context.registers[R0] = u32::from(wimp.read_start_task_byte(
                    task,
                    context.registers[R0],
                    context.registers[R1],
                )?);
                Ok(())
            }
            "HOST.WIMP.QUEUECOMMANDSTASK" | "HOST.WIMP.QUEUEBASICWINDOWTASK" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program(
                        "Wimp task lifecycle requires the hosted Wimp service".into(),
                    )
                })?;
                let kind = if name.eq_ignore_ascii_case("Host.Wimp.QueueCommandsTask") {
                    DesktopTaskKind::Commands
                } else {
                    DesktopTaskKind::BasicWindow
                };
                context.registers[R0] = wimp.queue_launch_for_caller(task, kind, "")?;
                Ok(())
            }
            "HOST.WIMP.QUEUEBASICFILETASK" => {
                let wimp = self.wimp.as_ref().ok_or_else(|| {
                    RuntimeError::Program(
                        "Wimp task lifecycle requires the hosted Wimp service".into(),
                    )
                })?;
                let address = context.registers[R0]
                    .checked_add(context.registers[R1])
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                let bytes = task
                    .memory
                    .read_caller_data_bytes(address, context.registers[R2] as usize)?;
                let path = std::str::from_utf8(&bytes).map_err(|_| {
                    RuntimeError::Program("Wimp_StartTask path is not valid UTF-8".into())
                })?;
                context.registers[R0] =
                    wimp.queue_launch_for_caller(task, DesktopTaskKind::File, path)?;
                Ok(())
            }
            "HOST.FILESWITCH.SETDIRECTORY" => {
                let kind = context.registers[R0];
                let path = fs_control_guest_string(task, context.registers[R1], MAX_STRING_BYTES)?;
                let path = String::from_utf8(path).map_err(|_| {
                    RuntimeError::Program("FileSwitch path is not valid UTF-8".into())
                })?;
                match kind {
                    0 => self
                        .file_system
                        .set_current_directory(
                            &mut task.file_system,
                            if path.is_empty() { "&" } else { &path },
                        )
                        .map_err(|_| file_switch_guest_error("directory selection")),
                    1 => {
                        let target = if path.is_empty() { "$.Library" } else { &path };
                        match self
                            .file_system
                            .set_library_directory(&mut task.file_system, target)
                            .map_err(|_| file_switch_guest_error("library selection"))
                        {
                            Ok(()) => Ok(()),
                            Err(_) if path.is_empty() => self
                                .file_system
                                .set_library_directory(&mut task.file_system, "@")
                                .map_err(|_| file_switch_guest_error("library selection")),
                            Err(error) => Err(error),
                        }
                    }
                    39 => self
                        .file_system
                        .set_user_root(&mut task.file_system, &path)
                        .map_err(|_| file_switch_guest_error("user-root selection")),
                    _ => Err(RuntimeError::Program(
                        "unsupported FileSwitch directory kind".into(),
                    )),
                }
            }
            "HOST.FILESWITCH.UNSETDIRECTORY" => {
                match context.registers[R0] {
                    0 => task.file_system.current_directory.clear(),
                    1 => task.file_system.user_root.clear(),
                    2 => task.file_system.library_directory.clear(),
                    _ => {
                        return Err(RuntimeError::Program(
                            "unsupported FileSwitch directory kind".into(),
                        ));
                    }
                }
                Ok(())
            }
            "HOST.FILESWITCH.SWAPDIRECTORIES" => {
                std::mem::swap(
                    &mut task.file_system.current_directory,
                    &mut task.file_system.previous_directory,
                );
                Ok(())
            }
            "HOST.FILESWITCH.SETTEMPORARYFROMPREFIX" => {
                let address = context.registers[R0];
                let bytes = fs_control_guest_string(task, address, MAX_STRING_BYTES)?;
                let path = String::from_utf8(bytes).map_err(|_| {
                    RuntimeError::Program("FileSwitch prefix is not valid UTF-8".into())
                })?;
                let Some(colon) = path.find(':') else {
                    context.registers[R0] = address;
                    context.registers[R1] = u32::MAX;
                    context.registers[R2] = 0;
                    return Ok(());
                };
                let file_system_name = &path[..colon];
                if file_system_name.contains('#') {
                    return Err(file_switch_guest_error(
                        "temporary filing-system special fields are unsupported",
                    ));
                }
                if !self.file_system.check_file_system_name(file_system_name) {
                    return Err(file_switch_guest_error(
                        "temporary filing-system prefix is unsupported",
                    ));
                }
                let previous = if task.file_system.temporary_file_system.is_empty() {
                    0
                } else if task
                    .file_system
                    .temporary_file_system
                    .eq_ignore_ascii_case(self.file_system.file_system_name())
                {
                    HOST_FS_NUMBER
                } else {
                    return Err(file_switch_guest_error(
                        "previous temporary filing system is unsupported",
                    ));
                };
                let next = address
                    .checked_add(u32::try_from(colon + 1).map_err(|_| {
                        RuntimeError::Program("FileSwitch prefix exceeds U32".into())
                    })?)
                    .ok_or_else(|| RuntimeError::Program("FileSwitch pointer overflow".into()))?;
                task.file_system.temporary_file_system = file_system_name.to_ascii_uppercase();
                context.registers[R0] = next;
                context.registers[R1] = previous;
                context.registers[R2] = 0;
                Ok(())
            }
            "HOST.FILESWITCH.READPATHVARIABLE" => {
                let raw_name =
                    fs_control_guest_string(task, context.registers[R0], MAX_NAME_BYTES)?;
                let name = std::str::from_utf8(&raw_name)
                    .map_err(|_| file_switch_guest_error("path-variable name is not ASCII"))?;
                validate_file_path_variable_name(name)
                    .map_err(|_| file_switch_guest_error("path-variable name is invalid"))?;
                match self.system_variables.read(name, None) {
                    Ok(variable) => {
                        if !matches!(
                            variable.variable_type,
                            SystemVariableType::String | SystemVariableType::LiteralString
                        ) {
                            return Err(file_switch_guest_error(
                                "path-variable type is unsupported",
                            ));
                        }
                        if variable.value.len() > 255 {
                            return Err(file_switch_guest_error(
                                "path-variable value exceeds 255 bytes",
                            ));
                        }
                        write_checked_guest_string(task, context.registers[R1], &variable.value)?;
                        context.registers[R0] = 1;
                        context.registers[R1] = variable.value.len() as u32;
                    }
                    Err(error) if is_system_variable_not_found(&error) => {
                        context.registers[R0] = 0;
                        context.registers[R1] = 0;
                    }
                    Err(_) => {
                        return Err(file_switch_guest_error("path-variable lookup"));
                    }
                }
                Ok(())
            }
            "HOST.FILESWITCH.READPATHSPECIFICATION" => {
                let bytes = read_control_terminated_path_spec(task, context.registers[R0], 255)?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| file_switch_guest_error("path specification is not UTF-8"))?;
                if text.len() > 255 {
                    return Err(file_switch_guest_error(
                        "path specification exceeds 255 bytes",
                    ));
                }
                write_checked_guest_string(task, context.registers[R1], &text)?;
                context.registers[R0] = text.len() as u32;
                Ok(())
            }
            "HOST.FILESWITCH.READPATHNAME" => {
                let bytes = fs_control_guest_string(task, context.registers[R0], MAX_STRING_BYTES)?;
                let text = String::from_utf8(bytes)
                    .map_err(|_| file_switch_guest_error("pathname is not UTF-8"))?;
                if text.len() + 1 > MAX_STRING_BYTES {
                    return Err(file_switch_guest_error("pathname exceeds 4095 bytes"));
                }
                write_checked_guest_string(task, context.registers[R1], &text)?;
                context.registers[R0] = text.len() as u32;
                Ok(())
            }
            "HOST.FILESWITCH.CANONICALPATHEXISTS" => {
                let raw = fs_control_guest_string(task, context.registers[R0], MAX_STRING_BYTES)?;
                let path = String::from_utf8(raw)
                    .map_err(|_| file_switch_guest_error("pathname is not UTF-8"))?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                    .map_err(|_| file_switch_guest_error("canonical candidate lookup"))?;
                context.registers[R0] = u32::from(resolved.host_path.exists());
                Ok(())
            }
            "HOST.FILESWITCH.RESTORETEMPORARY" => {
                task.file_system.temporary_file_system =
                    task.file_system.current_file_system.to_ascii_uppercase();
                Ok(())
            }
            "HOST.FILESWITCH.PROBEFILESYSTEMNUMBER" => {
                let number = context.registers[R0];
                let present = number == HOST_FS_NUMBER;
                context.registers[R0] = u32::from(present);
                context.registers[R1] = if present { HOST_FS_CONTROL_BLOCK } else { 0 };
                Ok(())
            }
            "HOST.FILESWITCH.PROBEFILESYSTEMNAME" => {
                let raw = read_file_system_name(
                    task,
                    context.registers[R0],
                    MAX_NAME_BYTES,
                    context.registers[R1] != 0,
                )?;
                let name = String::from_utf8(raw).map_err(|_| {
                    RuntimeError::Program("filing system name is not valid UTF-8".into())
                })?;
                if name.is_empty() || name.chars().any(char::is_control) {
                    return Err(RuntimeError::Program(
                        "filing system name is malformed".into(),
                    ));
                }
                if context.registers[R1] == 0
                    && name.bytes().any(|byte| matches!(byte, b'#' | b':' | b'-'))
                {
                    return Err(RuntimeError::Program(
                        "filing system name contains an invalid terminator character".into(),
                    ));
                }
                let present = self.file_system.check_file_system_name(&name);
                context.registers[R0] = u32::from(present);
                context.registers[R1] = if present { HOST_FS_NUMBER } else { 0 };
                context.registers[R2] = if present { HOST_FS_CONTROL_BLOCK } else { 0 };
                Ok(())
            }
            "HOST.FILESWITCH.SELECTFILESYSTEM" => {
                let number = context.registers[R0];
                if number != HOST_FS_NUMBER {
                    return Err(RuntimeError::Program(format!(
                        "filing system number {number} is not present"
                    )));
                }
                task.file_system.current_file_system =
                    self.file_system.file_system_name().to_string();
                task.file_system.temporary_file_system =
                    task.file_system.current_file_system.clone();
                Ok(())
            }
            "HOST.FILESWITCH.CLEARFILESYSTEMSELECTION" => {
                task.file_system.current_file_system.clear();
                task.file_system.temporary_file_system.clear();
                Ok(())
            }
            "HOST.FILESWITCH.CLOSEALLCHANNELS" => {
                task.file_system.open_files.clear();
                Ok(())
            }
            "HOST.FILESWITCH.RENAMEOBJECT" => {
                let from = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R0],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| RuntimeError::Program("source pathname is not valid UTF-8".into()))?;
                let to = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R1],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| {
                    RuntimeError::Program("destination pathname is not valid UTF-8".into())
                })?;
                self.file_object_validate_mutation(task, &from, true)?;
                self.file_object_validate_mutation(task, &to, true)?;
                self.file_system
                    .rename(&task.file_system, &from, &to)
                    .map_err(|_| file_object_error("rename"))
            }
            "HOST.FILESWITCH.SETVOLUMENAME" => {
                let object = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R0],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| RuntimeError::Program("object pathname is not valid UTF-8".into()))?;
                let name = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R1],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| RuntimeError::Program("volume name is not valid UTF-8".into()))?;
                if name.bytes().any(|byte| matches!(byte, b'/' | b'\\')) {
                    return Err(RuntimeError::Program("invalid volume name".into()));
                }
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &object)
                    .map_err(|_| file_switch_guest_error("volume-name object lookup"))?;
                if !resolved.host_path.exists() {
                    return Err(RuntimeError::Program(
                        "volume rename object was not found".into(),
                    ));
                }
                self.file_system.set_volume_name(&name).map_err(|_| {
                    RuntimeError::Program(
                        "volume name update failed in the guest filing system".into(),
                    )
                })
            }
            "HOST.FILESWITCH.FILETYPETEXT" => {
                let bytes = file_type_name(context.registers[R0]);
                context.registers[R0] = u32::from_le_bytes(bytes[..4].try_into().unwrap());
                context.registers[R1] = u32::from_le_bytes(bytes[4..].try_into().unwrap());
                Ok(())
            }
            "HOST.FILESWITCH.PARSEFILETYPE" => {
                let text =
                    String::from_utf8(fs_control_guest_string(task, context.registers[R0], 64)?)
                        .map_err(|_| {
                            RuntimeError::Program("file type text is not valid UTF-8".into())
                        })?;
                context.registers[R0] = parse_file_type(&text)?;
                Ok(())
            }
            "HOST.FILESWITCH.FILESYSTEMNAMELENGTH" => {
                context.registers[R0] = if context.registers[R0] == HOST_FS_NUMBER {
                    self.file_system.file_system_name().len() as u32
                } else {
                    0
                };
                Ok(())
            }
            "HOST.FILESWITCH.FILESYSTEMNAMEBYTE" => {
                let name = if context.registers[R0] == HOST_FS_NUMBER {
                    self.file_system.file_system_name().as_bytes()
                } else {
                    b""
                };
                context.registers[R0] =
                    u32::from(*name.get(context.registers[R1] as usize).ok_or_else(|| {
                        RuntimeError::Program("filesystem name byte is unavailable".into())
                    })?);
                Ok(())
            }
            "HOST.FILESWITCH.VOLUMENAMEWRITELENGTH" => {
                let capacity = context.registers[R0] as usize;
                let length = context.registers[R1] as usize;
                if capacity > 0 && capacity < length.saturating_add(1) {
                    return Err(RuntimeError::Program(
                        "filesystem name buffer is too small".into(),
                    ));
                }
                context.registers[R0] =
                    u32::try_from(if capacity == 0 { 0 } else { length }).unwrap_or(u32::MAX);
                Ok(())
            }
            "HOST.FILESWITCH.CANONICALPATHLENGTH" => {
                let path = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R0],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| RuntimeError::Program("pathname is not valid UTF-8".into()))?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                    .map_err(|_| file_switch_guest_error("canonical path lookup"))?;
                let canonical =
                    canonical_guest_name(self.file_system.volume_name(), &resolved.guest_path);
                context.registers[R0] = u32::try_from(canonical.len())
                    .map_err(|_| RuntimeError::Program("canonical name exceeds U32".into()))?;
                Ok(())
            }
            "HOST.FILESWITCH.CANONICALPATHBYTE" => {
                let path = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R0],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| RuntimeError::Program("pathname is not valid UTF-8".into()))?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                    .map_err(|_| file_switch_guest_error("canonical path lookup"))?;
                let canonical =
                    canonical_guest_name(self.file_system.volume_name(), &resolved.guest_path);
                context.registers[R0] = u32::from(
                    *canonical
                        .as_bytes()
                        .get(context.registers[R1] as usize)
                        .ok_or_else(|| {
                            RuntimeError::Program("canonical-name byte is unavailable".into())
                        })?,
                );
                Ok(())
            }
            "HOST.FILESWITCH.CANONICALCAPACITYFITS" => {
                context.registers[R0] = u32::from(
                    (context.registers[R0] as i32 as u32)
                        >= context.registers[R1].saturating_add(1),
                );
                Ok(())
            }
            "HOST.FILESWITCH.CANONICALSPAREBYTES" => {
                context.registers[R0] = ((context.registers[R0] as i32 as u32)
                    .wrapping_sub(context.registers[R1])
                    as i32) as u32;
                Ok(())
            }
            "HOST.FILESWITCH.WRITECANONICALPATH" => {
                let path = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R0],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| RuntimeError::Program("pathname is not valid UTF-8".into()))?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                    .map_err(|_| file_switch_guest_error("canonical path lookup"))?;
                let canonical =
                    canonical_guest_name(self.file_system.volume_name(), &resolved.guest_path);
                if canonical.len() as u32 != context.registers[R2] {
                    return Err(RuntimeError::Program(
                        "canonical-name length changed".into(),
                    ));
                }
                let mut bytes = canonical.into_bytes();
                bytes.push(0);
                task.memory
                    .write_caller_data_bytes(context.registers[R1], &bytes)?;
                Ok(())
            }
            "HOST.FILESWITCH.OPENCATALOGUESNAPSHOT" => {
                let directory = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R0],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| file_switch_guest_error("catalogue directory is not UTF-8"))?;
                let wildcard = String::from_utf8(fs_control_guest_string(
                    task,
                    context.registers[R1],
                    MAX_STRING_BYTES,
                )?)
                .map_err(|_| file_switch_guest_error("catalogue wildcard is not UTF-8"))?;
                let entries = self
                    .file_system
                    .enumerate_bounded(&task.file_system, &directory, &wildcard, 4096)
                    .map_err(|_| file_switch_guest_error("catalogue lookup"))?;
                let mut bytes = 0usize;
                for item in &entries {
                    bytes = bytes
                        .checked_add(item.guest_name.len() + 24)
                        .ok_or_else(|| RuntimeError::Program("catalogue size overflow".into()))?;
                }
                if bytes > crate::memory::GUEST_MEMORY_SIZE {
                    return Err(RuntimeError::Program(
                        "catalogue exceeds hosted bound".into(),
                    ));
                }
                let id = task
                    .file_system
                    .gbpb_directory_snapshot_id
                    .wrapping_add(1)
                    .max(1);
                task.file_system.gbpb_directory_snapshot = entries;
                task.file_system.gbpb_directory_snapshot_id = id;
                context.registers[R0] = id;
                context.registers[R1] = task.file_system.gbpb_directory_snapshot.len() as u32;
                Ok(())
            }
            "HOST.GRAPHICS.ACCEPTBYTE" => self.vdu_accept_byte(task, context),
            "HOST.GRAPHICS.SETPACKEDRGB" => {
                self.graphics_set_packed_rgb_for_task(task, context.registers[R0])
            }
            "HOST.GRAPHICS.PLOT" => self.graphics_plot_for_task(
                task,
                context.registers[R0] as u8,
                context.registers[R1] as i32,
                context.registers[R2] as i32,
            ),
            "HOST.GRAPHICS.READPOINT" => {
                let x = context.registers[R0] as i32;
                let y = context.registers[R1] as i32;
                let window = self.graphics_window_for_task(task.id);
                let graphics = match window {
                    Some(handle) => self.window_graphics.get(&handle).ok_or_else(|| {
                        RuntimeError::Program(
                            "active caller graphics window has no raster context".into(),
                        )
                    })?,
                    None if self.task_uses_shared_default_graphics(task.id) => &self.graphics,
                    None => self.task_default_graphics_mut(task.id)?,
                };
                let modern = graphics.snapshot().text_profile == TextRenderingProfile::Modern;
                let point = graphics.read_point(x, y);
                if let Some((colour, tint)) = point {
                    context.registers[R0] = colour;
                    context.registers[R1] = tint;
                    context.registers[R2] = 1;
                } else {
                    context.registers[R0] = 0;
                    context.registers[R1] = 0;
                    context.registers[R2] = 0;
                }
                context.registers[R3] = u32::from(modern);
                Ok(())
            }
            "HOST.DESKTOP.READCATALOGUEENTRY" => {
                let path = read_desktop_guest_path(task, context.registers[R0], MAX_STRING_BYTES)?;
                let path = String::from_utf8(path).map_err(|_| {
                    RuntimeError::Program("desktop guest directory is not valid UTF-8".into())
                })?;
                let capacity = context.registers[R3];
                if capacity == 0 || capacity as usize > MAX_STRING_BYTES {
                    return Err(RuntimeError::Program(
                        "desktop entry buffer size is outside the hosted limit".into(),
                    ));
                }
                let entries = self
                    .file_system
                    .enumerate_bounded(&task.file_system, &path, "*", 4096)
                    .map_err(|_| {
                        RuntimeError::Program(format!(
                            "desktop catalogue lookup failed for guest path '{path}'"
                        ))
                    })?;
                let Some(entry) = entries.get(context.registers[R1] as usize) else {
                    task.memory
                        .write_caller_data_bytes(context.registers[R2], &[0])?;
                    context.registers[R0] = 0;
                    context.registers[R1] = 0;
                    context.registers[R2] = 0;
                    context.registers[R3] = 0;
                    return Ok(());
                };
                if entry
                    .guest_name
                    .len()
                    .checked_add(1)
                    .is_none_or(|size| size > capacity as usize)
                {
                    return Err(RuntimeError::Program(
                        "desktop entry name does not fit the caller buffer".into(),
                    ));
                }
                let mut bytes = entry.guest_name.as_bytes().to_vec();
                bytes.push(0);
                task.memory
                    .validate_caller_data_span(context.registers[R2], bytes.len())?;
                task.memory
                    .write_caller_data_bytes(context.registers[R2], &bytes)?;
                context.registers[R0] = 1;
                context.registers[R1] = u32::from(entry.is_directory);
                context.registers[R2] = entry.metadata.file_type;
                context.registers[R3] = entry.length;
                Ok(())
            }
            "HOST.DESKTOP.WRITEVOLUMENAME" => {
                let capacity = context.registers[R1];
                let name = self.file_system.volume_name();
                let required = name.len().checked_add(1).ok_or_else(|| {
                    RuntimeError::Program("desktop volume name length overflowed".into())
                })?;
                if capacity as usize > MAX_STRING_BYTES || (capacity as usize) < required {
                    return Err(RuntimeError::Program(
                        "desktop volume name does not fit the caller buffer".into(),
                    ));
                }
                let mut bytes = name.as_bytes().to_vec();
                bytes.push(0);
                task.memory
                    .validate_caller_data_span(context.registers[R0], bytes.len())?;
                task.memory
                    .write_caller_data_bytes(context.registers[R0], &bytes)?;
                context.registers[R0] = u32::try_from(name.len()).unwrap_or(u32::MAX);
                Ok(())
            }
            "HOST.DESKTOP.READMODIFICATIONTIME" => {
                let path = read_desktop_guest_path(task, context.registers[R0], MAX_STRING_BYTES)?;
                let path = String::from_utf8(path).map_err(|_| {
                    RuntimeError::Program("desktop guest directory is not valid UTF-8".into())
                })?;
                let entries = self
                    .file_system
                    .enumerate_bounded(&task.file_system, &path, "*", 4096)
                    .map_err(|_| {
                        RuntimeError::Program(format!(
                            "desktop catalogue lookup failed for guest path '{path}'"
                        ))
                    })?;
                let modified = entries
                    .get(context.registers[R1] as usize)
                    .and_then(|entry| {
                        std::fs::metadata(&entry.host_path)
                            .ok()?
                            .modified()
                            .ok()?
                            .duration_since(std::time::UNIX_EPOCH)
                            .ok()
                            .and_then(|duration| u32::try_from(duration.as_secs()).ok())
                    });
                context.registers[R0] = u32::from(modified.is_some());
                context.registers[R1] = modified.unwrap_or(0);
                Ok(())
            }
            "HOST.DESKTOP.REGISTERSYSTEMMENU" => self
                .wimp
                .as_ref()
                .ok_or_else(|| RuntimeError::Program("desktop menu service is unavailable".into()))?
                .register_system_menu(task.id),
            "HOST.DISPLAY.QUERY" => {
                let wimp = self
                    .wimp
                    .as_ref()
                    .or(self.desktop_service.as_ref())
                    .ok_or_else(|| {
                        RuntimeError::Program(
                            "display settings require the hosted Wimp desktop".into(),
                        )
                    })?;
                let mut query = SwiContext::default();
                write_display_query(wimp, &mut query);
                context.registers[R0] = query.registers[R2];
                context.registers[R1] = query.registers[R3];
                context.registers[R2] = query.registers[R4];
                context.registers[R3] = query.registers[R5];
                context.registers[R4] = query.registers[R6];
                context.registers[R5] = query.registers[R7];
                Ok(())
            }
            "HOST.DISPLAY.APPLY" => {
                task.require_configuration_write()?;
                let resolution_id = context.registers[R0];
                let colour_id = context.registers[R1];
                let resolution = DesktopResolution::from_id(resolution_id).ok_or_else(|| {
                    RuntimeError::Program("display resolution ID is invalid".into())
                })?;
                let colour = DisplayColour::from_id(colour_id)
                    .ok_or_else(|| RuntimeError::Program("display colour ID is invalid".into()))?;
                let wimp = self
                    .wimp
                    .as_ref()
                    .or(self.desktop_service.as_ref())
                    .ok_or_else(|| {
                        RuntimeError::Program(
                            "display settings require the hosted Wimp desktop".into(),
                        )
                    })?;
                let saved = wimp
                    .apply_display_settings(DisplaySettings { resolution, colour })
                    .is_ok();
                let mut query = SwiContext::default();
                write_display_query(wimp, &mut query);
                context.registers[R0] = u32::from(!saved);
                context.registers[R1] = query.registers[R2];
                context.registers[R2] = query.registers[R3];
                context.registers[R3] = query.registers[R4];
                context.registers[R4] = query.registers[R5];
                context.registers[R5] = query.registers[R6];
                context.registers[R6] = query.registers[R7];
                Ok(())
            }
            "HOST.DISPLAY.REQUIRECONFIGURATIONWRITE" => {
                task.require_configuration_write()?;
                Ok(())
            }
            "HOST.CONSOLE.WRITEBYTE" => {
                self.console.write_byte(context.registers[R0] as u8)?;
                self.console.flush().map_err(RuntimeError::from)
            }
            "HOST.CONSOLE.READBYTESTATUS" => self.console_read_byte_status(task, context),
            "HOST.MOSINPUT.INSERTKEY" => {
                context.registers[R0] = u32::from(self.mos.insert_key(context.registers[R0] as u8));
                Ok(())
            }
            "HOST.MOSINPUT.FLUSHKEYBOARD" => {
                self.mos.flush_keyboard(&self.console);
                Ok(())
            }
            "HOST.MOSINPUT.READTIMED" => {
                let timeout = context.registers[R0] as u16;
                let (key, status) = self.mos.read_timed_key(task, &self.console, timeout);
                context.registers[R0] = u32::from(key);
                context.registers[R1] = u32::from(status);
                Ok(())
            }
            "HOST.CLOCK.READSYSTEMCHUNKS" => {
                let clock = self.mos.system_clock.read();
                context.registers[R0] = (clock & 0xFFFF) as u32;
                context.registers[R1] = ((clock >> 16) & 0xFFFF) as u32;
                context.registers[R2] = ((clock >> 32) & 0xFF) as u32;
                Ok(())
            }
            "HOST.CLOCK.WRITESYSTEMCHUNKS" => {
                let clock = clock_from_chunks(context);
                self.mos.system_clock.set(clock);
                Ok(())
            }
            "HOST.CLOCK.READINTERVALCHUNKS" => {
                let clock = self.mos.interval_timer.read();
                context.registers[R0] = (clock & 0xFFFF) as u32;
                context.registers[R1] = ((clock >> 16) & 0xFFFF) as u32;
                context.registers[R2] = ((clock >> 32) & 0xFF) as u32;
                Ok(())
            }
            "HOST.CLOCK.WRITEINTERVALCHUNKS" => {
                let clock = clock_from_chunks(context);
                self.mos.interval_timer.set(clock);
                Ok(())
            }
            "HOST.MEMORY.READFIVEBYTES" => {
                let bytes = task
                    .memory
                    .read_caller_data_bytes(context.registers[R0], 5)?;
                for (register, byte) in bytes.into_iter().enumerate() {
                    context.registers[register] = u32::from(byte);
                }
                Ok(())
            }
            "HOST.MEMORY.WRITEFIVEBYTES" => {
                let bytes = context.registers[R1..=R5]
                    .iter()
                    .map(|byte| *byte as u8)
                    .collect::<Vec<_>>();
                task.memory
                    .write_caller_data_bytes(context.registers[R0], &bytes)?;
                Ok(())
            }
            "HOST.FILECHANNEL.OPEN" => {
                if !task.file_system.has_file_slot() {
                    return Err(RuntimeError::Program(
                        "no FileSwitch handles are available".into(),
                    ));
                }
                let address = context.registers[R0];
                let mode = context.registers[R1];
                let path_bytes = mos::read_mos_string(task, address, MAX_STRING_BYTES)?;
                let path = String::from_utf8(path_bytes).map_err(|_| {
                    RuntimeError::Program("guest pathname is not valid UTF-8".into())
                })?;
                let (read, write, create, truncate) = match mode {
                    0 => (true, false, false, false),
                    1 => (true, true, true, true),
                    2 => (true, true, false, false),
                    _ => {
                        return Err(RuntimeError::Program(
                            "unsupported FileSwitch open mode".into(),
                        ));
                    }
                };
                match self.file_system.open_file(
                    &task.file_system,
                    &path,
                    read,
                    write,
                    create,
                    truncate,
                ) {
                    Ok((file, resolved)) => {
                        let handle = task
                            .file_system
                            .insert_file(OpenFile {
                                file,
                                can_read: read,
                                can_write: write,
                                guest_path: resolved.guest_path,
                                eof_error_next: false,
                            })
                            .ok_or_else(|| {
                                RuntimeError::Program("no FileSwitch handles are available".into())
                            })?;
                        context.registers[R0] = handle;
                        context.registers[R1] = 0;
                        Ok(())
                    }
                    Err(RuntimeError::Io(error))
                        if error.kind() == std::io::ErrorKind::NotFound && mode != 1 =>
                    {
                        context.registers[R0] = 0;
                        context.registers[R1] = 1;
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            "HOST.FILECHANNEL.CLOSE" => {
                let handle = context.registers[R0];
                if handle == 0 {
                    task.file_system.open_files.clear();
                } else {
                    task.file_system.open_files.remove(&handle);
                }
                Ok(())
            }
            "HOST.FILECHANNEL.READBYTE" => {
                let handle = context.registers[R0];
                let file = task
                    .file_system
                    .open_files
                    .get_mut(&handle)
                    .ok_or_else(|| {
                        RuntimeError::Program(format!("invalid file handle {handle}"))
                    })?;
                if !file.can_read {
                    return Err(RuntimeError::Program("file is not open for reading".into()));
                }
                if file.eof_error_next {
                    file.eof_error_next = false;
                    return Err(RuntimeError::Program("end of file".into()));
                }
                let mut byte = [0_u8; 1];
                if file.file.read(&mut byte)? == 0 {
                    file.eof_error_next = true;
                    context.registers[R0] = 0;
                    context.registers[R1] = 1;
                } else {
                    context.registers[R0] = u32::from(byte[0]);
                    context.registers[R1] = 0;
                }
                Ok(())
            }
            "HOST.FILECHANNEL.WRITEBYTE" => {
                let handle = context.registers[R0];
                let byte = context.registers[R1] as u8;
                let file = task
                    .file_system
                    .open_files
                    .get_mut(&handle)
                    .ok_or_else(|| {
                        RuntimeError::Program(format!("invalid file handle {handle}"))
                    })?;
                if !file.can_write {
                    return Err(RuntimeError::Program("file is not open for writing".into()));
                }
                file.file.write_all(&[byte])?;
                file.eof_error_next = false;
                Ok(())
            }
            "HOST.FILECHANNEL.READPOSITION" => {
                let handle = context.registers[R0];
                let file = task
                    .file_system
                    .open_files
                    .get_mut(&handle)
                    .ok_or_else(|| {
                        RuntimeError::Program(format!("invalid file handle {handle}"))
                    })?;
                context.registers[R0] =
                    u32::try_from(file.file.stream_position()?).map_err(|_| {
                        RuntimeError::Program("file position exceeds the hosted U32 range".into())
                    })?;
                Ok(())
            }
            "HOST.FILECHANNEL.SETPOSITION" => {
                let handle = context.registers[R0];
                let position = context.registers[R1];
                let file = task
                    .file_system
                    .open_files
                    .get_mut(&handle)
                    .ok_or_else(|| {
                        RuntimeError::Program(format!("invalid file handle {handle}"))
                    })?;
                let extent = file.file.metadata()?.len();
                if u64::from(position) > extent {
                    if !file.can_write {
                        return Err(RuntimeError::Program("file is not open for writing".into()));
                    }
                    file.file.set_len(u64::from(position))?;
                }
                file.file.seek(SeekFrom::Start(u64::from(position)))?;
                file.eof_error_next = false;
                Ok(())
            }
            "HOST.FILECHANNEL.READEXTENT" => {
                let handle = context.registers[R0];
                let file = task.file_system.open_files.get(&handle).ok_or_else(|| {
                    RuntimeError::Program(format!("invalid file handle {handle}"))
                })?;
                context.registers[R0] =
                    u32::try_from(file.file.metadata()?.len()).map_err(|_| {
                        RuntimeError::Program("file extent exceeds the hosted U32 range".into())
                    })?;
                Ok(())
            }
            "HOST.FILECHANNEL.SETEXTENT" => {
                let handle = context.registers[R0];
                let extent = context.registers[R1];
                let file = task
                    .file_system
                    .open_files
                    .get_mut(&handle)
                    .ok_or_else(|| {
                        RuntimeError::Program(format!("invalid file handle {handle}"))
                    })?;
                if !file.can_write {
                    return Err(RuntimeError::Program("file is not open for writing".into()));
                }
                file.file.set_len(u64::from(extent))?;
                if file.file.stream_position()? > u64::from(extent) {
                    file.file.seek(SeekFrom::Start(u64::from(extent)))?;
                }
                file.eof_error_next = false;
                Ok(())
            }
            "HOST.FILECHANNEL.CANONICALNAMELENGTH" => {
                let handle = context.registers[R0];
                let file = task.file_system.open_files.get(&handle).ok_or_else(|| {
                    RuntimeError::Program(format!("invalid file handle {handle}"))
                })?;
                let name = canonical_guest_name(self.file_system.volume_name(), &file.guest_path);
                let length = u32::try_from(name.len()).map_err(|_| {
                    RuntimeError::Program("canonical guest name is too long".into())
                })?;
                if length >= MAX_STRING_BYTES as u32 {
                    return Err(RuntimeError::Program(
                        "canonical guest name exceeds the hosted bound".into(),
                    ));
                }
                context.registers[R0] = length;
                Ok(())
            }
            "HOST.FILECHANNEL.ARGS7SPAREBYTES" => {
                context.registers[R0] = context.registers[R0].wrapping_sub(context.registers[R1]);
                Ok(())
            }
            "HOST.FILECHANNEL.WRITECANONICALNAME" => {
                let handle = context.registers[R0];
                let address = context.registers[R1];
                let file = task.file_system.open_files.get(&handle).ok_or_else(|| {
                    RuntimeError::Program(format!("invalid file handle {handle}"))
                })?;
                let name = canonical_guest_name(self.file_system.volume_name(), &file.guest_path);
                let mut bytes = name.into_bytes();
                if bytes.len() >= MAX_STRING_BYTES {
                    return Err(RuntimeError::Program(
                        "canonical guest name exceeds the hosted bound".into(),
                    ));
                }
                bytes.push(0);
                task.memory.write_caller_data_bytes(address, &bytes)?;
                Ok(())
            }
            "HOST.FILECHANNEL.VALIDATETRANSFERSPAN" => {
                let address = context.registers[R0];
                let length = context.registers[R1];
                if length as usize > crate::memory::GUEST_MEMORY_SIZE {
                    return Err(RuntimeError::Program(
                        "OS_GBPB transfer exceeds the hosted 1 MiB limit".into(),
                    ));
                }
                task.memory
                    .validate_caller_data_span(address, length as usize)?;
                address.checked_add(length).ok_or_else(|| {
                    RuntimeError::Program("OS_GBPB caller buffer range overflowed".into())
                })?;
                Ok(())
            }
            "HOST.FILECHANNEL.VALIDATEDIRECTORYINFOADDRESS" => {
                if context.registers[R0] & 3 != 0 {
                    return Err(RuntimeError::Program(
                        "OS_GBPB reason 10 requires a word-aligned output buffer".into(),
                    ));
                }
                Ok(())
            }
            "HOST.FILECHANNEL.VALIDATETRANSFERRANGE" => {
                let handle = context.registers[R0];
                let offset = context.registers[R1];
                let length = context.registers[R2];
                let writing = context.registers[R3] != 0;
                offset.checked_add(length).ok_or_else(|| {
                    RuntimeError::Program(
                        "OS_GBPB file range exceeds the U32 position limit".into(),
                    )
                })?;
                let file = task.file_system.open_files.get(&handle).ok_or_else(|| {
                    RuntimeError::Program(format!("invalid file handle {handle}"))
                })?;
                if writing && !file.can_write {
                    return Err(RuntimeError::Program("file is not open for writing".into()));
                }
                if !writing && !file.can_read {
                    return Err(RuntimeError::Program("file is not open for reading".into()));
                }
                let end = offset.checked_add(length).ok_or_else(|| {
                    RuntimeError::Program(
                        "OS_GBPB file range exceeds the U32 position limit".into(),
                    )
                })?;
                context.registers[R0] =
                    u32::from(!writing && u64::from(offset) > file.file.metadata()?.len());
                context.registers[R1] = end;
                Ok(())
            }
            "HOST.FILECHANNEL.VALIDATETRANSFERRANGERAW" => {
                let handle = context.registers[R0];
                let offset = context.registers[R1];
                let length = context.registers[R2];
                let writing = context.registers[R3] != 0;
                let end = offset.checked_add(length).ok_or_else(|| {
                    RuntimeError::Program(
                        "OS_GBPB file range exceeds the U32 position limit".into(),
                    )
                })?;
                let file = task.file_system.open_files.get(&handle).ok_or_else(|| {
                    RuntimeError::Program(format!("invalid file handle {handle}"))
                })?;
                if writing && !file.can_write {
                    return Err(RuntimeError::Program("file is not open for writing".into()));
                }
                if !writing && !file.can_read {
                    return Err(RuntimeError::Program("file is not open for reading".into()));
                }
                context.registers[R0] =
                    u32::from(!writing && u64::from(offset) > file.file.metadata()?.len());
                context.registers[R1] = end;
                Ok(())
            }
            "HOST.FILECHANNEL.READTRANSFER" => {
                let handle = context.registers[R0];
                let address = context.registers[R1];
                let requested = context.registers[R2] as usize;
                if requested > crate::memory::GUEST_MEMORY_SIZE {
                    return Err(RuntimeError::Program(
                        "OS_GBPB transfer exceeds the hosted 1 MiB limit".into(),
                    ));
                }
                task.memory.validate_caller_data_span(address, requested)?;
                let file = task
                    .file_system
                    .open_files
                    .get_mut(&handle)
                    .ok_or_else(|| {
                        RuntimeError::Program(format!("invalid file handle {handle}"))
                    })?;
                if !file.can_read {
                    return Err(RuntimeError::Program("file is not open for reading".into()));
                }
                let mut bytes = vec![0; requested];
                let transferred = if requested == 0 {
                    0
                } else {
                    file.file.read(&mut bytes)?
                };
                bytes.truncate(transferred);
                task.memory.write_caller_data_bytes(address, &bytes)?;
                file.eof_error_next = false;
                context.registers[R0] = u32::try_from(transferred)
                    .map_err(|_| RuntimeError::Program("OS_GBPB byte count exceeds U32".into()))?;
                Ok(())
            }
            "HOST.FILECHANNEL.WRITETRANSFER" => {
                let handle = context.registers[R0];
                let address = context.registers[R1];
                let length = context.registers[R2] as usize;
                if length > crate::memory::GUEST_MEMORY_SIZE {
                    return Err(RuntimeError::Program(
                        "OS_GBPB transfer exceeds the hosted 1 MiB limit".into(),
                    ));
                }
                let bytes = task.memory.read_caller_data_bytes(address, length)?;
                let file = task
                    .file_system
                    .open_files
                    .get_mut(&handle)
                    .ok_or_else(|| {
                        RuntimeError::Program(format!("invalid file handle {handle}"))
                    })?;
                if !file.can_write {
                    return Err(RuntimeError::Program("file is not open for writing".into()));
                }
                if !bytes.is_empty() {
                    file.file.write_all(&bytes)?;
                }
                file.eof_error_next = false;
                Ok(())
            }
            "HOST.FILECHANNEL.FIXEDNAMELENGTH" => {
                let kind = context.registers[R0] as u8;
                let bytes = fixed_file_switch_name(self, task, kind)?;
                context.registers[R0] = u32::try_from(bytes.len())
                    .map_err(|_| RuntimeError::Program("FileSwitch name exceeds U32".into()))?;
                Ok(())
            }
            "HOST.FILECHANNEL.FIXEDNAMEBYTE" => {
                let kind = context.registers[R0] as u8;
                let offset = context.registers[R1] as usize;
                let bytes = fixed_file_switch_name(self, task, kind)?;
                context.registers[R0] = u32::from(*bytes.get(offset).ok_or_else(|| {
                    RuntimeError::Program("FileSwitch name byte offset is outside the name".into())
                })?);
                Ok(())
            }
            "HOST.FILECHANNEL.OPENDIRECTORYSNAPSHOT" => {
                let path = if context.registers[R0] == 0 {
                    String::new()
                } else {
                    String::from_utf8(read_caller_guest_string(
                        task,
                        context.registers[R0],
                        MAX_STRING_BYTES,
                    )?)
                    .map_err(|_| {
                        RuntimeError::Program("guest directory is not valid UTF-8".into())
                    })?
                };
                let wildcard = if context.registers[R1] == 0 {
                    "*".to_string()
                } else {
                    String::from_utf8(read_caller_guest_string(
                        task,
                        context.registers[R1],
                        MAX_STRING_BYTES,
                    )?)
                    .map_err(|_| {
                        RuntimeError::Program("guest wildcard is not valid UTF-8".into())
                    })?
                };
                let entries = self.file_system.enumerate_bounded(
                    &task.file_system,
                    if path.is_empty() { "@" } else { &path },
                    &wildcard,
                    4096,
                )?;
                let mut staged_bytes = 0usize;
                for entry in &entries {
                    if entry.guest_name.len() > u8::MAX as usize {
                        return Err(RuntimeError::Program("guest leaf name is too long".into()));
                    }
                    staged_bytes = staged_bytes
                        .checked_add(entry.guest_name.len().saturating_add(24))
                        .ok_or_else(|| {
                            RuntimeError::Program("catalogue snapshot size overflowed".into())
                        })?;
                    if staged_bytes > crate::memory::GUEST_MEMORY_SIZE {
                        return Err(RuntimeError::Program(
                            "directory exceeds the hosted 1 MiB catalogue bound".into(),
                        ));
                    }
                }
                let snapshot_id = task
                    .file_system
                    .gbpb_directory_snapshot_id
                    .wrapping_add(1)
                    .max(1);
                task.file_system.gbpb_directory_snapshot = entries;
                task.file_system.gbpb_directory_snapshot_id = snapshot_id;
                context.registers[R0] = snapshot_id;
                context.registers[R1] = u32::try_from(
                    task.file_system.gbpb_directory_snapshot.len(),
                )
                .map_err(|_| RuntimeError::Program("directory entry count exceeds U32".into()))?;
                Ok(())
            }
            "HOST.FILECHANNEL.READDIRECTORYENTRY" => {
                let snapshot_id = context.registers[R0];
                let index = usize::try_from(context.registers[R1]).map_err(|_| {
                    RuntimeError::Program("directory continuation exceeds host range".into())
                })?;
                if snapshot_id != task.file_system.gbpb_directory_snapshot_id {
                    return Err(RuntimeError::Program(
                        "directory snapshot is no longer active".into(),
                    ));
                }
                let entry = task
                    .file_system
                    .gbpb_directory_snapshot
                    .get(index)
                    .ok_or_else(|| {
                        RuntimeError::Program(
                            "directory continuation is outside the snapshot".into(),
                        )
                    })?;
                context.registers[R0] = u32::try_from(entry.guest_name.len())
                    .map_err(|_| RuntimeError::Program("guest leaf name is too long".into()))?;
                context.registers[R1] = if entry.is_directory { 2 } else { 1 };
                Ok(())
            }
            "HOST.FILECHANNEL.NORMALIZEDIRECTORYSTART" => {
                let snapshot_id = context.registers[R0];
                if snapshot_id != task.file_system.gbpb_directory_snapshot_id {
                    return Err(RuntimeError::Program(
                        "directory snapshot is no longer active".into(),
                    ));
                }
                let requested = context.registers[R1] as i32;
                let length = task.file_system.gbpb_directory_snapshot.len();
                let start = if requested < 0 {
                    length
                } else {
                    (requested as usize).min(length)
                };
                context.registers[R0] = u32::try_from(start).unwrap_or(u32::MAX);
                Ok(())
            }
            "HOST.FILECHANNEL.READDIRECTORYFIELDBYTE" => {
                let snapshot_id = context.registers[R0];
                if snapshot_id != task.file_system.gbpb_directory_snapshot_id {
                    return Err(RuntimeError::Program(
                        "directory snapshot is no longer active".into(),
                    ));
                }
                let index = usize::try_from(context.registers[R1]).map_err(|_| {
                    RuntimeError::Program("directory continuation exceeds host range".into())
                })?;
                let field = context.registers[R2] as usize;
                let byte = context.registers[R3] as usize;
                if byte > 3 {
                    return Err(RuntimeError::Program(
                        "directory field byte index exceeds a word".into(),
                    ));
                }
                let entry = task
                    .file_system
                    .gbpb_directory_snapshot
                    .get(index)
                    .ok_or_else(|| {
                        RuntimeError::Program(
                            "directory continuation is outside the snapshot".into(),
                        )
                    })?;
                let word = match field {
                    0 => riscos_load_address(&entry.metadata),
                    1 => entry.metadata.execution_address,
                    2 => entry.length,
                    3 => entry.metadata.attributes,
                    4 => {
                        if entry.is_directory {
                            2
                        } else {
                            1
                        }
                    }
                    _ => {
                        return Err(RuntimeError::Program(
                            "unsupported directory information field".into(),
                        ));
                    }
                };
                context.registers[R0] = u32::from(word.to_le_bytes()[byte]);
                Ok(())
            }
            "HOST.FILECHANNEL.READDIRECTORYNAMEBYTE" => {
                let snapshot_id = context.registers[R0];
                if snapshot_id != task.file_system.gbpb_directory_snapshot_id {
                    return Err(RuntimeError::Program(
                        "directory snapshot is no longer active".into(),
                    ));
                }
                let index = usize::try_from(context.registers[R1]).map_err(|_| {
                    RuntimeError::Program("directory continuation exceeds host range".into())
                })?;
                let byte_index = usize::try_from(context.registers[R2]).map_err(|_| {
                    RuntimeError::Program("directory name offset exceeds host range".into())
                })?;
                let entry = task
                    .file_system
                    .gbpb_directory_snapshot
                    .get(index)
                    .ok_or_else(|| {
                        RuntimeError::Program(
                            "directory continuation is outside the snapshot".into(),
                        )
                    })?;
                context.registers[R0] =
                    u32::from(*entry.guest_name.as_bytes().get(byte_index).ok_or_else(|| {
                        RuntimeError::Program("directory name offset is outside the name".into())
                    })?);
                Ok(())
            }
            "HOST.FILECHANNEL.WRITECALLERBYTE" => {
                task.memory.write_caller_data_bytes(
                    context.registers[R0],
                    &[context.registers[R1] as u8],
                )?;
                Ok(())
            }
            "HOST.FILEOBJECT.CATALOGUE" => {
                let path = read_file_object_path(task, context.registers[R0])?;
                let (object_type, metadata, length) = self
                    .file_object_catalogue(task, &path)
                    .map_err(|_| file_object_error("catalogue lookup"))?;
                context.registers[R0] = object_type;
                if object_type != 0 {
                    context.registers[R1] = riscos_load_address(&metadata);
                    context.registers[R2] = metadata.execution_address;
                    context.registers[R3] = length;
                    context.registers[R4] = metadata.attributes;
                }
                Ok(())
            }
            "HOST.FILEOBJECT.CATALOGUECANDIDATE" => {
                let path_source = context.registers[R0];
                let object_address = context.registers[R1];
                let path_info_address = context.registers[R2];
                let candidate_index = context.registers[R3];
                let candidate = self.file_object_search_candidate(
                    task,
                    path_source,
                    object_address,
                    path_info_address,
                    candidate_index,
                )?;
                context.registers[R0] = 0;
                context.registers[R1..=R5].fill(0);
                if let Some(path) = candidate {
                    context.registers[R0] = 1;
                    let catalogue = self.file_object_catalogue(task, &path);
                    let (object_type, metadata, length) = match catalogue {
                        Ok(result) => result,
                        Err(RuntimeError::Io(error))
                            if error.kind() == std::io::ErrorKind::NotFound =>
                        {
                            (0, metadata_for_new_guest_path(&path), 0)
                        }
                        Err(_) => return Err(file_object_error("catalogue search")),
                    };
                    context.registers[R1] = object_type;
                    if object_type != 0 {
                        context.registers[R2] = riscos_load_address(&metadata);
                        context.registers[R3] = metadata.execution_address;
                        context.registers[R4] = length;
                        context.registers[R5] = metadata.attributes;
                    }
                }
                Ok(())
            }
            "HOST.FILEOBJECT.LOADCANDIDATE" => {
                let path_source = context.registers[R0];
                let object_address = context.registers[R1];
                let path_info_address = context.registers[R2];
                let candidate_index = context.registers[R3];
                let destination_hint = context.registers[R4];
                let force_catalogue = context.registers[R5] != 0;
                let path = self
                    .file_object_search_candidate(
                        task,
                        path_source,
                        object_address,
                        path_info_address,
                        candidate_index,
                    )?
                    .ok_or_else(|| file_object_error("search candidate disappeared"))?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                    .map_err(|_| file_object_error("load lookup"))?;
                if !resolved.host_path.exists() {
                    return Err(file_object_error("load file was not found"));
                }
                if resolved.is_directory {
                    return Err(file_object_error("load of a directory"));
                }
                let stored_metadata = resolved.metadata.clone();
                if stored_metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.attributes & 0x11 == 0)
                {
                    return Err(file_object_error("load denied by guest read permission"));
                }
                let metadata = stored_metadata
                    .unwrap_or_else(|| metadata_for_new_guest_path(&resolved.guest_path));
                let host_length = std::fs::metadata(&resolved.host_path)
                    .map_err(|_| file_object_error("load lookup"))?
                    .len();
                if host_length > crate::memory::GUEST_MEMORY_SIZE as u64 {
                    return Err(file_object_error("load exceeds the hosted 1 MiB limit"));
                }
                let destination = if force_catalogue {
                    metadata.load_address
                } else {
                    destination_hint
                };
                task.memory
                    .validate_caller_data_span(destination, host_length as usize)?;
                let (bytes, metadata) = self
                    .file_system
                    .read_file_limited(&task.file_system, &path, crate::memory::GUEST_MEMORY_SIZE)
                    .map_err(|_| file_object_error("load"))?;
                task.memory.write_caller_data_bytes(destination, &bytes)?;
                context.registers[R0] = riscos_load_address(&metadata);
                context.registers[R1] = metadata.execution_address;
                context.registers[R2] = u32::try_from(bytes.len())
                    .map_err(|_| file_object_error("loaded length exceeds the hosted U32 range"))?;
                context.registers[R3] = metadata.attributes;
                Ok(())
            }
            "HOST.FILEOBJECT.SEARCHPATHNOTFOUND" => {
                Err(file_object_error("search found no matching object"))
            }
            "HOST.FILEOBJECT.SAVEBLOCK" => {
                let path = read_file_object_path(task, context.registers[R0])?;
                self.file_object_validate_mutation(task, &path, false)
                    .map_err(|_| file_object_error("save"))?;
                let address = context.registers[R1];
                let length = context.registers[R2];
                if length as usize > crate::memory::GUEST_MEMORY_SIZE {
                    return Err(file_object_error("save exceeds the hosted 1 MiB limit"));
                }
                let end = address.checked_add(length).ok_or_else(|| {
                    RuntimeError::Program("OS_File save range exceeds the U32 address limit".into())
                })?;
                if end < address {
                    return Err(RuntimeError::Program(
                        "OS_File save range exceeds the U32 address limit".into(),
                    ));
                }
                let bytes = task
                    .memory
                    .read_caller_data_bytes(address, length as usize)?;
                let mut metadata = metadata_for_new_guest_path(&path);
                metadata.file_type = context.registers[R5] & 0xFFF;
                metadata.load_address = context.registers[R3] & 0x000F_FFFF;
                metadata.execution_address = context.registers[R4];
                metadata.attributes = context.registers[R6];
                apply_riscos_load_address(&mut metadata, context.registers[R3]);
                self.file_system
                    .write_file(&task.file_system, &path, &bytes, metadata)
                    .map_err(|_| file_object_error("save"))
            }
            "HOST.FILEOBJECT.LOADBLOCK" => {
                let path = read_file_object_path(task, context.registers[R0])?;
                let destination_hint = context.registers[R1];
                let force_catalogue = context.registers[R2] != 0;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                    .map_err(|_| file_object_error("load lookup"))?;
                if resolved.is_directory {
                    return Err(file_object_error("load of a directory"));
                }
                let metadata = resolved
                    .metadata
                    .clone()
                    .unwrap_or_else(|| metadata_for_new_guest_path(&resolved.guest_path));
                let host_length = std::fs::metadata(&resolved.host_path)
                    .map_err(|_| file_object_error("load lookup"))?
                    .len();
                if host_length > crate::memory::GUEST_MEMORY_SIZE as u64 {
                    return Err(file_object_error("load exceeds the hosted 1 MiB limit"));
                }
                let destination = if force_catalogue {
                    metadata.load_address
                } else {
                    destination_hint
                };
                task.memory
                    .validate_caller_data_span(destination, host_length as usize)?;
                let (bytes, metadata) = self
                    .file_system
                    .read_file_limited(&task.file_system, &path, crate::memory::GUEST_MEMORY_SIZE)
                    .map_err(|_| file_object_error("load"))?;
                task.memory.write_caller_data_bytes(destination, &bytes)?;
                context.registers[R0] = riscos_load_address(&metadata);
                context.registers[R1] = metadata.execution_address;
                context.registers[R2] = u32::try_from(bytes.len())
                    .map_err(|_| file_object_error("loaded length exceeds the hosted U32 range"))?;
                context.registers[R3] = metadata.attributes;
                Ok(())
            }
            "HOST.FILEOBJECT.SETMETADATA" => {
                let path = read_file_object_path(task, context.registers[R0])?;
                let field_mask = context.registers[R1];
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                    .map_err(|_| file_object_error("metadata update"))?;
                if resolved.is_directory {
                    return Err(file_object_error("metadata update on a directory"));
                }
                if !resolved.host_path.is_file() {
                    return Err(file_object_error("metadata update on a missing object"));
                }
                let mut metadata = resolved
                    .metadata
                    .unwrap_or_else(|| metadata_for_new_guest_path(&resolved.guest_path));
                if field_mask & 1 != 0 {
                    apply_riscos_load_address(&mut metadata, context.registers[R2]);
                }
                if field_mask & 2 != 0 {
                    metadata.execution_address = context.registers[R3];
                }
                if field_mask & 4 != 0 {
                    metadata.attributes = context.registers[R5];
                }
                if field_mask & 8 != 0 {
                    metadata.file_type = context.registers[R4] & 0xFFF;
                }
                if field_mask & 16 != 0 && metadata.file_type == 0 {
                    metadata.file_type = 0xFFD;
                }
                self.file_system
                    .set_metadata(&task.file_system, &path, &metadata)
                    .map_err(|_| file_object_error("metadata update"))
            }
            "HOST.FILEOBJECT.CREATEEMPTY" => {
                let path = read_file_object_path(task, context.registers[R0])?;
                self.file_object_validate_mutation(task, &path, false)
                    .map_err(|_| file_object_error("empty-file creation"))?;
                let mut metadata = metadata_for_new_guest_path(&path);
                metadata.file_type = context.registers[R3] & 0xFFF;
                metadata.load_address = context.registers[R1] & 0x000F_FFFF;
                metadata.execution_address = context.registers[R2];
                metadata.attributes = context.registers[R4];
                apply_riscos_load_address(&mut metadata, context.registers[R1]);
                self.file_system
                    .write_file(&task.file_system, &path, &[], metadata)
                    .map_err(|_| file_object_error("empty-file creation"))
            }
            "HOST.FILEOBJECT.CREATEDIRECTORY" => {
                let path = read_file_object_path(task, context.registers[R0])?;
                match self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                {
                    Ok(resolved) if resolved.is_directory && resolved.host_path.exists() => Ok(()),
                    Ok(resolved) if resolved.host_path.exists() => {
                        Err(file_object_error("directory creation on a file"))
                    }
                    Ok(_) => self
                        .file_system
                        .create_directory(&task.file_system, &path)
                        .map(|_| ())
                        .map_err(|_| file_object_error("directory creation")),
                    Err(_) => self
                        .file_system
                        .create_directory(&task.file_system, &path)
                        .map(|_| ())
                        .map_err(|_| file_object_error("directory creation")),
                }
            }
            "HOST.FILEOBJECT.DELETEFILE" => {
                let path = read_file_object_path(task, context.registers[R0])?;
                self.file_object_validate_mutation(task, &path, false)
                    .map_err(|_| file_object_error("file deletion"))?;
                self.file_system
                    .delete_file(&task.file_system, &path)
                    .map_err(|_| file_object_error("file deletion"))
            }
            "HOST.FILEOBJECT.DELETEDIRECTORY" => {
                let path = read_file_object_path(task, context.registers[R0])?;
                self.file_object_validate_mutation(task, &path, true)
                    .map_err(|_| file_object_error("directory deletion"))?;
                self.file_system
                    .remove_directory(&task.file_system, &path)
                    .map_err(|_| file_object_error("directory deletion"))
            }
            "HOST.FILEOBJECT.INVALIDSAVERANGE" => Err(file_object_error("save range is invalid")),
            "HOST.CONSOLE.SOFTWAREECHO" => {
                context.registers[R0] = u32::from(self.console.software_echo());
                Ok(())
            }
            "HOST.CONFIGURATION.READSTARTUPLANGUAGE" => {
                let configuration = self.load_basic_configuration()?;
                context.registers[R0] = match configuration.startup_language {
                    StartupLanguage::Mos => 0,
                    StartupLanguage::Desktop => 3,
                };
                context.registers[R1] = self.effective_configure_store().recovery_code();
                Ok(())
            }
            "HOST.CONFIGURATION.READVALUE" => self.configuration_read_value(task, context),
            "HOST.CONFIGURATION.WRITEVALUE" => self.configuration_write_value(task, context),
            "HOST.CONFIGURATION.REPLACEALL" => self.configuration_replace_all(task, context),
            "HOST.BOOT.REQUESTMOS" => {
                self.startup_target = Some(BootStartupTarget::MosPrompt);
                Ok(())
            }
            "HOST.BOOT.REQUESTDESKTOP" => {
                self.startup_target = Some(BootStartupTarget::Desktop);
                Ok(())
            }
            "HOST.RESOURCE.READBYTE" => {
                context.registers[R0] = u32::from(self.read_resource_byte(
                    context.registers[R0],
                    "BufferHandle",
                    task.id,
                    context.registers[R1],
                )?);
                Ok(())
            }
            "HOST.SYSTEM.READMONOTONICTIME" => {
                context.registers[R0] = self.mos.monotonic_timer.read() as u32;
                Ok(())
            }
            "HOST.SYSTEM.SWINUMBERTOSTRING" => self.system_swi_number_to_string(task, context),
            "HOST.SYSTEM.SWINUMBERFROMSTRING" => self.system_swi_number_from_string(task, context),
            "HOST.SYSTEMVARIABLES.READ" => self.system_variable_read(task, context),
            "HOST.SYSTEMVARIABLES.WRITE" => self.system_variable_write(task, context),
            "HOST.RUNTIME.MISSINGTERMINATOR" => {
                Err(crate::memory::MemoryError::MissingNullTerminator(context.registers[R0]).into())
            }
            "HOST.RUNTIME.ENDOFINPUT" => Err(RuntimeError::EndOfInput),
            "HOST.RUNTIME.UNSUPPORTEDSERVICEREASON" => Err(RuntimeError::Structured {
                type_name: "UnsupportedServiceReason".into(),
                code: context.registers[R1],
                message: format!(
                    "hosted service &{:X} does not support reason &{:X}",
                    context.registers[R0], context.registers[R1]
                ),
            }),
            "HOST.ERROR.RAISEERRORBLOCK" => {
                let address = context.registers[R0];
                let number = task.memory.read_bytes(address, 4)?;
                let code = u32::from_le_bytes(number.try_into().expect("four-byte error number"));
                let message_address = address
                    .checked_add(4)
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                let message = task
                    .memory
                    .read_c_string(message_address, 252)?
                    .into_iter()
                    .map(char::from)
                    .collect::<String>();
                Err(RuntimeError::StandardErrorBlock { code, message })
            }
            "HOST.MODULEMANAGER.READINFO" => {
                let cursor = context.registers[R0];
                let address = context.registers[R1];
                let capacity = context.registers[R2] as usize;
                let modules = self.module_registry.active_modules_sorted();
                let Some(record) = usize::try_from(cursor)
                    .ok()
                    .and_then(|index| modules.get(index).copied())
                else {
                    context.registers[R0] = cursor;
                    for register in 1..=5 {
                        context.registers[register] = 0;
                    }
                    return Ok(());
                };
                let name = record.manifest.name.as_bytes();
                let required = name
                    .len()
                    .checked_add(1)
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                if capacity < required || capacity > 128 {
                    return Err(RuntimeError::Structured {
                        type_name: "ModuleInfoBufferError".into(),
                        code: u32::try_from(required).unwrap_or(u32::MAX),
                        message: format!(
                            "module name needs {required} bytes, caller supplied {capacity}"
                        ),
                    });
                }
                let mut terminated = Vec::with_capacity(required);
                terminated.extend_from_slice(name);
                terminated.push(0);
                task.memory.write_bytes(address, &terminated)?;
                context.registers[R0] = cursor + 1;
                context.registers[R1] = 1;
                context.registers[R2] = u32::from(record.manifest.version.major);
                context.registers[R3] = u32::from(record.manifest.version.minor);
                context.registers[R4] = u32::from(record.manifest.version.patch);
                context.registers[R5] = match record.state {
                    ModuleState::Validated => 0,
                    ModuleState::Linked => 1,
                    ModuleState::Published => 2,
                    ModuleState::Starting => 3,
                    ModuleState::Active => 4,
                    ModuleState::Quiescing => 5,
                    ModuleState::Retired => 6,
                };
                Ok(())
            }
            "HOST.MODULEMANAGER.LOADSOURCE" => {
                task.require_module_management()?;
                self.load_guest_module_source(task, context.registers[R0])
            }
            "HOST.MODULEMANAGER.UNLOAD" => {
                task.require_module_management()?;
                self.unload_guest_module(task, context.registers[R0])
            }
            "HOST.MODULEMANAGER.AUTHORIZESOURCEREAD" => task.require_source_read(),
            "HOST.MODULEMANAGER.AUTHORIZEMANAGEMENT" => task.require_module_management(),
            "HOST.MODULEMANAGER.LOOKUPMODULE" => {
                let address = context.registers[R0];
                let raw_name = task.memory.read_c_string(address, 128)?;
                let name = String::from_utf8(raw_name).map_err(|_| RuntimeError::Structured {
                    type_name: "ModuleLookupError".into(),
                    code: 3,
                    message: "module name is not valid UTF-8".into(),
                })?;
                let modules = self.module_registry.active_modules_sorted();
                if let Some((ordinal, record)) = modules
                    .iter()
                    .enumerate()
                    .find(|(_, record)| record.manifest.name.eq_ignore_ascii_case(&name))
                {
                    context.registers[R0] = 1;
                    context.registers[R1] = u32::try_from(ordinal + 1).unwrap_or(u32::MAX);
                    context.registers[R2] = u32::from(record.manifest.version.major);
                    context.registers[R3] = u32::from(record.manifest.version.minor);
                    context.registers[R4] = u32::from(record.manifest.version.patch);
                    context.registers[R5] = match record.state {
                        ModuleState::Validated => 0,
                        ModuleState::Linked => 1,
                        ModuleState::Published => 2,
                        ModuleState::Starting => 3,
                        ModuleState::Active => 4,
                        ModuleState::Quiescing => 5,
                        ModuleState::Retired => 6,
                    };
                } else {
                    context.registers[R0..=R5].fill(0);
                }
                Ok(())
            }
            "HOST.MODULEMANAGER.LOOKUPSWI" => {
                let number = context.registers[R0];
                let Some(identity) = self.module_registry.active_swi_identity(number) else {
                    return Err(RuntimeError::Structured {
                        type_name: "SwiIdentityNotFound".into(),
                        code: number,
                        message: format!("no active manifest-owned SWI &{number:X}"),
                    });
                };
                let name_len = write_c_string_with_capacity(
                    &mut task.memory,
                    context.registers[R1],
                    context.registers[R2],
                    &identity.name,
                    "SWI name",
                )?;
                let module_len = write_c_string_with_capacity(
                    &mut task.memory,
                    context.registers[R3],
                    context.registers[R4],
                    &identity.module_name,
                    "module name",
                )?;
                let definition_len = write_c_string_with_capacity(
                    &mut task.memory,
                    context.registers[R5],
                    context.registers[R6],
                    &identity.definition_name,
                    "definition name",
                )?;
                context.registers[R0] = name_len;
                context.registers[R1] = module_len;
                context.registers[R2] = definition_len;
                context.registers[R3] =
                    u32::try_from(identity.generation_number).unwrap_or(u32::MAX);
                Ok(())
            }
            "HOST.MODULEMANAGER.LOOKUPMODULEEXPORT" => {
                let raw_name = task.memory.read_c_string(context.registers[R0], 128)?;
                let module_name = String::from_utf8(raw_name).map_err(|_| {
                    module_service_error("ModuleLookupError", 3, "module name is not valid UTF-8")
                })?;
                let Some(module) = self.module_registry.module_named(&module_name) else {
                    return Err(module_service_error(
                        "ModuleNotFound",
                        1,
                        format!("module {module_name} is not active"),
                    ));
                };
                if module.state != ModuleState::Active {
                    return Err(module_service_error(
                        "ModuleNotActive",
                        1,
                        format!("module {} is not Active", module.manifest.name),
                    ));
                }
                let mut exports = module.manifest.exports.iter().collect::<Vec<_>>();
                exports.sort_by_key(|export| export.number);
                let cursor = context.registers[R1];
                let Some(export) = usize::try_from(cursor)
                    .ok()
                    .and_then(|index| exports.get(index).copied())
                else {
                    context.registers[R0] = cursor;
                    context.registers[R1] = 0;
                    context.registers[R2] = 0;
                    context.registers[R3] = 0;
                    return Ok(());
                };
                let identity = self
                    .module_registry
                    .active_swi_identity(export.number)
                    .filter(|identity| identity.module == module.id)
                    .ok_or_else(|| {
                        module_service_error(
                            "SwiIdentityNotFound",
                            export.number,
                            format!("SWI {} has no active entry cell", export.name),
                        )
                    })?;
                write_c_string_with_capacity(
                    &mut task.memory,
                    context.registers[R2],
                    context.registers[R3],
                    &identity.name,
                    "SWI name",
                )?;
                write_c_string_with_capacity(
                    &mut task.memory,
                    context.registers[R4],
                    context.registers[R5],
                    &identity.definition_name,
                    "definition name",
                )?;
                context.registers[R0] = cursor.saturating_add(1);
                context.registers[R1] = 1;
                context.registers[R2] = identity.number;
                context.registers[R3] =
                    u32::try_from(identity.generation_number).unwrap_or(u32::MAX);
                Ok(())
            }
            "HOST.MODULEMANAGER.READDEFINITIONSOURCE" => {
                task.require_source_read()?;
                let raw_selector = task.memory.read_c_string(context.registers[R0], 128)?;
                let selector = String::from_utf8(raw_selector).map_err(|_| {
                    module_service_error(
                        "DefinitionSourceSelectorError",
                        1,
                        "definition selector is not valid UTF-8",
                    )
                })?;
                let Some((module_name, definition_name)) = selector.split_once('/') else {
                    return Err(module_service_error(
                        "DefinitionSourceSelectorError",
                        1,
                        "definition selector must be Module/Definition",
                    ));
                };
                if module_name.is_empty() || definition_name.is_empty() {
                    return Err(module_service_error(
                        "DefinitionSourceSelectorError",
                        1,
                        "definition selector must include both module and definition names",
                    ));
                }
                let Some(module) = self.module_registry.module_named(module_name) else {
                    return Err(module_service_error(
                        "ModuleNotFound",
                        1,
                        format!("module {module_name} is not loaded"),
                    ));
                };
                if module.state != ModuleState::Active {
                    return Err(module_service_error(
                        "ModuleNotActive",
                        1,
                        format!("module {} is not Active", module.manifest.name),
                    ));
                }
                let normalized_definition = normalize_definition_selector(definition_name);
                let Some(descriptor) = module.definitions.get(&normalized_definition).cloned()
                else {
                    return Err(module_service_error(
                        "DefinitionSourceNotFound",
                        1,
                        format!(
                            "definition {definition_name} is not present in active module {}",
                            module.manifest.name
                        ),
                    ));
                };
                let source_program = self
                    .module_programs
                    .get(&descriptor.id)
                    .cloned()
                    .ok_or_else(|| {
                        module_service_error(
                            "DefinitionSourceNotFound",
                            1,
                            "active definition has no retained BASIC64 source",
                        )
                    })?;
                let source = source_program
                    .definition_source(&normalized_definition)
                    .ok_or_else(|| {
                        module_service_error(
                            "DefinitionSourceNotFound",
                            1,
                            format!("source block for {definition_name} is unavailable"),
                        )
                    })?;
                let source_bytes = source.as_bytes();
                let total = u32::try_from(source_bytes.len()).map_err(|_| {
                    module_service_error(
                        "DefinitionSourceBufferError",
                        u32::MAX,
                        "definition source exceeds the hosted byte limit",
                    )
                })?;
                let offset = context.registers[R1];
                if offset > total {
                    return Err(module_service_error(
                        "DefinitionSourceOffsetError",
                        offset,
                        format!("source offset {offset} is past the {total}-byte definition"),
                    ));
                }
                let capacity = context.registers[R3];
                if capacity == 0 || capacity > 1025 || (capacity == 1 && offset < total) {
                    return Err(module_service_error(
                        "DefinitionSourceBufferError",
                        capacity,
                        "source capacity must be 2..=1025 bytes while data remains, or one byte at EOF",
                    ));
                }
                let start = usize::try_from(offset).unwrap_or(usize::MAX);
                if !source.is_char_boundary(start) {
                    return Err(module_service_error(
                        "DefinitionSourceOffsetError",
                        offset,
                        "source offset must be a UTF-8 character boundary",
                    ));
                }
                let available = capacity.saturating_sub(1) as usize;
                let mut end = start.saturating_add(available).min(source_bytes.len());
                while end > start && !source.is_char_boundary(end) {
                    end -= 1;
                }
                let chunk = &source_bytes[start..end];
                let mut terminated_chunk = Vec::with_capacity(chunk.len() + 1);
                terminated_chunk.extend_from_slice(chunk);
                terminated_chunk.push(0);
                task.memory
                    .write_bytes(context.registers[R2], &terminated_chunk)?;

                let source_path = &module.manifest.source_path;
                write_c_string_with_capacity(
                    &mut task.memory,
                    context.registers[R4],
                    context.registers[R5],
                    source_path,
                    "source path",
                )?;
                let module_identities = module
                    .manifest
                    .exports
                    .iter()
                    .filter_map(|export| self.module_registry.active_swi_identity(export.number))
                    .filter(|identity| identity.module == module.id)
                    .collect::<Vec<_>>();
                let generation = module_identities
                    .iter()
                    .find(|identity| identity.definition == descriptor.id)
                    .map(|identity| identity.generation_number)
                    .or_else(|| {
                        (descriptor.source_path == module.manifest.source_path)
                            .then(|| {
                                let generations = module_identities
                                    .iter()
                                    .map(|identity| identity.generation_number)
                                    .collect::<BTreeSet<_>>();
                                (generations.len() == 1)
                                    .then(|| *generations.first().expect("one generation exists"))
                            })
                            .flatten()
                    })
                    .unwrap_or(0);
                let id = descriptor.id.diagnostic_value();
                context.registers[R0] = u32::try_from(chunk.len()).unwrap_or(u32::MAX);
                context.registers[R1] = u32::try_from(end).unwrap_or(u32::MAX);
                context.registers[R2] = total;
                context.registers[R3] = u32::try_from(generation).unwrap_or(u32::MAX);
                context.registers[R4] = id as u32;
                context.registers[R5] = (id >> 32) as u32;
                Ok(())
            }
            "HOST.COMMANDREGISTRY.READENTRY" => {
                let cursor = context.registers[R0];
                let address = context.registers[R1];
                let capacity = context.registers[R2];
                if capacity == 0 || capacity > 1024 {
                    return Err(module_service_error(
                        "CommandRegistryBufferError",
                        capacity,
                        "command entry capacity must be 1..=1024 bytes",
                    ));
                }
                let commands = self.module_registry.active_commands();
                let Some(entry) = usize::try_from(cursor)
                    .ok()
                    .and_then(|index| commands.get(index))
                else {
                    context.registers[R0] = cursor;
                    context.registers[R1] = 0;
                    return Ok(());
                };
                let (kind, handler) = match &entry.command.handler {
                    CommandHandler::Basic64Proc(handler) => ("PROC", handler.as_str()),
                    CommandHandler::RustBridge => ("BRIDGE", entry.command.name.as_str()),
                };
                let category = match entry.command.category {
                    crate::ricochet::CommandCategory::Commands => "Commands",
                    crate::ricochet::CommandCategory::FileCommands => "FileCommands",
                };
                let row = [
                    entry.command.name.as_str(),
                    entry.module_name.as_str(),
                    &format!(
                        "{}.{}.{}",
                        entry.module_version.major,
                        entry.module_version.minor,
                        entry.module_version.patch
                    ),
                    kind,
                    handler,
                    category,
                    entry.command.syntax.as_str(),
                    entry.command.description.as_str(),
                ]
                .join("|");
                let required = row.len().saturating_add(1);
                if required > capacity as usize {
                    return Err(module_service_error(
                        "CommandRegistryBufferError",
                        u32::try_from(required).unwrap_or(u32::MAX),
                        "command entry does not fit the caller buffer",
                    ));
                }
                let mut bytes = row.into_bytes();
                bytes.push(0);
                task.memory.write_bytes(address, &bytes)?;
                context.registers[R0] = cursor.saturating_add(1);
                context.registers[R1] = 1;
                Ok(())
            }
            "HOST.COMMANDREGISTRY.INVOKE" => {
                let read = |address: u32, bound: usize| -> Result<String, RuntimeError> {
                    let bytes = task.memory.read_c_string(address, bound)?;
                    Ok(String::from_utf8_lossy(&bytes).into_owned())
                };
                let command_name = read(context.registers[R0], 33)?;
                let owner_name = read(context.registers[R1], 65)?;
                let handler_kind = read(context.registers[R2], 17)?;
                let handler_name = read(context.registers[R3], 65)?;
                let arguments = read(context.registers[R4], MAX_CLI_BYTES)?;
                let Some(active) = self.module_registry.active_command(
                    &owner_name,
                    &command_name,
                    &handler_kind,
                    &handler_name,
                ) else {
                    return Err(module_service_error(
                        "CommandNotFound",
                        1,
                        format!("command {command_name} is no longer active in {owner_name}"),
                    ));
                };
                self.invoke_registered_command(active, arguments, task, context)
            }
            "HOST.COMMANDSCRIPTS.OPEN" => {
                let raw_path = task
                    .memory
                    .read_c_string(context.registers[R0], MAX_CLI_BYTES)?;
                let path = std::str::from_utf8(&raw_path).map_err(|_| {
                    module_service_error("ObeyPathError", 1, "guest path is not valid UTF-8")
                })?;
                if path.is_empty() {
                    return Err(module_service_error(
                        "ObeyPathError",
                        2,
                        "Obey requires one guest pathname",
                    ));
                }
                let raw_arguments = task
                    .memory
                    .read_c_string(context.registers[R1], MAX_CLI_BYTES)?;
                let arguments = String::from_utf8(raw_arguments).map_err(|_| {
                    module_service_error(
                        "ObeyParameterError",
                        1,
                        "Obey parameters are not valid UTF-8",
                    )
                })?;
                let session = self.obey_scripts.get(&task.id);
                if session.is_some_and(|session| session.frames.len() >= MAX_OBEY_NESTING) {
                    return Err(module_service_error(
                        "ObeyNestingLimit",
                        u32::try_from(MAX_OBEY_NESTING).unwrap_or(u32::MAX),
                        "nested Obey depth exceeds the hosted limit of 8",
                    ));
                }
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, path)
                    .map_err(|_| {
                        module_service_error(
                            "ObeyFileError",
                            1,
                            "Obey could not resolve the guest file",
                        )
                    })?;
                let (bytes, _) = self
                    .file_system
                    .read_file_limited(&task.file_system, path, MAX_OBEY_SCRIPT_BYTES)
                    .map_err(|_| {
                        module_service_error(
                            "ObeyFileError",
                            2,
                            "Obey could not read the guest file within its 65,536-byte limit",
                        )
                    })?;
                let source = std::str::from_utf8(&bytes).map_err(|_| {
                    module_service_error("ObeyFileError", 3, "Obey source is not valid UTF-8")
                })?;
                let source_path = if resolved.guest_path.is_empty() {
                    format!("HostFS::{}.$", self.file_system.volume_name())
                } else {
                    format!(
                        "HostFS::{}.$.{}",
                        self.file_system.volume_name(),
                        resolved.guest_path
                    )
                };
                let guest_parent = resolved
                    .guest_path
                    .rsplit_once('.')
                    .map(|(parent, _)| parent)
                    .unwrap_or("");
                let obey_directory = if guest_parent.is_empty() {
                    format!("HostFS::{}.$", self.file_system.volume_name())
                } else {
                    format!(
                        "HostFS::{}.$.{}",
                        self.file_system.volume_name(),
                        guest_parent
                    )
                };
                if let Some((line_number, character)) = first_unsupported_obey_control(source) {
                    let error = module_service_error(
                        if character == '\0' {
                            "ObeyEmbeddedNul"
                        } else {
                            "ObeyControlCharacter"
                        },
                        u32::from(character),
                        if character == '\0' {
                            "Obey source contains an embedded NUL; the whole file was rejected before execution"
                                .to_owned()
                        } else {
                            format!(
                                "Obey source contains unsupported control U+{:04X}; only tab and line endings are permitted",
                                u32::from(character)
                            )
                        },
                    );
                    return Err(obey_source_error(&source_path, line_number, error));
                }
                let session = self.obey_scripts.entry(task.id).or_default();
                let next_total = session
                    .total_bytes
                    .checked_add(bytes.len())
                    .ok_or_else(|| {
                        module_service_error("ObeyByteLimit", 1, "Obey byte count overflowed")
                    })?;
                if next_total > MAX_OBEY_SCRIPT_BYTES {
                    return Err(module_service_error(
                        "ObeyByteLimit",
                        u32::try_from(next_total).unwrap_or(u32::MAX),
                        "simultaneously open Obey source exceeds the 65,536-byte aggregate limit",
                    ));
                }
                let handle = self.next_obey_handle;
                self.next_obey_handle = handle.checked_add(1).ok_or_else(|| {
                    module_service_error("ObeyHandleLimit", handle, "Obey handle space exhausted")
                })?;
                let buffer = task.memory.create_dynamic_area(
                    u32::MAX,
                    (MAX_CLI_BYTES * 2) as u32,
                    u32::MAX,
                    0,
                    (MAX_CLI_BYTES * 2) as u32,
                    0,
                    0,
                    0,
                    "Obey line buffer".into(),
                )?;
                session.total_bytes = next_total;
                session.frames.push(ObeyScriptFrame {
                    handle,
                    source_path,
                    obey_directory,
                    arguments,
                    bytes,
                    buffer_number: buffer.number,
                    buffer_base: buffer.base_address,
                    offset: 0,
                    next_line: 1,
                    current_line: None,
                });
                context.registers[R0] = handle;
                Ok(())
            }
            "HOST.COMMANDSCRIPTS.READLINE" => {
                let handle = context.registers[R0];
                let session = self.obey_scripts.get_mut(&task.id).ok_or_else(|| {
                    module_service_error("ObeyContextError", handle, "no active Obey source")
                })?;
                let frame = session.frames.last_mut().ok_or_else(|| {
                    module_service_error("ObeyContextError", handle, "no active Obey source")
                })?;
                if handle != 0 && frame.handle != handle {
                    return Err(module_service_error(
                        "ObeyContextError",
                        handle,
                        "Obey handle is not the active nested source",
                    ));
                }
                if self.quit_requested {
                    context.registers[R0] = 0;
                    context.registers[R1] = frame.next_line;
                    context.registers[R2] = 2;
                    context.registers[R3] = frame.buffer_base;
                    context.registers[R4] = frame.buffer_base + MAX_CLI_BYTES as u32;
                    return Ok(());
                }
                if frame.offset == frame.bytes.len() {
                    context.registers[R0] = 0;
                    context.registers[R1] = frame.next_line;
                    context.registers[R2] = 0;
                    context.registers[R3] = frame.buffer_base;
                    context.registers[R4] = frame.buffer_base + MAX_CLI_BYTES as u32;
                    return Ok(());
                }
                if session.total_lines >= MAX_OBEY_LINES {
                    let source_path = frame.source_path.clone();
                    let line_number = frame.next_line;
                    return Err(obey_source_error(
                        &source_path,
                        line_number,
                        module_service_error(
                            "ObeyWorkLimit",
                            u32::try_from(MAX_OBEY_LINES).unwrap_or(u32::MAX),
                            "script work exceeds the 4,096-line hosted limit",
                        ),
                    ));
                }
                let line_number = frame.next_line;
                let start = frame.offset;
                let mut end = start;
                while end < frame.bytes.len() && !matches!(frame.bytes[end], b'\r' | b'\n') {
                    end += 1;
                }
                let length = end - start;
                if length > MAX_OBEY_LINE_BYTES {
                    let source_path = frame.source_path.clone();
                    return Err(obey_source_error(
                        &source_path,
                        line_number,
                        module_service_error(
                            "ObeyLineTooLong",
                            u32::try_from(length).unwrap_or(u32::MAX),
                            "script line exceeds the 255-byte OS_CLI content limit",
                        ),
                    ));
                }
                let line = std::str::from_utf8(&frame.bytes[start..end]).map_err(|_| {
                    obey_source_error(
                        &frame.source_path,
                        line_number,
                        module_service_error("ObeyFileError", 4, "script line is not valid UTF-8"),
                    )
                })?;
                if end < frame.bytes.len() {
                    let first = frame.bytes[end];
                    end += 1;
                    if first == b'\r' && frame.bytes.get(end) == Some(&b'\n') {
                        end += 1;
                    }
                }
                let mut bytes = line.as_bytes().to_vec();
                bytes.push(0);
                task.memory.write_bytes(frame.buffer_base, &bytes)?;
                let mut arguments = frame.arguments.as_bytes().to_vec();
                arguments.push(0);
                task.memory
                    .write_bytes(frame.buffer_base + MAX_CLI_BYTES as u32, &arguments)?;
                frame.offset = end;
                frame.current_line = Some(line_number);
                frame.next_line = line_number.saturating_add(1);
                session.total_lines = session.total_lines.saturating_add(1);
                context.registers[R0] = u32::try_from(length).unwrap_or(u32::MAX);
                context.registers[R1] = line_number;
                context.registers[R2] = 1;
                context.registers[R3] = frame.buffer_base;
                context.registers[R4] = frame.buffer_base + MAX_CLI_BYTES as u32;
                Ok(())
            }
            "HOST.COMMANDSCRIPTS.CLOSE" => {
                let handle = context.registers[R0];
                let Some(session) = self.obey_scripts.get_mut(&task.id) else {
                    return Err(module_service_error(
                        "ObeyContextError",
                        handle,
                        "no active Obey source",
                    ));
                };
                let Some(frame) = session.frames.last() else {
                    return Err(module_service_error(
                        "ObeyContextError",
                        handle,
                        "no active Obey source",
                    ));
                };
                if handle != 0 && frame.handle != handle {
                    return Err(module_service_error(
                        "ObeyContextError",
                        handle,
                        "Obey handle is not the active nested source",
                    ));
                }
                let frame = session.frames.pop().expect("active Obey source exists");
                session.total_bytes = session.total_bytes.saturating_sub(frame.bytes.len());
                task.memory.remove_dynamic_area(frame.buffer_number)?;
                if session.frames.is_empty() {
                    self.obey_scripts.remove(&task.id);
                }
                Ok(())
            }
            "HOST.EXECINPUT.REPLACE" => self.replace_exec_input(task, context),
            "HOST.TASK.READIDENTITY" => {
                context.registers[R0] = u32::try_from(task.id).map_err(|_| {
                    RuntimeError::Program("caller task identity exceeds the public U32 ABI".into())
                })?;
                context.registers[R1] =
                    u32::try_from(task.memory.logical_size()).map_err(|_| {
                        RuntimeError::Program(
                            "caller logical memory exceeds the public U32 ABI".into(),
                        )
                    })?;
                context.registers[R2] =
                    u32::try_from(task.memory.dynamic_area_count()).map_err(|_| {
                        RuntimeError::Program(
                            "dynamic area count exceeds the public U32 ABI".into(),
                        )
                    })?;
                Ok(())
            }
            "HOST.MEMORY.CREATEDYNAMICAREA" => {
                let name_address = context.registers[R7];
                let raw_name = task.memory.read_c_string(name_address, 128)?;
                let name = raw_name.into_iter().map(char::from).collect::<String>();
                let area = task.memory.create_dynamic_area(
                    context.registers[R0],
                    context.registers[R1],
                    context.registers[R2],
                    context.registers[R3],
                    context.registers[R4],
                    context.registers[R5],
                    context.registers[R6],
                    name_address,
                    name,
                )?;
                context.registers[R0] = area.number;
                context.registers[R1] = area.base_address;
                context.registers[R2] = area.maximum_size;
                Ok(())
            }
            "HOST.MEMORY.REMOVEDYNAMICAREA" => {
                task.memory.remove_dynamic_area(context.registers[R0])?;
                Ok(())
            }
            "HOST.MEMORY.ACQUIRECOMMANDSCRATCH" => {
                let area = task.memory.acquire_command_scratch()?;
                context.registers[R0] = area.number;
                context.registers[R1] = area.base_address;
                Ok(())
            }
            "HOST.MEMORY.RELEASECOMMANDSCRATCH" => {
                task.memory.release_command_scratch(context.registers[R0])?;
                Ok(())
            }
            "HOST.MEMORY.READDYNAMICAREA" => {
                let area = task.memory.dynamic_area(context.registers[R0])?;
                context.registers[R0] = area.current_size;
                context.registers[R1] = area.base_address;
                context.registers[R2] = area.flags;
                context.registers[R3] = area.maximum_size;
                context.registers[R4] = area.handler_address;
                context.registers[R5] = area.workspace_address;
                context.registers[R6] = area.name_address;
                Ok(())
            }
            "HOST.MEMORY.NEXTDYNAMICAREA" => {
                context.registers[R0] = task
                    .memory
                    .next_dynamic_area(context.registers[R0])
                    .unwrap_or(u32::MAX);
                Ok(())
            }
            "HOST.MEMORY.CHANGEDYNAMICAREA" => {
                let change = context.registers[R1] as i32;
                let (amount_moved, _) = task
                    .memory
                    .change_dynamic_area(context.registers[R0], change)?;
                context.registers[R0] = amount_moved;
                Ok(())
            }
            _ => Err(RuntimeError::Program(format!(
                "primitive {name} has no host implementation"
            ))),
        }
    }

    fn system_variable_read(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let incoming_context = context.registers[R3];
        let selector = match read_system_variable_selector(&task.memory, context.registers[R0]) {
            Ok(selector) => selector,
            Err(error) => {
                if incoming_context != 0
                    && task.system_variable_read_cursor(incoming_context).is_some()
                {
                    task.remove_system_variable_read_cursor(incoming_context);
                }
                return Err(error);
            }
        };
        let selector_key = selector.to_ascii_uppercase();
        let wildcard = selector.contains('*') || selector.contains('#');
        let requested_type = context.registers[R4];
        // The PRM only gives entry R4 special meaning when it is 3 (request
        // conversion). Every other value requests the raw representation.
        // Our store contains only String and LiteralString, so raw and
        // converted reads both return their stored bytes without evaluation.
        let probe = context.registers[R2] & 0x8000_0000 != 0;
        let capacity = context.registers[R2] as usize;

        let cursor = if wildcard && incoming_context != 0 {
            let Some(cursor) = task.system_variable_read_cursor(incoming_context).cloned() else {
                return Err(name_error(
                    "wildcard context is not an active caller-owned enumeration",
                ));
            };
            if cursor.pattern != selector_key {
                task.remove_system_variable_read_cursor(incoming_context);
                return Err(name_error(
                    "wildcard context does not match this selector and caller task",
                ));
            }
            let returned_name = match task
                .memory
                .read_c_string(cursor.scratch_base, MAX_NAME_BYTES + 1)
            {
                Ok(name) => name,
                Err(error) => {
                    task.remove_system_variable_read_cursor(incoming_context);
                    return Err(error.into());
                }
            };
            if cursor
                .after_key
                .as_ref()
                .is_none_or(|after| returned_name.to_ascii_uppercase() != after.as_bytes())
            {
                task.remove_system_variable_read_cursor(incoming_context);
                return Err(name_error("wildcard context was modified by the caller"));
            }
            Some(cursor)
        } else {
            if incoming_context != 0 {
                if task.system_variable_read_cursor(incoming_context).is_some() {
                    task.remove_system_variable_read_cursor(incoming_context);
                }
                return Err(name_error(
                    "a wildcard context is valid only for a wildcard selector",
                ));
            }
            None
        };

        let after_key = cursor
            .as_ref()
            .and_then(|cursor| cursor.after_key.as_deref());
        let active_obey_directory = self.obey_directory(task.id).map(str::to_owned);
        let variable = if wildcard {
            self.system_variables.read_with_obey_directory(
                &selector,
                after_key,
                active_obey_directory.as_deref(),
            )
        } else if selector_key == "OBEY$DIR" {
            active_obey_directory
                .as_ref()
                .map(|directory| SystemVariable {
                    name: "Obey$Dir".into(),
                    value: directory.clone(),
                    variable_type: SystemVariableType::String,
                })
                .map(Ok)
                .unwrap_or_else(|| self.system_variables.read(&selector, after_key))
        } else {
            self.system_variables.read(&selector, after_key)
        };
        let variable = match variable {
            Ok(variable) => variable,
            Err(error)
                if is_system_variable_not_found(&error) && probe && incoming_context == 0 =>
            {
                context.registers[R2] = 0;
                context.registers[R3] = 0;
                context.registers[R4] = 0;
                return Ok(());
            }
            Err(error) => {
                if incoming_context != 0 {
                    task.remove_system_variable_read_cursor(incoming_context);
                }
                return Err(error);
            }
        };
        let value_bytes = variable.value.as_bytes();
        if !probe {
            if capacity < value_bytes.len() {
                if incoming_context != 0 {
                    task.remove_system_variable_read_cursor(incoming_context);
                }
                return Err(buffer_error(value_bytes.len(), capacity));
            }
            let buffer_preflight = if value_bytes.is_empty() {
                task.memory.read_byte(context.registers[R1]).map(|_| ())
            } else {
                task.memory
                    .read_bytes(context.registers[R1], value_bytes.len())
                    .map(|_| ())
            };
            if let Err(error) = buffer_preflight {
                if incoming_context != 0 {
                    task.remove_system_variable_read_cursor(incoming_context);
                }
                return Err(error.into());
            }
        }

        let result_context = if wildcard {
            let (scratch_number, scratch_base) = match cursor.as_ref() {
                Some(cursor) => (cursor.scratch_number, cursor.scratch_base),
                None => {
                    let area = task.memory.acquire_system_variable_context()?;
                    (area.number, area.base_address)
                }
            };
            let mut terminated_name = variable.name.as_bytes().to_vec();
            terminated_name.push(0);
            if let Err(error) = task.memory.write_bytes(scratch_base, &terminated_name) {
                if cursor.is_some() {
                    task.remove_system_variable_read_cursor(incoming_context);
                } else {
                    let _ = task.memory.release_system_variable_context(scratch_number);
                }
                return Err(error.into());
            }
            let next_key = variable.name.to_ascii_uppercase();
            if cursor.is_some() {
                task.update_system_variable_read_cursor_after_key(incoming_context, next_key)?;
            } else {
                task.set_system_variable_read_cursor(SystemVariableReadCursor {
                    pattern: selector_key,
                    after_key: Some(next_key),
                    scratch_number,
                    scratch_base,
                });
            }
            scratch_base
        } else {
            0
        };

        if !probe && !value_bytes.is_empty() {
            task.memory
                .write_bytes(context.registers[R1], value_bytes)?;
        }
        context.registers[R2] = if probe {
            if requested_type == 3 {
                0x8000_0000 | u32::try_from(value_bytes.len()).unwrap_or(u32::MAX)
            } else {
                !u32::try_from(value_bytes.len()).unwrap_or(u32::MAX)
            }
        } else {
            u32::try_from(value_bytes.len()).unwrap_or(u32::MAX)
        };
        context.registers[R3] = result_context;
        context.registers[R4] = variable.variable_type.register_value();
        Ok(())
    }

    fn system_variable_write(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        // Enforce caller rights before reading any caller-supplied selector or
        // value pointer, so a privileged provider cannot become a confused
        // deputy for an ordinary task.
        task.require_system_variable_write()?;
        let variable_type = SystemVariableType::from_register(context.registers[R4])?;
        let selector = read_system_variable_selector(&task.memory, context.registers[R0])?;
        if self.obey_directory(task.id).is_some() && selector_matches_name(&selector, "Obey$Dir") {
            return Err(name_error("Obey$Dir is a runtime-owned read-only variable"));
        }
        if context.registers[R3] != 0 {
            return Err(name_error(
                "hosted variable writes do not accept a continuation context",
            ));
        }
        let requested_length = context.registers[R2] as i32;
        if requested_length < 0 {
            self.system_variables.delete(&selector)?;
            context.registers[R3] = 0;
            return Ok(());
        }
        let length = usize::try_from(requested_length).unwrap_or(usize::MAX);
        if length > MAX_VALUE_BYTES {
            return Err(limit_error(format!(
                "hosted system-variable values are limited to {MAX_VALUE_BYTES} UTF-8 bytes"
            )));
        }
        let value_address = context.registers[R1];
        let value = match variable_type {
            SystemVariableType::String => {
                let bytes = task.memory.read_bytes(value_address, length)?;
                let terminator_address = value_address
                    .checked_add(
                        u32::try_from(length)
                            .map_err(|_| crate::memory::MemoryError::AddressOverflow)?,
                    )
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                let terminator = task.memory.read_byte(terminator_address)?;
                if !matches!(terminator, 0 | 10 | 13) {
                    return Err(buffer_error(length.saturating_add(1), length));
                }
                String::from_utf8(bytes).map_err(|_| RuntimeError::Structured {
                    type_name: "SystemVariableTypeError".into(),
                    code: 5,
                    message: "hosted string variables require valid UTF-8".into(),
                })?
            }
            SystemVariableType::LiteralString => {
                let bytes = task.memory.read_bytes(value_address, length)?;
                String::from_utf8(bytes).map_err(|_| RuntimeError::Structured {
                    type_name: "SystemVariableTypeError".into(),
                    code: 5,
                    message: "hosted literal variables require valid UTF-8".into(),
                })?
            }
        };
        let obey_directory = self.obey_directory(task.id).map(str::to_owned);
        self.system_variables.set_with_obey_directory(
            &selector,
            value,
            variable_type,
            obey_directory.as_deref(),
        )?;
        context.registers[R3] = 0;
        context.registers[R4] = variable_type.register_value();
        Ok(())
    }

    fn system_swi_number_to_string(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let encoded_number = context.registers[R0];
        let x_form = encoded_number & SWI_X_BIT != 0;
        let number = encoded_number & !SWI_X_BIT;
        let identity = self
            .module_registry
            .active_swi_identity(number)
            .ok_or_else(|| {
                module_service_error(
                    "SwiIdentityNotFound",
                    number,
                    format!("no active manifest-owned SWI &{number:X}"),
                )
            })?;
        let name = if x_form {
            format!("X{}", identity.name)
        } else {
            identity.name
        };
        context.registers[R0] = write_swi_name_with_capacity(
            &mut task.memory,
            context.registers[R1],
            context.registers[R2],
            &name,
        )?;
        Ok(())
    }

    fn system_swi_number_from_string(
        &self,
        task: &Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let bytes = read_control_terminated_bytes(
            &task.memory,
            context.registers[R0],
            SYSTEM_SWI_NAME_MAX_BYTES,
        )?;
        if bytes.is_empty() || !bytes.is_ascii() {
            return Err(module_service_error(
                "SwiNameInputError",
                if bytes.is_empty() { 2 } else { 3 },
                if bytes.is_empty() {
                    "SWI name is empty"
                } else {
                    "SWI names must contain ASCII bytes"
                },
            ));
        }
        let (x_form, name) = match bytes.strip_prefix(b"X") {
            Some(name) => (true, name),
            None => (false, bytes.as_slice()),
        };
        if name.is_empty() {
            return Err(module_service_error(
                "SwiNameInputError",
                2,
                "SWI name is empty after the X prefix",
            ));
        }
        let identity = self
            .module_registry
            .active_modules_sorted()
            .into_iter()
            .flat_map(|module| module.manifest.exports.iter())
            .find(|export| {
                export.name.as_bytes() == name
                    && self
                        .module_registry
                        .active_swi_identity(export.number)
                        .is_some()
            })
            .ok_or_else(|| {
                let displayed = String::from_utf8_lossy(&bytes);
                module_service_error(
                    "SwiIdentityNotFound",
                    1,
                    format!("no active manifest-owned SWI named '{displayed}'"),
                )
            })?;
        context.registers[R0] = identity.number | if x_form { SWI_X_BIT } else { 0 };
        Ok(())
    }

    fn load_guest_module_source(
        &mut self,
        task: &mut Task,
        path_address: u32,
    ) -> Result<(), RuntimeError> {
        const MAX_GUEST_MODULE_SOURCE_BYTES: usize = 4 * 1024 * 1024;
        const MAX_GUEST_MODULE_PATH_BYTES: usize = 256;

        let raw_path = task
            .memory
            .read_c_string(path_address, MAX_GUEST_MODULE_PATH_BYTES)?;
        if raw_path.is_empty() {
            return Err(module_service_error(
                "ModuleLoadError",
                1,
                "OS_Module Load requires a non-empty HostFS guest pathname",
            ));
        }
        let guest_path = String::from_utf8(raw_path).map_err(|_| {
            module_service_error("ModuleLoadError", 2, "module pathname is not valid UTF-8")
        })?;
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, &guest_path)?;
        let (bytes, metadata) = self.file_system.read_file_limited(
            &task.file_system,
            &guest_path,
            MAX_GUEST_MODULE_SOURCE_BYTES,
        )?;
        if metadata.file_type & 0xFFF != FILETYPE_BASIC64 {
            return Err(module_service_error(
                "ModuleLoadError",
                metadata.file_type,
                format!(
                    "OS_Module Load accepts BASIC64 source file type &064, not &{:03X}",
                    metadata.file_type & 0xFFF
                ),
            ));
        }
        let source = String::from_utf8(bytes).map_err(|_| {
            module_service_error(
                "ModuleLoadError",
                3,
                "BASIC64 module source is not valid UTF-8",
            )
        })?;
        let module = SystemModule::parse(
            &source,
            resolved.guest_path.clone(),
            &self.module_registry.allocator(),
        )
        .map_err(|error| module_service_error("ModuleSourceError", 1, error.to_string()))?;

        if !module
            .manifest
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(module_service_error(
                "UnsupportedModuleTitle",
                1,
                "OS_Module Load requires a RISC OS-style alphanumeric module title",
            ));
        }

        if let Some((module_id, module_name, state)) = self
            .module_registry
            .module_named(&module.manifest.name)
            .map(|record| (record.id, record.manifest.name.clone(), record.state))
        {
            if self.foundation_module_ids.contains(&module_id) {
                return Err(module_service_error(
                    "ProtectedFoundationModule",
                    1,
                    format!("foundation module {module_name} cannot be replaced"),
                ));
            }
            if state != ModuleState::Active {
                return Err(module_service_error(
                    "ModuleReplacementIncompatible",
                    1,
                    format!("module {module_name} is not Active (state {state:?})"),
                ));
            }
            return self.replace_guest_module_source(module_id, module);
        }

        if module.manifest.target_profile != "HOSTED" {
            return Err(module_service_error(
                "ModuleLoadError",
                4,
                "guest modules must target HOSTED in this runtime",
            ));
        }
        if !module.manifest.requested_capabilities.is_empty()
            || !module.manifest.primitive_imports.is_empty()
        {
            return Err(module_service_error(
                "ModuleCapabilityDenied",
                1,
                "guest source modules cannot request host capabilities or import protected primitives",
            ));
        }
        for (dependency_name, minimum_version) in &module.manifest.dependencies {
            let Some(dependency) = self.module_registry.module_named(dependency_name) else {
                return Err(module_service_error(
                    "ModuleDependencyError",
                    1,
                    format!("required module {dependency_name} is not loaded"),
                ));
            };
            if dependency.state != ModuleState::Active
                || dependency.manifest.version < *minimum_version
            {
                return Err(module_service_error(
                    "ModuleDependencyError",
                    2,
                    format!(
                        "required module {dependency_name} is not active at version {}.{}.{}",
                        minimum_version.major, minimum_version.minor, minimum_version.patch
                    ),
                ));
            }
        }
        module
            .validate_primitive_shapes(&self.module_registry.primitives)
            .map_err(|error| module_service_error("ModuleSourceError", 2, error.to_string()))?;

        let module_id = self
            .module_registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .map_err(|error| module_service_error("ModuleLoadError", 5, error.to_string()))?;
        if let Err(error) = self.module_registry.link_module(module_id, BTreeSet::new()) {
            let _ = self.module_registry.discard_unpublished_module(module_id);
            return Err(module_service_error(
                "ModuleLinkError",
                1,
                error.to_string(),
            ));
        }

        let program = Arc::new(module.clone());
        for definition in self
            .module_registry
            .module(module_id)
            .expect("staged guest module exists")
            .definitions
            .values()
        {
            self.module_programs
                .insert(definition.id, Arc::clone(&program));
        }
        if let Err(error) = self.module_registry.publish_modules(&[module_id]) {
            self.discard_guest_module(module_id);
            return Err(module_service_error(
                "ModulePublicationError",
                1,
                error.to_string(),
            ));
        }
        if let Err(error) = self.module_registry.begin_module_start(module_id) {
            let _ = self.module_registry.rollback_module_publication(module_id);
            self.discard_guest_module(module_id);
            return Err(module_service_error(
                "ModuleStartError",
                1,
                error.to_string(),
            ));
        }
        if let Err(error) = module.invoke_lifecycle_transactional("START", module_id, task, self) {
            let _ = self
                .module_registry
                .fail_module_start(module_id, error.to_string());
            self.discard_guest_module(module_id);
            return Err(module_service_error(
                "ModuleStartError",
                2,
                error.to_string(),
            ));
        }
        if let Err(error) = self.module_registry.complete_module_start(module_id) {
            let _ = self
                .module_registry
                .fail_module_start(module_id, error.to_string());
            self.discard_guest_module(module_id);
            return Err(module_service_error(
                "ModuleStartError",
                3,
                error.to_string(),
            ));
        }
        Ok(())
    }

    fn replace_guest_module_source(
        &mut self,
        module_id: ModuleId,
        mut module: SystemModule,
    ) -> Result<(), RuntimeError> {
        let (old_manifest, old_definitions, old_state) = {
            let old_record = self
                .module_registry
                .module(module_id)
                .expect("replacement module was found by title");
            (
                old_record.manifest.clone(),
                old_record.definitions.clone(),
                old_record.state,
            )
        };
        if old_state != ModuleState::Active {
            return Err(module_service_error(
                "ModuleReplacementIncompatible",
                1,
                format!("module {} is not Active", old_manifest.name),
            ));
        }
        if old_manifest.replacement_policy
            != crate::ricochet::ReplacementPolicy::CompatibleImmediate
        {
            return Err(module_service_error(
                "ModuleReplacementIncompatible",
                1,
                format!(
                    "module {} uses unsupported replacement policy {:?}",
                    old_manifest.name, old_manifest.replacement_policy
                ),
            ));
        }

        // Module identity is case-insensitive, but inspection keeps the
        // installed title spelling stable across a source reload.
        module.preserve_module_title(&old_manifest.name);

        if !same_replacement_manifest(&old_manifest, &module.manifest) {
            return Err(module_service_error(
                "ModuleReplacementIncompatible",
                1,
                "replacement changes module identity/version, dependencies, capabilities, lifecycle, or the public export set",
            ));
        }

        let old_program = old_definitions
            .values()
            .filter_map(|definition| self.module_programs.get(&definition.id).cloned())
            .find(|program| {
                program.manifest.source_path == old_manifest.source_path
                    && program.manifest.source_hash == old_manifest.source_hash
            })
            .ok_or_else(|| {
                module_service_error(
                    "ModuleReplacementIncompatible",
                    1,
                    "active module source is not retained for compatible replacement",
                )
            })?;
        if !module.has_compatible_workspace_schema(&old_program) {
            return Err(module_service_error(
                "ModuleReplacementIncompatible",
                1,
                "replacement changes persistent-state declarations or named type layouts; state migration is not supported",
            ));
        }
        if !module.has_compatible_public_abi(&old_program) {
            return Err(module_service_error(
                "ModuleReplacementIncompatible",
                1,
                "replacement changes an exported PROC/FN or lifecycle hook signature",
            ));
        }
        module.inherit_workspace(&old_program).map_err(|error| {
            module_service_error("ModuleReplacementIncompatible", 1, error.to_string())
        })?;
        if module.manifest.target_profile != "HOSTED"
            || !module.manifest.requested_capabilities.is_empty()
            || !module.manifest.primitive_imports.is_empty()
        {
            return Err(module_service_error(
                "ModuleReplacementIncompatible",
                1,
                "guest replacement cannot change target, host capabilities, or protected primitive imports",
            ));
        }
        for (dependency_name, minimum_version) in &module.manifest.dependencies {
            let Some(dependency) = self.module_registry.module_named(dependency_name) else {
                return Err(module_service_error(
                    "ModuleReplacementIncompatible",
                    1,
                    format!("required module {dependency_name} is not loaded"),
                ));
            };
            if dependency.state != ModuleState::Active
                || dependency.manifest.version < *minimum_version
            {
                return Err(module_service_error(
                    "ModuleReplacementIncompatible",
                    1,
                    format!(
                        "required module {dependency_name} is not active at the required version"
                    ),
                ));
            }
        }
        for import in &module.manifest.symbol_imports {
            let Some(dependency) = self.module_registry.module_named(&import.module) else {
                return Err(module_service_error(
                    "ModuleReplacementIncompatible",
                    1,
                    format!("symbol dependency {} is not loaded", import.module),
                ));
            };
            if dependency.state != ModuleState::Active
                || !dependency
                    .manifest
                    .symbol_exports
                    .iter()
                    .any(|export| export.eq_ignore_ascii_case(&import.symbol))
            {
                return Err(module_service_error(
                    "ModuleReplacementIncompatible",
                    1,
                    format!(
                        "symbol dependency {}.{} is not active and exported",
                        import.module, import.symbol
                    ),
                ));
            }
        }
        module
            .validate_primitive_shapes(&self.module_registry.primitives)
            .map_err(|error| {
                module_service_error("ModuleReplacementIncompatible", 1, error.to_string())
            })?;

        let old_definition_ids = old_definitions
            .values()
            .map(|definition| definition.id)
            .collect::<BTreeSet<_>>();
        let replacement_manifest = module.manifest.clone();
        let replacement_program = Arc::new(module);
        let replacement_generations = self
            .module_registry
            .replace_module_generation(
                module_id,
                replacement_manifest.clone(),
                replacement_program.definitions.clone(),
            )
            .map_err(|error| {
                module_service_error("ModuleReplacementIncompatible", 1, error.to_string())
            })?;

        for definition_id in old_definition_ids {
            self.derived_targets.invalidate_definition(definition_id);
        }
        self.derived_targets.invalidate_dependency(
            &old_manifest.name,
            &DependencyFingerprint {
                module: old_manifest.name.clone(),
                version: replacement_manifest.version,
                source_path: replacement_manifest.source_path.clone(),
                source_hash: replacement_manifest.source_hash.clone(),
                language_profile: replacement_manifest.language_profile.clone(),
                target_profile: replacement_manifest.target_profile.clone(),
            },
        );
        let program = Arc::clone(&replacement_program);
        for definition in self
            .module_registry
            .module(module_id)
            .expect("replaced module identity is stable")
            .definitions
            .values()
        {
            self.module_programs
                .insert(definition.id, Arc::clone(&program));
        }
        debug_assert_eq!(
            replacement_generations.len(),
            replacement_manifest.exports.len()
        );
        self.collect_retired_module_programs();
        Ok(())
    }

    fn unload_guest_module(
        &mut self,
        task: &mut Task,
        name_address: u32,
    ) -> Result<(), RuntimeError> {
        let raw_name = task.memory.read_c_string(name_address, 128)?;
        if raw_name.is_empty() {
            return Err(module_service_error(
                "ModuleUnloadError",
                1,
                "OS_Module Delete requires a full module title",
            ));
        }
        let name = String::from_utf8(raw_name).map_err(|_| {
            module_service_error("ModuleUnloadError", 2, "module title is not valid UTF-8")
        })?;
        if name.contains('%') {
            return Err(module_service_error(
                "UnsupportedModuleInstantiation",
                1,
                "hosted source modules do not support RISC OS %instantiation names",
            ));
        }
        let Some(record) = self.module_registry.module_named(&name) else {
            return Err(module_service_error(
                "ModuleNotFound",
                1,
                format!("module {name} is not loaded"),
            ));
        };
        let module_id = record.id;
        let canonical_name = record.manifest.name.clone();
        let state = record.state;
        if self.foundation_module_ids.contains(&module_id) {
            return Err(module_service_error(
                "ProtectedFoundationModule",
                1,
                format!("foundation module {canonical_name} cannot be unloaded"),
            ));
        }
        let dependents = self
            .module_registry
            .module_dependents(module_id)
            .map_err(|error| module_service_error("ModuleUnloadError", 3, error.to_string()))?;
        if !dependents.is_empty() {
            return Err(module_service_error(
                "ModuleInUse",
                1,
                format!(
                    "module {canonical_name} is required by {}",
                    dependents.join(", ")
                ),
            ));
        }
        let authority = self.module_management_authority.clone();
        match state {
            ModuleState::Active => {
                self.quiesce_basic64_module(&authority, &canonical_name, task)
                    .map_err(|error| {
                        module_service_error("ModuleQuiesceError", 1, error.to_string())
                    })?;
            }
            ModuleState::Quiescing => {
                // A prior Finalise may have failed transactionally. Retry it
                // without rerunning Quiesce or restoring admission.
            }
            other => {
                return Err(module_service_error(
                    "ModuleUnloadError",
                    4,
                    format!("module {canonical_name} cannot be deleted from state {other:?}"),
                ));
            }
        }
        self.retire_basic64_module(&authority, &canonical_name, task)
            .map_err(|error| module_service_error("ModuleFinaliseError", 1, error.to_string()))?;
        self.module_registry
            .forget_retired_module(module_id)
            .map_err(|error| module_service_error("ModuleUnloadError", 5, error.to_string()))?;
        Ok(())
    }

    fn discard_guest_module(&mut self, module_id: ModuleId) {
        if let Some(record) = self.module_registry.module(module_id) {
            let definition_ids = record
                .definitions
                .values()
                .map(|definition| definition.id)
                .collect::<BTreeSet<_>>();
            self.module_programs
                .retain(|definition_id, _| !definition_ids.contains(definition_id));
        }
        let _ = self.module_registry.discard_unpublished_module(module_id);
    }

    fn vdu_accept_byte(
        &mut self,
        task: &Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let character = context.registers[R0] as u8;
        let window = self.graphics_window_for_task(task.id);
        let uses_shared_default = self.task_uses_shared_default_graphics(task.id);
        if window.is_none() && !uses_shared_default {
            self.task_default_graphics_mut(task.id)?;
        }
        let next_mode = match window {
            Some(handle) => self.window_graphics.get(&handle),
            None if uses_shared_default => Some(&self.graphics),
            None => self.task_default_graphics.get(&task.id),
        }
        .ok_or_else(|| RuntimeError::Program("caller graphics context is unavailable".into()))?
        .mode_after_vdu_byte(character);
        if let Some(mode) = next_mode {
            let replacement_pixels = u64::from(mode.pixel_width) * u64::from(mode.pixel_height);
            if window.is_some() {
                self.ensure_graphics_pixel_budget(window, replacement_pixels)?;
            } else if !uses_shared_default {
                self.ensure_task_default_graphics_pixel_budget(task.id, replacement_pixels)?;
            }
        }
        let grid_changed = self.sync_modern_shell_grid_from_wimp();
        let graphics = match window {
            Some(handle) => self.window_graphics.get_mut(&handle).ok_or_else(|| {
                RuntimeError::Program("active caller graphics window has no raster context".into())
            })?,
            None if uses_shared_default => &mut self.graphics,
            None => self.task_default_graphics_mut(task.id)?,
        };
        let previous_mode = graphics.snapshot().mode;
        let output_byte = graphics.write_byte(character)?;
        let mode_changed = graphics.snapshot().mode != previous_mode;
        let snapshot = graphics.snapshot().clone();
        if mode_changed || grid_changed {
            self.publish_graphics_snapshot_for_task(task.id, window, snapshot);
        } else if !self.display_batch_active || output_byte.is_some() {
            self.publish_display_event(DisplayEvent::WriteByte {
                task_id: task.id,
                window_handle: window,
                byte: character,
            });
        } else {
            self.publish_graphics_snapshot_for_task(task.id, window, snapshot);
        }
        context.registers[R0] = u32::from(output_byte.unwrap_or_default());
        context.registers[R1] = u32::from(output_byte.is_some());
        Ok(())
    }

    fn graphics_window_for_task(&self, task_id: u64) -> Option<u32> {
        self.wimp
            .as_ref()
            .or(self.desktop_service.as_ref())
            .and_then(|wimp| wimp.current_graphics_context(task_id))
            .map(|(window_handle, _, _)| window_handle)
    }

    fn task_uses_shared_default_graphics(&self, task_id: u64) -> bool {
        task_id == self.display_task_id || (self.wimp.is_none() && self.desktop_service.is_none())
    }

    fn task_default_graphics_mut(
        &mut self,
        task_id: u64,
    ) -> Result<&mut GraphicsService, RuntimeError> {
        if !self.task_default_graphics.contains_key(&task_id) {
            if self.task_default_graphics.len() >= MAX_TASK_DEFAULT_GRAPHICS_CONTEXTS {
                return Err(RuntimeError::Program(
                    "hosted task-default graphics context limit is exhausted".into(),
                ));
            }
            let initial = self.graphics.new_window_output();
            let initial_pixels = initial.mode_pixel_count();
            let existing_pixels = self
                .task_default_graphics
                .values()
                .map(GraphicsService::mode_pixel_count)
                .sum::<u64>()
                .saturating_add(self.graphics.mode_pixel_count())
                .saturating_add(
                    self.window_graphics
                        .values()
                        .map(GraphicsService::mode_pixel_count)
                        .sum::<u64>(),
                );
            if existing_pixels.saturating_add(initial_pixels) > MAX_TASK_GRAPHICS_PIXELS {
                return Err(RuntimeError::Program(
                    "hosted task-default graphics pixel budget is exhausted".into(),
                ));
            }
            self.task_default_graphics.insert(task_id, initial);
        }
        self.task_default_graphics
            .get_mut(&task_id)
            .ok_or_else(|| RuntimeError::Program("caller graphics context is unavailable".into()))
    }

    fn ensure_task_default_graphics_pixel_budget(
        &self,
        task_id: u64,
        replacement_pixels: u64,
    ) -> Result<(), RuntimeError> {
        let other_pixels = self
            .task_default_graphics
            .iter()
            .filter(|(id, _)| **id != task_id)
            .map(|(_, graphics)| graphics.mode_pixel_count())
            .sum::<u64>()
            .saturating_add(self.graphics.mode_pixel_count())
            .saturating_add(
                self.window_graphics
                    .values()
                    .map(GraphicsService::mode_pixel_count)
                    .sum::<u64>(),
            );
        if other_pixels.saturating_add(replacement_pixels) > MAX_TASK_GRAPHICS_PIXELS {
            return Err(RuntimeError::Program(
                "hosted task-default graphics pixel budget is exhausted".into(),
            ));
        }
        Ok(())
    }

    fn graphics_plot_for_task(
        &mut self,
        task: &Task,
        plot_code: u8,
        x: i32,
        y: i32,
    ) -> Result<(), RuntimeError> {
        let window = self.graphics_window_for_task(task.id);
        let snapshot = {
            let graphics = match window {
                Some(handle) => self.window_graphics.get_mut(&handle).ok_or_else(|| {
                    RuntimeError::Program(
                        "active caller graphics window has no raster context".into(),
                    )
                })?,
                None if self.task_uses_shared_default_graphics(task.id) => &mut self.graphics,
                None => self.task_default_graphics_mut(task.id)?,
            };
            graphics.plot(plot_code, x, y)?;
            graphics.snapshot().clone()
        };
        if self.display_batch_active {
            if self.display_events.is_some()
                && self
                    .last_display_batch_publish
                    .is_some_and(|last| last.elapsed() >= DISPLAY_BATCH_FRAME_INTERVAL)
            {
                self.publish_graphics_snapshot_for_task(task.id, window, snapshot);
                self.last_display_batch_publish = Some(Instant::now());
            }
        } else {
            self.publish_display_event(DisplayEvent::Plot {
                task_id: task.id,
                window_handle: window,
                code: plot_code,
                x,
                y,
            });
        }
        Ok(())
    }

    fn graphics_set_packed_rgb_for_task(
        &mut self,
        task: &Task,
        packed_rgb: u32,
    ) -> Result<(), RuntimeError> {
        let window = self.graphics_window_for_task(task.id);
        let snapshot = {
            let graphics = match window {
                Some(handle) => self.window_graphics.get_mut(&handle).ok_or_else(|| {
                    RuntimeError::Program(
                        "active caller graphics window has no raster context".into(),
                    )
                })?,
                None if self.task_uses_shared_default_graphics(task.id) => &mut self.graphics,
                None => self.task_default_graphics_mut(task.id)?,
            };
            graphics.set_rgb_gcol(packed_rgb);
            graphics.snapshot().clone()
        };
        // Publish an immediate state change outside batched drawing. Within a
        // batch, the normal final snapshot carries the new GCOL without
        // flooding the display channel for per-pixel ColourTrans calls.
        if !self.display_batch_active {
            self.publish_graphics_snapshot_for_task(task.id, window, snapshot);
        }
        Ok(())
    }

    fn publish_graphics_snapshot_for_task(
        &self,
        task_id: u64,
        window_handle: Option<u32>,
        snapshot: GraphicsSnapshot,
    ) {
        if let Some(wimp) = self.wimp.as_ref().or(self.desktop_service.as_ref()) {
            wimp.sync_console_mode(task_id, window_handle, snapshot.mode);
        }
        self.publish_display_event(DisplayEvent::GraphicsSnapshot {
            task_id,
            window_handle,
            snapshot,
        });
    }

    fn replace_exec_input(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let raw_path = task
            .memory
            .read_c_string(context.registers[R0], MAX_CLI_BYTES)?;
        if raw_path.is_empty() {
            task.close_exec_input();
            return Ok(());
        }
        let path = std::str::from_utf8(&raw_path).map_err(|_| {
            module_service_error("ExecPathError", 1, "Exec path is not valid UTF-8")
        })?;
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)
            .map_err(|_| {
                module_service_error("ExecFileError", 1, "Exec could not resolve the guest file")
            })?;
        let guest_path = if resolved.guest_path.is_empty() {
            format!("HostFS::{}.$", self.file_system.volume_name())
        } else {
            format!(
                "HostFS::{}.$.{}",
                self.file_system.volume_name(),
                resolved.guest_path
            )
        };
        let (bytes, _) = self
            .file_system
            .read_file_limited(&task.file_system, path, MAX_EXEC_SOURCE_BYTES)
            .map_err(|_| {
                module_service_error(
                    "ExecFileError",
                    2,
                    format!("{guest_path}: Exec could not read the guest file within its 65,536-byte limit"),
                )
            })?;
        let source = std::str::from_utf8(&bytes).map_err(|error| {
            let line = exec_source_line_at(&bytes, error.valid_up_to());
            module_service_error(
                "ExecSourceError",
                line,
                format!("{guest_path}:{line}: Exec source is not valid UTF-8"),
            )
        })?;
        for (byte_offset, character) in source.char_indices() {
            if character != '\t' && character != '\r' && character != '\n' && character.is_control()
            {
                let line = exec_source_line_at(&bytes, byte_offset);
                return Err(module_service_error(
                    "ExecSourceControlCharacter",
                    line,
                    format!(
                        "{guest_path}:{line}: Exec source contains unsupported control U+{:04X}; the active input source was not replaced",
                        u32::from(character)
                    ),
                ));
            }
        }

        let mut normalized = Vec::with_capacity(bytes.len());
        let mut offset = 0_usize;
        let mut physical_lines = 0_usize;
        let mut line_bytes = 0_usize;
        let mut current_line = 1_u32;
        while offset < bytes.len() {
            let byte = bytes[offset];
            if let Some(terminator_len) = exec_line_terminator_len(&bytes, offset) {
                physical_lines += 1;
                if physical_lines > MAX_EXEC_LINES {
                    return Err(module_service_error(
                        "ExecLineLimit",
                        u32::try_from(physical_lines).unwrap_or(u32::MAX),
                        format!(
                            "{guest_path}:{physical_lines}: Exec source exceeds the 4,096-line limit; the active input source was not replaced"
                        ),
                    ));
                }
                if line_bytes > MAX_EXEC_LINE_BYTES {
                    return Err(module_service_error(
                        "ExecLineLength",
                        current_line,
                        format!(
                            "{guest_path}:{current_line}: Exec source contains a line longer than the 255-byte input limit; the active input source was not replaced"
                        ),
                    ));
                }
                line_bytes = 0;
                current_line = current_line.saturating_add(1);
                normalized.push(b'\r');
                offset += terminator_len - 1;
            } else {
                line_bytes += 1;
                if line_bytes > MAX_EXEC_LINE_BYTES {
                    return Err(module_service_error(
                        "ExecLineLength",
                        current_line,
                        format!(
                            "{guest_path}:{current_line}: Exec source contains a line longer than the 255-byte input limit; the active input source was not replaced"
                        ),
                    ));
                }
                normalized.push(byte);
            }
            offset += 1;
        }
        if line_bytes > 0 {
            physical_lines += 1;
            if physical_lines > MAX_EXEC_LINES {
                return Err(module_service_error(
                    "ExecLineLimit",
                    u32::try_from(physical_lines).unwrap_or(u32::MAX),
                    format!(
                        "{guest_path}:{physical_lines}: Exec source exceeds the 4,096-line limit; the active input source was not replaced"
                    ),
                ));
            }
        }

        task.install_exec_input(guest_path, normalized);
        Ok(())
    }

    fn console_read_byte_status(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let byte = match task.read_exec_input() {
            ExecInputRead::Byte {
                byte,
                guest_path,
                line,
            } => Some((byte, Some((guest_path, line)))),
            ExecInputRead::UnterminatedLineEnd => {
                context.registers[R0] = 0;
                context.registers[R1] = 1;
                context.carry = false;
                return Ok(());
            }
            ExecInputRead::Inactive => {
                // Once the file source has ended, subsequent keyboard input
                // belongs to the caller's normal stream, not the old file.
                task.set_exec_input_provenance(None);
                let byte = match self.mos.input.pop_front() {
                    Some(byte) => Some(byte),
                    None => self.console.read_byte()?,
                };
                byte.map(|byte| (byte, None))
            }
        };
        if let Some(byte) = byte {
            context.registers[R0] = u32::from(byte.0);
            context.registers[R1] = 0;
            context.carry = byte.0 == 0x1B;
            task.set_exec_input_provenance(byte.1);
        } else {
            context.registers[R0] = 0;
            context.registers[R1] = 1;
            context.carry = false;
        }
        Ok(())
    }

    fn dispatch_named_module_service(
        &mut self,
        service_name: &str,
        owner_module: &str,
        exported_definition: &str,
        contract: SwiContract,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let record = self
            .module_registry
            .module_named(owner_module)
            .ok_or_else(|| {
                RuntimeError::Program(format!(
                    "named service {service_name} has no registered BASIC64 owner"
                ))
            })?;
        if record.state != ModuleState::Active {
            return Err(RuntimeError::Program(format!(
                "named service {service_name} is published but its owning module is not active"
            )));
        }
        if !record
            .manifest
            .symbol_exports
            .iter()
            .any(|export| export.eq_ignore_ascii_case(exported_definition))
        {
            return Err(RuntimeError::Program(format!(
                "module {owner_module} does not export named service {service_name}"
            )));
        }
        let Some(definition) = record.definitions.get(exported_definition).cloned() else {
            return Err(RuntimeError::Program(format!(
                "named service {service_name} has no live definition"
            )));
        };
        let module_id = record.id;
        let Some(module) = self.module_programs.get(&definition.id).cloned() else {
            return Err(RuntimeError::Program(format!(
                "named service {service_name} has no retained BASIC64 source"
            )));
        };
        self.module_dispatch_count = self.module_dispatch_count.saturating_add(1);
        let result = module.invoke(module_id, &definition, &contract, task, self, context);
        self.last_dispatch_route = Some(SwiDispatchRoute::ModuleOwnedNamed {
            name: service_name.to_owned(),
            module: owner_module.to_owned(),
            definition: definition.name,
            definition_id: definition.id.diagnostic_value(),
            source_hash: definition.source_hash,
        });
        self.collect_retired_module_programs();
        result
    }

    pub(crate) fn dispatch_named_swi(
        &mut self,
        name: &str,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let upper = name.to_ascii_uppercase();
        context.overflow = false;
        let (x_form, service_name) = upper
            .strip_prefix('X')
            .map(|name| (true, name))
            .unwrap_or((false, upper.as_str()));
        let x_bit = if x_form { SWI_X_BIT } else { 0 };
        if let Some(number) = self.module_registry.swi_number(service_name) {
            return self.dispatch(number | x_bit, task, context);
        }
        let named_owner = match service_name {
            "COLOURTRANS_CONVERTHSVTORGB" | "COLOURTRANS_SETGCOL" | "COLOURTRANS_WRITEPALETTE" => {
                Some((
                    "ColourTrans",
                    service_name,
                    named_colourtrans_contract(service_name),
                ))
            }
            "RICOCHET_DESKTOP" => Some((
                "DesktopServices",
                "DESKTOPSERVICE",
                named_project_service_contract(service_name),
            )),
            "RICOCHET_DISPLAY" => Some((
                "DisplayManager",
                "DISPLAYSERVICE",
                named_project_service_contract(service_name),
            )),
            _ => None,
        };
        if let Some((owner, definition, contract)) = named_owner {
            let result = self.dispatch_named_module_service(
                service_name,
                owner,
                definition,
                contract,
                task,
                context,
            );
            return if x_form {
                return_x_form_error(result, task, context)
            } else {
                result
            };
        }
        let result = match service_name {
            "OS_WRITEC" => self.dispatch(OS_WRITE_C | x_bit, task, context),
            "OS_WRITES" => self.dispatch(OS_WRITE_S | x_bit, task, context),
            "OS_WRITE0" => self.dispatch(OS_WRITE_0 | x_bit, task, context),
            "OS_NEWLINE" => self.dispatch(OS_NEW_LINE | x_bit, task, context),
            "OS_READC" => self.dispatch(OS_READ_C | x_bit, task, context),
            "OS_READLINE" => self.dispatch(OS_READ_LINE | x_bit, task, context),
            "OS_READPOINT" => self.dispatch(OS_READ_POINT | x_bit, task, context),
            "OS_PLOT" => self.dispatch(OS_PLOT | x_bit, task, context),
            "OS_CLI" => self.dispatch(OS_CLI | x_bit, task, context),
            "OS_FILE" => self.dispatch(OS_FILE | x_bit, task, context),
            "OS_ARGS" => self.dispatch(OS_ARGS | x_bit, task, context),
            "OS_BGET" => self.dispatch(OS_BGET | x_bit, task, context),
            "OS_BPUT" => self.dispatch(OS_BPUT | x_bit, task, context),
            "OS_GBPB" => self.dispatch(OS_GBPB | x_bit, task, context),
            "OS_FIND" => self.dispatch(OS_FIND | x_bit, task, context),
            "OS_FSCONTROL" => self.dispatch(OS_FSCONTROL | x_bit, task, context),
            "OS_CHANGEDYNAMICAREA" => self.dispatch(OS_CHANGE_DYNAMIC_AREA | x_bit, task, context),
            "OS_GENERATEERROR" => self.dispatch(OS_GENERATE_ERROR | x_bit, task, context),
            "OS_DYNAMICAREA" => self.dispatch(OS_DYNAMIC_AREA | x_bit, task, context),
            "WIMP_INITIALISE" => self.dispatch(WIMP_INITIALISE | x_bit, task, context),
            "WIMP_CREATEWINDOW" => self.dispatch(WIMP_CREATE_WINDOW | x_bit, task, context),
            "WIMP_CREATEICON" => self.dispatch(WIMP_CREATE_ICON | x_bit, task, context),
            "WIMP_CREATEICONEX" => self.dispatch(WIMP_CREATE_ICON_EX | x_bit, task, context),
            "WIMP_CREATEMENU" => self.dispatch(WIMP_CREATE_MENU | x_bit, task, context),
            "WIMP_DELETEICON" => self.dispatch(WIMP_DELETE_ICON | x_bit, task, context),
            "WIMP_OPENWINDOW" => self.dispatch(WIMP_OPEN_WINDOW | x_bit, task, context),
            "WIMP_REDRAWWINDOW" => self.dispatch(WIMP_REDRAW_WINDOW | x_bit, task, context),
            "WIMP_UPDATEWINDOW" => self.dispatch(WIMP_UPDATE_WINDOW | x_bit, task, context),
            "WIMP_GETRECTANGLE" => self.dispatch(WIMP_GET_RECTANGLE | x_bit, task, context),
            "WIMP_FORCEREDRAW" => self.dispatch(WIMP_FORCE_REDRAW | x_bit, task, context),
            "WIMP_CLOSEWINDOW" => self.dispatch(WIMP_CLOSE_WINDOW | x_bit, task, context),
            "WIMP_POLL" => self.dispatch(WIMP_POLL | x_bit, task, context),
            "WIMP_GETWINDOWSTATE" => self.dispatch(WIMP_GET_WINDOW_STATE | x_bit, task, context),
            "WIMP_GETPOINTERINFO" => self.dispatch(WIMP_GET_POINTER_INFO | x_bit, task, context),
            "WIMP_SETICONSTATE" => self.dispatch(WIMP_SET_ICON_STATE | x_bit, task, context),
            "WIMP_SETEXTENT" => self.dispatch(WIMP_SET_EXTENT | x_bit, task, context),
            "WIMP_CLOSEDOWN" => self.dispatch(WIMP_CLOSE_DOWN | x_bit, task, context),
            "WIMP_STARTTASK" => self.dispatch(WIMP_START_TASK | x_bit, task, context),
            _ => Err(RuntimeError::Structured {
                type_name: "UnknownSwi".into(),
                code: SWI_UNKNOWN_ERROR_CODE,
                message: format!("no such SWI '{service_name}'"),
            }),
        };
        let unknown_name = matches!(
            &result,
            Err(RuntimeError::Structured { type_name, .. }) if type_name == "UnknownSwi"
        );
        if !matches!(&result, Err(RuntimeError::InvalidSwi(_))) && !unknown_name {
            self.transitional_dispatch_count = self.transitional_dispatch_count.saturating_add(1);
            self.last_dispatch_route = Some(SwiDispatchRoute::TransitionalNamed {
                name: service_name.into(),
            });
        }
        if x_form {
            return return_x_form_error(result, task, context);
        }
        result.map_err(normalize_swi_error)
    }

    pub fn dispatch(
        &mut self,
        encoded_number: u32,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let x_form = encoded_number & SWI_X_BIT != 0;
        let number = encoded_number & !SWI_X_BIT;
        context.overflow = false;
        let obey_depth_before = self
            .obey_scripts
            .get(&task.id)
            .map_or(0, |session| session.frames.len());
        let system_variable_contexts_before =
            (number == OS_CLI).then(|| task.system_variable_read_context_addresses());
        let command_scratch_before = ((number == OS_CLI && self.active_command_context.is_some())
            || number == OS_FS_CONTROL)
            .then(|| task.memory.command_scratch_snapshot());
        let result = match self.dispatch_unchecked(number, task, context) {
            Err(error) if number == OS_CLI => {
                let error = if let Some((path, line)) = self.obey_source(task.id) {
                    obey_source_error(&path, line, error)
                } else if let Some((path, line)) = task.exec_input_provenance() {
                    if matches!(
                        &error,
                        RuntimeError::Structured { type_name, .. }
                            if type_name == "ExecInputSourceError"
                    ) {
                        error
                    } else {
                        exec_input_source_error(path, line, error.to_string())
                    }
                } else {
                    error
                };
                self.unwind_obey_scripts(task, obey_depth_before);
                Err(error)
            }
            result => result,
        };
        let result = if number == OS_CLI || number == OS_FS_CONTROL {
            if let Some(existing) = &system_variable_contexts_before {
                task.clear_new_system_variable_read_cursors(existing);
            }
            let cleanup = match &command_scratch_before {
                Some(existing) => task.memory.release_command_scratch_after(existing),
                None => task.memory.release_all_command_scratch(),
            };
            match (result, cleanup) {
                (Err(error), _) => Err(error),
                (Ok(()), Ok(())) => Ok(()),
                (Ok(()), Err(error)) => Err(error.into()),
            }
        } else {
            result
        };
        if x_form {
            let result = return_x_form_error(result, task, context);
            if number == OS_CLI && !task.has_exec_input() {
                task.set_exec_input_provenance(None);
            }
            return result;
        }
        let result = result.map_err(normalize_swi_error);
        if number == OS_CLI && !task.has_exec_input() {
            task.set_exec_input_provenance(None);
        }
        result
    }

    fn dispatch_unchecked(
        &mut self,
        number: u32,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        if let Some(result) = self.dispatch_module_owned_swi(number, task, context) {
            return result;
        }
        if matches!(
            number,
            WIMP_CREATE_WINDOW
                | WIMP_REDRAW_WINDOW
                | WIMP_UPDATE_WINDOW
                | WIMP_GET_RECTANGLE
                | WIMP_FORCE_REDRAW
                | WIMP_CREATE_ICON
                | WIMP_CREATE_ICON_EX
                | WIMP_DELETE_ICON
                | WIMP_POLL
                | WIMP_SET_ICON_STATE
                | WIMP_GET_POINTER_INFO
                | WIMP_CREATE_MENU
        ) {
            self.record_transitional_numeric_dispatch(number);
            return self.dispatch_wimp(number, task, context);
        }
        let result = match number {
            OS_WRITE_C => {
                let character = context.registers[R0] as u8;
                if let Some(mode) = self.current_graphics().mode_after_vdu_byte(character) {
                    self.ensure_graphics_pixel_budget(
                        self.active_graphics_window,
                        u64::from(mode.pixel_width) * u64::from(mode.pixel_height),
                    )?;
                }
                let grid_changed = self.sync_modern_shell_grid_from_wimp();
                let previous_mode = self.current_graphics().snapshot().mode;
                let output_byte = self.current_graphics_mut().write_byte(character)?;
                let mode_changed = self.current_graphics().snapshot().mode != previous_mode;
                if mode_changed || grid_changed {
                    self.publish_snapshot(self.current_graphics().snapshot().clone());
                } else if !self.display_batch_active || output_byte.is_some() {
                    self.publish_display_event(DisplayEvent::WriteByte {
                        task_id: self.display_task_id,
                        window_handle: self.active_graphics_window,
                        byte: character,
                    });
                } else {
                    self.publish_snapshot(self.current_graphics().snapshot().clone());
                }
                if let Some(byte) = output_byte {
                    self.console.write_byte(byte)?;
                }
                self.console.flush().map_err(RuntimeError::from)
            }
            OS_WRITE_S => self.write_inline_string(task, context),
            OS_WRITE_0 => self.write_indirect_string(task, context),
            OS_NEW_LINE => {
                self.emit_via_write_c(task, b'\n')?;
                self.emit_via_write_c(task, b'\r')
            }
            OS_READ_C => self.read_character(context),
            OS_CLI => {
                let source = task
                    .exec_input_provenance()
                    .map(|(path, line)| (path.to_owned(), line));
                self.execute_cli(task, context).map_err(|error| {
                    if matches!(
                        &error,
                        RuntimeError::Structured { type_name, .. }
                            if type_name == "ExecInputSourceError"
                    ) {
                        error
                    } else if let Some((path, line)) = source {
                        exec_input_source_error(&path, line, error.to_string())
                    } else {
                        error
                    }
                })
            }
            OS_READ_LINE => self.read_line(task, context),
            other => Err(RuntimeError::InvalidSwi(other)),
        };
        if !matches!(&result, Err(RuntimeError::InvalidSwi(_))) {
            self.record_transitional_numeric_dispatch(number);
        }
        result
    }

    fn dispatch_wimp(
        &mut self,
        number: u32,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let Some(wimp) = self.wimp.clone() else {
            return Err(RuntimeError::InvalidSwi(number));
        };
        wimp.dispatch(number, task, context)?;
        if let Some((window_handle, work, extent)) = wimp.current_graphics_context(task.id) {
            if self.active_graphics_window != Some(window_handle) {
                if !self.window_graphics.contains_key(&window_handle) {
                    self.ensure_graphics_pixel_budget(
                        Some(window_handle),
                        self.graphics.mode_pixel_count(),
                    )?;
                    self.window_graphics
                        .insert(window_handle, self.graphics.new_window_output());
                }
                self.active_graphics_window = Some(window_handle);
            }
            let mode_height = self.current_graphics().snapshot().mode.logical_height;
            let clip = work_area_to_graphics_clip(work, extent, mode_height);
            self.current_graphics_mut().set_wimp_redraw_clip(Some(clip));
            if wimp.current_redraw_clears_background(task.id) {
                let background = wimp.current_redraw_background_colour(task.id).unwrap_or(7);
                self.current_graphics_mut()
                    .clear_wimp_region(clip, background);
            }
            self.publish_snapshot(self.current_graphics().snapshot().clone());
        } else if let Some(window_handle) = self.active_graphics_window.take() {
            if let Some(graphics) = self.window_graphics.get_mut(&window_handle) {
                graphics.set_wimp_redraw_clip(None);
                let snapshot = graphics.snapshot().clone();
                self.publish_snapshot_for_window(Some(window_handle), snapshot);
            }
        }
        Ok(())
    }

    fn publish_snapshot(&self, snapshot: GraphicsSnapshot) {
        self.publish_snapshot_for_window(self.active_graphics_window, snapshot);
    }

    fn publish_snapshot_for_window(&self, window_handle: Option<u32>, snapshot: GraphicsSnapshot) {
        if let Some(wimp) = &self.wimp {
            wimp.sync_console_mode(self.display_task_id, window_handle, snapshot.mode);
        }
        self.publish_display_event(DisplayEvent::GraphicsSnapshot {
            task_id: self.display_task_id,
            window_handle,
            snapshot,
        });
    }

    pub(crate) fn read_guest_file(
        &self,
        task: &Task,
        path: &str,
    ) -> Result<(Vec<u8>, FileMetadata), RuntimeError> {
        self.file_system.read_file(&task.file_system, path)
    }

    pub(crate) fn set_program_working_directory(
        &self,
        task: &mut Task,
        path: &str,
    ) -> Result<(), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if resolved.is_directory {
            return Err(RuntimeError::Program(format!("'{path}' is a directory")));
        }
        let parent = resolved
            .guest_path
            .rsplit_once('.')
            .map(|(parent, _)| parent)
            .filter(|parent| !parent.is_empty())
            .map(|parent| format!("$.{parent}"))
            .unwrap_or_else(|| "$".to_string());
        self.file_system
            .set_current_directory(&mut task.file_system, &parent)
    }

    fn file_object_catalogue(
        &self,
        task: &Task,
        path: &str,
    ) -> Result<(u32, FileMetadata, u32), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if !resolved.host_path.exists() {
            return Ok((0, metadata_for_new_guest_path(&resolved.guest_path), 0));
        }
        if resolved.is_directory {
            return Ok((
                2,
                FileMetadata {
                    guest_name: leaf_name(&resolved.guest_path).to_string(),
                    file_type: 0,
                    load_address: 0,
                    execution_address: 0,
                    attributes: 0,
                },
                0,
            ));
        }
        let metadata = resolved
            .metadata
            .unwrap_or_else(|| metadata_for_new_guest_path(&resolved.guest_path));
        let length =
            u32::try_from(std::fs::metadata(&resolved.host_path)?.len()).map_err(|_| {
                RuntimeError::Program("file length exceeds the hosted U32 range".into())
            })?;
        Ok((1, metadata, length))
    }

    fn file_object_search_candidate(
        &self,
        task: &Task,
        path_source: u32,
        object_address: u32,
        path_info_address: u32,
        candidate_index: u32,
    ) -> Result<Option<String>, RuntimeError> {
        if candidate_index >= 16 {
            return Ok(None);
        }
        let object_name = read_file_object_path(task, object_address)?;
        if object_name.contains(':') {
            return if candidate_index == 0 {
                Ok(Some(object_name))
            } else {
                Ok(None)
            };
        }

        let path_spec = match path_source {
            1 => match self.system_variables.read("File$Path", None) {
                Ok(variable) => supported_file_path_variable(variable)?,
                Err(error) if is_system_variable_not_found(&error) => String::new(),
                Err(error) => return Err(error),
            },
            2 => read_guest_path_spec(task, path_info_address, 255)?,
            3 => {
                let name = read_guest_path_spec(task, path_info_address, MAX_NAME_BYTES)?;
                validate_file_path_variable_name(&name)?;
                let variable = self.system_variables.read(&name, None)?;
                supported_file_path_variable(variable)?
            }
            0 => String::new(),
            _ => return Err(file_object_error("unsupported search-path reason")),
        };
        let path_items = if path_source == 0 {
            vec![String::new()]
        } else {
            parse_file_search_path(&path_spec)?
        };
        let Some(prefix) = path_items.get(candidate_index as usize) else {
            return Ok(None);
        };
        let candidate = format!("{}{}", prefix, object_name);
        if candidate.len() > 4096 {
            return Err(file_object_error("search candidate exceeds 4096 bytes"));
        }
        Ok(Some(candidate))
    }

    fn file_object_validate_mutation(
        &self,
        task: &Task,
        path: &str,
        allow_directory: bool,
    ) -> Result<(), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if resolved.is_directory && !allow_directory {
            return Err(file_object_error("operation on a directory"));
        }
        if !resolved.host_path.exists() {
            return Ok(());
        }
        if resolved
            .metadata
            .as_ref()
            .is_some_and(|metadata| metadata.attributes & (1 << 3) != 0)
        {
            return Err(file_object_error("operation on a deletion-locked file"));
        }
        if task.file_system.open_files.values().any(|open_file| {
            open_file
                .guest_path
                .eq_ignore_ascii_case(&resolved.guest_path)
        }) {
            return Err(file_object_error("operation on an open file"));
        }
        Ok(())
    }

    pub(crate) fn write_via_os_write_c(
        &mut self,
        task: &mut Task,
        bytes: &[u8],
    ) -> Result<(), RuntimeError> {
        for byte in bytes {
            self.emit_via_write_c(task, *byte)?;
        }
        Ok(())
    }

    pub(crate) fn plot(
        &mut self,
        task: &mut Task,
        plot_code: u8,
        x: i32,
        y: i32,
    ) -> Result<(), RuntimeError> {
        let mut context = SwiContext::default();
        context.registers[R0] = u32::from(plot_code);
        context.registers[R1] = x as u32;
        context.registers[R2] = y as u32;
        self.dispatch(OS_PLOT, task, &mut context)
    }

    pub fn flush(&mut self) -> Result<(), RuntimeError> {
        self.console.flush().map_err(RuntimeError::from)
    }

    pub fn publish_display_event(&self, event: DisplayEvent) {
        if let Some(sender) = &self.display_events {
            let _ = sender.send(event);
        }
    }

    pub(crate) fn begin_display_batch(&mut self) {
        self.display_batch_active = true;
        self.last_display_batch_publish = Some(Instant::now());
    }

    pub(crate) fn finish_display_batch(&mut self) {
        if !self.display_batch_active {
            return;
        }
        self.display_batch_active = false;
        self.last_display_batch_publish = None;
        self.sync_modern_shell_grid_from_wimp();
        if self.display_events.is_some() {
            self.publish_snapshot(self.current_graphics().snapshot().clone());
        }
    }

    pub fn write_inline(&mut self, task: &mut Task, text: &[u8]) -> Result<(), RuntimeError> {
        self.write_string_to_output_buffer(task, text)?;

        let mut context = SwiContext {
            pc: OUTPUT_BUFFER,
            ..SwiContext::default()
        };
        self.dispatch(OS_WRITE_S, task, &mut context)
    }

    pub fn write_indirect(&mut self, task: &mut Task, text: &[u8]) -> Result<(), RuntimeError> {
        self.write_string_to_output_buffer(task, text)?;
        let mut context = SwiContext::default();
        context.registers[R0] = OUTPUT_BUFFER;
        self.dispatch(OS_WRITE_0, task, &mut context)
    }

    fn write_string_to_output_buffer(
        &mut self,
        task: &mut Task,
        text: &[u8],
    ) -> Result<u32, RuntimeError> {
        task.memory.write_bytes(OUTPUT_BUFFER, text)?;
        let terminator =
            OUTPUT_BUFFER
                .checked_add(u32::try_from(text.len()).map_err(|_| {
                    RuntimeError::Memory(crate::memory::MemoryError::AddressOverflow)
                })?)
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        task.memory.write_byte(terminator, 0)?;
        Ok(terminator)
    }

    fn emit_via_write_c(&mut self, task: &mut Task, byte: u8) -> Result<(), RuntimeError> {
        let mut context = SwiContext::default();
        context.registers[R0] = u32::from(byte);
        self.dispatch(OS_WRITE_C, task, &mut context)
    }

    fn write_inline_string(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let mut address = context.pc;
        for _ in 0..MAX_STRING_BYTES {
            let byte = task.memory.read_byte(address)?;
            address = address
                .checked_add(1)
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
            if byte == 0 {
                let after_string = address
                    .checked_add(3)
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                context.pc = after_string & !3;
                return Ok(());
            }
            self.emit_via_write_c(task, byte)?;
        }
        Err(crate::memory::MemoryError::MissingNullTerminator(context.pc).into())
    }

    fn write_indirect_string(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let start = context.registers[R0];
        let bytes = task.memory.read_c_string(start, MAX_STRING_BYTES)?;
        for byte in &bytes {
            self.emit_via_write_c(task, *byte)?;
        }
        context.registers[R0] =
            start
                .checked_add(u32::try_from(bytes.len()).map_err(|_| {
                    RuntimeError::Memory(crate::memory::MemoryError::AddressOverflow)
                })?)
                .and_then(|address| address.checked_add(1))
                .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        Ok(())
    }

    fn read_character(&mut self, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let byte = match self.mos.input.pop_front() {
            Some(byte) => byte,
            None => self.console.read_byte()?.ok_or(RuntimeError::EndOfInput)?,
        };
        context.registers[R0] = u32::from(byte);
        context.carry = byte == 0x1B;
        Ok(())
    }

    fn read_line(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let buffer = context.registers[R0] & GUEST_ADDRESS_MASK;
        let maximum = context.registers[R1] as usize;
        let lowest = context.registers[R2].min(255) as u8;
        let highest = context.registers[R3].min(255) as u8;
        let echo_only_buffered = context.registers[R0] & READ_LINE_ECHO_ONLY_BUFFERED != 0;
        let echo_r4 = context.registers[R0] & READ_LINE_ECHO_R4 != 0;
        let echo_byte = context.registers[R4] as u8;
        let software_echo = self.console.software_echo();
        let mut length = 0_usize;
        context.carry = false;

        loop {
            let mut character = SwiContext::default();
            match self.dispatch(OS_READ_C, task, &mut character) {
                Ok(()) => {}
                Err(RuntimeError::EndOfInput) if length > 0 => break,
                Err(error) => return Err(error),
            }
            let byte = character.registers[R0] as u8;

            if byte == b'\r' || byte == b'\n' {
                if software_echo {
                    self.write_new_line(task)?;
                }
                break;
            }
            if byte == 0x1B {
                context.carry = true;
                if software_echo {
                    self.write_new_line(task)?;
                }
                break;
            }
            if byte == 0x04 && length == 0 {
                return Err(RuntimeError::EndOfInput);
            }
            if byte == 0x08 || byte == 0x7F {
                if length > 0 {
                    length -= 1;
                    task.memory.write_byte(
                        buffer
                            .checked_add(
                                u32::try_from(length)
                                    .map_err(|_| crate::memory::MemoryError::AddressOverflow)?,
                            )
                            .ok_or(crate::memory::MemoryError::AddressOverflow)?,
                        0,
                    )?;
                    if software_echo {
                        self.emit_via_write_c(task, 0x08)?;
                        self.emit_via_write_c(task, b' ')?;
                        self.emit_via_write_c(task, 0x08)?;
                    }
                }
                continue;
            }

            let accepted = byte >= lowest && byte <= highest;
            let buffer_full = accepted && length >= maximum;
            if accepted && length < maximum {
                let address = buffer
                    .checked_add(
                        u32::try_from(length)
                            .map_err(|_| crate::memory::MemoryError::AddressOverflow)?,
                    )
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
                task.memory.write_byte(address, byte)?;
                length += 1;
                if software_echo {
                    self.emit_via_write_c(task, if echo_r4 { echo_byte } else { byte })?;
                }
            } else if software_echo && !echo_only_buffered {
                self.emit_via_write_c(task, if echo_r4 { echo_byte } else { byte })?;
            }

            if buffer_full && software_echo {
                self.emit_via_write_c(task, 0x07)?;
            }
        }

        context.registers[R0] = 0;
        context.registers[R1] =
            u32::try_from(length).map_err(|_| crate::memory::MemoryError::AddressOverflow)?;
        context.registers[R2] = 0;
        context.registers[R3] = 0;
        Ok(())
    }

    fn execute_cli(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let bytes = mos::read_mos_string(task, context.registers[R0], MAX_CLI_BYTES)?;
        let command = String::from_utf8_lossy(&bytes);
        let command = command.trim();
        let command = command.trim_start_matches('*').trim_start();
        if command.is_empty() {
            return Ok(());
        }

        let compact_fx = command
            .get(..2)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("FX"))
            && command
                .as_bytes()
                .get(2)
                .is_some_and(|next| !next.is_ascii_whitespace());
        let (verb, arguments) = if compact_fx {
            (&command[..2], command[2..].trim())
        } else {
            let mut words = command.splitn(2, char::is_whitespace);
            (
                words.next().unwrap_or_default(),
                words.next().unwrap_or_default().trim(),
            )
        };

        self.execute_cli_command(task, verb, arguments)
    }

    fn execute_cli_command(
        &mut self,
        task: &mut Task,
        verb: &str,
        arguments: &str,
    ) -> Result<(), RuntimeError> {
        let scratch = task.memory.acquire_command_scratch()?;
        let result =
            self.execute_cli_command_with_scratch(task, verb, arguments, scratch.base_address);
        task.memory.release_command_scratch(scratch.number)?;
        result
    }

    fn execute_cli_command_with_scratch(
        &mut self,
        task: &mut Task,
        verb: &str,
        arguments: &str,
        scratch_base: u32,
    ) -> Result<(), RuntimeError> {
        if cli_command_matches(verb, "QUIT") && arguments.is_empty() {
            task.close_exec_input();
            self.quit_requested = true;
            Ok(())
        } else if verb.eq_ignore_ascii_case("BASIC64") {
            let (path, launch) = match parse_basic64_cli_arguments(arguments) {
                Ok(parsed) => parsed,
                Err(message) => {
                    self.write_inline(task, message.as_bytes())?;
                    return self.write_new_line(task);
                }
            };
            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    let message = format!("BASIC64 configuration error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    return self.write_new_line(task);
                }
            };
            let result = self.with_mos_shell_suspended(|dispatcher| {
                dispatcher.begin_display_batch();
                let result = crate::basic64::run_guest_file_with_launch_options(
                    &path,
                    task,
                    dispatcher,
                    &configuration,
                    launch,
                );
                dispatcher.finish_display_batch();
                result
            });
            match result {
                Ok(Some(report)) => log_jit_report("BASIC64", report),
                Ok(None) => {}
                Err(error) => {
                    let message = format!("BASIC64 error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)?;
                }
            }
            Ok(())
        } else if cli_command_matches(verb, "BASIC") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: *BASIC <file>")?;
                return self.write_new_line(task);
            }

            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    let message = format!("BASIC configuration error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    return self.write_new_line(task);
                }
            };
            let path = unquote_single_argument(arguments);
            let result = self.with_mos_shell_suspended(|dispatcher| {
                dispatcher.begin_display_batch();
                let result = crate::basic64::run_guest_file_configured(
                    path,
                    task,
                    dispatcher,
                    &configuration,
                );
                dispatcher.finish_display_batch();
                result
            });
            match result {
                Ok(Some(report)) => log_jit_report("BASIC", report),
                Ok(None) => {}
                Err(error) => {
                    let message = format!("BASIC error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)?;
                }
            }
            Ok(())
        } else if cli_command_matches(verb, "RUN") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: RUN <file.bas64|bas|txt|asc|bbc>")?;
                return self.write_new_line(task);
            }

            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    let message = format!("BASIC configuration error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    return self.write_new_line(task);
                }
            };
            let path = unquote_single_argument(arguments);
            let result = self.with_mos_shell_suspended(|dispatcher| {
                dispatcher.begin_display_batch();
                let result = crate::basic64::run_guest_file_configured(
                    path,
                    task,
                    dispatcher,
                    &configuration,
                );
                dispatcher.finish_display_batch();
                result
            });
            match result {
                Ok(Some(report)) => {
                    log_jit_report("RUN", report);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(error) => {
                    let message = format!("BASIC error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if verb == "." || cli_command_matches(verb, "CAT") {
            let path = unquote_single_argument(arguments);
            let mut call = SwiContext::default();
            call.registers[R0] = 5;
            if !path.is_empty() {
                call.registers[R1] = scratch_base;
                write_guest_string(task, scratch_base, path)?;
            }
            self.dispatch(OS_FSCONTROL, task, &mut call)
        } else if cli_command_matches(verb, "DIR") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                let selected = canonical_guest_name(
                    self.file_system.volume_name(),
                    &task.file_system.current_directory.join("."),
                );
                self.write_inline(task, selected.as_bytes())?;
                self.write_new_line(task)
            } else {
                write_guest_string(task, scratch_base, path)?;
                let mut call = SwiContext::default();
                call.registers[R0] = 0;
                call.registers[R1] = scratch_base;
                self.dispatch(OS_FSCONTROL, task, &mut call)
            }
        } else if cli_command_matches(verb, "CDIR") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *CDIR <directory>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, scratch_base, path)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 8;
            call.registers[R1] = scratch_base;
            self.dispatch(OS_FILE, task, &mut call)
        } else if cli_command_matches(verb, "DELETE") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *DELETE <file>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, scratch_base, path)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 6;
            call.registers[R1] = scratch_base;
            self.dispatch(OS_FILE, task, &mut call)
        } else if cli_command_matches(verb, "RENAME") {
            let Some((from, to)) = split_two_cli_arguments(arguments) else {
                self.write_inline(task, b"Syntax: *RENAME <old> <new>")?;
                return self.write_new_line(task);
            };
            write_guest_string(task, scratch_base, &from)?;
            write_guest_string(task, scratch_base + 0x200, &to)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 25;
            call.registers[R1] = scratch_base;
            call.registers[R2] = scratch_base + 0x200;
            self.dispatch(OS_FSCONTROL, task, &mut call)
        } else if cli_command_matches(verb, "FILETYPE") {
            let Some((path, type_name)) = split_two_cli_arguments(arguments) else {
                self.write_inline(task, b"Syntax: *FILETYPE <file> <type>")?;
                return self.write_new_line(task);
            };
            write_guest_string(task, scratch_base, &path)?;
            write_guest_string(task, scratch_base + 0x200, &type_name)?;
            let mut convert = SwiContext::default();
            convert.registers[R0] = 31;
            convert.registers[R1] = scratch_base + 0x200;
            self.dispatch(OS_FSCONTROL, task, &mut convert)?;
            let mut set_type = SwiContext::default();
            set_type.registers[R0] = 18;
            set_type.registers[R1] = scratch_base;
            set_type.registers[R2] = convert.registers[R2];
            self.dispatch(OS_FILE, task, &mut set_type)
        } else if cli_command_matches(verb, "TYPE") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *TYPE <file>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, scratch_base, path)?;
            let mut open = SwiContext::default();
            open.registers[R0] = 0x40;
            open.registers[R1] = scratch_base;
            self.dispatch(OS_FIND, task, &mut open)?;
            if open.registers[R0] == 0 {
                self.write_inline(task, b"File not found")?;
                return self.write_new_line(task);
            }
            let handle = open.registers[R0];
            let read_result = loop {
                let mut read = SwiContext::default();
                read.registers[R1] = handle;
                if let Err(error) = self.dispatch(OS_BGET, task, &mut read) {
                    break Err(error);
                }
                if read.carry {
                    break Ok(());
                }
                let mut output = SwiContext::default();
                output.registers[R0] = read.registers[R0];
                if let Err(error) = self.dispatch(OS_WRITE_C, task, &mut output) {
                    break Err(error);
                }
            };
            let mut close = SwiContext::default();
            close.registers[R0] = 0;
            close.registers[R1] = handle;
            self.dispatch(OS_FIND, task, &mut close)?;
            read_result
        } else if cli_command_matches(verb, "DISC") {
            if arguments.is_empty() {
                let mut call = SwiContext::default();
                call.registers[R0] = 5;
                call.registers[R2] = scratch_base;
                self.dispatch(OS_GBPB, task, &mut call)?;
                let length = task.memory.read_byte(scratch_base)? as usize;
                let name = task.memory.read_bytes(scratch_base + 1, length)?;
                self.write_inline(task, &name)?;
                self.write_new_line(task)
            } else {
                write_guest_string(task, scratch_base + 0x200, arguments.trim())?;
                write_guest_string(task, scratch_base, "@")?;
                let mut call = SwiContext::default();
                call.registers[R0] = 50;
                call.registers[R1] = scratch_base;
                call.registers[R2] = scratch_base + 0x200;
                self.dispatch(OS_FSCONTROL, task, &mut call)
            }
        } else if cli_command_matches(verb, "HOSTFS") {
            write_guest_string(task, scratch_base, "HostFS")?;
            let mut call = SwiContext::default();
            call.registers[R0] = 14;
            call.registers[R1] = scratch_base;
            self.dispatch(OS_FSCONTROL, task, &mut call)
        } else if cli_command_matches(verb, "DESKTOP") {
            if !arguments.is_empty() {
                self.write_inline(task, b"Syntax: DESKTOP")?;
                return self.write_new_line(task);
            }
            self.begin_desktop()
        } else if cli_command_matches(verb, "FX") {
            let compact = arguments
                .chars()
                .filter(|character| !character.is_ascii_whitespace())
                .collect::<String>();
            if compact == "151,78,243" {
                // ClockSP5 resets machine-specific display and timing state
                // here; the hosted profile has no such hardware state.
                return Ok(());
            }
            let fields = arguments.split(',').map(str::trim).collect::<Vec<_>>();
            if fields.is_empty() || fields.len() > 3 || fields.iter().any(|field| field.is_empty())
            {
                self.write_inline(task, b"Syntax: FX <reason>[,<r1>[,<r2>]]")?;
                return self.write_new_line(task);
            }
            let mut registers = [0_u32; 3];
            for (register, field) in registers.iter_mut().zip(fields) {
                let parsed = if let Some(hex) = field.strip_prefix('&') {
                    u32::from_str_radix(hex, 16)
                } else {
                    field.parse::<u32>()
                };
                let Ok(value) = parsed else {
                    self.write_inline(task, b"Syntax: FX <reason>[,<r1>[,<r2>]]")?;
                    return self.write_new_line(task);
                };
                *register = value;
            }
            let mut call = SwiContext::default();
            call.registers[..3].copy_from_slice(&registers);
            self.dispatch(OS_BYTE, task, &mut call)
        } else {
            self.write_inline(task, b"Bad command")?;
            self.write_new_line(task)
        }
    }

    fn begin_desktop(&mut self) -> Result<(), RuntimeError> {
        let Some(wimp) = self.desktop_service.as_ref().cloned() else {
            return Err(RuntimeError::Program(
                "DESKTOP requires the windowed host; restart without --stdio".into(),
            ));
        };

        // The BASIC64 Boot module decides whether this mechanism is requested;
        // the host only binds the Wimp instance and transfers display ownership.
        let configuration = self.load_basic_configuration()?;
        self.desktop_service = None;
        wimp.set_configure_store(self.configure.clone(), configuration.display);

        self.desktop_requested = true;
        self.wimp = Some(Arc::clone(&wimp));
        self.publish_display_event(DisplayEvent::DesktopStarted);
        // Keep the MOS/BASIC caller suspended at the command boundary while
        // the shared Wimp desktop owns the hosted display.
        wimp.wait_until_stopped()?;
        Ok(())
    }

    fn write_new_line(&mut self, task: &mut Task) -> Result<(), RuntimeError> {
        self.dispatch(OS_NEW_LINE, task, &mut SwiContext::default())
    }
}

impl Drop for SwiDispatcher {
    fn drop(&mut self) {
        if let Some(wimp) = &self.wimp {
            wimp.task_exited(self.display_task_id);
        }
    }
}

fn cli_command_matches(command_token: &str, command_name: &str) -> bool {
    let Some(prefix) = command_token.strip_suffix('.') else {
        return command_token.eq_ignore_ascii_case(command_name);
    };

    !prefix.is_empty()
        && command_name
            .get(..prefix.len())
            .is_some_and(|leading| leading.eq_ignore_ascii_case(prefix))
}

fn parse_basic64_cli_arguments(arguments: &str) -> Result<(String, BasicLaunchOptions), String> {
    let tokens = tokenize_cli_arguments(arguments)?;
    if tokens.is_empty() {
        return Err("Syntax: *BASIC64 [--mode CLASSIC|BASIC64|HYBRID] [--text CLASSIC|MODERN] [--override] <file>".into());
    }
    let mut launch = BasicLaunchOptions {
        default_mode: Some(BasicLanguageMode::Basic64),
        default_text_profile: Some(TextRenderingProfile::Modern),
        ..BasicLaunchOptions::default()
    };
    let mut path = None;
    let mut index = 0;
    while index < tokens.len() {
        let token = &tokens[index];
        if token.eq_ignore_ascii_case("--override") {
            launch.override_declarations = true;
        } else if token.eq_ignore_ascii_case("--mode") {
            index += 1;
            let Some(value) = tokens.get(index) else {
                return Err("*BASIC64 --mode requires CLASSIC, BASIC64, or HYBRID".into());
            };
            launch.selected_mode = Some(if value.eq_ignore_ascii_case("CLASSIC") {
                BasicLanguageMode::Classic
            } else if value.eq_ignore_ascii_case("BASIC64") {
                BasicLanguageMode::Basic64
            } else if value.eq_ignore_ascii_case("HYBRID") {
                BasicLanguageMode::Hybrid
            } else {
                return Err("*BASIC64 --mode requires CLASSIC, BASIC64, or HYBRID".into());
            });
        } else if token.eq_ignore_ascii_case("--text") {
            index += 1;
            let Some(value) = tokens.get(index) else {
                return Err("*BASIC64 --text requires CLASSIC or MODERN".into());
            };
            launch.selected_text_profile = Some(if value.eq_ignore_ascii_case("CLASSIC") {
                TextRenderingProfile::Classic
            } else if value.eq_ignore_ascii_case("MODERN") {
                TextRenderingProfile::Modern
            } else {
                return Err("*BASIC64 --text requires CLASSIC or MODERN".into());
            });
        } else if token.starts_with('-') {
            return Err(format!("unknown *BASIC64 option {token}"));
        } else if path.replace(token.clone()).is_some() {
            return Err("Syntax: *BASIC64 accepts one program path".into());
        }
        index += 1;
    }
    let Some(path) = path else {
        return Err("Syntax: *BASIC64 requires a program path".into());
    };
    if launch.override_declarations
        && launch.selected_mode.is_none()
        && launch.selected_text_profile.is_none()
    {
        return Err("*BASIC64 --override requires --mode and/or --text".into());
    }
    Ok((path, launch))
}

fn tokenize_cli_arguments(arguments: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut started = false;
    for character in arguments.chars() {
        match (quote, character) {
            (None, '"' | '\'') => {
                quote = Some(character);
                started = true;
            }
            (Some(active), character) if active == character => quote = None,
            (None, character) if character.is_whitespace() => {
                if started {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(character);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("*BASIC64 has an unterminated quoted argument".into());
    }
    if started {
        tokens.push(current);
    }
    Ok(tokens)
}

fn read_guest_string(task: &Task, address: u32) -> Result<String, RuntimeError> {
    let bytes = task.memory.read_c_string(address, MAX_STRING_BYTES)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn write_guest_string(task: &mut Task, address: u32, value: &str) -> Result<(), RuntimeError> {
    if value.len() + 1 > MAX_STRING_BYTES {
        return Err(RuntimeError::Program("guest string is too long".into()));
    }
    task.memory.write_bytes(address, value.as_bytes())?;
    task.memory
        .write_byte(address.wrapping_add(value.len() as u32), 0)?;
    Ok(())
}

fn write_guest_buffer(
    task: &mut Task,
    address: u32,
    capacity: usize,
    bytes: &[u8],
) -> Result<(), RuntimeError> {
    if capacity == 0 {
        return Ok(());
    }
    let length = bytes.len().min(capacity - 1);
    task.memory.write_bytes(address, &bytes[..length])?;
    task.memory
        .write_byte(address.wrapping_add(length as u32), 0)?;
    Ok(())
}

fn validate_configuration_error_capacity(capacity: usize) -> Result<(), RuntimeError> {
    if capacity == 0 || capacity > CONFIG_ERROR_BUFFER_MAX {
        return Err(RuntimeError::Structured {
            type_name: "ConfigurationBufferError".into(),
            code: u32::try_from(capacity).unwrap_or(u32::MAX),
            message: format!(
                "configuration error buffer capacity must be between 1 and {CONFIG_ERROR_BUFFER_MAX} bytes"
            ),
        });
    }
    Ok(())
}

fn write_configuration_message(
    task: &mut Task,
    address: u32,
    capacity: usize,
    message: &str,
) -> Result<(), RuntimeError> {
    validate_configuration_error_capacity(capacity)?;
    write_guest_buffer(task, address, capacity, message.as_bytes())
}

fn metadata_for_new_guest_path(path: &str) -> FileMetadata {
    let path = path
        .rsplit(':')
        .next()
        .unwrap_or(path)
        .trim_end_matches('.');
    let guest_name = path.rsplit('.').next().unwrap_or(path);
    FileMetadata {
        guest_name: guest_name.to_string(),
        file_type: FILETYPE_TEXT,
        load_address: 0,
        execution_address: 0,
        attributes: 0,
    }
}

fn leaf_name(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

fn riscos_load_address(metadata: &FileMetadata) -> u32 {
    ((metadata.file_type & 0xFFF) << 20) | (metadata.load_address & 0x000F_FFFF)
}

fn apply_riscos_load_address(metadata: &mut FileMetadata, load_address: u32) {
    let encoded_file_type = load_address >> 20;
    if encoded_file_type != 0 {
        metadata.file_type = encoded_file_type & 0xFFF;
        metadata.load_address = load_address & 0x000F_FFFF;
    } else {
        metadata.load_address = load_address;
    }
}

fn read_file_object_path(task: &Task, address: u32) -> Result<String, RuntimeError> {
    let bytes = mos::read_mos_string(task, address, MAX_STRING_BYTES)?;
    let path = String::from_utf8(bytes)
        .map_err(|_| RuntimeError::Program("OS_File guest pathname is not valid UTF-8".into()))?;
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(RuntimeError::Program(
            "OS_File requires a non-empty printable guest pathname".into(),
        ));
    }
    if path.contains('*') || path.contains('#') {
        return Err(RuntimeError::Program(
            "wildcard OS_File pathnames are not supported by the hosted FileSwitch".into(),
        ));
    }
    Ok(path)
}

fn read_guest_path_spec(
    task: &Task,
    address: u32,
    max_bytes: usize,
) -> Result<String, RuntimeError> {
    let mut bytes = Vec::new();
    for offset in 0..=max_bytes {
        let current =
            address
                .checked_add(u32::try_from(offset).map_err(|_| {
                    RuntimeError::Program("OS_File path pointer exceeds U32".into())
                })?)
                .ok_or_else(|| RuntimeError::Program("OS_File path pointer exceeds U32".into()))?;
        let byte = task.memory.read_caller_data_bytes(current, 1)?[0];
        if byte < 0x20 || byte == 0x7f {
            return String::from_utf8(bytes)
                .map_err(|_| file_object_error("path specification is not valid UTF-8"));
        }
        bytes.push(byte);
    }
    Err(file_object_error(
        "path specification is unterminated or too long",
    ))
}

fn validate_file_path_variable_name(name: &str) -> Result<(), RuntimeError> {
    if name.is_empty()
        || name.len() > MAX_NAME_BYTES
        || !name.is_ascii()
        || name
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() || matches!(byte, b'*' | b'#'))
    {
        return Err(file_object_error("path-variable name is invalid"));
    }
    Ok(())
}

fn supported_file_path_variable(variable: SystemVariable) -> Result<String, RuntimeError> {
    if !matches!(
        variable.variable_type,
        SystemVariableType::String | SystemVariableType::LiteralString
    ) {
        return Err(file_object_error("path variable type is unsupported"));
    }
    if variable.value.len() > 255 {
        return Err(file_object_error("path variable exceeds 255 bytes"));
    }
    Ok(variable.value)
}

fn parse_file_search_path(specification: &str) -> Result<Vec<String>, RuntimeError> {
    if specification.len() > 255 || specification.chars().any(char::is_control) {
        return Err(file_object_error(
            "path specification is invalid or too long",
        ));
    }
    let entries = specification.split(',').collect::<Vec<_>>();
    if entries.len() > 16 {
        return Err(file_object_error(
            "path specification exceeds 16 candidates",
        ));
    }
    let mut prefixes = Vec::with_capacity(entries.len());
    for entry in entries {
        let prefix = entry.trim_matches(' ');
        if !prefix.is_empty()
            && ((!prefix.ends_with('.') && !prefix.ends_with(':'))
                || prefix.contains('*')
                || prefix.contains('#'))
        {
            return Err(file_object_error("path prefix is not supported"));
        }
        prefixes.push(prefix.to_owned());
    }
    Ok(prefixes)
}

fn file_object_error(operation: &str) -> RuntimeError {
    RuntimeError::Program(format!(
        "OS_File {operation} failed in the guest filing system"
    ))
}

fn canonical_guest_name(volume_name: &str, guest_path: &str) -> String {
    if guest_path.is_empty() {
        format!("HostFS::{volume_name}.$")
    } else {
        format!("HostFS::{volume_name}.$.{guest_path}")
    }
}

fn fixed_file_switch_name(
    dispatcher: &SwiDispatcher,
    task: &Task,
    kind: u8,
) -> Result<Vec<u8>, RuntimeError> {
    match kind {
        0 => Ok(dispatcher.file_system.volume_name().as_bytes().to_vec()),
        1 => Ok(guest_directory_name(&task.file_system.current_directory)?.into_bytes()),
        2 => Ok(guest_directory_name(&task.file_system.library_directory)?.into_bytes()),
        _ => Err(RuntimeError::Program(
            "unsupported fixed FileSwitch name kind".into(),
        )),
    }
}

fn read_caller_guest_string(
    task: &Task,
    address: u32,
    max_bytes: usize,
) -> Result<Vec<u8>, RuntimeError> {
    let mut output = Vec::new();
    for index in 0..max_bytes {
        let offset =
            u32::try_from(index).map_err(|_| crate::memory::MemoryError::AddressOverflow)?;
        let current = address
            .checked_add(offset)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = task.memory.read_caller_data_bytes(current, 1)?[0];
        if byte == 0 {
            return Ok(output);
        }
        output.push(byte);
    }
    Err(crate::memory::MemoryError::MissingNullTerminator(address).into())
}

/// Desktop's existing Filer passes both ordinary NUL strings and BASIC
/// indirect strings, whose storage terminator is carriage return. Keep this
/// compatibility at the narrow desktop mechanism boundary without treating
/// arbitrary controls as terminators.
fn read_desktop_guest_path(
    task: &Task,
    address: u32,
    max_bytes: usize,
) -> Result<Vec<u8>, RuntimeError> {
    let mut output = Vec::new();
    for index in 0..max_bytes {
        let offset =
            u32::try_from(index).map_err(|_| crate::memory::MemoryError::AddressOverflow)?;
        let current = address
            .checked_add(offset)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = task.memory.read_caller_data_bytes(current, 1)?[0];
        if byte == 0 || byte == b'\r' {
            return Ok(output);
        }
        if byte < 0x20 || byte == 0x7f {
            return Err(RuntimeError::Program(
                "desktop guest pathname contains an unsupported control character".into(),
            ));
        }
        output.push(byte);
    }
    Err(crate::memory::MemoryError::MissingNullTerminator(address).into())
}

fn fs_control_guest_string(
    task: &Task,
    address: u32,
    max_bytes: usize,
) -> Result<Vec<u8>, RuntimeError> {
    if address == 0 {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    for offset in 0..=max_bytes {
        let offset =
            u32::try_from(offset).map_err(|_| crate::memory::MemoryError::AddressOverflow)?;
        let current = address
            .checked_add(offset)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = task.memory.read_caller_data_bytes(current, 1)?[0];
        if byte == 0 {
            return Ok(bytes);
        }
        if byte < 0x20 || byte == 0x7f {
            return Err(RuntimeError::Program(
                "FileSwitch string contains an unsupported control character".into(),
            ));
        }
        bytes.push(byte);
    }
    Err(crate::memory::MemoryError::MissingNullTerminator(address).into())
}

fn read_control_terminated_path_spec(
    task: &Task,
    address: u32,
    max_bytes: usize,
) -> Result<Vec<u8>, RuntimeError> {
    if address == 0 {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    for offset in 0..=max_bytes {
        let offset =
            u32::try_from(offset).map_err(|_| crate::memory::MemoryError::AddressOverflow)?;
        let current = address
            .checked_add(offset)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = task.memory.read_caller_data_bytes(current, 1)?[0];
        if byte < 0x20 || byte == 0x7f {
            return Ok(bytes);
        }
        bytes.push(byte);
    }
    Err(file_switch_guest_error(
        "path specification is unterminated or too long",
    ))
}

fn write_checked_guest_string(
    task: &mut Task,
    address: u32,
    value: &str,
) -> Result<(), RuntimeError> {
    let output_length = value
        .len()
        .checked_add(1)
        .ok_or_else(|| file_switch_guest_error("string output length overflowed"))?;
    let mut bytes = Vec::with_capacity(output_length);
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
    task.memory.write_caller_data_bytes(address, &bytes)?;
    Ok(())
}

fn file_switch_guest_error(action: &str) -> RuntimeError {
    RuntimeError::Program(format!(
        "FileSwitch {action} failed in the guest filing system"
    ))
}

fn read_file_system_name(
    task: &Task,
    address: u32,
    max_bytes: usize,
    special_terminators: bool,
) -> Result<Vec<u8>, RuntimeError> {
    let mut bytes = Vec::new();
    for offset in 0..=max_bytes {
        let offset =
            u32::try_from(offset).map_err(|_| crate::memory::MemoryError::AddressOverflow)?;
        let current = address
            .checked_add(offset)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = task.memory.read_caller_data_bytes(current, 1)?[0];
        if byte == 0 || byte < 0x20 || (special_terminators && matches!(byte, b'#' | b':' | b'-')) {
            return Ok(bytes);
        }
        if !byte.is_ascii_graphic() {
            return Err(RuntimeError::Program(
                "filing system name contains an invalid character".into(),
            ));
        }
        bytes.push(byte);
    }
    Err(RuntimeError::Program(
        "filing system name is unterminated or too long".into(),
    ))
}

fn guest_directory_name(components: &[String]) -> Result<String, RuntimeError> {
    let value = if components.is_empty() {
        "$".to_string()
    } else {
        format!("$.{}", components.join("."))
    };
    if value.len() > u8::MAX as usize {
        return Err(RuntimeError::Program("directory name is too long".into()));
    }
    Ok(value)
}

fn file_type_name(file_type: u32) -> [u8; 8] {
    let numeric;
    let name = match file_type & 0xFFF {
        0xFFB => "BASIC",
        FILETYPE_BASIC64 => "BASIC64",
        0xFFF => "Text",
        0xFEB => "Obey",
        0xFFD => "Data",
        value => {
            numeric = format!("{value:03X}");
            numeric.as_str()
        }
    };
    let mut bytes = [b' '; 8];
    let length = name.len().min(bytes.len());
    bytes[..length].copy_from_slice(&name.as_bytes()[..length]);
    bytes
}

fn parse_file_type(value: &str) -> Result<u32, RuntimeError> {
    let value = value.trim();
    let named = if value.eq_ignore_ascii_case("BASIC") {
        Some(FILETYPE_BASIC)
    } else if value.eq_ignore_ascii_case("BASIC64") {
        Some(FILETYPE_BASIC64)
    } else if value.eq_ignore_ascii_case("TEXT") {
        Some(FILETYPE_TEXT)
    } else if value.eq_ignore_ascii_case("OBEY") {
        Some(0xFEB)
    } else if value.eq_ignore_ascii_case("DATA") {
        Some(0xFFD)
    } else {
        None
    };
    if let Some(file_type) = named {
        return Ok(file_type);
    }
    let (radix, digits) = if let Some((radix, digits)) = value.split_once('_') {
        let radix = radix
            .parse::<u32>()
            .map_err(|_| RuntimeError::Program(format!("invalid file type '{value}'")))?;
        (radix, digits)
    } else {
        (
            16,
            value
                .strip_prefix("&")
                .or_else(|| value.strip_prefix("0x"))
                .unwrap_or(value),
        )
    };
    if !(2..=36).contains(&radix) {
        return Err(RuntimeError::Program(format!(
            "invalid file type radix {radix}"
        )));
    }
    let file_type = u32::from_str_radix(digits, radix)
        .map_err(|_| RuntimeError::Program(format!("invalid file type '{value}'")))?;
    if file_type > 0xFFF {
        return Err(RuntimeError::Program(format!(
            "RISC OS file type '{value}' must fit in 12 bits"
        )));
    }
    Ok(file_type)
}

fn unquote_single_argument(arguments: &str) -> &str {
    arguments
        .strip_prefix('"')
        .and_then(|path| path.strip_suffix('"'))
        .unwrap_or(arguments)
}

fn obey_source_error(source_path: &str, source_line: u32, error: RuntimeError) -> RuntimeError {
    if matches!(
        &error,
        RuntimeError::Structured { type_name, .. } if type_name == "ObeySourceError"
    ) {
        return error;
    }
    RuntimeError::Structured {
        type_name: "ObeySourceError".into(),
        code: 1,
        message: format!("{source_path}:{source_line}: {error}"),
    }
}

fn exec_input_source_error(source_path: &str, source_line: u32, error: String) -> RuntimeError {
    RuntimeError::Structured {
        type_name: "ExecInputSourceError".into(),
        code: 1,
        message: format!("{source_path}:{source_line}: {error}"),
    }
}

/// Return the first control character that would be ambiguous or truncated by
/// the line-oriented CLI bridge. Tabs are valid horizontal whitespace; CR/LF
/// are handled as line separators. Validate before installing a source frame
/// so no prefix of a malformed script can run.
fn first_unsupported_obey_control(source: &str) -> Option<(u32, char)> {
    let mut line_number = 1u32;
    let mut previous_was_cr = false;
    for character in source.chars() {
        match character {
            '\r' => {
                line_number = line_number.saturating_add(1);
                previous_was_cr = true;
            }
            '\n' => {
                if !previous_was_cr {
                    line_number = line_number.saturating_add(1);
                }
                previous_was_cr = false;
            }
            '\t' => previous_was_cr = false,
            other => {
                previous_was_cr = false;
                if other.is_control() {
                    return Some((line_number, other));
                }
            }
        }
    }
    None
}

fn split_two_cli_arguments(arguments: &str) -> Option<(String, String)> {
    let arguments = arguments.trim();
    let (first, remainder) = if let Some(quoted) = arguments.strip_prefix('"') {
        let end = quoted.find('"')?;
        (&quoted[..end], quoted[end + 1..].trim())
    } else {
        let split = arguments.find(char::is_whitespace)?;
        (&arguments[..split], arguments[split..].trim())
    };
    let second = unquote_single_argument(remainder).trim();
    (!first.is_empty() && !second.is_empty()).then(|| (first.to_string(), second.to_string()))
}

fn format_elapsed(duration: Duration) -> String {
    if duration.as_secs_f64() < 1.0 {
        format!("{:.2} ms", duration.as_secs_f64() * 1_000.0)
    } else {
        format!("{:.2} s", duration.as_secs_f64())
    }
}

fn log_jit_report(command: &str, report: crate::basic_compat::JitExecutionReport) {
    let summary = if report.strict_native {
        format!(
            "{command} strict native: {} units, {} native calls, {} runtime helpers, {} interpreter statements, {} interpreter expressions (compile {}, run {}).",
            report.compiled_units.len(),
            report.compiled_calls,
            report.runtime_helper_calls,
            report.interpreted_statement_count,
            report.interpreted_expression_count,
            format_elapsed(report.compile_time),
            format_elapsed(report.compiled_time),
        )
    } else {
        let fallback = report
            .fallback_reason
            .unwrap_or_else(|| "remaining statements used the interpreter".into());
        format!(
            "{command}: {fallback}; {} interpreted statements, {} expressions (compile {}).",
            report.interpreted_statement_count,
            report.interpreted_expression_count,
            format_elapsed(report.compile_time),
        )
    };
    eprintln!("{summary}");
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::mpsc;

    use crate::boot::BootModuleInput;
    use crate::configure::BasicEngine;
    use crate::display::{DesktopResolution, DisplayColour, DisplaySettings};

    use super::*;

    #[test]
    fn basic64_command_does_not_capture_existing_basic_abbreviations() {
        for (command, expected) in [
            ("BA.", "Syntax: *BASIC <file>"),
            ("BASIC.", "Syntax: *BASIC <file>"),
            ("BASIC64", "Syntax: *BASIC64"),
        ] {
            let (_input_sender, input_receiver) = mpsc::channel();
            let (display_sender, display_receiver) = mpsc::channel();
            let mut dispatcher =
                SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
            let mut task = Task::new(1);
            dispatch_cli_line(&mut dispatcher, &mut task, command).unwrap();
            let output: Vec<u8> = display_receiver
                .try_iter()
                .filter_map(|event| match event {
                    DisplayEvent::WriteByte { byte, .. } => Some(byte),
                    _ => None,
                })
                .collect();
            assert!(
                String::from_utf8_lossy(&output).contains(expected),
                "{command}"
            );
        }
    }

    #[test]
    fn basic64_cli_selection_defaults_and_quotes_are_explicit() {
        let (path, launch) = parse_basic64_cli_arguments("\"My programs/hello.bas64\"").unwrap();
        assert_eq!(path, "My programs/hello.bas64");
        assert_eq!(launch.default_mode, Some(BasicLanguageMode::Basic64));
        assert_eq!(
            launch.default_text_profile,
            Some(TextRenderingProfile::Modern)
        );
        assert!(!launch.override_declarations);

        let (_, launch) = parse_basic64_cli_arguments(
            "--mode CLASSIC --text CLASSIC --override \"My programs/legacy.bas\"",
        )
        .unwrap();
        assert_eq!(launch.selected_mode, Some(BasicLanguageMode::Classic));
        assert_eq!(
            launch.selected_text_profile,
            Some(TextRenderingProfile::Classic)
        );
        assert!(launch.override_declarations);
        assert!(parse_basic64_cli_arguments("--override file.bas64").is_err());
        assert!(parse_basic64_cli_arguments("\"unfinished file.bas64").is_err());
    }

    #[test]
    fn modern_text_rejects_os_read_point_with_a_profile_specific_error() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        dispatcher
            .set_display_profiles(
                GraphicsProfile::Hosted,
                TextRenderingProfile::Modern,
                TextEncoding::Utf8,
            )
            .unwrap();
        let mut task = Task::new(1);
        let error = dispatcher
            .dispatch(OS_READ_POINT, &mut task, &mut SwiContext::default())
            .expect_err("the separate modern overlay is not pixel-readable");
        assert!(error.to_string().contains("TEXT=CLASSIC"));
    }

    fn append_remaining_foundation_inputs<'a>(inputs: &mut Vec<crate::boot::BootModuleInput<'a>>) {
        inputs.extend([
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
                source_path: "modules/RicochetCommands.bas64",
                source: include_str!("../modules/RicochetCommands.bas64"),
                grants: &[
                    "CommandRegistry",
                    "ConfigurationStoreRead",
                    "ConfigurationStoreWrite",
                    "CommandScripts",
                    "ExecInput",
                    "RuntimeErrors",
                    "TaskMemory",
                ],
            },
        ]);
    }

    fn write_guest_module_source(root: &std::path::Path, guest_name: &str, source: &str) {
        std::fs::create_dir_all(root).unwrap();
        let filename = format!("{guest_name}.bas64");
        std::fs::write(root.join(&filename), source).unwrap();
        std::fs::write(
            root.join(format!("{filename}.ricochetmeta")),
            format!(
                "Ricochet file metadata v1\nformat-version=1\nguest-name={guest_name}\nfile-type=0x00000064\nload-address=0x00000000\nexecution-address=0x00000000\nattributes=0x00000000\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn native_boot_linker_reaches_linked_unpublished_modules_from_empty_table() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let capsule =
            BootCapsule::decode(embedded_capsule_bytes().unwrap(), RUNTIME_ABI_VERSION).unwrap();
        dispatcher.reset_boot_registry();
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);
        let ids = dispatcher.link_capsule_unpublished(&capsule).unwrap();
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);
        assert_eq!(ids.len(), capsule.modules.len());
        for module in &capsule.modules {
            let record = dispatcher
                .module_registry
                .module_named(&module.manifest.name)
                .unwrap();
            assert_eq!(record.state, ModuleState::Linked);
        }
    }

    #[test]
    fn incomplete_foundation_capsule_is_rejected_before_any_swi_publication() {
        let inputs = [
            BootModuleInput {
                source_path: "modules/Console.bas64",
                source: "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Console 1.0.0\nREM @EXPORT PROC Entry\nDEF PROC Entry\nENDPROC\n",
                grants: &[],
            },
            BootModuleInput {
                source_path: "modules/System.bas64",
                source: "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE System 1.0.0\nREM @EXPORT PROC Entry\nDEF PROC Entry\nENDPROC\n",
                grants: &[],
            },
            BootModuleInput {
                source_path: "modules/Boot.bas64",
                source: "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Boot 1.0.0\nREM @EXPORT PROC Entry\nDEF PROC Entry\nENDPROC\n",
                grants: &[],
            },
        ];
        let bytes = BootCapsule::build(RUNTIME_ABI_VERSION, &inputs).unwrap();
        let (_input_sender, input_receiver) = mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(HostConsole::windowed(input_receiver));

        let failure = dispatcher.bootstrap_capsule(&bytes).unwrap_err();

        assert_eq!(failure.stage, BootStage::ModuleValidation);
        assert_eq!(failure.module.as_deref(), Some("ModuleManager"));
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);
        assert!(dispatcher.module_registry.module_named("Console").is_none());
    }

    #[test]
    fn basic64_boot_module_owns_command_vs_desktop_startup_selection() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let (updates, _update_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        let mut dispatcher = SwiDispatcher::windowed_with_desktop(
            HostConsole::windowed(input_receiver),
            display_sender,
            wimp,
        );
        let path = std::env::temp_dir().join(format!(
            "ricochet-boot-policy-{}.configure",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let configure = ConfigureStore::with_path(&path);

        configure.set("Language", "0").unwrap();
        dispatcher.set_configure_store_for_test(configure.clone());
        assert_eq!(
            dispatcher.startup_target,
            Some(BootStartupTarget::MosPrompt),
            "Boot.Start failure: {:?}",
            dispatcher.boot_failure
        );
        assert!(!dispatcher.desktop_is_configured_for_startup());

        configure.set("Language", "3").unwrap();
        dispatcher.set_configure_store_for_test(configure.clone());
        assert_eq!(dispatcher.startup_target, Some(BootStartupTarget::Desktop));
        assert!(dispatcher.desktop_is_configured_for_startup());

        configure.set("Language", "0").unwrap();
        dispatcher.set_configure_store_for_test(configure);
        assert_eq!(
            dispatcher.startup_target,
            Some(BootStartupTarget::MosPrompt)
        );
        assert!(!dispatcher.desktop_is_configured_for_startup());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn error_task_and_module_foundation_sw_is_are_owned_by_basic64_modules() {
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        assert!(
            dispatcher.boot_failure.is_none(),
            "{:?}",
            dispatcher.boot_failure
        );
        let mut task = Task::new(0x1234);

        let error_address = 0x2400;
        let mut error_block = 0xC001_2345_u32.to_le_bytes().to_vec();
        error_block.extend_from_slice(b"disk changed\0");
        task.memory
            .write_bytes(error_address, &error_block)
            .unwrap();
        let mut error_call = SwiContext::default();
        error_call.registers[R0] = error_address;
        let error = dispatcher
            .dispatch(OS_GENERATE_ERROR, &mut task, &mut error_call)
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Structured { type_name, code, message }
                if type_name == "OSError" && code == 0xC001_2345 && message == "disk changed"
        ));
        assert!(
            matches!(
                dispatcher.last_dispatch_route(),
                Some(SwiDispatchRoute::ModuleOwned { number: OS_GENERATE_ERROR, module, definition, .. })
                    if module == "Error" && definition == "GENERATEERROR"
            ),
            "route: {:?}",
            dispatcher.last_dispatch_route()
        );

        let x_error_address = 0x2410;
        let mut x_error_block = 0xC001_2346_u32.to_le_bytes().to_vec();
        x_error_block.extend_from_slice(b"x form preserves input block\0");
        task.memory
            .write_bytes(x_error_address, &x_error_block)
            .unwrap();
        let mut x_error_call = SwiContext::default();
        x_error_call.registers[R0] = x_error_address;
        dispatcher
            .dispatch(OS_GENERATE_ERROR | SWI_X_BIT, &mut task, &mut x_error_call)
            .unwrap();
        assert!(x_error_call.overflow);
        assert_eq!(
            x_error_call.registers[R0], x_error_address,
            "XOS_GenerateError returns the caller's supplied error block"
        );

        let mut task_info = SwiContext::default();
        task_info.registers[R0] = 1;
        dispatcher
            .dispatch(RICOCHET_TASK_INFO, &mut task, &mut task_info)
            .unwrap();
        assert_eq!(task_info.registers[R1], 0x1234);
        assert_eq!(
            task_info.registers[R2],
            (crate::memory::GUEST_MEMORY_SIZE + crate::memory::SWI_ERROR_BLOCK_SIZE) as u32
        );
        assert_eq!(task_info.registers[R3], 0);
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number: RICOCHET_TASK_INFO, module, definition, .. })
                if module == "TaskManager" && definition == "READTASKINFO"
        ));

        let name_buffer = 0x3000;
        let mut cursor = 0_u32;
        let mut module_names = BTreeSet::new();
        loop {
            let mut module_info = SwiContext::default();
            module_info.registers[R0] = 1; // Ricochet_ModuleInfo ABI version.
            module_info.registers[R1] = cursor;
            module_info.registers[R2] = name_buffer;
            module_info.registers[R3] = 128;
            dispatcher
                .dispatch(RICOCHET_MODULE_INFO, &mut task, &mut module_info)
                .unwrap();
            if module_info.registers[R4] == 0 {
                break;
            }
            let name = task.memory.read_c_string(name_buffer, 128).unwrap();
            module_names.insert(String::from_utf8(name).unwrap());
            assert_eq!(module_info.registers[R5], 1, "enumerated module version");
            assert_eq!(module_info.registers[R8], 4, "enumerated module is active");
            cursor = module_info.registers[R1];
        }
        assert!(module_names.contains("Boot"));
        assert!(module_names.contains("Console"));
        assert!(module_names.contains("Error"));
        assert!(module_names.contains("Memory"));
        assert!(module_names.contains("ModuleManager"));
        assert!(module_names.contains("System"));
        assert!(module_names.contains("TaskManager"));
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number: RICOCHET_MODULE_INFO, module, definition, .. })
                if module == "ModuleManager" && definition == "READMODULEINFO"
        ));
    }

    #[test]
    fn system_query_swis_use_active_manifest_identity_and_checked_buffers() {
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        assert!(
            dispatcher.boot_failure.is_none(),
            "{:?}",
            dispatcher.boot_failure
        );
        let mut task = Task::new(0x1235);

        let mut first_time = SwiContext::default();
        dispatcher
            .dispatch(OS_READ_MONOTONIC_TIME, &mut task, &mut first_time)
            .unwrap();
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number: OS_READ_MONOTONIC_TIME, module, definition, .. })
                if module == "System" && definition == "READMONOTONICTIME"
        ));
        std::thread::sleep(Duration::from_millis(25));
        let mut second_time = SwiContext::default();
        dispatcher
            .dispatch(OS_READ_MONOTONIC_TIME, &mut task, &mut second_time)
            .unwrap();
        assert!(second_time.registers[R0].wrapping_sub(first_time.registers[R0]) > 0);

        let name_buffer = 0x3200;
        let mut to_string = SwiContext::default();
        to_string.registers[R0] = OS_READ_MONOTONIC_TIME;
        to_string.registers[R1] = name_buffer;
        to_string.registers[R2] = SYSTEM_SWI_NAME_MAX_BYTES as u32;
        dispatcher
            .dispatch(OS_SWI_NUMBER_TO_STRING, &mut task, &mut to_string)
            .unwrap();
        assert_eq!(to_string.registers[R0], OS_READ_MONOTONIC_TIME);
        assert_eq!(to_string.registers[R1], name_buffer);
        assert_eq!(
            task.memory
                .read_c_string(name_buffer, SYSTEM_SWI_NAME_MAX_BYTES)
                .unwrap(),
            b"OS_ReadMonotonicTime"
        );
        assert_eq!(
            to_string.registers[R2],
            b"OS_ReadMonotonicTime".len() as u32
        );
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number: OS_SWI_NUMBER_TO_STRING, module, definition, .. })
                if module == "System" && definition == "SWINUMBERTOSTRING"
        ));

        let mut x_to_string = SwiContext::default();
        x_to_string.registers[R0] = OS_READ_MONOTONIC_TIME | SWI_X_BIT;
        x_to_string.registers[R1] = name_buffer;
        x_to_string.registers[R2] = SYSTEM_SWI_NAME_MAX_BYTES as u32;
        dispatcher
            .dispatch(OS_SWI_NUMBER_TO_STRING, &mut task, &mut x_to_string)
            .unwrap();
        assert_eq!(
            task.memory
                .read_c_string(name_buffer, SYSTEM_SWI_NAME_MAX_BYTES)
                .unwrap(),
            b"XOS_ReadMonotonicTime"
        );

        let input_buffer = 0x3400;
        task.memory
            .write_bytes(input_buffer, b"OS_ReadMonotonicTime \0")
            .unwrap();
        let mut from_string = SwiContext::default();
        from_string.registers[R1] = input_buffer;
        dispatcher
            .dispatch(OS_SWI_NUMBER_FROM_STRING, &mut task, &mut from_string)
            .unwrap();
        assert_eq!(from_string.registers[R0], OS_READ_MONOTONIC_TIME);
        assert_eq!(from_string.registers[R1], input_buffer);
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number: OS_SWI_NUMBER_FROM_STRING, module, definition, .. })
                if module == "System" && definition == "SWINUMBERFROMSTRING"
        ));

        task.memory
            .write_bytes(input_buffer, b"XOS_ReadMonotonicTime\0")
            .unwrap();
        let mut x_from_string = SwiContext::default();
        x_from_string.registers[R1] = input_buffer;
        dispatcher
            .dispatch(OS_SWI_NUMBER_FROM_STRING, &mut task, &mut x_from_string)
            .unwrap();
        assert_eq!(
            x_from_string.registers[R0],
            OS_READ_MONOTONIC_TIME | SWI_X_BIT
        );
        assert_eq!(x_from_string.registers[R1], input_buffer);

        task.memory
            .write_bytes(input_buffer, b"os_ReadMonotonicTime\0")
            .unwrap();
        let mut wrong_case = SwiContext::default();
        wrong_case.registers[R1] = input_buffer;
        let error = dispatcher
            .dispatch(OS_SWI_NUMBER_FROM_STRING, &mut task, &mut wrong_case)
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Structured { type_name, .. } if type_name == "SwiIdentityNotFound"
        ));

        let mut too_small = SwiContext::default();
        too_small.registers[R0] = OS_READ_MONOTONIC_TIME;
        too_small.registers[R1] = name_buffer;
        too_small.registers[R2] = 1;
        let error = dispatcher
            .dispatch(OS_SWI_NUMBER_TO_STRING, &mut task, &mut too_small)
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Structured { type_name, .. } if type_name == "SwiNameBufferError"
        ));

        let mut graphics_swi = SwiContext::default();
        graphics_swi.registers[R0] = OS_READ_POINT;
        graphics_swi.registers[R1] = name_buffer;
        graphics_swi.registers[R2] = SYSTEM_SWI_NAME_MAX_BYTES as u32;
        dispatcher
            .dispatch(OS_SWI_NUMBER_TO_STRING, &mut task, &mut graphics_swi)
            .unwrap();
        assert_eq!(
            task.memory
                .read_c_string(name_buffer, SYSTEM_SWI_NAME_MAX_BYTES)
                .unwrap(),
            b"OS_ReadPoint"
        );
        assert_eq!(graphics_swi.registers[R0], OS_READ_POINT);
    }

    #[test]
    fn module_manager_loads_inspects_and_unloads_guest_source_modules() {
        let root = std::env::temp_dir().join(format!(
            "ricochet-wp51-module-lifecycle-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE GuestService 1.0.0\nREM @LIFECYCLE START Start\nREM @LIFECYCLE QUIESCE Quiesce\nREM @LIFECYCLE FINALISE Finalise\nREM @STATE STARTCOUNT% UINT32\nREM @STATE QUIESCECOUNT% UINT32\nREM @STATE FINALISECOUNT% UINT32\nREM @SWI Guest_Service &4FF20 Entry REGISTERS=R0:U32:INOUT\nREM @SWI Guest_Fail &4FF21 Fail REGISTERS=R0:U32:INOUT\nREM @SWI Guest_Flags &4FF22 FlagsProbe REGISTERS=R0:U32:INOUT\nDEF PROC Start\n    STARTCOUNT% = STARTCOUNT% + 1\nENDPROC\nDEF PROC Quiesce\n    QUIESCECOUNT% = QUIESCECOUNT% + 1\nENDPROC\nDEF PROC Finalise\n    FINALISECOUNT% = FINALISECOUNT% + 1\nENDPROC\nDEF PROC Entry\n    R0% = R0% + STARTCOUNT%\nENDPROC\nDEF PROC Fail\n    SYS \"NoSuchInnerSwi\"\nENDPROC\nDEF PROC FlagsProbe\n    SYS \"XOS_Module\", 18, 0 TO ERRORADDRESS%, INNERFLAGS% ; FLAGS%\n    R0% = FLAGS%\nENDPROC\n";
        write_guest_module_source(&root, "GuestService", source);

        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        assert!(
            dispatcher.boot_failure.is_none(),
            "{:?}",
            dispatcher.boot_failure
        );
        dispatcher.file_system = HostFileSystem::new(&root);
        let mut task = Task::trusted_mos_session(0x5120);
        let path_address = 0x2200;
        let module_name_address = 0x2300;
        task.memory
            .write_bytes(path_address, b"GuestService\0")
            .unwrap();
        task.memory
            .write_bytes(module_name_address, b"GuestService\0")
            .unwrap();
        let module_swi = dispatcher.module_registry.swi_number("OS_Module").unwrap();
        assert_eq!(
            dispatcher.module_registry.swi_module_name(module_swi),
            Some("ModuleManager")
        );

        let mut load = SwiContext::default();
        load.registers[R0] = 1;
        load.registers[R1] = path_address;
        load.registers[R2] = 0xAA55_1234;
        dispatcher
            .dispatch(module_swi, &mut task, &mut load)
            .unwrap();
        assert_eq!(load.registers[R0], 1, "OS_Module Load preserves R0");
        assert_eq!(
            load.registers[R1], path_address,
            "OS_Module Load preserves R1"
        );
        assert_eq!(load.registers[R2], 0xAA55_1234);
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number, module, definition, .. })
                if *number == module_swi && module == "ModuleManager" && definition == "OPERATEMODULE"
        ));
        let guest_swi = dispatcher
            .module_registry
            .swi_number("Guest_Service")
            .unwrap();
        let module_record = dispatcher
            .module_registry
            .module_named("GuestService")
            .unwrap();
        assert_eq!(module_record.state, ModuleState::Active);
        let start_program = dispatcher
            .module_programs
            .values()
            .find(|program| program.manifest.name == "GuestService")
            .unwrap();
        assert_eq!(start_program.workspace_number("STARTCOUNT%"), Some(1.0));

        let mut service = SwiContext::default();
        service.registers[R0] = 40;
        dispatcher
            .dispatch(guest_swi, &mut task, &mut service)
            .unwrap();
        assert_eq!(service.registers[R0], 41);
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number, module, definition, .. })
                if *number == guest_swi && module == "GuestService" && definition == "ENTRY"
        ));

        let mut flags_probe = SwiContext::default();
        let flags_swi = dispatcher
            .module_registry
            .swi_number("Guest_Flags")
            .unwrap();
        dispatcher
            .dispatch(flags_swi, &mut task, &mut flags_probe)
            .unwrap();
        assert_eq!(
            flags_probe.registers[R0], 1,
            "SYS ;flags reports the X-form V flag"
        );

        let mut lookup = SwiContext::default();
        lookup.registers[R0] = 1;
        lookup.registers[R1] = module_name_address;
        dispatcher
            .dispatch(RICOCHET_MODULE_LOOKUP, &mut task, &mut lookup)
            .unwrap();
        assert!(
            matches!(
                dispatcher.last_dispatch_route(),
                Some(SwiDispatchRoute::ModuleOwned { number, module, definition, .. })
                    if *number == RICOCHET_MODULE_LOOKUP && module == "ModuleManager" && definition == "LOOKUPMODULE"
            ),
            "route: {:?}",
            dispatcher.last_dispatch_route()
        );
        assert_eq!(lookup.registers[R1], 1, "module was found");
        assert!(lookup.registers[R2] > 0, "one-based active module number");
        assert_eq!(lookup.registers[R3..=R5], [1, 0, 0]);
        assert_eq!(lookup.registers[R6], 4, "module is active");

        task.memory
            .write_bytes(module_name_address, b"MissingModule\0")
            .unwrap();
        let mut missing_lookup = SwiContext::default();
        missing_lookup.registers[R0] = 1;
        missing_lookup.registers[R1] = module_name_address;
        dispatcher
            .dispatch(RICOCHET_MODULE_LOOKUP, &mut task, &mut missing_lookup)
            .unwrap();
        assert_eq!(missing_lookup.registers[R1..=R6], [0; 6]);
        task.memory
            .write_bytes(module_name_address, b"GuestService\0")
            .unwrap();

        let swi_name = 0x2400;
        let owner_name = 0x2500;
        let definition_name = 0x2600;
        let mut swi_info = SwiContext::default();
        swi_info.registers[R0] = 1;
        swi_info.registers[R1] = guest_swi;
        swi_info.registers[R2] = swi_name;
        swi_info.registers[R3] = 128;
        swi_info.registers[R4] = owner_name;
        swi_info.registers[R5] = 128;
        swi_info.registers[R6] = definition_name;
        swi_info.registers[R7] = 128;
        dispatcher
            .dispatch(RICOCHET_SWI_INFO, &mut task, &mut swi_info)
            .unwrap();
        assert_eq!(swi_info.registers[R8], 1, "first definition generation");
        assert_eq!(
            task.memory.read_c_string(swi_name, 128).unwrap(),
            b"Guest_Service"
        );
        assert_eq!(
            task.memory.read_c_string(owner_name, 128).unwrap(),
            b"GuestService"
        );
        assert_eq!(
            task.memory.read_c_string(definition_name, 128).unwrap(),
            b"ENTRY"
        );

        let mut failed_service = SwiContext::default();
        failed_service.registers[R0] = 0x1234;
        let guest_fail_swi = dispatcher.module_registry.swi_number("Guest_Fail").unwrap();
        dispatcher
            .dispatch(guest_fail_swi | SWI_X_BIT, &mut task, &mut failed_service)
            .unwrap();
        assert!(failed_service.overflow);
        let error_address = failed_service.registers[R0];
        assert!(error_address >= task.memory.swi_error_block_address());
        let error_code = u32::from_le_bytes(
            task.memory
                .read_bytes(error_address, 4)
                .unwrap()
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            error_code, SWI_UNKNOWN_ERROR_CODE,
            "unknown named SWI has the stable generic error code"
        );
        let failed_message =
            String::from_utf8(task.memory.read_c_string(error_address + 4, 252).unwrap()).unwrap();
        assert!(
            failed_message.contains("NOSUCHINNERSWI"),
            "{failed_message:?}"
        );

        let mut delete = SwiContext::default();
        delete.registers[R0] = 4;
        delete.registers[R1] = module_name_address;
        delete.registers[R2] = 0xCAFE_BABE;
        dispatcher
            .dispatch(module_swi, &mut task, &mut delete)
            .unwrap();
        assert_eq!(delete.registers[R0], 4, "OS_Module Delete preserves R0");
        assert_eq!(
            delete.registers[R1], module_name_address,
            "OS_Module Delete preserves R1"
        );
        assert_eq!(delete.registers[R2], 0xCAFE_BABE);
        assert!(
            dispatcher
                .module_registry
                .module_named("GuestService")
                .is_none()
        );
        assert!(
            dispatcher
                .module_registry
                .swi_number("Guest_Service")
                .is_none()
        );
        assert!(
            dispatcher
                .module_programs
                .values()
                .all(|program| { !program.manifest.name.eq_ignore_ascii_case("GuestService") })
        );

        let mut reload = SwiContext::default();
        reload.registers[R0] = 1;
        reload.registers[R1] = path_address;
        dispatcher
            .dispatch(module_swi, &mut task, &mut reload)
            .unwrap();
        assert_eq!(
            dispatcher
                .module_programs
                .values()
                .find(|program| program.manifest.name == "GuestService")
                .unwrap()
                .workspace_number("STARTCOUNT%"),
            Some(1.0),
            "a fresh load gets a private fresh module workspace"
        );
        let mut final_delete = SwiContext::default();
        final_delete.registers[R0] = 4;
        final_delete.registers[R1] = module_name_address;
        dispatcher
            .dispatch(module_swi, &mut task, &mut final_delete)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn module_manager_replaces_guest_exports_as_one_generation_and_preserves_workspace() {
        let root = std::env::temp_dir().join(format!(
            "ricochet-wp51-module-replacement-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let original = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE GuestSwap 1.0.0\nREM @STATE COUNT% UINT32\nREM @SWI Swap_Run &4FF40 Run REGISTERS=R0:U32:INOUT\nREM @SWI Swap_Check &4FF41 Check REGISTERS=R0:U32:INOUT\nDEF PROC Run\n    COUNT% = COUNT% + 1\n    R0% = R0% + 10\nENDPROC\nDEF PROC Check\n    R0% = R0% + COUNT%\nENDPROC\n";
        let replacement = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE GuestSwap 1.0.0\nREM @STATE COUNT% UINT32\nREM @SWI Swap_Run &4FF40 Run REGISTERS=R0:U32:INOUT\nREM @SWI Swap_Check &4FF41 Check REGISTERS=R0:U32:INOUT\nDEF PROC Run\n    COUNT% = COUNT% + 100\n    R0% = R0% + 1000\nENDPROC\nDEF PROC Check\n    R0% = R0% + COUNT%\nENDPROC\n";
        let incompatible = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE GuestSwap 1.0.0\nREM @STATE COUNT% UINT32\nREM @SWI Swap_Run &4FF40 Run REGISTERS=R0:U32:INOUT\nREM @SWI Swap_Check &4FF41 Check REGISTERS=R0:U32:OUT\nDEF PROC Run\n    COUNT% = COUNT% + 100\n    R0% = R0% + 1000\nENDPROC\nDEF PROC Check\n    R0% = COUNT%\nENDPROC\n";
        write_guest_module_source(&root, "GuestSwap", original);

        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        assert!(
            dispatcher.boot_failure.is_none(),
            "{:?}",
            dispatcher.boot_failure
        );
        dispatcher.file_system = HostFileSystem::new(&root);
        let mut task = Task::trusted_mos_session(0x5122);
        let path_address = 0x2200;
        task.memory
            .write_bytes(path_address, b"GuestSwap\0")
            .unwrap();
        let module_swi = dispatcher.module_registry.swi_number("OS_Module").unwrap();
        let load_guest = |dispatcher: &mut SwiDispatcher,
                          task: &mut Task,
                          module_swi: u32,
                          path_address: u32|
         -> Result<(), RuntimeError> {
            let mut load = SwiContext::default();
            load.registers[R0] = 1;
            load.registers[R1] = path_address;
            dispatcher.dispatch(module_swi, task, &mut load)?;
            assert_eq!(load.registers[R0], 1);
            assert_eq!(load.registers[R1], path_address);
            Ok(())
        };

        load_guest(&mut dispatcher, &mut task, module_swi, path_address).unwrap();
        let old_source_hash = dispatcher
            .module_registry
            .module_named("GuestSwap")
            .unwrap()
            .manifest
            .source_hash
            .clone();
        let module_id = dispatcher
            .module_registry
            .module_named("GuestSwap")
            .unwrap()
            .id;
        let instance_id = dispatcher
            .module_registry
            .module_named("GuestSwap")
            .unwrap()
            .instance_id;
        let entry_ids = [0x4FF40, 0x4FF41]
            .map(|number| dispatcher.module_registry.swi_entry_id(number).unwrap());
        let run_swi = dispatcher.module_registry.swi_number("Swap_Run").unwrap();
        let check_swi = dispatcher.module_registry.swi_number("Swap_Check").unwrap();
        let mut initial_call = SwiContext::default();
        dispatcher
            .dispatch(run_swi, &mut task, &mut initial_call)
            .unwrap();
        assert_eq!(initial_call.registers[R0], 10);

        // Keep a real old-generation lease across the module-level commit.
        // This is the paused half of an in-flight call; after publication it
        // must still resolve to the old descriptor and old source unit.
        let (old_ownership, old_lease) = dispatcher
            .module_registry
            .acquire_swi(run_swi)
            .expect("guest SWI is active");
        let old_program = dispatcher.module_programs[&old_lease.id].clone();
        write_guest_module_source(&root, "GuestSwap", replacement);
        load_guest(&mut dispatcher, &mut task, module_swi, path_address).unwrap();

        let replacement_source_hash = {
            let record = dispatcher
                .module_registry
                .module_named("GuestSwap")
                .unwrap();
            assert_eq!(record.id, module_id);
            assert_eq!(record.instance_id, instance_id);
            assert_eq!(record.state, ModuleState::Active);
            record.manifest.source_hash.clone()
        };
        assert_ne!(replacement_source_hash, old_source_hash);
        assert_eq!(
            [0x4FF40, 0x4FF41]
                .map(|number| { dispatcher.module_registry.swi_entry_id(number).unwrap() }),
            entry_ids,
            "all public entry cells retain their stable identities"
        );
        let new_run = dispatcher
            .module_registry
            .active_swi_identity(run_swi)
            .unwrap();
        let new_check = dispatcher
            .module_registry
            .active_swi_identity(check_swi)
            .unwrap();
        assert_eq!(new_run.generation_number, 2);
        assert_eq!(new_check.generation_number, 2);
        assert_eq!(
            dispatcher
                .module_registry
                .current_swi_definition(run_swi)
                .unwrap()
                .source_hash,
            replacement_source_hash
        );
        assert_eq!(
            dispatcher
                .module_registry
                .current_swi_definition(check_swi)
                .unwrap()
                .source_hash,
            replacement_source_hash
        );
        assert_ne!(new_run.definition, old_ownership.definition);

        let mut new_call = SwiContext::default();
        new_call.registers[R0] = 5;
        dispatcher
            .dispatch(run_swi, &mut task, &mut new_call)
            .unwrap();
        assert_eq!(
            new_call.registers[R0], 1005,
            "subsequent calls use new source"
        );
        let mut retained_state = SwiContext::default();
        dispatcher
            .dispatch(check_swi, &mut task, &mut retained_state)
            .unwrap();
        assert_eq!(retained_state.registers[R0], 101, "workspace is shared");

        let mut old_call = SwiContext::default();
        old_call.registers[R0] = 7;
        old_program
            .invoke(
                old_ownership.module,
                &old_lease,
                &old_ownership.contract,
                &mut task,
                &mut dispatcher,
                &mut old_call,
            )
            .unwrap();
        assert_eq!(
            old_call.registers[R0], 17,
            "the retained lease uses old code"
        );
        let mut state_after_old_call = SwiContext::default();
        dispatcher
            .dispatch(check_swi, &mut task, &mut state_after_old_call)
            .unwrap();
        assert_eq!(state_after_old_call.registers[R0], 102);

        write_guest_module_source(&root, "GuestSwap", incompatible);
        let rejected = load_guest(&mut dispatcher, &mut task, module_swi, path_address)
            .expect_err("incompatible export contracts must roll back");
        assert!(matches!(
            rejected,
            RuntimeError::Structured { ref type_name, code: 1, .. }
                if type_name == "ModuleReplacementIncompatible"
        ));
        let after_rejection = dispatcher
            .module_registry
            .active_swi_identity(run_swi)
            .unwrap();
        assert_eq!(after_rejection.generation_number, 2);
        assert_eq!(
            dispatcher
                .module_registry
                .current_swi_definition(run_swi)
                .unwrap()
                .source_hash,
            replacement_source_hash
        );
        let mut still_new = SwiContext::default();
        dispatcher
            .dispatch(run_swi, &mut task, &mut still_new)
            .unwrap();
        assert_eq!(still_new.registers[R0], 1000);

        drop(old_lease);
        drop(old_program);
        let mut drained_state = SwiContext::default();
        dispatcher
            .dispatch(check_swi, &mut task, &mut drained_state)
            .unwrap();
        assert!(!drained_state.overflow);
        assert!(
            !dispatcher
                .module_programs
                .contains_key(&old_ownership.definition)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn module_manager_rolls_back_failed_guest_start_and_protects_foundation() {
        let root = std::env::temp_dir().join(format!(
            "ricochet-wp51-module-start-failure-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bad_source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE RetryService 1.0.0\nREM @LIFECYCLE START Start\nREM @PRIVATE PROC Start\nREM @SWI Retry_Service &4FF30 Entry\nDEF PROC Start\n    SYS \"NoSuchStartupSwi\"\nENDPROC\nDEF PROC Entry\nENDPROC\n";
        write_guest_module_source(&root, "RetryService", bad_source);
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        assert!(
            dispatcher.boot_failure.is_none(),
            "{:?}",
            dispatcher.boot_failure
        );
        dispatcher.file_system = HostFileSystem::new(&root);
        let mut task = Task::trusted_mos_session(0x5121);
        let path_address = 0x2200;
        let module_name_address = 0x2300;
        task.memory
            .write_bytes(path_address, b"RetryService\0")
            .unwrap();
        task.memory
            .write_bytes(module_name_address, b"RetryService\0")
            .unwrap();
        let module_swi = dispatcher.module_registry.swi_number("OS_Module").unwrap();

        let mut failed_load = SwiContext::default();
        failed_load.registers[R0] = 1;
        failed_load.registers[R1] = path_address;
        let error = dispatcher
            .dispatch(module_swi, &mut task, &mut failed_load)
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Structured { type_name, .. } if type_name == "ModuleStartError"
        ));
        assert!(
            dispatcher
                .module_registry
                .module_named("RetryService")
                .is_none()
        );
        assert!(
            dispatcher
                .module_registry
                .swi_number("Retry_Service")
                .is_none()
        );

        let valid_source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE RetryService 1.0.0\nREM @LIFECYCLE START Start\nREM @PRIVATE PROC Start\nREM @SWI Retry_Service &4FF30 Entry\nDEF PROC Start\nENDPROC\nDEF PROC Entry\nENDPROC\n";
        write_guest_module_source(&root, "RetryService", valid_source);
        let mut retry = SwiContext::default();
        retry.registers[R0] = 1;
        retry.registers[R1] = path_address;
        dispatcher
            .dispatch(module_swi, &mut task, &mut retry)
            .unwrap();
        assert_eq!(
            dispatcher
                .module_registry
                .module_named("RetryService")
                .unwrap()
                .state,
            ModuleState::Active
        );

        let registered_swi_count = dispatcher.module_registry.registered_swi_count();
        let mut unload_foundation = SwiContext::default();
        unload_foundation.registers[R0] = 4;
        unload_foundation.registers[R1] = 0x2400;
        task.memory
            .write_bytes(unload_foundation.registers[R1], b"ModuleManager\0")
            .unwrap();
        let error = dispatcher
            .dispatch(module_swi, &mut task, &mut unload_foundation)
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Structured { type_name, .. } if type_name == "ProtectedFoundationModule"
        ));
        assert!(
            dispatcher
                .module_registry
                .module_named("ModuleManager")
                .is_some()
        );
        assert_eq!(
            dispatcher.module_registry.registered_swi_count(),
            registered_swi_count,
            "rejected foundation delete changed public SWI registration"
        );

        let mut unload_guest = SwiContext::default();
        unload_guest.registers[R0] = 4;
        unload_guest.registers[R1] = module_name_address;
        dispatcher
            .dispatch(module_swi, &mut task, &mut unload_guest)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn module_manager_enforces_guest_capability_and_dependency_boundaries() {
        let root = std::env::temp_dir().join(format!(
            "ricochet-wp51-module-dependencies-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let provider = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE GuestProvider 1.0.0\nREM @EXPORT PROC Value\nDEF PROC Value\nENDPROC\n";
        let consumer = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE GuestConsumer 1.0.0\nREM @DEPENDS GuestProvider 1.0.0\nREM @IMPORT_SYMBOL GuestProvider PROC Value\nREM @SWI Guest_Consumer &4FF40 Entry REGISTERS=R0:U32:INOUT\nDEF PROC Entry\n    PROC GuestProvider.Value\n    R0% = R0% + 1\nENDPROC\n";
        let privileged = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE GuestPrivileged 1.0.0\nREM @CAPABILITY ConsoleOutput\nREM @IMPORT Host.Console.WriteByte ConsoleOutput\nREM @SWI Guest_Privileged &4FF41 Entry\nDEF PROC Entry\n    PRIMITIVE Host.Console.WriteByte, 65\nENDPROC\n";
        let non_riscos_title = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Guest.Title 1.0.0\nREM @SWI Guest_Title &4FF42 Entry\nDEF PROC Entry\nENDPROC\n";
        write_guest_module_source(&root, "GuestProvider", provider);
        write_guest_module_source(&root, "GuestConsumer", consumer);
        write_guest_module_source(&root, "GuestPrivileged", privileged);
        write_guest_module_source(&root, "GuestBadTitle", non_riscos_title);
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        assert!(
            dispatcher.boot_failure.is_none(),
            "{:?}",
            dispatcher.boot_failure
        );
        dispatcher.file_system = HostFileSystem::new(&root);
        let mut task = Task::trusted_mos_session(0x5124);
        let path_address = 0x2200;
        let module_name_address = 0x2300;
        let module_swi = dispatcher.module_registry.swi_number("OS_Module").unwrap();

        for guest_name in ["GuestProvider", "GuestConsumer"] {
            task.memory
                .write_bytes(path_address, format!("{guest_name}\0").as_bytes())
                .unwrap();
            let mut load = SwiContext::default();
            load.registers[R0] = 1;
            load.registers[R1] = path_address;
            dispatcher
                .dispatch(module_swi, &mut task, &mut load)
                .unwrap();
        }
        let consumer_swi = dispatcher
            .module_registry
            .swi_number("Guest_Consumer")
            .unwrap();
        let mut call = SwiContext::default();
        call.registers[R0] = 9;
        dispatcher
            .dispatch(consumer_swi, &mut task, &mut call)
            .unwrap();
        assert_eq!(
            call.registers[R0], 10,
            "qualified import calls the loaded module"
        );

        task.memory
            .write_bytes(module_name_address, b"GuestProvider\0")
            .unwrap();
        let mut unload_provider = SwiContext::default();
        unload_provider.registers[R0] = 4;
        unload_provider.registers[R1] = module_name_address;
        let error = dispatcher
            .dispatch(module_swi, &mut task, &mut unload_provider)
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Structured { type_name, .. } if type_name == "ModuleInUse"
        ));
        assert!(
            dispatcher
                .module_registry
                .swi_number("Guest_Consumer")
                .is_some()
        );

        task.memory
            .write_bytes(path_address, b"GuestPrivileged\0")
            .unwrap();
        let mut privileged_load = SwiContext::default();
        privileged_load.registers[R0] = 1;
        privileged_load.registers[R1] = path_address;
        let error = dispatcher
            .dispatch(module_swi, &mut task, &mut privileged_load)
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Structured { type_name, .. } if type_name == "ModuleCapabilityDenied"
        ));
        assert!(
            dispatcher
                .module_registry
                .module_named("GuestPrivileged")
                .is_none()
        );
        assert!(
            dispatcher
                .module_registry
                .swi_number("Guest_Privileged")
                .is_none()
        );

        task.memory
            .write_bytes(path_address, b"GuestBadTitle\0")
            .unwrap();
        let mut invalid_title_load = SwiContext::default();
        invalid_title_load.registers[R0] = 1;
        invalid_title_load.registers[R1] = path_address;
        let error = dispatcher
            .dispatch(module_swi, &mut task, &mut invalid_title_load)
            .unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Structured { type_name, .. } if type_name == "UnsupportedModuleTitle"
        ));
        assert!(
            dispatcher
                .module_registry
                .module_named("Guest.Title")
                .is_none()
        );

        task.memory
            .write_bytes(module_name_address, b"GuestConsumer\0")
            .unwrap();
        let mut unload_consumer = SwiContext::default();
        unload_consumer.registers[R0] = 4;
        unload_consumer.registers[R1] = module_name_address;
        dispatcher
            .dispatch(module_swi, &mut task, &mut unload_consumer)
            .unwrap();
        task.memory
            .write_bytes(module_name_address, b"GuestProvider\0")
            .unwrap();
        let mut unload_provider = SwiContext::default();
        unload_provider.registers[R0] = 4;
        unload_provider.registers[R1] = module_name_address;
        dispatcher
            .dispatch(module_swi, &mut task, &mut unload_provider)
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn swi_errors_and_x_form_errors_use_the_documented_task_scoped_block() {
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        assert!(
            dispatcher.boot_failure.is_none(),
            "{:?}",
            dispatcher.boot_failure
        );
        let mut task = Task::new(0x5122);

        let mut unknown = SwiContext::default();
        let normal_error = dispatcher
            .dispatch(0x7FFE, &mut task, &mut unknown)
            .unwrap_err();
        assert!(matches!(
            normal_error,
            RuntimeError::Structured { type_name, code, .. }
                if type_name == "UnknownSwi" && code == SWI_UNKNOWN_ERROR_CODE
        ));
        let mut unknown_x = SwiContext::default();
        dispatcher
            .dispatch(0x7FFE | SWI_X_BIT, &mut task, &mut unknown_x)
            .unwrap();
        assert!(unknown_x.overflow);
        let address = unknown_x.registers[R0];
        assert_eq!(address, task.memory.swi_error_block_address());
        assert_eq!(
            u32::from_le_bytes(
                task.memory
                    .read_bytes(address, 4)
                    .unwrap()
                    .try_into()
                    .unwrap()
            ),
            SWI_UNKNOWN_ERROR_CODE
        );
        assert!(
            String::from_utf8(task.memory.read_c_string(address + 4, 252).unwrap())
                .unwrap()
                .contains("no such SWI")
        );
        assert_eq!(Task::new(0x5123).memory.read_byte(address).unwrap(), 0);

        let module_swi = dispatcher.module_registry.swi_number("OS_Module").unwrap();
        for reason in (0..=20)
            .filter(|reason| !matches!(reason, 1 | 4))
            .chain([21, u32::MAX])
        {
            let mut call = SwiContext::default();
            call.registers[R0] = reason;
            // Unsupported reasons must be rejected by BASIC64's R0 policy
            // before interpreting R1 as any pointer-shaped argument.
            call.registers[R1] = u32::MAX;
            let unsupported = dispatcher
                .dispatch(module_swi, &mut task, &mut call)
                .unwrap_err();
            assert!(
                matches!(
                    unsupported,
                    RuntimeError::Structured { ref type_name, code, .. }
                        if type_name == "UnsupportedServiceReason" && code == reason
                ),
                "OS_Module reason {reason} returned {unsupported:?}"
            );
        }

        let mut successful_x = SwiContext::default();
        successful_x.registers[R0] = 1;
        successful_x.registers[R1] = 0;
        successful_x.registers[R2] = 0x2800;
        successful_x.registers[R3] = 128;
        dispatcher
            .dispatch(
                RICOCHET_MODULE_INFO | SWI_X_BIT,
                &mut task,
                &mut successful_x,
            )
            .unwrap();
        assert!(!successful_x.overflow, "successful X form clears V");

        let mut reason_18_x = SwiContext::default();
        reason_18_x.registers[R0] = 18;
        reason_18_x.registers[R1] = 0x2000;
        dispatcher
            .dispatch(module_swi | SWI_X_BIT, &mut task, &mut reason_18_x)
            .unwrap();
        assert!(reason_18_x.overflow);
        let error_address = reason_18_x.registers[R0];
        assert_eq!(
            u32::from_le_bytes(
                task.memory
                    .read_bytes(error_address, 4)
                    .unwrap()
                    .try_into()
                    .unwrap()
            ),
            18
        );
        assert!(
            String::from_utf8(task.memory.read_c_string(error_address + 4, 252).unwrap())
                .unwrap()
                .contains("&1E")
        );
    }

    #[test]
    fn basic64_memory_module_owns_task_scoped_dynamic_area_lifecycle() {
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        assert!(
            dispatcher.boot_failure.is_none(),
            "{:?}",
            dispatcher.boot_failure
        );
        let mut task = Task::new(90);
        let name_address = 0x2500;
        task.memory
            .write_bytes(name_address, b"Module cache\0")
            .unwrap();

        let mut create = SwiContext::default();
        create.registers[R0] = 0;
        create.registers[R1] = u32::MAX;
        create.registers[R2] = 16;
        create.registers[R3] = u32::MAX;
        create.registers[R4] = 0;
        create.registers[R5] = 256;
        create.registers[R6] = 0;
        create.registers[R7] = 0;
        create.registers[R8] = name_address;
        dispatcher
            .dispatch(OS_DYNAMIC_AREA, &mut task, &mut create)
            .unwrap();
        let area_number = create.registers[R1];
        let base = create.registers[R3];
        assert!(area_number >= 256);
        assert_eq!(base % crate::memory::DYNAMIC_AREA_PAGE_SIZE, 0);
        assert_eq!(create.registers[R5], 256);
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number: OS_DYNAMIC_AREA, module, definition, .. })
                if module == "Memory" && definition == "DYNAMICAREA"
        ));

        task.memory.write_byte(base + 15, b'X').unwrap();
        assert!(task.memory.write_byte(base + 16, b'Y').is_err());
        let other_task = Task::new(91);
        assert!(other_task.memory.read_byte(base).is_err());

        let mut grow = SwiContext::default();
        grow.registers[R0] = area_number;
        grow.registers[R1] = 32;
        dispatcher
            .dispatch(OS_CHANGE_DYNAMIC_AREA, &mut task, &mut grow)
            .unwrap();
        assert_eq!(grow.registers[R1], 32);
        task.memory.write_byte(base + 47, b'Z').unwrap();

        let mut shrink = SwiContext::default();
        shrink.registers[R0] = area_number;
        shrink.registers[R1] = (-8_i32) as u32;
        dispatcher
            .dispatch(OS_CHANGE_DYNAMIC_AREA, &mut task, &mut shrink)
            .unwrap();
        assert_eq!(shrink.registers[R1], 8);
        assert!(task.memory.read_byte(base + 40).is_err());

        let mut info = SwiContext::default();
        info.registers[R0] = 2;
        info.registers[R1] = area_number;
        dispatcher
            .dispatch(OS_DYNAMIC_AREA, &mut task, &mut info)
            .unwrap();
        assert_eq!(info.registers[R2], 40);
        assert_eq!(info.registers[R3], base);
        assert_eq!(info.registers[R5], 256);
        assert_eq!(info.registers[R8], name_address);

        let mut enumerate = SwiContext::default();
        enumerate.registers[R0] = 3;
        enumerate.registers[R1] = u32::MAX;
        dispatcher
            .dispatch(OS_DYNAMIC_AREA, &mut task, &mut enumerate)
            .unwrap();
        assert_eq!(enumerate.registers[R1], area_number);

        let mut remove = SwiContext::default();
        remove.registers[R0] = 1;
        remove.registers[R1] = area_number;
        dispatcher
            .dispatch(OS_DYNAMIC_AREA, &mut task, &mut remove)
            .unwrap();
        assert!(task.memory.read_byte(base).is_err());
        assert_eq!(task.memory.dynamic_area_count(), 0);
        assert_eq!(task.memory.next_dynamic_area(u32::MAX), None);
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number: OS_DYNAMIC_AREA, module, definition, .. })
                if module == "Memory" && definition == "DYNAMICAREA"
        ));
    }

    #[test]
    fn failed_foundation_start_rolls_back_the_complete_public_namespace() {
        let console = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Console 1.0.0\nREM @SWI Test_Console &501 Entry\nDEF PROC Entry\nENDPROC\n";
        let alpha = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Alpha 1.0.0\nREM @LIFECYCLE START Start\nREM @SWI Test_Alpha &500 Entry\nREM @PRIVATE PROC Start\nDEF PROC Start\nENDPROC\nDEF PROC Entry\nENDPROC\n";
        let beta = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Beta 1.0.0\nREM @LIFECYCLE START Start\nREM @PRIVATE PROC Start\nDEF PROC Start\nSYS \"NoSuchStartupSwi\"\nENDPROC\n";
        let mut inputs = vec![
            crate::boot::BootModuleInput {
                source_path: "modules/Console.bas64",
                source: console,
                grants: &[],
            },
            crate::boot::BootModuleInput {
                source_path: "modules/Alpha.bas64",
                source: alpha,
                grants: &[],
            },
            crate::boot::BootModuleInput {
                source_path: "modules/Beta.bas64",
                source: beta,
                grants: &[],
            },
            crate::boot::BootModuleInput {
                source_path: "modules/Boot.bas64",
                source: include_str!("../modules/Boot.bas64"),
                grants: &[],
            },
            crate::boot::BootModuleInput {
                source_path: "modules/System.bas64",
                source: include_str!("../modules/System.bas64"),
                grants: &["StartupPolicy", "SystemQueries", "SystemVariableStore"],
            },
        ];
        append_remaining_foundation_inputs(&mut inputs);
        let bytes = BootCapsule::build(RUNTIME_ABI_VERSION, &inputs).unwrap();
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);

        let failure = dispatcher.bootstrap_capsule(&bytes).unwrap_err();

        assert_eq!(failure.stage, BootStage::Start);
        assert_eq!(failure.module.as_deref(), Some("Beta"));
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);
        assert!(dispatcher.module_registry.module_named("Alpha").is_none());
        assert!(dispatcher.module_registry.module_named("Beta").is_none());
        assert!(dispatcher.module_registry.module_named("Console").is_none());
        assert!(dispatcher.module_registry.module_named("System").is_none());
        assert!(dispatcher.module_registry.module_named("Boot").is_none());
        assert!(dispatcher.module_programs.is_empty());
    }

    #[test]
    fn unresolved_primitive_import_fails_during_link_with_no_public_exports() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Console 1.0.0\nREM @CAPABILITY ConsoleOutput\nREM @IMPORT Host.Console.MissingPrimitive ConsoleOutput\nREM @SWI Test_Console &510 Entry\nDEF PROC Entry\nENDPROC\n";
        let mut inputs = vec![
            crate::boot::BootModuleInput {
                source_path: "modules/Console.bas64",
                source,
                grants: &["ConsoleOutput"],
            },
            crate::boot::BootModuleInput {
                source_path: "modules/Boot.bas64",
                source: include_str!("../modules/Boot.bas64"),
                grants: &[],
            },
            crate::boot::BootModuleInput {
                source_path: "modules/System.bas64",
                source: include_str!("../modules/System.bas64"),
                grants: &["StartupPolicy", "SystemQueries", "SystemVariableStore"],
            },
        ];
        append_remaining_foundation_inputs(&mut inputs);
        let bytes = BootCapsule::build(RUNTIME_ABI_VERSION, &inputs).unwrap();
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);

        let failure = dispatcher.bootstrap_capsule(&bytes).unwrap_err();

        assert_eq!(failure.stage, BootStage::Linking);
        assert_eq!(failure.module.as_deref(), Some("Console"));
        assert!(failure.cause_message.contains("not registered"));
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);
    }

    #[test]
    fn native_recovery_can_select_and_retry_an_alternate_capsule_without_swis() {
        let path = std::env::temp_dir().join(format!(
            "ricochet-boot-recovery-{}-{}.capsule",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, embedded_capsule_bytes().unwrap()).unwrap();
        let (input_sender, input_receiver) = mpsc::channel();
        for byte in format!("A {}\r", path.display()).bytes() {
            input_sender.send(byte).unwrap();
        }
        drop(input_sender);
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let corrupt = [0_u8; 32];
        let failure = dispatcher.bootstrap_capsule(&corrupt).unwrap_err();
        dispatcher.boot_failure = Some(failure);
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);

        assert!(dispatcher.recover_boot().unwrap());

        assert!(dispatcher.boot_failure.is_none());
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 40);
        let recovery_output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(
            recovery_output
                .windows("Ricochet native recovery".len())
                .any(|bytes| bytes == b"Ricochet native recovery")
        );
        assert!(
            recovery_output
                .windows(b"capsule validation".len())
                .any(|bytes| bytes == b"capsule validation")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn native_recovery_retry_reloads_the_embedded_capsule() {
        let (input_sender, input_receiver) = mpsc::channel();
        for byte in b"R\r" {
            input_sender.send(*byte).unwrap();
        }
        drop(input_sender);
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let failure = dispatcher.bootstrap_capsule(&[0_u8; 32]).unwrap_err();
        dispatcher.boot_failure = Some(failure);

        assert!(dispatcher.recover_boot().unwrap());
        assert!(dispatcher.boot_failure.is_none());
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 40);
    }

    #[test]
    fn native_recovery_reports_abi_mismatch_and_can_exit_without_publishing_swis() {
        let mut inputs = vec![
            crate::boot::BootModuleInput {
                source_path: "modules/Boot.bas64",
                source: include_str!("../modules/Boot.bas64"),
                grants: &[],
            },
            crate::boot::BootModuleInput {
                source_path: "modules/System.bas64",
                source: include_str!("../modules/System.bas64"),
                grants: &["StartupPolicy", "SystemQueries", "SystemVariableStore"],
            },
            crate::boot::BootModuleInput {
                source_path: "modules/Console.bas64",
                source: include_str!("../modules/Console.bas64"),
                grants: &[
                    "ConsoleInput",
                    "ConsoleOutput",
                    "RuntimeErrors",
                    "GraphicsVduStream",
                ],
            },
        ];
        append_remaining_foundation_inputs(&mut inputs);
        let invalid_abi = BootCapsule::build(RUNTIME_ABI_VERSION + 1, &inputs).unwrap();
        let (input_sender, input_receiver) = mpsc::channel();
        for byte in b"Q\r" {
            input_sender.send(*byte).unwrap();
        }
        drop(input_sender);
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);

        let failure = dispatcher.bootstrap_capsule(&invalid_abi).unwrap_err();
        assert_eq!(failure.stage, BootStage::CapsuleValidation);
        dispatcher.boot_failure = Some(failure);
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);
        assert!(!dispatcher.recover_boot().unwrap());
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);
        let recovery_output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        let output = String::from_utf8_lossy(&recovery_output);
        assert!(output.contains("capsule runtime ABI 2"));
        assert!(output.contains("runtime ABI 1"));
    }

    #[test]
    fn native_recovery_missing_alternate_path_is_reported_without_guest_file_access() {
        let missing = std::env::temp_dir().join(format!(
            "ricochet-boot-missing-{}-{}.capsule",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_file(&missing);
        let (input_sender, input_receiver) = mpsc::channel();
        for byte in format!("A {}\rQ\r", missing.display()).bytes() {
            input_sender.send(byte).unwrap();
        }
        drop(input_sender);
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let failure = dispatcher.bootstrap_capsule(&[0_u8; 32]).unwrap_err();
        dispatcher.boot_failure = Some(failure);

        assert!(!dispatcher.recover_boot().unwrap());
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 0);
        let recovery_output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        let output = String::from_utf8_lossy(&recovery_output);
        assert!(output.contains("NativeCapsuleReadError"));
        assert!(output.contains("could not read selected capsule"));
    }

    fn put_word(block: &mut [u8], offset: usize, value: u32) {
        block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn get_word(task: &Task, address: u32) -> u32 {
        u32::from_le_bytes(
            task.memory
                .read_bytes(address, 4)
                .unwrap()
                .try_into()
                .unwrap(),
        )
    }

    fn initialise_wimp_task(dispatcher: &mut SwiDispatcher, task: &mut Task) -> u32 {
        const DESCRIPTION: u32 = 0x1000;
        task.memory
            .write_bytes(DESCRIPTION, b"Graphics routing test\0")
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[R0] = 310;
        context.registers[R1] = u32::from_le_bytes(*b"TASK");
        context.registers[R2] = DESCRIPTION;
        dispatcher
            .dispatch(WIMP_INITIALISE, task, &mut context)
            .unwrap();
        context.registers[R1]
    }

    fn create_wimp_window(
        dispatcher: &mut SwiDispatcher,
        task: &mut Task,
        block_address: u32,
        title: &str,
        area: WorkArea,
    ) -> u32 {
        let mut definition = [0_u8; 88];
        put_word(&mut definition, 0, area.min_x as u32);
        put_word(&mut definition, 4, area.min_y as u32);
        put_word(&mut definition, 8, area.max_x as u32);
        put_word(&mut definition, 12, area.max_y as u32);
        put_word(&mut definition, 28, 0xB600_0002);
        put_word(&mut definition, 40, 0);
        put_word(&mut definition, 44, (-512_i32) as u32);
        put_word(&mut definition, 48, 640);
        put_word(&mut definition, 52, 0);
        put_word(&mut definition, 56, 1);
        put_word(&mut definition, 60, 0);
        definition[72..72 + title.len()].copy_from_slice(title.as_bytes());
        task.memory.write_bytes(block_address, &definition).unwrap();
        let mut context = SwiContext::default();
        context.registers[R1] = block_address;
        dispatcher
            .dispatch(WIMP_CREATE_WINDOW, task, &mut context)
            .unwrap();
        let handle = context.registers[R0];

        let mut open = [0_u8; 32];
        put_word(&mut open, 0, handle);
        put_word(&mut open, 4, area.min_x as u32);
        put_word(&mut open, 8, area.min_y as u32);
        put_word(&mut open, 12, area.max_x as u32);
        put_word(&mut open, 16, area.max_y as u32);
        put_word(&mut open, 28, u32::MAX);
        task.memory
            .write_bytes(block_address + 0x100, &open)
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[R1] = block_address + 0x100;
        dispatcher
            .dispatch(WIMP_OPEN_WINDOW, task, &mut context)
            .unwrap();
        handle
    }

    fn paint_next_wimp_redraw(
        dispatcher: &mut SwiDispatcher,
        task: &mut Task,
        address: u32,
        character: u8,
    ) -> (u32, i32, i32, u32) {
        let mut poll = SwiContext::default();
        poll.registers[R1] = address;
        dispatcher.dispatch(WIMP_POLL, task, &mut poll).unwrap();
        assert_eq!(poll.registers[R0], 1);
        let mut redraw = SwiContext::default();
        redraw.registers[R1] = address;
        dispatcher
            .dispatch(WIMP_REDRAW_WINDOW, task, &mut redraw)
            .unwrap();
        let handle = get_word(task, address);
        let colour = u32::from(character - b'A' + 1);
        let mut painted_point = None;
        while redraw.registers[R0] != 0 {
            let packed_colour = 0x0102_0300 + colour;
            let mut set_gcol = SwiContext::default();
            set_gcol.registers[R0] = packed_colour;
            dispatcher
                .dispatch_named_swi("COLOURTRANS_SETGCOL", task, &mut set_gcol)
                .unwrap();
            assert_eq!(
                dispatcher.window_graphics[&handle]
                    .snapshot()
                    .graphics_colour,
                packed_colour,
                "named SetGCOL follows the active redraw window context"
            );
            for byte in [18, 0, colour as u8] {
                let mut output = SwiContext::default();
                output.registers[R0] = u32::from(byte);
                dispatcher.dispatch(OS_WRITE_C, task, &mut output).unwrap();
            }
            let clip = dispatcher
                .graphics()
                .snapshot()
                .wimp_clip
                .expect("Wimp redraw installs a graphics clip");
            let point = (clip.left + 1, clip.bottom + 1);
            let mut plot = SwiContext::default();
            plot.registers[R0] = 0x45;
            plot.registers[R1] = point.0 as u32;
            plot.registers[R2] = point.1 as u32;
            dispatcher.dispatch(OS_PLOT, task, &mut plot).unwrap();
            let mut read = SwiContext::default();
            read.registers[R0] = point.0 as u32;
            read.registers[R1] = point.1 as u32;
            dispatcher.dispatch(OS_READ_POINT, task, &mut read).unwrap();
            assert_eq!(
                (read.registers[R2], read.registers[R3], read.registers[R4]),
                (colour, 0, 0)
            );
            painted_point = Some(point);

            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(character);
            dispatcher.dispatch(OS_WRITE_C, task, &mut output).unwrap();
            let mut rectangle = SwiContext::default();
            rectangle.registers[R1] = address;
            dispatcher
                .dispatch(WIMP_GET_RECTANGLE, task, &mut rectangle)
                .unwrap();
            redraw.registers[R0] = rectangle.registers[R0];
        }
        let (x, y) = painted_point.expect("the Wimp redraw had a visible rectangle");
        (handle, x, y, colour)
    }

    fn dispatch_cli_line(
        dispatcher: &mut SwiDispatcher,
        task: &mut Task,
        line: &str,
    ) -> Result<(), RuntimeError> {
        task.memory
            .write_bytes(CLI_STRING_BUFFER, line.as_bytes())?;
        task.memory
            .write_byte(CLI_STRING_BUFFER + line.len() as u32, 0)?;
        let mut context = SwiContext::default();
        context.registers[R0] = CLI_STRING_BUFFER;
        dispatcher.dispatch(OS_CLI, task, &mut context)
    }

    #[test]
    fn console_swis_dispatch_through_interpreted_module_definitions() {
        let (input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(42);

        let mut write = SwiContext::default();
        write.registers[R0] = u32::from(b'A');
        dispatcher
            .dispatch(OS_WRITE_C, &mut task, &mut write)
            .unwrap();
        assert_eq!(
            dispatcher.last_dispatch_route(),
            Some(&SwiDispatchRoute::ModuleOwned {
                number: OS_WRITE_C,
                name: "OS_WriteC".into(),
                module: "Console".into(),
                definition: "WRITEC".into(),
                generation: 1,
                backend: InvocationBackend::Interpreter,
            })
        );
        assert!(matches!(
            display_receiver.try_recv().unwrap(),
            DisplayEvent::WriteByte { byte: b'A', .. }
        ));

        task.memory.write_bytes(0x2000, b"BASIC\0").unwrap();
        let mut write0 = SwiContext::default();
        write0.registers[R0] = 0x2000;
        dispatcher
            .dispatch(OS_WRITE_0, &mut task, &mut write0)
            .unwrap();
        assert_eq!(write0.registers[R0], 0x2006);
        assert!(
            matches!(dispatcher.last_dispatch_route(), Some(SwiDispatchRoute::ModuleOwned { name, definition, .. }) if name == "OS_Write0" && definition == "WRITE0")
        );

        task.memory.write_bytes(0x2100, b"X\0\0\0").unwrap();
        let mut write_s = SwiContext {
            pc: 0x2100,
            ..SwiContext::default()
        };
        dispatcher
            .dispatch(OS_WRITE_S, &mut task, &mut write_s)
            .unwrap();
        assert_eq!(write_s.pc, 0x2104);

        let mut newline = SwiContext::default();
        dispatcher
            .dispatch(OS_NEW_LINE, &mut task, &mut newline)
            .unwrap();
        assert!(
            matches!(dispatcher.last_dispatch_route(), Some(SwiDispatchRoute::ModuleOwned { name, .. }) if name == "OS_NewLine")
        );
        input_sender.send(0x1B).unwrap();
        let mut read = SwiContext::default();
        dispatcher
            .dispatch(OS_READ_C, &mut task, &mut read)
            .unwrap();
        assert_eq!(read.registers[R0], 0x1B);
        assert!(read.carry);
        assert_eq!(dispatcher.module_dispatch_count(), 5);
        assert_eq!(dispatcher.transitional_dispatch_count(), 0);
    }

    #[test]
    fn console_string_swi_enforces_the_declared_terminator_bound() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(44);
        let address = 0x3000;
        task.memory.write_bytes(address, &vec![b'X'; 4096]).unwrap();
        let mut write0 = SwiContext::default();
        write0.registers[R0] = address;

        let error = dispatcher
            .dispatch(OS_WRITE_0, &mut task, &mut write0)
            .unwrap_err();
        assert!(
            matches!(error, RuntimeError::Memory(crate::memory::MemoryError::MissingNullTerminator(found)) if found == address)
        );

        let mut write_s = SwiContext {
            pc: address,
            ..SwiContext::default()
        };
        let error = dispatcher
            .dispatch(OS_WRITE_S, &mut task, &mut write_s)
            .unwrap_err();
        assert!(
            matches!(error, RuntimeError::Memory(crate::memory::MemoryError::MissingNullTerminator(found)) if found == address)
        );
    }

    #[test]
    fn os_read_line_is_a_module_definition_with_checked_caller_memory() {
        let (input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(46);
        let buffer = 0x5000;
        task.memory.write_bytes(buffer, &[0; 16]).unwrap();
        for byte in [b'a', b'b', 8, b'c', 0x1B] {
            input_sender.send(byte).unwrap();
        }
        let mut context = SwiContext::default();
        context.registers[R0] = buffer;
        context.registers[R1] = 8;
        context.registers[R2] = 32;
        context.registers[R3] = 126;
        dispatcher
            .dispatch(OS_READ_LINE, &mut task, &mut context)
            .unwrap();
        assert_eq!(task.memory.read_bytes(buffer, 3).unwrap(), b"ac\0");
        assert_eq!(context.registers[R0], 0);
        assert_eq!(context.registers[R1], 2);
        assert!(context.carry);
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { name, definition, .. })
                if name == "OS_ReadLine" && definition == "READLINE"
        ));
        let written = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(written.contains(&b'a'));
        assert!(written.contains(&b'b'));
        assert!(written.contains(&b'c'));
        assert!(written.windows(3).any(|window| window == [8, b' ', 8]));
    }

    #[test]
    fn linked_module_proc_and_fn_imports_are_executable_and_scoped() {
        let provider_source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Counter 1.0.0\nREM @STATE COUNT UINT64\nREM @SWI Counter_Test &700 ProviderEntry\nREM @EXPORT PROC Increment\nREM @EXPORT FN ReadCount\nDEF PROC ProviderEntry\nENDPROC\nDEF PROC Increment\n    COUNT = COUNT + 1\nENDPROC\nDEF FN ReadCount() AS UINT64\n=COUNT\n";
        let consumer_source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE CounterClient 1.0.0\nREM @IMPORT_MODULE Counter 1.0.0\nREM @IMPORT_SYMBOL Counter PROC Increment\nREM @IMPORT_SYMBOL Counter FN ReadCount\nREM @STATE RESULT UINT64\nREM @STATE CHECK% UINT32\nREM @SWI CounterClient_Test &701 Entry\nDEF PROC Entry\n    PROC Counter.Increment\n    RESULT = FN Counter.ReadCount()\n    IF RESULT = 1 THEN CHECK% = 1\nENDPROC\n";
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        let allocator = dispatcher.module_registry.allocator();
        let provider =
            SystemModule::parse(provider_source, "modules/Counter.bas64", &allocator).unwrap();
        let provider = Arc::new(provider);
        let provider_id = dispatcher
            .module_registry
            .stage_module(provider.manifest.clone(), provider.definitions.clone())
            .unwrap();
        for definition in dispatcher
            .module_registry
            .module(provider_id)
            .unwrap()
            .definitions
            .values()
        {
            dispatcher
                .module_programs
                .insert(definition.id, Arc::clone(&provider));
        }
        dispatcher
            .module_registry
            .link_module(provider_id, BTreeSet::new())
            .unwrap();
        dispatcher
            .module_registry
            .publish_modules(&[provider_id])
            .unwrap();
        dispatcher
            .module_registry
            .start_module(provider_id, true)
            .unwrap();

        let consumer = SystemModule::parse(
            consumer_source,
            "modules/CounterClient.bas64",
            &dispatcher.module_registry.allocator(),
        )
        .unwrap();
        let consumer = Arc::new(consumer);
        let consumer_id = dispatcher
            .module_registry
            .stage_module(consumer.manifest.clone(), consumer.definitions.clone())
            .unwrap();
        for definition in dispatcher
            .module_registry
            .module(consumer_id)
            .unwrap()
            .definitions
            .values()
        {
            dispatcher
                .module_programs
                .insert(definition.id, Arc::clone(&consumer));
        }
        dispatcher
            .module_registry
            .link_module(consumer_id, BTreeSet::new())
            .unwrap();
        dispatcher
            .module_registry
            .publish_modules(&[consumer_id])
            .unwrap();
        dispatcher
            .module_registry
            .start_module(consumer_id, true)
            .unwrap();

        let mut task = Task::new(81);
        dispatcher
            .dispatch(0x701, &mut task, &mut SwiContext::default())
            .unwrap();
        assert_eq!(provider.test_read_workspace_number("COUNT"), Some(1.0));
        assert_eq!(consumer.test_read_workspace_number("RESULT"), Some(1.0));
        assert_eq!(consumer.test_read_workspace_number("CHECK%"), Some(1.0));
    }

    #[test]
    fn opaque_resource_handles_check_identity_owner_and_rights_when_used() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE ResourceReader 1.0.0\nREM @CAPABILITY ResourceRead\nREM @IMPORT Host.Resource.ReadByte ResourceRead\nREM @SWI Resource_Read &702 ReadByteAt REGISTERS=R0:HANDLE<BufferHandle>:IN|R1:U32:IN|R2:U32:OUT\nHANDLE BufferHandle\nDEF PROC ReadByteAt\n    PRIMITIVE Host.Resource.ReadByte, R0%, R1% TO BYTE%\n    R2% = BYTE%\nENDPROC\n";
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        let module = SystemModule::parse(
            source,
            "modules/ResourceReader.bas64",
            &dispatcher.module_registry.allocator(),
        )
        .unwrap();
        module
            .validate_primitive_shapes(&dispatcher.module_registry.primitives)
            .unwrap();
        let module = Arc::new(module);
        let module_id = dispatcher
            .module_registry
            .stage_module(module.manifest.clone(), module.definitions.clone())
            .unwrap();
        for definition in dispatcher
            .module_registry
            .module(module_id)
            .unwrap()
            .definitions
            .values()
        {
            dispatcher
                .module_programs
                .insert(definition.id, Arc::clone(&module));
        }
        dispatcher
            .module_registry
            .link_module(
                module_id,
                BTreeSet::from([
                    CapabilityName::new("ResourceRead").expect("test capability is valid")
                ]),
            )
            .unwrap();
        dispatcher
            .module_registry
            .publish_modules(&[module_id])
            .unwrap();
        dispatcher
            .module_registry
            .start_module(module_id, true)
            .unwrap();

        let mut task = Task::new(82);
        let handle = dispatcher
            .register_resource_buffer("BufferHandle", task.id, vec![0x5A], [ResourceRight::Read])
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[0] = handle;
        dispatcher.dispatch(0x702, &mut task, &mut context).unwrap();
        assert_eq!(context.registers[2], 0x5A);

        let no_rights = dispatcher
            .register_resource_buffer("BufferHandle", task.id, vec![0x2A], [])
            .unwrap();
        let mut denied = SwiContext::default();
        denied.registers[0] = no_rights;
        let error = dispatcher
            .dispatch(0x702, &mut task, &mut denied)
            .unwrap_err();
        assert!(error.to_string().contains("lacks Read authority"));

        let foreign = dispatcher
            .register_resource_buffer(
                "BufferHandle",
                task.id + 1,
                vec![0x2A],
                [ResourceRight::Read],
            )
            .unwrap();
        let mut wrong_owner = SwiContext::default();
        wrong_owner.registers[0] = foreign;
        let error = dispatcher
            .dispatch(0x702, &mut task, &mut wrong_owner)
            .unwrap_err();
        assert!(error.to_string().contains("different caller task"));

        let mut forged = SwiContext::default();
        forged.registers[0] = u32::MAX;
        let error = dispatcher
            .dispatch(0x702, &mut task, &mut forged)
            .unwrap_err();
        assert!(error.to_string().contains("not registered"));
    }

    #[test]
    fn os_read_line_preserves_eof_and_range_echo_contracts() {
        let (input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(48);
        let buffer = 0x5200;
        input_sender.send(b'a').unwrap();
        input_sender.send(b'3').unwrap();
        input_sender.send(b'4').unwrap();
        input_sender.send(b'5').unwrap();
        input_sender.send(b'\r').unwrap();
        drop(input_sender);
        let mut context = SwiContext::default();
        context.registers[R0] = buffer | (1 << 31);
        context.registers[R1] = 2;
        context.registers[R2] = u32::from(b'0');
        context.registers[R3] = u32::from(b'9');
        dispatcher
            .dispatch(OS_READ_LINE, &mut task, &mut context)
            .unwrap();
        assert_eq!(task.memory.read_bytes(buffer, 2).unwrap(), b"34");
        assert_eq!(context.registers[R1], 2);
        assert!(!context.carry);
        let output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(output.contains(&b'3'));
        assert!(output.contains(&b'4'));
        assert!(output.contains(&7));
        assert!(!output.contains(&b'a'));
        assert!(!output.contains(&b'5'));

        let (eof_sender, eof_receiver) = mpsc::channel();
        let (eof_display_sender, eof_display_receiver) = mpsc::channel();
        let mut eof_dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(eof_receiver), eof_display_sender);
        let mut eof_task = Task::new(49);
        eof_sender.send(b'x').unwrap();
        drop(eof_sender);
        let mut eof_context = SwiContext::default();
        eof_context.registers[R0] = 0x5300;
        eof_context.registers[R1] = 4;
        eof_context.registers[R2] = 32;
        eof_context.registers[R3] = 126;
        eof_dispatcher
            .dispatch(OS_READ_LINE, &mut eof_task, &mut eof_context)
            .unwrap();
        assert_eq!(eof_task.memory.read_byte(0x5300).unwrap(), b'x');
        assert_eq!(eof_context.registers[R1], 1);
        assert!(!eof_context.carry);
        let eof_output = eof_display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(eof_output, [b'x']);
    }

    #[test]
    fn os_read_line_keeps_legacy_range_clamping_and_control_d_behavior() {
        let (range_sender, range_receiver) = mpsc::channel();
        let (range_display_sender, _range_display_receiver) = mpsc::channel();
        let mut range_dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(range_receiver), range_display_sender);
        let mut range_task = Task::new(51);
        range_sender.send(1).unwrap();
        range_sender.send(b'\r').unwrap();
        let mut range_context = SwiContext::default();
        range_context.registers[R0] = 0x5400;
        range_context.registers[R1] = 4;
        range_context.registers[R2] = 0x101;
        range_context.registers[R3] = 0x1FF;
        range_dispatcher
            .dispatch(OS_READ_LINE, &mut range_task, &mut range_context)
            .unwrap();
        assert_eq!(range_context.registers[R1], 0);

        let (control_sender, control_receiver) = mpsc::channel();
        let (control_display_sender, _control_display_receiver) = mpsc::channel();
        let mut control_dispatcher = SwiDispatcher::windowed(
            HostConsole::windowed(control_receiver),
            control_display_sender,
        );
        let mut control_task = Task::new(52);
        control_sender.send(b'a').unwrap();
        control_sender.send(4).unwrap();
        control_sender.send(b'\r').unwrap();
        let mut control_context = SwiContext::default();
        control_context.registers[R0] = 0x5500;
        control_context.registers[R1] = 4;
        control_context.registers[R2] = 0;
        control_context.registers[R3] = 255;
        control_dispatcher
            .dispatch(OS_READ_LINE, &mut control_task, &mut control_context)
            .unwrap();
        assert_eq!(control_context.registers[R1], 2);
        assert_eq!(
            control_task.memory.read_bytes(0x5500, 2).unwrap(),
            [b'a', 4]
        );
    }

    #[test]
    fn os_read_line_substitutes_r4_for_echo_without_changing_buffered_input() {
        let (input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(53);
        let buffer = 0x5600;
        input_sender.send(b'a').unwrap();
        input_sender.send(b'\r').unwrap();
        let mut context = SwiContext::default();
        context.registers[R0] = buffer | READ_LINE_ECHO_R4;
        context.registers[R1] = 4;
        context.registers[R2] = 0;
        context.registers[R3] = 255;
        context.registers[R4] = u32::from(b'*');

        dispatcher
            .dispatch(OS_READ_LINE, &mut task, &mut context)
            .unwrap();

        assert_eq!(task.memory.read_byte(buffer).unwrap(), b'a');
        assert_eq!(context.registers[R1], 1);
        let output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(output.contains(&b'*'));
        assert!(!output.contains(&b'a'));
    }

    #[test]
    fn os_read_line_reports_caller_memory_boundary_failures() {
        let (input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(50);
        input_sender.send(b'a').unwrap();
        input_sender.send(b'b').unwrap();
        let buffer = crate::memory::GUEST_MEMORY_BASE
            + u32::try_from(crate::memory::GUEST_MEMORY_SIZE + crate::memory::SWI_ERROR_BLOCK_SIZE)
                .unwrap()
            - 1;
        let mut context = SwiContext::default();
        context.registers[R0] = buffer;
        context.registers[R1] = 2;
        context.registers[R2] = 32;
        context.registers[R3] = 126;
        let error = dispatcher
            .dispatch(OS_READ_LINE, &mut task, &mut context)
            .unwrap_err();
        assert!(matches!(error, RuntimeError::Memory(_)));
        assert_eq!(task.memory.read_byte(buffer).unwrap(), b'a');
    }

    #[test]
    fn internal_module_manager_runs_console_quiesce_and_finalise_hooks() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(HostConsole::windowed(input_receiver));
        let console_definition = dispatcher
            .module_registry()
            .current_swi_definition(OS_WRITE_C)
            .expect("Console publishes OS_WriteC");
        let console = dispatcher.module_programs[&console_definition.id].clone();
        assert_eq!(console.workspace_number("STARTCOUNT%"), Some(1.0));
        let mut task = Task::new(47);
        dispatcher
            .basic64_module_manager()
            .quiesce("Console", &mut task)
            .unwrap();
        dispatcher
            .basic64_module_manager()
            .retire("Console", &mut task)
            .unwrap();
        assert_eq!(console.workspace_number("QUIESCECOUNT%"), Some(1.0));
        assert_eq!(console.workspace_number("FINALISECOUNT%"), Some(1.0));
        // The current capsule publishes forty exports; retiring Console
        // removes its six character SWIs.
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 40 - 6);
        assert!(
            dispatcher
                .module_registry()
                .acquire_swi(OS_WRITE_C)
                .is_none()
        );
    }

    #[test]
    fn failing_quiesce_restores_active_state_workspace_and_swi_admission() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let original = include_str!("../modules/Console.bas64");
        let failing = original
            .replace(
                "DEF PROC Start\n",
                "ERROR LifecycleFailure\n    Code AS UINT32 READONLY\n    Message AS STRING READONLY\nEND ERROR\n\nDEF PROC Start\n",
            )
            .replace(
                "DEF PROC Quiesce\n    QUIESCECOUNT% = QUIESCECOUNT% + 1\nENDPROC",
                "DEF PROC Quiesce THROWS LifecycleFailure\n    QUIESCECOUNT% = QUIESCECOUNT% + 1\n    THROW LifecycleFailure, 99, \"quiesce denied\"\nENDPROC",
            );
        assert_ne!(failing, original);
        dispatcher
            .basic64_module_manager()
            .replace_swi_definition(
                OS_WRITE_C,
                &failing,
                "modules/Console.quiesce-failure.bas64",
            )
            .unwrap();

        let current = dispatcher
            .module_registry()
            .current_swi_definition(OS_WRITE_C)
            .unwrap();
        let module_program = dispatcher.module_programs[&current.id].clone();
        let mut task = Task::new(53);
        let error = dispatcher
            .basic64_module_manager()
            .quiesce("Console", &mut task)
            .unwrap_err();
        assert!(error.to_string().contains("quiesce denied"));
        assert_eq!(module_program.workspace_number("QUIESCECOUNT%"), Some(0.0));
        assert_eq!(
            dispatcher
                .module_registry()
                .module_named("Console")
                .unwrap()
                .state,
            crate::ricochet::ModuleState::Active
        );
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 40);
        assert!(
            dispatcher
                .module_registry()
                .acquire_swi(OS_WRITE_C)
                .is_some()
        );

        let mut context = SwiContext::default();
        context.registers[R0] = u32::from(b'Q');
        dispatcher
            .dispatch(OS_WRITE_C, &mut task, &mut context)
            .unwrap();
        assert!(
            display_receiver
                .try_iter()
                .any(|event| matches!(event, DisplayEvent::WriteByte { byte: b'Q', .. }))
        );

        dispatcher
            .basic64_module_manager()
            .replace_swi_definition(OS_WRITE_C, original, "modules/Console.bas64")
            .unwrap();
        dispatcher
            .basic64_module_manager()
            .quiesce("Console", &mut task)
            .unwrap();
        assert_eq!(module_program.workspace_number("QUIESCECOUNT%"), Some(1.0));
    }

    #[test]
    fn failing_finalise_keeps_quiesced_module_retryable_without_partial_cleanup() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let mut dispatcher = SwiDispatcher::new(HostConsole::windowed(input_receiver));
        let original = include_str!("../modules/Console.bas64");
        let failing = original
            .replace(
                "DEF PROC Start\n",
                "ERROR LifecycleFailure\n    Code AS UINT32 READONLY\n    Message AS STRING READONLY\nEND ERROR\n\nDEF PROC Start\n",
            )
            .replace(
                "DEF PROC Finalise\n    FINALISECOUNT% = FINALISECOUNT% + 1\nENDPROC",
                "DEF PROC Finalise THROWS LifecycleFailure\n    FINALISECOUNT% = FINALISECOUNT% + 1\n    THROW LifecycleFailure, 99, \"finalise denied\"\nENDPROC",
            );
        dispatcher
            .basic64_module_manager()
            .replace_swi_definition(
                OS_WRITE_C,
                &failing,
                "modules/Console.finalise-failure.bas64",
            )
            .unwrap();

        let current = dispatcher
            .module_registry()
            .current_swi_definition(OS_WRITE_C)
            .unwrap();
        let module_program = dispatcher.module_programs[&current.id].clone();
        let mut task = Task::new(54);
        let error = dispatcher
            .basic64_module_manager()
            .retire("Console", &mut task)
            .unwrap_err();
        assert!(error.to_string().contains("must be quiesced"));
        assert_eq!(module_program.workspace_number("FINALISECOUNT%"), Some(0.0));
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 40);
        assert_eq!(
            dispatcher
                .module_registry()
                .module_named("Console")
                .unwrap()
                .state,
            ModuleState::Active
        );
        dispatcher
            .basic64_module_manager()
            .quiesce("Console", &mut task)
            .unwrap();
        let error = dispatcher
            .basic64_module_manager()
            .retire("Console", &mut task)
            .unwrap_err();
        assert!(error.to_string().contains("finalise denied"));
        assert_eq!(module_program.workspace_number("FINALISECOUNT%"), Some(0.0));
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 40);
        assert!(
            dispatcher
                .module_registry()
                .acquire_swi(OS_WRITE_C)
                .is_none()
        );
        assert_eq!(
            dispatcher
                .module_registry()
                .module_named("Console")
                .unwrap()
                .state,
            crate::ricochet::ModuleState::Quiescing
        );
        assert!(
            dispatcher
                .module_programs
                .values()
                .any(|program| program.manifest.source_hash == module_program.manifest.source_hash)
        );

        dispatcher
            .basic64_module_manager()
            .replace_swi_definition(OS_WRITE_C, original, "modules/Console.bas64")
            .unwrap();
        dispatcher
            .basic64_module_manager()
            .retire("Console", &mut task)
            .unwrap();
        assert_eq!(module_program.workspace_number("FINALISECOUNT%"), Some(1.0));
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 40 - 6);
        assert!(
            dispatcher
                .module_programs
                .values()
                .all(|program| { !program.manifest.name.eq_ignore_ascii_case("Console") })
        );
    }

    #[test]
    fn guest_primitive_invocation_is_rejected_outside_an_active_module() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(45);
        let mut context = SwiContext::default();
        context.registers[R0] = u32::from(b'X');

        let error = dispatcher
            .call_module_primitive("Host.Console.WriteByte", &mut task, &mut context)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("outside a BASIC64 module invocation")
        );
    }

    #[test]
    fn console_definition_replacement_keeps_the_entry_cell_and_rejects_bad_contracts() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(43);
        let (_other_sender, other_receiver) = mpsc::channel();
        let other_dispatcher = SwiDispatcher::new(HostConsole::windowed(other_receiver));
        let foreign_authority = other_dispatcher.module_management_authority().clone();
        assert!(
            dispatcher
                .replace_basic64_swi(
                    &foreign_authority,
                    OS_WRITE_C,
                    include_str!("../modules/Console.bas64"),
                    "modules/Console.foreign.bas64",
                )
                .is_err()
        );
        let entry_id = dispatcher
            .module_registry()
            .swi_entry_id(OS_WRITE_C)
            .unwrap();
        let derived_identity = dispatcher
            .module_registry()
            .derived_target_identity(OS_WRITE_C)
            .unwrap();
        dispatcher
            .derived_targets
            .insert(derived_identity.clone(), Arc::new(vec![0xCA, 0xFE]));
        let (_, old_call) = dispatcher
            .module_registry()
            .acquire_swi(OS_WRITE_C)
            .unwrap();
        let old_generation = old_call.generation_id();

        let original = include_str!("../modules/Console.bas64");
        let replacement = original.replace(
            "    PROC EmitByte(R0% AND &FF)",
            "    PROC EmitByte(R0% AND &FF): PROC EmitByte(33)",
        );
        let next_generation = dispatcher
            .basic64_module_manager()
            .replace_swi_definition(
                OS_WRITE_C,
                &replacement,
                "modules/Console.replacement.bas64",
            )
            .unwrap();
        assert!(dispatcher.derived_targets.is_empty());
        assert!(old_call.retired());
        assert_ne!(old_generation, next_generation);
        let replacement_identity = dispatcher
            .module_registry()
            .derived_target_identity(OS_WRITE_C)
            .unwrap();
        let replacement_definition = dispatcher
            .module_registry()
            .current_swi_definition(OS_WRITE_C)
            .unwrap();
        assert_eq!(
            dispatcher.module_programs[&replacement_definition.id].workspace_number("STARTCOUNT%"),
            Some(1.0)
        );
        assert_ne!(
            replacement_identity.source_hash,
            derived_identity.source_hash
        );
        assert_eq!(
            replacement_identity.source_path,
            "modules/Console.replacement.bas64"
        );
        assert_eq!(
            dispatcher
                .module_registry()
                .module_named("Console")
                .unwrap()
                .manifest
                .source_path,
            "modules/Console.replacement.bas64"
        );
        assert_eq!(
            dispatcher.module_registry().swi_entry_id(OS_WRITE_C),
            Some(entry_id)
        );

        let mut output = SwiContext::default();
        output.registers[R0] = u32::from(b'B');
        dispatcher
            .dispatch(OS_WRITE_C, &mut task, &mut output)
            .unwrap();
        let emitted = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(emitted, [b'B', b'!']);
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { generation: 2, .. })
        ));

        let bad_contract = replacement.replace("REGISTERS=R0:U32:IN", "REGISTERS=R0:BYTE:OUT");
        assert!(
            dispatcher
                .basic64_module_manager()
                .replace_swi_definition(OS_WRITE_C, &bad_contract, "modules/Console.bad.bas64")
                .is_err()
        );
        assert!(matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { generation: 2, .. })
        ));

        drop(old_call);
        assert_eq!(
            dispatcher
                .module_registry()
                .swi_retired_generation_count(OS_WRITE_C),
            Some(0)
        );
        dispatcher
            .basic64_module_manager()
            .replace_swi_definition(OS_WRITE_C, original, "modules/Console.bas64")
            .unwrap();
        assert!(
            !dispatcher
                .module_programs
                .contains_key(&derived_identity.definition),
            "a retired source definition is released after its old lease drains"
        );
        let restored_identity = dispatcher
            .module_registry()
            .derived_target_identity(OS_WRITE_C)
            .unwrap();
        let restored_definition = dispatcher
            .module_registry()
            .current_swi_definition(OS_WRITE_C)
            .unwrap();
        assert_eq!(
            dispatcher.module_programs[&restored_definition.id].workspace_number("STARTCOUNT%"),
            Some(1.0)
        );
        assert_eq!(restored_identity.source_path, "modules/Console.bas64");
        assert_eq!(
            dispatcher
                .module_registry()
                .module_named("Console")
                .unwrap()
                .manifest
                .source_path,
            "modules/Console.bas64"
        );
        assert_ne!(
            restored_identity.source_hash,
            replacement_identity.source_hash
        );
        assert_eq!(
            dispatcher.module_registry().swi_entry_id(OS_WRITE_C),
            Some(entry_id)
        );
        dispatcher
            .basic64_module_manager()
            .quiesce("Console", &mut task)
            .unwrap();
        dispatcher
            .basic64_module_manager()
            .retire("Console", &mut task)
            .unwrap();
        assert!(
            !dispatcher
                .module_programs
                .values()
                .any(|program| program.manifest.name.eq_ignore_ascii_case("Console"))
        );
    }

    #[test]
    fn configure_accepts_supported_values_and_conf_abbreviation() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let config_path =
            std::env::temp_dir().join(format!("ricochet-configure-cli-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&config_path);
        dispatcher.configure = ConfigureStore::with_path(&config_path);
        let mut task = Task::trusted_configuration_manager(1);

        dispatch_cli_line(&mut dispatcher, &mut task, "*CONFIGURE BASICEngine Strict").unwrap();
        assert_eq!(
            dispatcher.configure.load().unwrap().engine,
            BasicEngine::StrictJit
        );

        dispatch_cli_line(&mut dispatcher, &mut task, "*CONF. BASICEngine Hybrid").unwrap();
        assert_eq!(
            dispatcher.configure.load().unwrap().engine,
            BasicEngine::HybridJit
        );

        dispatch_cli_line(&mut dispatcher, &mut task, "*CONFIGURE Language 3").unwrap();
        assert_eq!(
            dispatcher
                .configure
                .load()
                .unwrap()
                .status_value("Language")
                .unwrap()
                .1,
            "3"
        );
        let _ = display_receiver.try_iter().count();
        dispatch_cli_line(&mut dispatcher, &mut task, "*STATUS Language").unwrap();
        let status_output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(String::from_utf8_lossy(&status_output).contains("Language=3"));

        for value in ["HybridJIT", "Hybrid-JIT", "StrictJIT", "Strict-JIT"] {
            let command = format!("*CONFIGURE BASICEngine {value}");
            dispatch_cli_line(&mut dispatcher, &mut task, &command).unwrap();
            assert_eq!(
                dispatcher.configure.load().unwrap().engine,
                BasicEngine::HybridJit,
                "accepted {value}"
            );
        }
        let _ = std::fs::remove_file(config_path);
    }

    #[test]
    fn ricochet_display_swi_queries_applies_and_reports_save_failure_in_registers() {
        let path =
            std::env::temp_dir().join(format!("ricochet-display-swi-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let (display_sender, _display_receiver) = mpsc::channel();
        let wimp = WimpServer::new(mpsc::channel().0);
        wimp.set_configure_store(ConfigureStore::with_path(&path), DisplaySettings::default());
        let mut dispatcher = SwiDispatcher::desktop_task(
            HostConsole::windowed(mpsc::channel().1),
            display_sender,
            77,
            wimp.clone(),
        );
        let mut task = Task::new(77);
        let mut query = SwiContext::default();
        query.registers[R0] = RICOCHET_DISPLAY_ABI_VERSION;
        query.registers[R1] = RICOCHET_DISPLAY_QUERY;
        dispatcher
            .dispatch_named_swi("RICOCHET_DISPLAY", &mut task, &mut query)
            .unwrap();
        assert_eq!(query.registers[R2], DesktopResolution::Window.id());
        assert_eq!(query.registers[R3], DisplayColour::Rgb888.id());
        assert_eq!((query.registers[R4], query.registers[R5]), (800, 600));
        assert_eq!((query.registers[R6], query.registers[R7]), (800, 600));

        let mut denied_apply = SwiContext::default();
        denied_apply.registers[R0] = RICOCHET_DISPLAY_ABI_VERSION;
        denied_apply.registers[R1] = RICOCHET_DISPLAY_APPLY;
        denied_apply.registers[R2] = DesktopResolution::R640x480.id();
        denied_apply.registers[R3] = DisplayColour::Rgb555.id();
        assert!(matches!(
            dispatcher.dispatch_named_swi("RICOCHET_DISPLAY", &mut task, &mut denied_apply),
            Err(RuntimeError::Structured { type_name, code: 4, ref message })
                if type_name == "TaskAuthorizationDenied"
                    && message == "caller task lacks configuration-write authority"
        ));
        assert_eq!(wimp.display_settings(), DisplaySettings::default());
        assert_eq!(
            ConfigureStore::with_path(&path).load().unwrap().display,
            DisplaySettings::default()
        );

        let mut task = Task::trusted_mos_session(77);

        let mut apply = SwiContext::default();
        apply.registers[R0] = RICOCHET_DISPLAY_ABI_VERSION;
        apply.registers[R1] = RICOCHET_DISPLAY_APPLY;
        apply.registers[R2] = DesktopResolution::R640x480.id();
        apply.registers[R3] = DisplayColour::Rgb555.id();
        dispatcher
            .dispatch_named_swi("RICOCHET_DISPLAY", &mut task, &mut apply)
            .unwrap();
        assert_eq!(apply.registers[R8], 0);
        assert_eq!((apply.registers[R4], apply.registers[R5]), (640, 480));
        assert_eq!(
            ConfigureStore::with_path(&path).load().unwrap().display,
            DisplaySettings {
                resolution: DesktopResolution::R640x480,
                colour: DisplayColour::Rgb555,
            }
        );

        let mut invalid_version = SwiContext::default();
        invalid_version.registers[R0] = 2;
        invalid_version.registers[R1] = RICOCHET_DISPLAY_QUERY;
        assert!(
            dispatcher
                .dispatch_named_swi("RICOCHET_DISPLAY", &mut task, &mut invalid_version)
                .is_err()
        );
        let _ = std::fs::remove_file(path);

        #[cfg(target_os = "linux")]
        {
            let (failed_display_sender, _failed_display_receiver) = mpsc::channel();
            let failed_wimp = WimpServer::new(mpsc::channel().0);
            failed_wimp.set_configure_store(
                ConfigureStore::with_path("/proc/self/ricochet-display-test/configure"),
                DisplaySettings::default(),
            );
            let mut failed_dispatcher = SwiDispatcher::desktop_task(
                HostConsole::windowed(mpsc::channel().1),
                failed_display_sender,
                78,
                failed_wimp.clone(),
            );
            let mut failed_apply = SwiContext::default();
            failed_apply.registers[R0] = RICOCHET_DISPLAY_ABI_VERSION;
            failed_apply.registers[R1] = RICOCHET_DISPLAY_APPLY;
            failed_apply.registers[R2] = DesktopResolution::R640x480.id();
            failed_apply.registers[R3] = DisplayColour::Grey4.id();
            failed_dispatcher
                .dispatch_named_swi(
                    "RICOCHET_DISPLAY",
                    &mut Task::trusted_mos_session(78),
                    &mut failed_apply,
                )
                .unwrap();
            assert_eq!(failed_apply.registers[R8], 1);
            assert_eq!(failed_wimp.display_settings(), DisplaySettings::default());
        }
    }

    #[test]
    fn pre_handoff_display_apply_shares_latched_configuration_recovery() {
        let path = std::env::temp_dir().join(format!(
            "ricochet-prehandoff-recovery-{}.configure",
            std::process::id()
        ));
        let original = b"# Ricochet MOS configuration v3\nLanguage=3\n";
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, original).unwrap();

        let (display_sender, _display_receiver) = mpsc::channel();
        let wimp = WimpServer::new(mpsc::channel().0);
        // A desktop host can prepare its configuration store before creating
        // the command dispatcher. The constructor must adopt that same store
        // rather than leaving pre-handoff RICOCHET_DISPLAY on a fresh fallback.
        wimp.bind_configure_store(ConfigureStore::with_path(&path));
        let mut dispatcher = SwiDispatcher::windowed_with_desktop(
            HostConsole::windowed(mpsc::channel().1),
            display_sender,
            Arc::clone(&wimp),
        );
        assert_eq!(
            dispatcher.load_basic_configuration().unwrap(),
            BasicConfiguration::default(),
            "malformed partial v3 input must latch safe defaults"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let mut apply = SwiContext::default();
        apply.registers[R0] = RICOCHET_DISPLAY_ABI_VERSION;
        apply.registers[R1] = RICOCHET_DISPLAY_APPLY;
        apply.registers[R2] = DesktopResolution::R640x480.id();
        apply.registers[R3] = DisplayColour::Colour16.id();
        dispatcher
            .dispatch_named_swi(
                "RICOCHET_DISPLAY",
                &mut Task::trusted_mos_session(0xD15A),
                &mut apply,
            )
            .unwrap();
        assert_eq!(apply.registers[R8], 0);

        let applied = DisplaySettings {
            resolution: DesktopResolution::R640x480,
            colour: DisplayColour::Colour16,
        };
        assert_eq!(wimp.display_settings(), applied);
        assert_eq!(
            dispatcher.load_basic_configuration().unwrap().display,
            applied
        );
        assert_eq!(
            ConfigureStore::with_path(&path).load().unwrap().display,
            applied
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# Ricochet MOS configuration v3\nLanguage=0\nBASICMode=AUTO\nBASICProfile=AUTO\nBASICTarget=AUTO\nBASICEngine=INTERPRETER\nWimpMode=X640 Y480 C16\n"
        );

        // The later Language 3 handoff must consume the same repaired store,
        // rather than reapplying the pre-recovery defaults over this display.
        let stop_wimp = Arc::clone(&wimp);
        let stop_thread = std::thread::spawn(move || stop_wimp.stop());
        dispatcher.begin_desktop().unwrap();
        stop_thread.join().unwrap();
        assert_eq!(wimp.display_settings(), applied);
        let _ = std::fs::remove_file(&path);
        let backup_prefix = format!("{}.recovery-", path.file_name().unwrap().to_string_lossy());
        if let Some(parent) = path.parent()
            && let Ok(entries) = std::fs::read_dir(parent)
        {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(&backup_prefix)
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    #[test]
    fn os_read_point_reads_immediate_pixels_and_preserves_coordinates() {
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        let mut task = Task::new(1);
        for byte in [22_u8, 2, 18, 0, 2] {
            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(byte);
            dispatcher
                .dispatch(OS_WRITE_C, &mut task, &mut output)
                .unwrap();
        }
        assert_eq!(dispatcher.graphics().snapshot().graphics_colour, 2);
        for byte in [29_u8, 5, 0, 7, 0] {
            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(byte);
            dispatcher
                .dispatch(OS_WRITE_C, &mut task, &mut output)
                .unwrap();
        }
        let mut plot = SwiContext::default();
        plot.registers[R0] = 0x45;
        plot.registers[R1] = 10;
        plot.registers[R2] = 20;
        dispatcher.dispatch(OS_PLOT, &mut task, &mut plot).unwrap();
        assert!(dispatcher.graphics().snapshot().graphics_content_present);
        let mut read = SwiContext::default();
        read.registers[R0] = 10;
        read.registers[R1] = 20;
        dispatcher
            .dispatch_named_swi("OS_READPOINT", &mut task, &mut read)
            .unwrap();
        assert_eq!((read.registers[R0], read.registers[R1]), (10, 20));
        assert_eq!(
            (read.registers[R2], read.registers[R3], read.registers[R4]),
            (2, 0, 0)
        );

        let mut off_screen = SwiContext::default();
        off_screen.registers[R0] = (-10_000_i32) as u32;
        off_screen.registers[R1] = 20;
        dispatcher
            .dispatch(OS_READ_POINT, &mut task, &mut off_screen)
            .unwrap();
        assert_eq!(off_screen.registers[R0], (-10_000_i32) as u32);
        assert_eq!(off_screen.registers[R1], 20);
        assert_eq!(off_screen.registers[R2], u32::MAX);
        assert_eq!(off_screen.registers[R4], u32::MAX);
    }

    #[test]
    fn graphics_swis_isolate_non_display_task_defaults_in_desktop_mode() {
        let (display_sender, _display_receiver) = mpsc::channel();
        let wimp = WimpServer::new(mpsc::channel().0);
        let mut dispatcher = SwiDispatcher::desktop_task(
            HostConsole::windowed(mpsc::channel().1),
            display_sender,
            77,
            wimp,
        );
        let mut display_task = Task::new(77);
        let mut guest_task = Task::new(78);

        for byte in [22_u8, 0, 18, 0, 3] {
            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(byte);
            dispatcher
                .dispatch(OS_WRITE_C, &mut display_task, &mut output)
                .unwrap();
        }
        let mut display_plot = SwiContext::default();
        display_plot.registers[R0] = 0x45;
        display_plot.registers[R1] = 40;
        display_plot.registers[R2] = 40;
        dispatcher
            .dispatch(OS_PLOT, &mut display_task, &mut display_plot)
            .unwrap();

        let mut guest_read = SwiContext::default();
        guest_read.registers[R0] = 40;
        guest_read.registers[R1] = 40;
        dispatcher
            .dispatch(OS_READ_POINT, &mut guest_task, &mut guest_read)
            .unwrap();
        assert_eq!(guest_read.registers[R2], 0);

        let mut guest_plot = SwiContext::default();
        guest_plot.registers[R0] = 0x45;
        guest_plot.registers[R1] = 48;
        guest_plot.registers[R2] = 48;
        dispatcher
            .dispatch(OS_PLOT, &mut guest_task, &mut guest_plot)
            .unwrap();
        let mut display_read = SwiContext::default();
        display_read.registers[R0] = 48;
        display_read.registers[R1] = 48;
        dispatcher
            .dispatch(OS_READ_POINT, &mut display_task, &mut display_read)
            .unwrap();
        assert_eq!(display_read.registers[R2], 0);
    }

    #[test]
    fn wimp_redraw_routes_to_independent_window_surfaces_and_restores_task_default() {
        let (input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let (desktop_sender, _desktop_receiver) = mpsc::channel();
        let wimp = WimpServer::new(desktop_sender);
        let mut dispatcher = SwiDispatcher::desktop_task(
            HostConsole::windowed(input_receiver),
            display_sender,
            77,
            wimp,
        );
        let mut task = Task::new(77);
        for byte in [22_u8, 2] {
            let mut output = SwiContext::default();
            output.registers[R0] = u32::from(byte);
            dispatcher
                .dispatch(OS_WRITE_C, &mut task, &mut output)
                .unwrap();
        }
        let wimp_task_handle = initialise_wimp_task(&mut dispatcher, &mut task);

        let first = create_wimp_window(
            &mut dispatcher,
            &mut task,
            0x2000,
            "First",
            WorkArea {
                min_x: 100,
                min_y: 400,
                max_x: 600,
                max_y: 900,
            },
        );
        let second = create_wimp_window(
            &mut dispatcher,
            &mut task,
            0x2200,
            "Second",
            WorkArea {
                min_x: 700,
                min_y: 400,
                max_x: 1200,
                max_y: 900,
            },
        );

        let (first_painted, first_x, first_y, first_colour) =
            paint_next_wimp_redraw(&mut dispatcher, &mut task, 0x2400, b'A');
        let (second_painted, second_x, second_y, second_colour) =
            paint_next_wimp_redraw(&mut dispatcher, &mut task, 0x2600, b'B');
        assert_eq!([first_painted, second_painted], [first, second]);
        assert_eq!(
            dispatcher.window_graphics[&first].read_point(first_x, first_y),
            Some((first_colour, 0))
        );
        assert_eq!(
            dispatcher.window_graphics[&second].read_point(second_x, second_y),
            Some((second_colour, 0))
        );
        assert_eq!(dispatcher.active_graphics_window, None);
        assert_eq!(dispatcher.graphics().snapshot().text_cells[0], b' ');
        assert_eq!(
            dispatcher.window_graphics[&first].snapshot().text_cells[0],
            b'A'
        );
        assert_eq!(
            dispatcher.window_graphics[&second].snapshot().text_cells[0],
            b'B'
        );

        let mut update_block = [0_u8; 44];
        put_word(&mut update_block, 0, first);
        put_word(&mut update_block, 4, 0);
        put_word(&mut update_block, 8, (-512_i32) as u32);
        put_word(&mut update_block, 12, 640);
        put_word(&mut update_block, 16, 0);
        task.memory.write_bytes(0x2800, &update_block).unwrap();
        let mut update = SwiContext::default();
        update.registers[R1] = 0x2800;
        dispatcher
            .dispatch(WIMP_UPDATE_WINDOW, &mut task, &mut update)
            .unwrap();
        while update.registers[R0] != 0 {
            for byte in [31, 1, 0, b'U'] {
                let mut output = SwiContext::default();
                output.registers[R0] = u32::from(byte);
                dispatcher
                    .dispatch(OS_WRITE_C, &mut task, &mut output)
                    .unwrap();
            }
            let mut rectangle = SwiContext::default();
            rectangle.registers[R1] = 0x2800;
            dispatcher
                .dispatch(WIMP_GET_RECTANGLE, &mut task, &mut rectangle)
                .unwrap();
            update.registers[R0] = rectangle.registers[R0];
        }
        assert_eq!(dispatcher.active_graphics_window, None);
        assert_eq!(
            dispatcher.window_graphics[&first].snapshot().text_cells[0],
            b'A'
        );
        assert_eq!(
            dispatcher.window_graphics[&first].snapshot().text_cells[1],
            b'U'
        );

        let mut default_output = SwiContext::default();
        default_output.registers[R0] = u32::from(b'D');
        dispatcher
            .dispatch(OS_WRITE_C, &mut task, &mut default_output)
            .unwrap();
        assert_eq!(dispatcher.graphics().snapshot().text_cells[0], b'D');
        assert_eq!(
            dispatcher.window_graphics[&first].snapshot().text_cells[0],
            b'A'
        );
        assert_eq!(
            dispatcher.window_graphics[&second].snapshot().text_cells[0],
            b'B'
        );
        assert_eq!(dispatcher.window_graphics.len(), 2);
        dispatcher.task_default_graphics_mut(task.id).unwrap();
        assert!(dispatcher.task_default_graphics.contains_key(&task.id));

        let mut close_down = SwiContext::default();
        close_down.registers[R0] = wimp_task_handle;
        close_down.registers[R1] = u32::from_le_bytes(*b"TASK");
        dispatcher
            .dispatch(WIMP_CLOSE_DOWN, &mut task, &mut close_down)
            .unwrap();
        assert!(dispatcher.window_graphics.is_empty());
        assert_eq!(dispatcher.active_graphics_window, None);
        assert!(
            dispatcher.task_default_graphics.contains_key(&task.id),
            "CloseDown releases Wimp window surfaces but keeps the live task-default raster"
        );
        drop(input_sender);
    }

    #[test]
    fn wimp_redraw_clip_maps_half_open_work_area_to_local_graphics_coordinates() {
        let clip = work_area_to_graphics_clip(
            WorkArea {
                min_x: 10,
                min_y: -300,
                max_x: 210,
                max_y: -100,
            },
            WorkArea {
                min_x: 10,
                min_y: -600,
                max_x: 400,
                max_y: 0,
            },
            512,
        );
        assert_eq!(
            clip,
            GraphicsWindow {
                left: 0,
                bottom: 212,
                right: 199,
                top: 411,
            }
        );
    }

    #[test]
    fn desktop_cli_requires_the_windowed_wimp_service_and_accepts_only_no_arguments() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(1);

        let error = dispatch_cli_line(&mut dispatcher, &mut task, "*dEsK.").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("DESKTOP requires the windowed host")
        );
        assert!(!dispatcher.desktop_requested());

        assert!(dispatch_cli_line(&mut dispatcher, &mut task, "DESKTOP extra").is_ok());
        assert!(!dispatcher.desktop_requested());
    }

    #[test]
    fn desktop_abbreviation_does_not_steal_the_existing_dir_abbreviation() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(1);

        dispatch_cli_line(&mut dispatcher, &mut task, "*D.").unwrap();
        assert!(!dispatcher.desktop_requested());
    }

    #[test]
    fn basic64_source_type_is_named_and_launchable_without_tokenisation() {
        assert_eq!(parse_file_type("BASIC64").unwrap(), FILETYPE_BASIC64);
        assert_eq!(parse_file_type("&064").unwrap(), FILETYPE_BASIC64);
        assert_eq!(file_type_name(FILETYPE_BASIC64), *b"BASIC64 ");
        assert_ne!(FILETYPE_BASIC64, FILETYPE_BASIC);
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        dispatcher.set_file_system_for_test(HostFileSystem::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo-volume"),
        ));
        let mut task = Task::new(31);
        task.memory.write_bytes(0x1800, b"$.System\0").unwrap();
        for index in 0..2 {
            let mut context = SwiContext::default();
            context.registers[R0] = 1;
            context.registers[R1] = 0x1800;
            context.registers[R2] = index;
            context.registers[R3] = 0x1900;
            context.registers[R4] = 256;
            dispatcher
                .dispatch_named_swi("RICOCHET_DESKTOP", &mut task, &mut context)
                .unwrap();
            assert_eq!(
                context.registers[R0], 1,
                "BASIC64 source must be launchable"
            );
            assert_eq!(context.registers[R1], FILETYPE_BASIC64);
        }
    }

    #[test]
    fn ricochet_desktop_catalogue_uses_checked_guest_buffers_and_hostfs_metadata() {
        let mut dispatcher = SwiDispatcher::new(HostConsole::stdio());
        dispatcher.set_file_system_for_test(HostFileSystem::new(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("demo-volume"),
        ));
        let mut task = Task::new(31);
        const DIRECTORY: u32 = 0x1800;
        const ENTRY: u32 = 0x1900;
        task.memory.write_bytes(DIRECTORY, b"$.Examples\0").unwrap();

        let mut context = SwiContext::default();
        context.registers[R0] = 1;
        context.registers[R1] = DIRECTORY;
        context.registers[R2] = 0;
        context.registers[R3] = ENTRY;
        context.registers[R4] = 256;
        dispatcher
            .dispatch_named_swi("RICOCHET_DESKTOP", &mut task, &mut context)
            .unwrap();
        assert!([1, 2, 3, 4].contains(&context.registers[R0]));
        let name = task.memory.read_c_string(ENTRY, 256).unwrap();
        assert!(!name.is_empty());
        if context.registers[R0] != 2 {
            assert!(matches!(
                context.registers[R1] & 0xFFF,
                FILETYPE_BASIC | FILETYPE_BASIC64 | FILETYPE_TEXT
            ));
        }

        let mut date = SwiContext::default();
        date.registers[R0] = 3;
        date.registers[R1] = DIRECTORY;
        date.registers[R2] = 0;
        dispatcher
            .dispatch_named_swi("RICOCHET_DESKTOP", &mut task, &mut date)
            .unwrap();
        assert_eq!(date.registers[R0], 1);
        assert!(date.registers[R1] > 0);
        date.registers[R0] = 3;
        date.registers[R1] = DIRECTORY;
        date.registers[R2] = u32::MAX;
        dispatcher
            .dispatch_named_swi("RICOCHET_DESKTOP", &mut task, &mut date)
            .unwrap();
        assert_eq!((date.registers[R0], date.registers[R1]), (0, 0));

        task.memory.write_bytes(DIRECTORY, b"../../etc\0").unwrap();
        let mut invalid = SwiContext::default();
        invalid.registers[R0] = 1;
        invalid.registers[R1] = DIRECTORY;
        invalid.registers[R3] = ENTRY;
        invalid.registers[R4] = 256;
        assert!(
            dispatcher
                .dispatch_named_swi("RICOCHET_DESKTOP", &mut task, &mut invalid)
                .is_err()
        );

        task.memory.write_bytes(DIRECTORY, b"$.Examples\0").unwrap();
        let mut bad_buffer = SwiContext::default();
        bad_buffer.registers[R0] = 1;
        bad_buffer.registers[R1] = DIRECTORY;
        bad_buffer.registers[R3] = u32::MAX - 4;
        bad_buffer.registers[R4] = 256;
        assert!(
            dispatcher
                .dispatch_named_swi("RICOCHET_DESKTOP", &mut task, &mut bad_buffer)
                .is_err()
        );
    }

    #[test]
    fn clocksp5_compact_fx_reset_is_a_quiet_hosted_noop() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(1);

        dispatch_cli_line(&mut dispatcher, &mut task, "*fx151,78,243").unwrap();
        assert!(display_receiver.try_iter().next().is_none());
    }
}
