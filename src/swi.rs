use crate::{
    basic_compat::system_profile::SystemModule,
    boot::{
        BootCapsule, BootFailure, BootStage, RUNTIME_ABI_VERSION, embedded_capsule_bytes,
        parse_recovery_action, RecoveryAction,
    },
    configure::{BasicConfiguration, BasicEngine, ConfigureStore, StartupLanguage},
    display::{DesktopResolution, DisplayColour, DisplaySettings},
    error::RuntimeError,
    filesystem::{
        FILETYPE_BASIC, FILETYPE_BASIC64, FILETYPE_TEXT, FileMetadata, HostFileSystem, OpenFile,
    },
    graphics::{GraphicsProfile, GraphicsService, GraphicsSnapshot, GraphicsWindow},
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, Task},
    trellis::{
        CapabilityName, DefinitionId, DependencyFingerprint, DerivedTargetCache, InvocationBackend,
        ModuleId, ModuleManagementAuthority, ModuleRegistry, ModuleState, RegisterKind,
        ResourceRight,
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
    left: &[crate::trellis::SwiExport],
    right: &[crate::trellis::SwiExport],
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
    WIMP_CLOSE_DOWN, WIMP_CLOSE_WINDOW, WIMP_CREATE_ICON, WIMP_CREATE_ICON_EX, WIMP_CREATE_MENU,
    WIMP_CREATE_WINDOW, WIMP_DELETE_ICON, WIMP_FORCE_REDRAW, WIMP_GET_POINTER_INFO,
    WIMP_GET_RECTANGLE, WIMP_GET_WINDOW_STATE, WIMP_INITIALISE, WIMP_OPEN_WINDOW, WIMP_POLL,
    WIMP_REDRAW_WINDOW, WIMP_SET_EXTENT, WIMP_SET_ICON_STATE, WIMP_START_TASK, WIMP_UPDATE_WINDOW,
    WimpServer, WorkArea,
};

const DISPLAY_BATCH_FRAME_INTERVAL: Duration = Duration::from_micros(16_667);
const INKEY_POLL_INTERVAL: Duration = Duration::from_millis(8);
const MAX_EXTENDED_MODE_PIXELS: u64 = 4_194_304;
const MAX_TASK_GRAPHICS_PIXELS: u64 = 8_388_608;

pub const OS_WRITE_C: u32 = 0x00;
pub const OS_WRITE_S: u32 = 0x01;
pub const OS_WRITE_0: u32 = 0x02;
pub const OS_NEW_LINE: u32 = 0x03;
pub const OS_READ_C: u32 = 0x04;
pub const OS_CLI: u32 = 0x05;
pub const OS_BYTE: u32 = 0x06;
pub const OS_WORD: u32 = 0x07;
pub const OS_FILE: u32 = 0x08;
pub const OS_ARGS: u32 = 0x09;
pub const OS_BGET: u32 = 0x0A;
pub const OS_BPUT: u32 = 0x0B;
pub const OS_GBPB: u32 = 0x0C;
pub const OS_FIND: u32 = 0x0D;
pub const OS_READ_LINE: u32 = 0x0E;
pub const OS_FSCONTROL: u32 = 0x29;
pub const OS_READ_POINT: u32 = 0x32;
pub const OS_PLOT: u32 = 0x45;
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
const R8: usize = 8;
const ACORN_DISPLAY_ABI_VERSION: u32 = 1;
const ACORN_DISPLAY_QUERY: u32 = 0;
const ACORN_DISPLAY_APPLY: u32 = 1;
const HOST_FS_NUMBER: u32 = 1;
const HOST_FS_CONTROL_BLOCK: u32 = GUEST_MEMORY_BASE;
const GUEST_ADDRESS_MASK: u32 = 0x3FFF_FFFF;
const READ_LINE_ECHO_ONLY_BUFFERED: u32 = 1 << 31;
const READ_LINE_ECHO_R4: u32 = 1 << 30;
const MAX_CLI_BYTES: usize = 256;
const MAX_STRING_BYTES: usize = 4096;
const OUTPUT_BUFFER: u32 = GUEST_MEMORY_BASE + 0x1000;
const CLI_STRING_BUFFER: u32 = GUEST_MEMORY_BASE + 0x3000;
const HELP_TEXT: &[u8] = b"Acorn-2026 MOS commands:\n\r  Commands can be abbreviated with a final dot (for example, *CA. and *CONF.); *. is a shortcut for *CAT.\n\r  *CAT [dir]             Catalogue a directory.\n\r  *DIR [dir]             Select the current directory.\n\r  *CDIR <dir>            Create a directory.\n\r  *DELETE <file>         Delete a file.\n\r  *RENAME <old> <new>    Rename a file or directory.\n\r  *FILETYPE <file> <id>  Set a RISC OS file type.\n\r  *TYPE <file>           Display a text file.\n\r  *DISC [name]           Read or set the volume name.\n\r  *HOSTFS                Select the HostFS filing system.\n\r  *CONFIGURE <option> <value> Save a BASIC or startup preference.\n\r  *CONFIGURE Language 0  Open the MOS prompt on load.\n\r  *CONFIGURE Language 3  Open the desktop on load.\n\r  *CONFIGURE DEFAULTS    Restore configuration defaults.\n\r  *STATUS [option]       Show saved configuration.\n\r  *BASIC <file>          Load and run BASIC with saved preferences.\n\r  DESKTOP                Start the hosted Wimp desktop.\n\r  RUN <file>             Run a BASIC source or tokenised file.\n\r  BASICLOAD <file>       Load a tokenised BASIC program.\n\r  BASICRUN               Run the loaded program.\n\r  BASICJIT [file]        Run with experimental native hot regions.\n\r  BASICJIT STRICT [file] Compile and run supported code without fallback.\n\r  HELP                   Show this help.\n\r  QUIT                   Exit the runtime.";

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
    } else if module_name.eq_ignore_ascii_case("Boot") {
        ["StartupPolicy"].as_slice()
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

fn hsv_to_rgb(hue: f64, saturation: f64, value: f64) -> (u8, u8, u8) {
    let hue = hue.rem_euclid(360.0) / 60.0;
    let chroma = value * saturation.clamp(0.0, 1.0);
    let secondary = chroma * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match hue as u8 {
        0 => (chroma, secondary, 0.0),
        1 => (secondary, chroma, 0.0),
        2 => (0.0, chroma, secondary),
        3 => (0.0, secondary, chroma),
        4 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    let match_value = value - chroma;
    let component = |channel: f64| ((channel + match_value) * 255.0).round() as u8;
    (component(red), component(green), component(blue))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SwiContext {
    pub registers: [u32; 16],
    /// For OS_WriteS, this is the caller's saved return address: the first
    /// byte immediately following the SWI instruction.
    pub pc: u32,
    pub carry: bool,
}

struct ManagedResourceHandle {
    type_name: String,
    owner_task: u64,
    rights: BTreeSet<ResourceRight>,
    bytes: Vec<u8>,
}

pub struct SwiDispatcher {
    mos: mos::MosState,
    configure: ConfigureStore,
    console: HostConsole,
    graphics: GraphicsService,
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
    module_registry: ModuleRegistry,
    module_management_authority: ModuleManagementAuthority,
    module_programs: HashMap<DefinitionId, Arc<SystemModule>>,
    resource_handles: HashMap<u32, ManagedResourceHandle>,
    next_resource_handle: u32,
    derived_targets: DerivedTargetCache<Vec<u8>>,
    last_dispatch_route: Option<SwiDispatchRoute>,
    module_dispatch_count: u64,
    transitional_dispatch_count: u64,
    startup_target: Option<BootStartupTarget>,
    boot_failure: Option<BootFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BootStartupTarget {
    MosPrompt,
    Desktop,
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
    ) -> Result<crate::trellis::GenerationId, RuntimeError> {
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
    ) -> Result<crate::trellis::PrimitiveDescriptor, RuntimeError> {
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
    ) -> Result<crate::trellis::GenerationId, RuntimeError> {
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
            module_registry,
            module_management_authority,
            module_programs: HashMap::new(),
            resource_handles: HashMap::new(),
            next_resource_handle: 1,
            derived_targets: DerivedTargetCache::default(),
            last_dispatch_route: None,
            module_dispatch_count: 0,
            transitional_dispatch_count: 0,
            startup_target: None,
            boot_failure: None,
        };
        if let Some(path) = std::env::var_os("ACORN_BOOT_CAPSULE") {
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
                    dispatcher.boot_failure = Some(BootFailure::host_capsule_read(
                        &display_path,
                        error,
                    ));
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
                    dispatcher.boot_failure = Some(BootFailure::from_capsule(
                        BootStage::CapsuleBuild,
                        error,
                    ));
                }
            }
        }
        dispatcher
    }

    fn reset_boot_registry(&mut self) {
        self.module_registry = ModuleRegistry::new();
        self.module_management_authority = self.module_registry.issue_module_management_authority();
        self.module_programs.clear();
        self.active_module = None;
        self.startup_target = None;
        self.register_boot_primitives();
    }

    fn register_boot_primitives(&mut self) {
        let byte = RegisterKind::Unsigned { bits: 8 };
        let boolean = RegisterKind::Unsigned { bits: 1 };
        let status = RegisterKind::Unsigned { bits: 32 };
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
                vec![RegisterKind::Unsigned { bits: 32 }],
                false,
                true,
                Vec::new(),
                "RuntimeResult<StartupLanguage>",
            )
            .expect("embedded Boot primitive names are unique");
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
            failure.diagnostic_log.push(format!(
                "capsule byte length: {}",
                bytes.len()
            ));
            failure
        })?;
        for required in ["Console", "Boot"] {
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
            if let Err(error) = module.invoke_lifecycle_transactional(
                "START",
                module_id,
                &mut Task::new(0),
                self,
            ) {
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
                    failure.diagnostic_log.push(format!(
                        "source: {}",
                        module_record.source_path
                    ));
                    failure
                })?;

            // Grants in a capsule are still constrained by the host's reviewed
            // boot policy; a selected capsule cannot invent authority.
            validate_boot_grants(module_record.manifest.name.as_str(), &module_record.grants)
                .map_err(|message| {
                    let mut failure = BootFailure::from_capsule(BootStage::ModuleValidation, message);
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

    pub(crate) fn desktop_is_configured_for_startup(&self) -> bool {
        self.desktop_service.is_some()
            && self.startup_target == Some(BootStartupTarget::Desktop)
    }

    pub(crate) fn has_boot_failure(&self) -> bool {
        self.boot_failure.is_some()
    }

    /// Runs the restricted native recovery interface before any public SWI is
    /// made available to the task. The only host-file operation is an
    /// operator-selected capsule read; no normal guest path or CLI exists here.
    pub(crate) fn recover_boot(&mut self) -> Result<bool, RuntimeError> {
        while let Some(failure) = self.boot_failure.clone() {
            self.native_recovery_write(b"Trellis native recovery\n\r")?;
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
        let bytes = std::fs::read(path)
            .map_err(|error| BootFailure::host_capsule_read(path, error))?;
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
        self.configure = configure;
        let Some(record) = self.module_registry.module_named("Boot") else {
            return;
        };
        let module_id = record.id;
        let source_path = record.manifest.source_path.clone();
        let source_hash = record.manifest.source_hash.clone();
        let Some(program) = self.module_programs.values().find(|program| {
            program.manifest.name.eq_ignore_ascii_case("Boot")
                && program.manifest.source_path == source_path
                && program.manifest.source_hash == source_hash
        }).cloned() else {
            return;
        };
        if let Err(error) = program.invoke_lifecycle_transactional(
            "START",
            module_id,
            &mut Task::new(0),
            self,
        ) {
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

    pub(crate) fn set_graphics_profile(
        &mut self,
        profile: GraphicsProfile,
    ) -> Result<(), RuntimeError> {
        let previous = self.current_graphics().snapshot().clone();
        self.current_graphics_mut().set_profile(profile)?;
        let snapshot = self.current_graphics().snapshot().clone();
        if snapshot != previous {
            self.publish_snapshot(snapshot);
        }
        Ok(())
    }

    pub(crate) fn poll_key(&mut self) -> Option<u8> {
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
        let result = module.invoke(
            ownership.module,
            &definition,
            &ownership.contract,
            task,
            self,
            context,
        );
        self.last_dispatch_route = Some(SwiDispatchRoute::ModuleOwned {
            number,
            name: ownership.name,
            module: ownership.module_name,
            definition: ownership.definition_name,
            generation: ownership.generation_number,
            backend: invocation.backend,
        });
        Some(result)
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
            "HOST.GRAPHICS.ACCEPTBYTE" => self.vdu_accept_byte(context),
            "HOST.CONSOLE.WRITEBYTE" => {
                self.console.write_byte(context.registers[R0] as u8)?;
                self.console.flush().map_err(RuntimeError::from)
            }
            "HOST.CONSOLE.READBYTESTATUS" => self.console_read_byte_status(context),
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
                Ok(())
            }
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
            "HOST.RUNTIME.MISSINGTERMINATOR" => {
                Err(crate::memory::MemoryError::MissingNullTerminator(context.registers[R0]).into())
            }
            "HOST.RUNTIME.ENDOFINPUT" => Err(RuntimeError::EndOfInput),
            _ => Err(RuntimeError::Program(format!(
                "primitive {name} has no host implementation"
            ))),
        }
    }

    fn vdu_accept_byte(&mut self, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let character = context.registers[R0] as u8;
        if let Some(mode) = self.current_graphics().mode_after_vdu_byte(character) {
            self.ensure_graphics_pixel_budget(
                self.active_graphics_window,
                u64::from(mode.pixel_width) * u64::from(mode.pixel_height),
            )?;
        }
        let previous_mode = self.current_graphics().snapshot().mode;
        let output_byte = self.current_graphics_mut().write_byte(character)?;
        let mode_changed = self.current_graphics().snapshot().mode != previous_mode;
        if mode_changed {
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
        context.registers[R0] = u32::from(output_byte.unwrap_or_default());
        context.registers[R1] = u32::from(output_byte.is_some());
        Ok(())
    }

    fn console_read_byte_status(&mut self, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let byte = match self.mos.input.pop_front() {
            Some(byte) => Some(byte),
            None => self.console.read_byte()?,
        };
        if let Some(byte) = byte {
            context.registers[R0] = u32::from(byte);
            context.registers[R1] = 0;
            context.carry = byte == 0x1B;
        } else {
            context.registers[R0] = 0;
            context.registers[R1] = 1;
            context.carry = false;
        }
        Ok(())
    }

    pub(crate) fn dispatch_named_swi(
        &mut self,
        name: &str,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        if let Some(number) = self.module_registry.swi_number(name) {
            return self.dispatch(number, task, context);
        }
        let result = match name {
            "OS_BYTE" => self.dispatch(OS_BYTE, task, context),
            "OS_WORD" => self.dispatch(OS_WORD, task, context),
            "OS_WRITEC" => self.dispatch(OS_WRITE_C, task, context),
            "OS_WRITES" => self.dispatch(OS_WRITE_S, task, context),
            "OS_WRITE0" => self.dispatch(OS_WRITE_0, task, context),
            "OS_NEWLINE" => self.dispatch(OS_NEW_LINE, task, context),
            "OS_READC" => self.dispatch(OS_READ_C, task, context),
            "OS_READLINE" => self.dispatch(OS_READ_LINE, task, context),
            "OS_READPOINT" => self.dispatch(OS_READ_POINT, task, context),
            "OS_CLI" => self.dispatch(OS_CLI, task, context),
            "OS_FILE" => self.os_file(task, context),
            "OS_ARGS" => self.os_args(task, context),
            "OS_BGET" => self.os_bget(task, context),
            "OS_BPUT" => self.os_bput(task, context),
            "OS_GBPB" => self.os_gbpb(task, context),
            "OS_FIND" => self.os_find(task, context),
            "OS_FSCONTROL" => self.os_fscontrol(task, context),
            "COLOURTRANS_CONVERTHSVTORGB" => {
                let hue = f64::from(context.registers[R0] as i32) / 65_536.0;
                let saturation = f64::from(context.registers[R1]) / 65_280.0;
                let value = f64::from(context.registers[R2] & 0xFF) / 255.0;
                let (red, green, blue) = hsv_to_rgb(hue, saturation, value);
                context.registers[R0] = u32::from(red);
                context.registers[R1] = u32::from(green);
                context.registers[R2] = u32::from(blue);
                Ok(())
            }
            "COLOURTRANS_SETGCOL" => {
                self.current_graphics_mut()
                    .set_rgb_gcol(context.registers[R0]);
                Ok(())
            }
            "COLOURTRANS_WRITEPALETTE" => Ok(()),
            "WIMP_INITIALISE" => self.dispatch_wimp(WIMP_INITIALISE, task, context),
            "WIMP_CREATEWINDOW" => self.dispatch_wimp(WIMP_CREATE_WINDOW, task, context),
            "WIMP_CREATEICON" => self.dispatch_wimp(WIMP_CREATE_ICON, task, context),
            "WIMP_CREATEICONEX" => self.dispatch_wimp(WIMP_CREATE_ICON_EX, task, context),
            "WIMP_CREATEMENU" => self.dispatch_wimp(WIMP_CREATE_MENU, task, context),
            "WIMP_DELETEICON" => self.dispatch_wimp(WIMP_DELETE_ICON, task, context),
            "WIMP_OPENWINDOW" => self.dispatch_wimp(WIMP_OPEN_WINDOW, task, context),
            "WIMP_REDRAWWINDOW" => self.dispatch_wimp(WIMP_REDRAW_WINDOW, task, context),
            "WIMP_UPDATEWINDOW" => self.dispatch_wimp(WIMP_UPDATE_WINDOW, task, context),
            "WIMP_GETRECTANGLE" => self.dispatch_wimp(WIMP_GET_RECTANGLE, task, context),
            "WIMP_FORCEREDRAW" => self.dispatch_wimp(WIMP_FORCE_REDRAW, task, context),
            "WIMP_CLOSEWINDOW" => self.dispatch_wimp(WIMP_CLOSE_WINDOW, task, context),
            "WIMP_POLL" => self.dispatch_wimp(WIMP_POLL, task, context),
            "WIMP_GETWINDOWSTATE" => self.dispatch_wimp(WIMP_GET_WINDOW_STATE, task, context),
            "WIMP_GETPOINTERINFO" => self.dispatch_wimp(WIMP_GET_POINTER_INFO, task, context),
            "WIMP_SETICONSTATE" => self.dispatch_wimp(WIMP_SET_ICON_STATE, task, context),
            "WIMP_SETEXTENT" => self.dispatch_wimp(WIMP_SET_EXTENT, task, context),
            "WIMP_CLOSEDOWN" => self.dispatch_wimp(WIMP_CLOSE_DOWN, task, context),
            "WIMP_STARTTASK" => self.dispatch_wimp(WIMP_START_TASK, task, context),
            "ACORN_DESKTOP" => self.acorn_desktop(task, context),
            "ACORN_DISPLAY" => self.acorn_display(context),
            _ => Err(RuntimeError::Program(format!(
                "named SWI {name} is not available in the hosted profile"
            ))),
        };
        if !matches!(&result, Err(RuntimeError::InvalidSwi(_))) {
            self.transitional_dispatch_count = self.transitional_dispatch_count.saturating_add(1);
            self.last_dispatch_route =
                Some(SwiDispatchRoute::TransitionalNamed { name: name.into() });
        }
        result
    }

    pub fn dispatch(
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
            WIMP_INITIALISE
                | WIMP_CREATE_WINDOW
                | WIMP_REDRAW_WINDOW
                | WIMP_UPDATE_WINDOW
                | WIMP_GET_RECTANGLE
                | WIMP_FORCE_REDRAW
                | WIMP_CREATE_ICON
                | WIMP_CREATE_ICON_EX
                | WIMP_DELETE_ICON
                | WIMP_OPEN_WINDOW
                | WIMP_CLOSE_WINDOW
                | WIMP_POLL
                | WIMP_GET_WINDOW_STATE
                | WIMP_SET_ICON_STATE
                | WIMP_GET_POINTER_INFO
                | WIMP_CREATE_MENU
                | WIMP_SET_EXTENT
                | WIMP_CLOSE_DOWN
                | WIMP_START_TASK
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
                let previous_mode = self.current_graphics().snapshot().mode;
                let output_byte = self.current_graphics_mut().write_byte(character)?;
                let mode_changed = self.current_graphics().snapshot().mode != previous_mode;
                if mode_changed {
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
            OS_CLI => self.execute_cli(task, context),
            OS_BYTE => self.os_byte(context),
            OS_WORD => self.os_word(task, context),
            OS_READ_LINE => self.read_line(task, context),
            OS_FILE => self.os_file(task, context),
            OS_ARGS => self.os_args(task, context),
            OS_BGET => self.os_bget(task, context),
            OS_BPUT => self.os_bput(task, context),
            OS_GBPB => self.os_gbpb(task, context),
            OS_FIND => self.os_find(task, context),
            OS_FSCONTROL => self.os_fscontrol(task, context),
            OS_PLOT => {
                let code = context.registers[R0] as u8;
                let x = context.registers[R1] as i32;
                let y = context.registers[R2] as i32;
                self.current_graphics_mut().plot(code, x, y)?;
                if self.display_batch_active {
                    self.publish_display_batch_snapshot_if_due();
                } else {
                    self.publish_display_event(DisplayEvent::Plot {
                        task_id: self.display_task_id,
                        window_handle: self.active_graphics_window,
                        code,
                        x,
                        y,
                    });
                }
                Ok(())
            }
            OS_READ_POINT => {
                let x = context.registers[R0] as i32;
                let y = context.registers[R1] as i32;
                if let Some((colour, tint)) = self.current_graphics().read_point(x, y) {
                    context.registers[R2] = colour;
                    context.registers[R3] = tint;
                    context.registers[R4] = 0;
                } else {
                    context.registers[R2] = u32::MAX;
                    context.registers[R3] = 0;
                    context.registers[R4] = u32::MAX;
                }
                Ok(())
            }
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

    /// Project extension for the BASIC64 Filer. It exposes checked HostFS
    /// catalogue records and the mounted volume name; Filer navigation and
    /// activation policy remain in BASIC64.
    fn acorn_desktop(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        match context.registers[R0] {
            1 => {
                let path = read_guest_control_string(task, context.registers[R1])?;
                let index = context.registers[R2] as usize;
                let capacity = context.registers[R4] as usize;
                if capacity == 0 || capacity > crate::memory::GUEST_MEMORY_SIZE as usize {
                    return Err(RuntimeError::Program(
                        "Acorn_Desktop entry buffer size is outside the hosted limit".into(),
                    ));
                }
                let entries = self.file_system.enumerate(&task.file_system, &path, "*")?;
                let Some(entry) = entries.get(index) else {
                    task.memory.write_byte(context.registers[R3], 0)?;
                    context.registers[R0] = 0;
                    context.registers[R1] = 0;
                    return Ok(());
                };
                if entry.guest_name.len() + 1 > capacity {
                    return Err(RuntimeError::Program(
                        "Acorn_Desktop entry name does not fit the caller buffer".into(),
                    ));
                }
                write_guest_string(task, context.registers[R3], &entry.guest_name)?;
                context.registers[R0] = if entry.is_directory {
                    2
                } else if matches!(
                    entry.metadata.file_type & 0xFFF,
                    FILETYPE_BASIC | FILETYPE_BASIC64
                ) {
                    1
                } else if entry.metadata.file_type & 0xFFF == FILETYPE_TEXT {
                    3
                } else {
                    4
                };
                context.registers[R1] = entry.metadata.file_type;
                context.registers[R2] = entry.length;
                Ok(())
            }
            2 => {
                let capacity = context.registers[R2] as usize;
                let name = self.file_system.volume_name();
                if name.len() + 1 > capacity {
                    return Err(RuntimeError::Program(
                        "Acorn_Desktop volume name does not fit the caller buffer".into(),
                    ));
                }
                write_guest_string(task, context.registers[R1], name)?;
                context.registers[R0] = u32::try_from(name.len()).unwrap_or(u32::MAX);
                Ok(())
            }
            4 => self
                .wimp
                .as_ref()
                .ok_or(RuntimeError::InvalidSwi(0x4FF00))?
                .register_system_menu(task.id),
            3 => {
                // Additive hosted catalogue metadata: leave reason 1 unchanged.
                let path = read_guest_control_string(task, context.registers[R1])?;
                let index = context.registers[R2] as usize;
                let entries = self.file_system.enumerate(&task.file_system, &path, "*")?;
                let modified = entries.get(index).and_then(|entry| {
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
            action => Err(RuntimeError::Program(format!(
                "Acorn_Desktop action {action} is not supported"
            ))),
        }
    }

    /// Versioned, register-only Display Manager service. A combined apply is
    /// persisted before Wimp state changes; R8 reports a persistence failure
    /// without terminating the BASIC64 Desktop task.
    fn acorn_display(&mut self, context: &mut SwiContext) -> Result<(), RuntimeError> {
        if context.registers[R0] != ACORN_DISPLAY_ABI_VERSION {
            return Err(RuntimeError::Program(format!(
                "Acorn_Display ABI version {} is unsupported",
                context.registers[R0]
            )));
        }
        let wimp = self.wimp.as_ref().ok_or_else(|| {
            RuntimeError::Program("Acorn_Display requires the hosted Wimp desktop".into())
        })?;
        match context.registers[R1] {
            ACORN_DISPLAY_QUERY => {
                write_display_query(wimp, context);
                context.registers[R8] = 0;
                Ok(())
            }
            ACORN_DISPLAY_APPLY => {
                let resolution =
                    DesktopResolution::from_id(context.registers[R2]).ok_or_else(|| {
                        RuntimeError::Program(format!(
                            "Acorn_Display resolution ID {} is invalid",
                            context.registers[R2]
                        ))
                    })?;
                let colour = DisplayColour::from_id(context.registers[R3]).ok_or_else(|| {
                    RuntimeError::Program(format!(
                        "Acorn_Display colour ID {} is invalid",
                        context.registers[R3]
                    ))
                })?;
                let settings = DisplaySettings { resolution, colour };
                let saved = wimp.apply_display_settings(settings).is_ok();
                write_display_query(wimp, context);
                context.registers[R8] = if saved { 0 } else { 1 };
                Ok(())
            }
            action => Err(RuntimeError::Program(format!(
                "Acorn_Display action {action} is unsupported"
            ))),
        }
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

    fn os_file(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let path = read_guest_string(task, context.registers[R1])?;
        match reason {
            0 | 10 => {
                let start = context.registers[R4];
                let end = context.registers[R5];
                let length = end.checked_sub(start).ok_or_else(|| {
                    RuntimeError::Program("OS_File save end precedes its start".into())
                })?;
                let bytes = task.memory.read_bytes(start, length as usize)?;
                let mut metadata = metadata_for_new_guest_path(&path);
                if reason == 0 {
                    apply_riscos_load_address(&mut metadata, context.registers[R2]);
                    metadata.execution_address = context.registers[R3];
                } else {
                    metadata.file_type = context.registers[R2] & 0xFFF;
                }
                self.file_system
                    .write_file(&task.file_system, &path, &bytes, metadata)
            }
            1 | 2 | 3 | 4 | 9 | 18 => {
                let mut metadata = self.metadata_for_guest_object(task, &path)?;
                match reason {
                    1 => {
                        apply_riscos_load_address(&mut metadata, context.registers[R2]);
                        metadata.execution_address = context.registers[R3];
                        metadata.attributes = context.registers[R5];
                    }
                    2 => apply_riscos_load_address(&mut metadata, context.registers[R2]),
                    3 => metadata.execution_address = context.registers[R3],
                    4 => metadata.attributes = context.registers[R5],
                    9 if metadata.file_type == 0 => metadata.file_type = 0xFFD,
                    18 => metadata.file_type = context.registers[R2] & 0xFFF,
                    _ => {}
                }
                self.file_system
                    .set_metadata(&task.file_system, &path, &metadata)
            }
            5 | 13 | 15 | 17 => {
                let (object_type, metadata, length) = self.catalogue_object(task, &path)?;
                context.registers[R0] = object_type;
                if object_type != 0 {
                    context.registers[R2] = riscos_load_address(&metadata);
                    context.registers[R3] = metadata.execution_address;
                    context.registers[R4] = length;
                    context.registers[R5] = metadata.attributes;
                }
                Ok(())
            }
            6 => {
                let (object_type, metadata, length) = self.catalogue_object(task, &path)?;
                context.registers[R0] = object_type;
                if object_type != 0 {
                    context.registers[R2] = riscos_load_address(&metadata);
                    context.registers[R3] = metadata.execution_address;
                    context.registers[R4] = length;
                    context.registers[R5] = metadata.attributes;
                    if object_type == 2 {
                        self.file_system
                            .remove_directory(&task.file_system, &path)?;
                    } else {
                        self.file_system.delete_file(&task.file_system, &path)?;
                    }
                }
                Ok(())
            }
            7 | 11 => {
                let mut metadata = metadata_for_new_guest_path(&path);
                if reason == 7 {
                    apply_riscos_load_address(&mut metadata, context.registers[R2]);
                    metadata.execution_address = context.registers[R3];
                } else {
                    metadata.file_type = context.registers[R2] & 0xFFF;
                }
                self.file_system
                    .write_file(&task.file_system, &path, &[], metadata)
            }
            8 => {
                match self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)
                {
                    Ok(resolved) if resolved.is_directory => Ok(()),
                    _ => self
                        .file_system
                        .create_directory(&task.file_system, &path)
                        .map(|_| ()),
                }
            }
            12 | 14 | 16 | 255 => {
                let (bytes, metadata) = self.file_system.read_file(&task.file_system, &path)?;
                let destination = if context.registers[R3] & 0xFF == 0 {
                    context.registers[R2]
                } else {
                    metadata.load_address
                };
                task.memory.write_bytes(destination, &bytes)?;
                context.registers[R0] = 1;
                context.registers[R2] = riscos_load_address(&metadata);
                context.registers[R3] = metadata.execution_address;
                context.registers[R4] = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
                context.registers[R5] = metadata.attributes;
                Ok(())
            }
            other => Err(RuntimeError::Program(format!(
                "OS_File reason {other} is not implemented by the hosted FileSwitch"
            ))),
        }
    }

    fn metadata_for_guest_object(
        &self,
        task: &Task,
        path: &str,
    ) -> Result<FileMetadata, RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if resolved.is_directory {
            return Err(RuntimeError::Program(format!("'{path}' is a directory")));
        }
        Ok(resolved
            .metadata
            .unwrap_or_else(|| metadata_for_new_guest_path(&resolved.guest_path)))
    }

    fn catalogue_object(
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
            u32::try_from(std::fs::metadata(&resolved.host_path)?.len()).unwrap_or(u32::MAX);
        Ok((1, metadata, length))
    }

    fn os_find(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let reason = context.registers[R0] as u8;
        if reason == 0 {
            let handle = context.registers[R1];
            if handle == 0 {
                task.file_system.open_files.clear();
            } else {
                task.file_system.open_files.remove(&handle);
            }
            return Ok(());
        }

        let path = String::from_utf8_lossy(&mos::read_mos_string(
            task,
            context.registers[R1],
            MAX_STRING_BYTES,
        )?)
        .into_owned();
        let mode = reason & 0xC0;
        if !matches!(mode, 0x40 | 0x80 | 0xC0) {
            return Err(RuntimeError::Program(format!(
                "unsupported OS_Find reason &{reason:02X}"
            )));
        }
        let read = mode != 0x80;
        let write = mode != 0x40;
        let create = mode == 0x80;
        let truncate = create;
        let (file, resolved) = match self.file_system.open_file(
            &task.file_system,
            &path,
            read,
            write,
            create,
            truncate,
        ) {
            Ok(opened) => opened,
            Err(RuntimeError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound
                    && !create
                    && reason & 0x08 == 0 =>
            {
                context.registers[R0] = 0;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let handle = task
            .file_system
            .insert_file(OpenFile {
                file,
                can_read: read,
                can_write: write,
                guest_path: resolved.guest_path,
                eof_error_next: false,
            })
            .ok_or_else(|| RuntimeError::Program("no FileSwitch handles are available".into()))?;
        context.registers[R0] = handle;
        Ok(())
    }

    fn os_args(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let handle = context.registers[R1];
        if reason == 0 && handle == 0 {
            context.registers[R0] = HOST_FS_NUMBER;
            return Ok(());
        }
        let file = task
            .file_system
            .open_files
            .get_mut(&handle)
            .ok_or_else(|| RuntimeError::Program(format!("invalid file handle {handle}")))?;
        match reason {
            0 => {
                context.registers[R2] =
                    u32::try_from(file.file.stream_position()?).unwrap_or(u32::MAX)
            }
            1 => {
                let position = u64::from(context.registers[R2]);
                let length = file.file.metadata()?.len();
                if position > length {
                    if !file.can_write {
                        return Err(RuntimeError::Program("file is not open for writing".into()));
                    }
                    file.file.set_len(position)?;
                }
                file.file.seek(SeekFrom::Start(position))?;
                file.eof_error_next = false;
            }
            2 | 4 => {
                context.registers[R2] =
                    u32::try_from(file.file.metadata()?.len()).unwrap_or(u32::MAX)
            }
            3 => {
                if !file.can_write {
                    return Err(RuntimeError::Program("file is not open for writing".into()));
                }
                let extent = u64::from(context.registers[R2]);
                file.file.set_len(extent)?;
                if file.file.stream_position()? > extent {
                    file.file.seek(SeekFrom::Start(extent))?;
                }
                file.eof_error_next = false;
            }
            5 => {
                context.registers[R2] =
                    u32::from(file.file.stream_position()? >= file.file.metadata()?.len());
            }
            7 => {
                let canonical =
                    canonical_guest_name(self.file_system.volume_name(), &file.guest_path);
                let bytes = canonical.as_bytes();
                let buffer = context.registers[R2];
                let capacity = context.registers[R5] as usize;
                let required = bytes.len() + 1;
                if required <= capacity {
                    task.memory.write_bytes(buffer, bytes)?;
                    task.memory.write_byte(buffer + bytes.len() as u32, 0)?;
                    context.registers[R5] = (capacity - required) as u32;
                } else {
                    context.registers[R5] = (capacity as i64 - required as i64) as i32 as u32;
                }
            }
            other => {
                return Err(RuntimeError::Program(format!(
                    "OS_Args reason {other} is not implemented by the hosted FileSwitch"
                )));
            }
        }
        Ok(())
    }

    fn os_bget(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let handle = context.registers[R1];
        let file = task
            .file_system
            .open_files
            .get_mut(&handle)
            .ok_or_else(|| RuntimeError::Program(format!("invalid file handle {handle}")))?;
        if !file.can_read {
            return Err(RuntimeError::Program("file is not open for reading".into()));
        }
        if file.eof_error_next {
            file.eof_error_next = false;
            return Err(RuntimeError::Program("end of file".into()));
        }
        let mut byte = [0u8; 1];
        if file.file.read(&mut byte)? == 0 {
            file.eof_error_next = true;
            context.carry = true;
        } else {
            context.registers[R0] = u32::from(byte[0]);
            context.carry = false;
        }
        Ok(())
    }

    fn os_bput(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let handle = context.registers[R1];
        let file = task
            .file_system
            .open_files
            .get_mut(&handle)
            .ok_or_else(|| RuntimeError::Program(format!("invalid file handle {handle}")))?;
        if !file.can_write {
            return Err(RuntimeError::Program("file is not open for writing".into()));
        }
        file.file.write_all(&[context.registers[R0] as u8])?;
        file.eof_error_next = false;
        Ok(())
    }

    fn os_gbpb(&mut self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        match context.registers[R0] {
            1 | 2 | 3 | 4 => self.os_gbpb_file(task, context),
            5 | 6 | 7 => self.os_gbpb_names(task, context),
            8 | 9 | 10 => self.os_gbpb_directory(task, context),
            reason => Err(RuntimeError::Program(format!(
                "OS_GBPB reason {reason} is not implemented by the hosted FileSwitch"
            ))),
        }
    }

    fn os_gbpb_file(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let handle = context.registers[R1];
        let buffer = context.registers[R2];
        let requested = context.registers[R3] as usize;
        let file = task
            .file_system
            .open_files
            .get_mut(&handle)
            .ok_or_else(|| RuntimeError::Program(format!("invalid file handle {handle}")))?;
        if reason <= 2 {
            if !file.can_write {
                return Err(RuntimeError::Program("file is not open for writing".into()));
            }
            let initial = if reason == 1 {
                u64::from(context.registers[R4])
            } else {
                file.file.stream_position()?
            };
            file.file.seek(SeekFrom::Start(initial))?;
            let bytes = task.memory.read_bytes(buffer, requested)?;
            file.file.write_all(&bytes)?;
            let end = initial.saturating_add(requested as u64);
            context.registers[R2] = buffer.wrapping_add(requested as u32);
            context.registers[R3] = 0;
            context.registers[R4] = u32::try_from(end).unwrap_or(u32::MAX);
            context.carry = false;
            file.eof_error_next = false;
        } else {
            if !file.can_read {
                return Err(RuntimeError::Program("file is not open for reading".into()));
            }
            if requested > crate::memory::GUEST_MEMORY_SIZE {
                return Err(RuntimeError::Program(
                    "OS_GBPB transfer exceeds the task's logical buffer size".into(),
                ));
            }
            let initial = if reason == 3 {
                u64::from(context.registers[R4])
            } else {
                file.file.stream_position()?
            };
            file.file.seek(SeekFrom::Start(initial))?;
            let mut bytes = vec![0; requested];
            let transferred = file.file.read(&mut bytes)?;
            bytes.truncate(transferred);
            task.memory.write_bytes(buffer, &bytes)?;
            let end = initial.saturating_add(transferred as u64);
            context.registers[R2] = buffer.wrapping_add(transferred as u32);
            context.registers[R3] = u32::try_from(requested - transferred).unwrap_or(u32::MAX);
            context.registers[R4] = u32::try_from(end).unwrap_or(u32::MAX);
            context.carry = transferred != requested;
            file.eof_error_next = false;
        }
        Ok(())
    }

    fn os_gbpb_names(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let bytes = match reason {
            5 => {
                let name = self.file_system.volume_name().as_bytes();
                let mut bytes = vec![u8::try_from(name.len()).unwrap_or(u8::MAX)];
                bytes.extend_from_slice(name);
                bytes.push(0);
                bytes
            }
            6 => directory_name_bytes(&task.file_system.current_directory)?,
            _ => directory_name_bytes(&task.file_system.library_directory)?,
        };
        task.memory.write_bytes(context.registers[R2], &bytes)?;
        Ok(())
    }

    fn os_gbpb_directory(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        let (directory, wildcard) = if reason == 8 {
            ("@".to_string(), "*".to_string())
        } else {
            let directory = read_guest_string(task, context.registers[R1])?;
            let wildcard = if context.registers[R6] == 0 {
                "*".to_string()
            } else {
                read_guest_string(task, context.registers[R6])?
            };
            (
                if directory.is_empty() {
                    "@".to_string()
                } else {
                    directory
                },
                wildcard,
            )
        };
        let objects = self
            .file_system
            .enumerate(&task.file_system, &directory, &wildcard)?;
        let start = context.registers[R4] as usize;
        let requested = context.registers[R3] as usize;
        if reason == 8 {
            let mut offset = context.registers[R2];
            let mut read_count = 0usize;
            for object in objects.iter().skip(start).take(requested) {
                let name = object.guest_name.as_bytes();
                if name.len() > u8::MAX as usize {
                    return Err(RuntimeError::Program("guest leaf name is too long".into()));
                }
                task.memory.write_byte(offset, name.len() as u8)?;
                offset = offset.wrapping_add(1);
                task.memory.write_bytes(offset, name)?;
                offset = offset.wrapping_add(name.len() as u32);
                read_count += 1;
            }
            context.registers[R3] = (requested - read_count) as u32;
            context.registers[R4] = if start + read_count >= objects.len() {
                u32::MAX
            } else {
                (start + read_count) as u32
            };
            context.carry = read_count < requested;
            return Ok(());
        }

        let mut offset = context.registers[R2];
        let mut capacity = context.registers[R5] as usize;
        let mut read_count = 0usize;
        for object in objects.iter().skip(start).take(requested) {
            let mut record = Vec::new();
            if reason == 9 {
                record.extend_from_slice(object.guest_name.as_bytes());
                record.push(0);
            } else {
                let metadata = &object.metadata;
                push_word(&mut record, riscos_load_address(metadata));
                push_word(&mut record, metadata.execution_address);
                push_word(&mut record, object.length);
                push_word(&mut record, metadata.attributes);
                push_word(&mut record, if object.is_directory { 2 } else { 1 });
                record.extend_from_slice(object.guest_name.as_bytes());
                record.push(0);
                while record.len() % 4 != 0 {
                    record.push(0);
                }
            }
            if record.len() > capacity {
                break;
            }
            task.memory.write_bytes(offset, &record)?;
            offset = offset.wrapping_add(record.len() as u32);
            capacity -= record.len();
            read_count += 1;
        }
        context.registers[R3] = read_count as u32;
        context.registers[R4] = if start + read_count >= objects.len() {
            u32::MAX
        } else {
            (start + read_count) as u32
        };
        context.carry = read_count != 0;
        Ok(())
    }

    fn os_fscontrol(
        &mut self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let reason = context.registers[R0];
        match reason {
            0 => {
                let path = read_guest_string(task, context.registers[R1])?;
                self.file_system
                    .set_current_directory(&mut task.file_system, &path)
            }
            1 => {
                let path = read_guest_string(task, context.registers[R1])?;
                self.file_system
                    .set_library_directory(&mut task.file_system, &path)
            }
            5 | 6 | 7 | 8 => {
                let path = if context.registers[R1] == 0 {
                    match reason {
                        7 | 8 => format!("%"),
                        _ => "@".to_string(),
                    }
                } else {
                    read_guest_string(task, context.registers[R1])?
                };
                self.catalogue_directory(task, &path, reason == 6 || reason == 8)
            }
            9 => {
                let pattern = if context.registers[R1] == 0 {
                    "*".to_string()
                } else {
                    read_guest_string(task, context.registers[R1])?
                };
                let (directory, wildcard) = pattern
                    .rsplit_once('.')
                    .map(|(directory, leaf)| (directory.to_string(), leaf.to_string()))
                    .unwrap_or_else(|| ("@".to_string(), pattern));
                self.catalogue_objects(task, &directory, &wildcard)
            }
            11 => {
                let prefix = if context.registers[R1] == 0 {
                    String::new()
                } else {
                    read_guest_string(task, context.registers[R1])?
                };
                if let Some((name, _)) = prefix.split_once(':') {
                    if !self.file_system.check_file_system_name(name) {
                        return Err(RuntimeError::Program(format!(
                            "filing system '{name}' is not present"
                        )));
                    }
                    task.file_system.temporary_file_system =
                        self.file_system.file_system_name().to_string();
                } else {
                    task.file_system.temporary_file_system =
                        task.file_system.current_file_system.clone();
                }
                Ok(())
            }
            19 => {
                task.file_system.temporary_file_system =
                    task.file_system.current_file_system.clone();
                Ok(())
            }
            13 => {
                let requested = context.registers[R1];
                let present = if requested < 0x100 {
                    requested == HOST_FS_NUMBER
                } else {
                    let name = read_guest_string(task, requested)?;
                    self.file_system
                        .check_file_system_name(name.trim_end_matches(['#', ':', '-']))
                };
                if present {
                    context.registers[R1] = HOST_FS_NUMBER;
                    context.registers[R2] = HOST_FS_CONTROL_BLOCK;
                } else {
                    context.registers[R2] = 0;
                }
                Ok(())
            }
            14 => {
                let requested = context.registers[R1];
                if requested == 0 {
                    task.file_system.current_file_system.clear();
                    task.file_system.temporary_file_system.clear();
                    return Ok(());
                }
                let name = if requested < 0x100 {
                    if requested != HOST_FS_NUMBER {
                        return Err(RuntimeError::Program(format!(
                            "filing system number {requested} is not present"
                        )));
                    }
                    self.file_system.file_system_name().to_string()
                } else {
                    read_guest_string(task, requested)?
                };
                if !self
                    .file_system
                    .check_file_system_name(name.trim_end_matches(['#', ':', '-']))
                {
                    return Err(RuntimeError::Program(format!(
                        "filing system '{name}' is not present"
                    )));
                }
                task.file_system.current_file_system =
                    self.file_system.file_system_name().to_string();
                task.file_system.temporary_file_system =
                    task.file_system.current_file_system.clone();
                Ok(())
            }
            18 => {
                let name = file_type_name(context.registers[R2] & 0xFFF);
                context.registers[R2] = u32::from_le_bytes(name[..4].try_into().unwrap());
                context.registers[R3] = u32::from_le_bytes(name[4..].try_into().unwrap());
                Ok(())
            }
            22 => {
                task.file_system.open_files.clear();
                Ok(())
            }
            25 => {
                let from = read_guest_string(task, context.registers[R1])?;
                let to = read_guest_string(task, context.registers[R2])?;
                self.file_system.rename(&task.file_system, &from, &to)
            }
            31 => {
                let name = read_guest_string(task, context.registers[R1])?;
                context.registers[R2] = parse_file_type(&name)?;
                Ok(())
            }
            33 => {
                if context.registers[R1] == HOST_FS_NUMBER {
                    write_guest_buffer(
                        task,
                        context.registers[R2],
                        context.registers[R3] as usize,
                        self.file_system.file_system_name().as_bytes(),
                    )?;
                } else {
                    task.memory.write_byte(context.registers[R2], 0)?;
                }
                Ok(())
            }
            37 => {
                let path = read_guest_string(task, context.registers[R1])?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &path)?;
                let canonical =
                    canonical_guest_name(self.file_system.volume_name(), &resolved.guest_path);
                let capacity = context.registers[R5] as usize;
                let required = canonical.len() + 1;
                if required <= capacity {
                    task.memory
                        .write_bytes(context.registers[R2], canonical.as_bytes())?;
                    task.memory.write_byte(
                        context.registers[R2].wrapping_add(canonical.len() as u32),
                        0,
                    )?;
                    context.registers[R5] = (capacity - required) as u32;
                } else {
                    context.registers[R5] = (capacity as i64 - required as i64) as i32 as u32;
                }
                Ok(())
            }
            39 => {
                let path = read_guest_string(task, context.registers[R1])?;
                self.file_system.set_user_root(&mut task.file_system, &path)
            }
            40 => {
                std::mem::swap(
                    &mut task.file_system.current_directory,
                    &mut task.file_system.previous_directory,
                );
                Ok(())
            }
            43 => {
                task.file_system.current_directory.clear();
                Ok(())
            }
            44 => {
                task.file_system.user_root.clear();
                Ok(())
            }
            45 => {
                task.file_system.library_directory.clear();
                Ok(())
            }
            50 => {
                let object = read_guest_string(task, context.registers[R1])?;
                let resolved = self
                    .file_system
                    .canonical_guest_path(&task.file_system, &object)?;
                if !resolved.host_path.exists() {
                    return Err(RuntimeError::Program(format!(
                        "object '{object}' does not exist on HostFS"
                    )));
                }
                let name = read_guest_string(task, context.registers[R2])?;
                self.file_system.set_volume_name(name.trim())
            }
            other => Err(RuntimeError::Program(format!(
                "OS_FSControl reason {other} is not implemented by the hosted FileSwitch"
            ))),
        }
    }

    fn catalogue_directory(
        &mut self,
        task: &mut Task,
        path: &str,
        detailed: bool,
    ) -> Result<(), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, path)?;
        if !resolved.is_directory {
            return Err(RuntimeError::Program(format!(
                "'{path}' is not a directory"
            )));
        }
        let objects = self.file_system.enumerate(&task.file_system, path, "*")?;
        let title = canonical_guest_name(self.file_system.volume_name(), &resolved.guest_path);
        self.write_inline(task, title.as_bytes())?;
        self.write_new_line(task)?;
        for object in objects {
            if detailed {
                let kind = if object.is_directory {
                    "<DIR>"
                } else {
                    "     "
                };
                let line = format!(
                    "{:<10} {:>8} {} {:03X}",
                    object.guest_name,
                    object.length,
                    kind,
                    object.metadata.file_type & 0xFFF
                );
                self.write_inline(task, line.as_bytes())?;
                self.write_new_line(task)?;
            } else {
                self.write_inline(task, object.guest_name.as_bytes())?;
                self.write_new_line(task)?;
            }
        }
        Ok(())
    }

    fn catalogue_objects(
        &mut self,
        task: &mut Task,
        directory: &str,
        wildcard: &str,
    ) -> Result<(), RuntimeError> {
        let resolved = self
            .file_system
            .canonical_guest_path(&task.file_system, directory)?;
        if !resolved.is_directory {
            return Err(RuntimeError::Program(format!(
                "'{directory}' is not a directory"
            )));
        }
        let objects = self
            .file_system
            .enumerate(&task.file_system, directory, wildcard)?;
        for object in objects {
            let kind = if object.is_directory {
                "<DIR>"
            } else {
                "     "
            };
            let line = format!(
                "{:<10} {:>8} {} {:03X}",
                object.guest_name,
                object.length,
                kind,
                object.metadata.file_type & 0xFFF
            );
            self.write_inline(task, line.as_bytes())?;
            self.write_new_line(task)?;
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
        if self.display_events.is_some() {
            self.publish_snapshot(self.current_graphics().snapshot().clone());
        }
    }

    fn publish_display_batch_snapshot_if_due(&mut self) {
        if self.display_events.is_none()
            || !self
                .last_display_batch_publish
                .is_some_and(|last| last.elapsed() >= DISPLAY_BATCH_FRAME_INTERVAL)
        {
            return;
        }

        let snapshot = self.current_graphics().snapshot().clone();
        self.publish_snapshot(snapshot);
        self.last_display_batch_publish = Some(Instant::now());
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

        if cli_command_matches(verb, "HELP") && arguments.is_empty() {
            self.write_inline(task, HELP_TEXT)?;
            self.write_new_line(task)
        } else if cli_command_matches(verb, "QUIT") && arguments.is_empty() {
            self.quit_requested = true;
            Ok(())
        } else if cli_command_matches(verb, "CONFIGURE") {
            self.execute_configure_command(task, arguments)
        } else if cli_command_matches(verb, "STATUS") {
            self.execute_status_command(task, arguments)
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
            self.begin_display_batch();
            let result =
                crate::basic64::run_guest_file_configured(path, task, self, &configuration);
            self.finish_display_batch();
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
            self.begin_display_batch();
            let result =
                crate::basic64::run_guest_file_configured(path, task, self, &configuration);
            self.finish_display_batch();
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
        } else if cli_command_matches(verb, "BASICLOAD") {
            if arguments.is_empty() {
                self.write_inline(task, b"Syntax: BASICLOAD <file>")?;
                return self.write_new_line(task);
            }

            let path = unquote_single_argument(arguments);
            match self.file_system.read_file(&task.file_system, path) {
                Ok((bytes, metadata)) if metadata.file_type & 0xFFF == FILETYPE_BASIC => {
                    let program =
                        match crate::tokenized_basic::TokenizedBasicProgram::decode(&bytes) {
                            Ok(program) => program,
                            Err(error) => {
                                let message = format!("BASICLOAD error: {error}");
                                self.write_inline(task, message.as_bytes())?;
                                return self.write_new_line(task);
                            }
                        };
                    let line_count = program.line_count();
                    let reference_count = program.line_reference_count();
                    let unresolved_count = program.unresolved_line_reference_count();
                    task.loaded_tokenized_program = Some(program);
                    let message = format!(
                        "Loaded {line_count} tokenised BASIC lines; {reference_count} line references, {unresolved_count} unresolved."
                    );
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
                Ok((_, metadata)) => {
                    let message = format!(
                        "BASICLOAD requires file type &FFB; '{}' has type &{:03X}.",
                        path,
                        metadata.file_type & 0xFFF
                    );
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
                Err(error) => {
                    let message = format!("BASICLOAD error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if cli_command_matches(verb, "BASICRUN") {
            if !arguments.is_empty() {
                self.write_inline(task, b"Syntax: BASICRUN")?;
                return self.write_new_line(task);
            }

            let Some(program) = task.loaded_tokenized_program.take() else {
                self.write_inline(
                    task,
                    b"No tokenised BASIC program is loaded; use BASICLOAD first.",
                )?;
                return self.write_new_line(task);
            };
            self.begin_display_batch();
            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    self.finish_display_batch();
                    task.loaded_tokenized_program = Some(program);
                    let message = format!("BASIC configuration error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    return self.write_new_line(task);
                }
            };
            let result =
                crate::basic_compat::run_program_configured(&program, task, self, &configuration);
            self.finish_display_batch();
            task.loaded_tokenized_program = Some(program);
            match result {
                Ok(Some(report)) => {
                    log_jit_report("BASICRUN", report);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(error) => {
                    let message = format!("BASICRUN error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            }
        } else if cli_command_matches(verb, "BASICJIT") {
            self.begin_display_batch();
            let arguments = arguments.trim();
            let (strict, arguments) = match arguments.split_once(char::is_whitespace) {
                Some((first, rest)) if first.eq_ignore_ascii_case("STRICT") => (true, rest.trim()),
                None if arguments.eq_ignore_ascii_case("STRICT") => (true, ""),
                _ => (false, arguments),
            };
            let (benchmark_validation, arguments) = if strict {
                match arguments.strip_prefix("--benchmark-validation") {
                    Some(rest) if rest.is_empty() || rest.starts_with(char::is_whitespace) => {
                        (true, rest.trim())
                    }
                    _ => (false, arguments),
                }
            } else {
                (false, arguments)
            };
            let options = crate::basic_compat::StrictJitOptions {
                benchmark_validation,
            };
            let configuration = match self.load_basic_configuration() {
                Ok(configuration) => configuration,
                Err(error) => {
                    self.finish_display_batch();
                    eprintln!("BASICJIT configuration error: {error}");
                    return Ok(());
                }
            };
            let engine = if strict {
                BasicEngine::StrictJit
            } else {
                BasicEngine::HybridJit
            };
            let result = if arguments.is_empty() {
                let Some(program) = task.loaded_tokenized_program.take() else {
                    self.finish_display_batch();
                    self.write_inline(
                        task,
                        b"No tokenised BASIC program is loaded; use BASICLOAD or pass a file.",
                    )?;
                    return self.write_new_line(task);
                };
                let result = crate::basic_compat::run_program_with_engine_options(
                    &program,
                    task,
                    self,
                    &configuration,
                    Some(engine),
                    options,
                );
                task.loaded_tokenized_program = Some(program);
                result
            } else {
                let path = unquote_single_argument(arguments);
                crate::basic64::run_guest_file_with_engine_options(
                    path,
                    task,
                    self,
                    &configuration,
                    Some(engine),
                    options,
                )
            };
            self.finish_display_batch();
            match result {
                Ok(Some(report)) => {
                    log_jit_report("BASICJIT", report);
                    Ok(())
                }
                Ok(None) => Ok(()),
                Err(error) => {
                    eprintln!("BASICJIT error: {error}");
                    Ok(())
                }
            }
        } else if verb == "." || cli_command_matches(verb, "CAT") {
            let path = unquote_single_argument(arguments);
            let mut call = SwiContext::default();
            call.registers[R0] = 5;
            if !path.is_empty() {
                call.registers[R1] = CLI_STRING_BUFFER;
                write_guest_string(task, CLI_STRING_BUFFER, path)?;
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
                write_guest_string(task, CLI_STRING_BUFFER, path)?;
                let mut call = SwiContext::default();
                call.registers[R0] = 0;
                call.registers[R1] = CLI_STRING_BUFFER;
                self.dispatch(OS_FSCONTROL, task, &mut call)
            }
        } else if cli_command_matches(verb, "CDIR") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *CDIR <directory>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, CLI_STRING_BUFFER, path)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 8;
            call.registers[R1] = CLI_STRING_BUFFER;
            self.dispatch(OS_FILE, task, &mut call)
        } else if cli_command_matches(verb, "DELETE") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *DELETE <file>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, CLI_STRING_BUFFER, path)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 6;
            call.registers[R1] = CLI_STRING_BUFFER;
            self.dispatch(OS_FILE, task, &mut call)
        } else if cli_command_matches(verb, "RENAME") {
            let Some((from, to)) = split_two_cli_arguments(arguments) else {
                self.write_inline(task, b"Syntax: *RENAME <old> <new>")?;
                return self.write_new_line(task);
            };
            write_guest_string(task, CLI_STRING_BUFFER, &from)?;
            write_guest_string(task, CLI_STRING_BUFFER + 0x1000, &to)?;
            let mut call = SwiContext::default();
            call.registers[R0] = 25;
            call.registers[R1] = CLI_STRING_BUFFER;
            call.registers[R2] = CLI_STRING_BUFFER + 0x1000;
            self.dispatch(OS_FSCONTROL, task, &mut call)
        } else if cli_command_matches(verb, "FILETYPE") {
            let Some((path, type_name)) = split_two_cli_arguments(arguments) else {
                self.write_inline(task, b"Syntax: *FILETYPE <file> <type>")?;
                return self.write_new_line(task);
            };
            write_guest_string(task, CLI_STRING_BUFFER, &path)?;
            write_guest_string(task, CLI_STRING_BUFFER + 0x1000, &type_name)?;
            let mut convert = SwiContext::default();
            convert.registers[R0] = 31;
            convert.registers[R1] = CLI_STRING_BUFFER + 0x1000;
            self.dispatch(OS_FSCONTROL, task, &mut convert)?;
            let mut set_type = SwiContext::default();
            set_type.registers[R0] = 18;
            set_type.registers[R1] = CLI_STRING_BUFFER;
            set_type.registers[R2] = convert.registers[R2];
            self.dispatch(OS_FILE, task, &mut set_type)
        } else if cli_command_matches(verb, "TYPE") {
            let path = unquote_single_argument(arguments);
            if path.is_empty() {
                self.write_inline(task, b"Syntax: *TYPE <file>")?;
                return self.write_new_line(task);
            }
            write_guest_string(task, CLI_STRING_BUFFER, path)?;
            let mut open = SwiContext::default();
            open.registers[R0] = 0x40;
            open.registers[R1] = CLI_STRING_BUFFER;
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
                call.registers[R2] = CLI_STRING_BUFFER;
                self.dispatch(OS_GBPB, task, &mut call)?;
                let length = task.memory.read_byte(CLI_STRING_BUFFER)? as usize;
                let name = task.memory.read_bytes(CLI_STRING_BUFFER + 1, length)?;
                self.write_inline(task, &name)?;
                self.write_new_line(task)
            } else {
                write_guest_string(task, CLI_STRING_BUFFER + 0x1000, arguments.trim())?;
                write_guest_string(task, CLI_STRING_BUFFER, "@")?;
                let mut call = SwiContext::default();
                call.registers[R0] = 50;
                call.registers[R1] = CLI_STRING_BUFFER;
                call.registers[R2] = CLI_STRING_BUFFER + 0x1000;
                self.dispatch(OS_FSCONTROL, task, &mut call)
            }
        } else if cli_command_matches(verb, "HOSTFS") {
            write_guest_string(task, CLI_STRING_BUFFER, "HostFS")?;
            let mut call = SwiContext::default();
            call.registers[R0] = 14;
            call.registers[R1] = CLI_STRING_BUFFER;
            self.dispatch(OS_FSCONTROL, task, &mut call)
        } else if cli_command_matches(verb, "DESKTOP") {
            if !arguments.is_empty() {
                self.write_inline(task, b"Syntax: DESKTOP")?;
                return self.write_new_line(task);
            }
            self.begin_desktop()
        } else if cli_command_matches(verb, "FX")
            && arguments
                .chars()
                .filter(|character| !character.is_ascii_whitespace())
                .collect::<String>()
                == "151,78,243"
        {
            // ClockSP5 resets machine-specific display and timing state here;
            // the hosted profile has no such hardware state to restore.
            Ok(())
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

    fn execute_configure_command(
        &mut self,
        task: &mut Task,
        arguments: &str,
    ) -> Result<(), RuntimeError> {
        let arguments = arguments.trim();
        if arguments.is_empty() {
            self.write_inline(
                task,
                b"Syntax: *CONFIGURE <option> <value>\n\r  Language 0|3 (MOS prompt|desktop on load)\n\r  WindowFurniture Flat|Bevelled (restart app to apply)\n\r  DisplayResolution Window|640x480|800x600|1024x768|1152x864|1280x1024|1600x1200\n\r  DisplayColour BW|4Grey|16Grey|16Colour|256Grey|256Colour|32KRGB555|16MRGB888\n\r  BASICMode Auto|Classic|BASIC64|Hybrid\n\r  BASICProfile Auto|<profile>\n\r  BASICTarget Auto|Hosted|RISCOS|Agon\n\r  BASICEngine Interpreter|Hybrid|Strict\n\r  *CONFIGURE DEFAULTS resets all configuration preferences.",
            )?;
            return self.write_new_line(task);
        }
        if arguments.eq_ignore_ascii_case("DEFAULTS") {
            return match self.effective_configure_store().reset() {
                Ok(_) => {
                    self.write_inline(task, b"Configuration preferences restored to defaults.")?;
                    self.write_new_line(task)
                }
                Err(error) => {
                    let message = format!("CONFIGURE error: {error}");
                    self.write_inline(task, message.as_bytes())?;
                    self.write_new_line(task)
                }
            };
        }

        let mut words = arguments.split_ascii_whitespace();
        let Some(option) = words.next() else {
            return self.write_new_line(task);
        };
        let Some(value) = words.next() else {
            self.write_inline(task, b"Syntax: *CONFIGURE <option> <value>")?;
            return self.write_new_line(task);
        };
        if words.next().is_some() {
            self.write_inline(task, b"Syntax: *CONFIGURE <option> <value>")?;
            return self.write_new_line(task);
        }

        match self.effective_configure_store().set(option, value) {
            Ok(configuration) => {
                let (canonical, value) = configuration
                    .status_value(option)
                    .expect("validated CONFIGURE option");
                let message = format!("{canonical} set to {value}.");
                self.write_inline(task, message.as_bytes())?;
                self.write_new_line(task)
            }
            Err(error) => {
                let message = format!("CONFIGURE error: {error}");
                self.write_inline(task, message.as_bytes())?;
                self.write_new_line(task)
            }
        }
    }

    fn execute_status_command(
        &mut self,
        task: &mut Task,
        arguments: &str,
    ) -> Result<(), RuntimeError> {
        let arguments = arguments.trim();
        if arguments.split_ascii_whitespace().nth(1).is_some() {
            self.write_inline(task, b"Syntax: *STATUS [option]")?;
            return self.write_new_line(task);
        }
        let configuration = match self.load_basic_configuration() {
            Ok(configuration) => configuration,
            Err(error) => {
                let message = format!("STATUS error: {error}");
                self.write_inline(task, message.as_bytes())?;
                return self.write_new_line(task);
            }
        };
        if arguments.is_empty() {
            let mut output = String::new();
            for (option, value) in configuration.status_entries() {
                output.push_str(option);
                output.push('=');
                output.push_str(&value);
                output.push_str("\n\r");
            }
            return self.write_inline(task, output.as_bytes());
        }
        match configuration.status_value(arguments) {
            Some((option, value)) => {
                let output = format!("{option}={value}");
                self.write_inline(task, output.as_bytes())?;
                self.write_new_line(task)
            }
            None => {
                let message = format!("STATUS error: unknown configuration option '{arguments}'");
                self.write_inline(task, message.as_bytes())?;
                self.write_new_line(task)
            }
        }
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

fn read_guest_string(task: &Task, address: u32) -> Result<String, RuntimeError> {
    let bytes = task.memory.read_c_string(address, MAX_STRING_BYTES)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_guest_control_string(task: &Task, address: u32) -> Result<String, RuntimeError> {
    let mut bytes = Vec::new();
    for offset in 0..MAX_STRING_BYTES {
        let current = address
            .checked_add(
                u32::try_from(offset).map_err(|_| crate::memory::MemoryError::AddressOverflow)?,
            )
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let byte = task.memory.read_byte(current)?;
        if byte <= 31 {
            return Ok(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.push(byte);
    }
    Err(RuntimeError::Program(
        "guest control-terminated string is too long".into(),
    ))
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

fn canonical_guest_name(volume_name: &str, guest_path: &str) -> String {
    if guest_path.is_empty() {
        format!("HostFS::{volume_name}.$")
    } else {
        format!("HostFS::{volume_name}.$.{guest_path}")
    }
}

fn directory_name_bytes(components: &[String]) -> Result<Vec<u8>, RuntimeError> {
    let directory = if components.is_empty() {
        "$".to_string()
    } else {
        format!("$.{}", components.join("."))
    };
    let bytes = directory.as_bytes();
    let length = u8::try_from(bytes.len())
        .map_err(|_| RuntimeError::Program("directory name is too long".into()))?;
    let mut result = vec![0, length];
    result.extend_from_slice(bytes);
    result.push(0);
    Ok(result)
}

fn push_word(bytes: &mut Vec<u8>, word: u32) {
    bytes.extend_from_slice(&word.to_le_bytes());
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

    use crate::display::{DesktopResolution, DisplayColour, DisplaySettings};

    use super::*;

    #[test]
    fn native_boot_linker_reaches_linked_unpublished_modules_from_empty_table() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let capsule = BootCapsule::decode(
            embedded_capsule_bytes().unwrap(),
            RUNTIME_ABI_VERSION,
        )
        .unwrap();
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
            "acorn-boot-policy-{}.configure",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let configure = ConfigureStore::with_path(&path);

        configure.set("Language", "0").unwrap();
        dispatcher.set_configure_store_for_test(configure.clone());
        assert_eq!(dispatcher.startup_target, Some(BootStartupTarget::MosPrompt));
        assert!(!dispatcher.desktop_is_configured_for_startup());

        configure.set("Language", "3").unwrap();
        dispatcher.set_configure_store_for_test(configure.clone());
        assert_eq!(dispatcher.startup_target, Some(BootStartupTarget::Desktop));
        assert!(dispatcher.desktop_is_configured_for_startup());

        configure.set("Language", "0").unwrap();
        dispatcher.set_configure_store_for_test(configure);
        assert_eq!(dispatcher.startup_target, Some(BootStartupTarget::MosPrompt));
        assert!(!dispatcher.desktop_is_configured_for_startup());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_foundation_start_rolls_back_the_complete_public_namespace() {
        let console = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Console 1.0.0\nREM @SWI Test_Console &501 Entry\nDEF PROC Entry\nENDPROC\n";
        let alpha = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Alpha 1.0.0\nREM @LIFECYCLE START Start\nREM @SWI Test_Alpha &500 Entry\nREM @PRIVATE PROC Start\nDEF PROC Start\nENDPROC\nDEF PROC Entry\nENDPROC\n";
        let beta = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Beta 1.0.0\nREM @LIFECYCLE START Start\nREM @PRIVATE PROC Start\nDEF PROC Start\nSYS \"NoSuchStartupSwi\"\nENDPROC\n";
        let bytes = BootCapsule::build(
            RUNTIME_ABI_VERSION,
            &[
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
                    grants: &["StartupPolicy"],
                },
            ],
        )
        .unwrap();
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
        assert!(dispatcher.module_registry.module_named("Boot").is_none());
        assert!(dispatcher.module_programs.is_empty());
    }

    #[test]
    fn unresolved_primitive_import_fails_during_link_with_no_public_exports() {
        let source = "REM @BASIC64 MODE=BASIC64\nREM @SYSTEM_PROFILE 0.1\nREM @MODULE Console 1.0.0\nREM @CAPABILITY ConsoleOutput\nREM @IMPORT Host.Console.MissingPrimitive ConsoleOutput\nREM @SWI Test_Console &510 Entry\nDEF PROC Entry\nENDPROC\n";
        let bytes = BootCapsule::build(
            RUNTIME_ABI_VERSION,
            &[
                crate::boot::BootModuleInput {
                    source_path: "modules/Console.bas64",
                    source,
                    grants: &["ConsoleOutput"],
                },
                crate::boot::BootModuleInput {
                    source_path: "modules/Boot.bas64",
                    source: include_str!("../modules/Boot.bas64"),
                    grants: &["StartupPolicy"],
                },
            ],
        )
        .unwrap();
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
            "acorn-boot-recovery-{}-{}.capsule",
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
        assert_eq!(dispatcher.module_registry.registered_swi_count(), 6);
        let recovery_output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(recovery_output
            .windows("Trellis native recovery".len())
            .any(|bytes| bytes == b"Trellis native recovery"));
        assert!(recovery_output
            .windows(b"capsule validation".len())
            .any(|bytes| bytes == b"capsule validation"));
        let _ = std::fs::remove_file(path);
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

    fn initialise_wimp_task(dispatcher: &mut SwiDispatcher, task: &mut Task) {
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
            + u32::try_from(crate::memory::GUEST_MEMORY_SIZE).unwrap()
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
        let console = dispatcher
            .module_programs
            .values()
            .next()
            .expect("Console source retained")
            .clone();
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
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 0);
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
            crate::trellis::ModuleState::Active
        );
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 6);
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
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 6);
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
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 6);
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
            crate::trellis::ModuleState::Quiescing
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
        assert_eq!(dispatcher.module_registry().registered_swi_count(), 0);
        assert!(dispatcher
            .module_programs
            .values()
            .all(|program| program.manifest.name.eq_ignore_ascii_case("Boot")));
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
        let config_path = std::env::temp_dir().join(format!(
            "acorn-2026-configure-cli-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&config_path);
        dispatcher.configure = ConfigureStore::with_path(&config_path);
        let mut task = Task::new(1);

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
    fn acorn_display_swi_queries_applies_and_reports_save_failure_in_registers() {
        let path =
            std::env::temp_dir().join(format!("acorn-2026-display-swi-{}.txt", std::process::id()));
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
        query.registers[R0] = ACORN_DISPLAY_ABI_VERSION;
        query.registers[R1] = ACORN_DISPLAY_QUERY;
        dispatcher
            .dispatch_named_swi("ACORN_DISPLAY", &mut task, &mut query)
            .unwrap();
        assert_eq!(query.registers[R2], DesktopResolution::Window.id());
        assert_eq!(query.registers[R3], DisplayColour::Rgb888.id());
        assert_eq!((query.registers[R4], query.registers[R5]), (800, 600));
        assert_eq!((query.registers[R6], query.registers[R7]), (800, 600));

        let mut apply = SwiContext::default();
        apply.registers[R0] = ACORN_DISPLAY_ABI_VERSION;
        apply.registers[R1] = ACORN_DISPLAY_APPLY;
        apply.registers[R2] = DesktopResolution::R640x480.id();
        apply.registers[R3] = DisplayColour::Rgb555.id();
        dispatcher
            .dispatch_named_swi("ACORN_DISPLAY", &mut task, &mut apply)
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
        invalid_version.registers[R1] = ACORN_DISPLAY_QUERY;
        assert!(
            dispatcher
                .dispatch_named_swi("ACORN_DISPLAY", &mut task, &mut invalid_version)
                .is_err()
        );
        let _ = std::fs::remove_file(path);

        #[cfg(target_os = "linux")]
        {
            let (failed_display_sender, _failed_display_receiver) = mpsc::channel();
            let failed_wimp = WimpServer::new(mpsc::channel().0);
            failed_wimp.set_configure_store(
                ConfigureStore::with_path("/proc/self/acorn-2026-display-test/configure"),
                DisplaySettings::default(),
            );
            let mut failed_dispatcher = SwiDispatcher::desktop_task(
                HostConsole::windowed(mpsc::channel().1),
                failed_display_sender,
                78,
                failed_wimp.clone(),
            );
            let mut failed_apply = SwiContext::default();
            failed_apply.registers[R0] = ACORN_DISPLAY_ABI_VERSION;
            failed_apply.registers[R1] = ACORN_DISPLAY_APPLY;
            failed_apply.registers[R2] = DesktopResolution::R640x480.id();
            failed_apply.registers[R3] = DisplayColour::Grey4.id();
            failed_dispatcher
                .dispatch_named_swi("ACORN_DISPLAY", &mut Task::new(78), &mut failed_apply)
                .unwrap();
            assert_eq!(failed_apply.registers[R8], 1);
            assert_eq!(failed_wimp.display_settings(), DisplaySettings::default());
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
        initialise_wimp_task(&mut dispatcher, &mut task);

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
                .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut context)
                .unwrap();
            assert_eq!(
                context.registers[R0], 1,
                "BASIC64 source must be launchable"
            );
            assert_eq!(context.registers[R1], FILETYPE_BASIC64);
        }
    }

    #[test]
    fn acorn_desktop_catalogue_uses_checked_guest_buffers_and_hostfs_metadata() {
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
            .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut context)
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
            .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut date)
            .unwrap();
        assert_eq!(date.registers[R0], 1);
        assert!(date.registers[R1] > 0);
        date.registers[R0] = 3;
        date.registers[R1] = DIRECTORY;
        date.registers[R2] = u32::MAX;
        dispatcher
            .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut date)
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
                .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut invalid)
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
                .dispatch_named_swi("ACORN_DESKTOP", &mut task, &mut bad_buffer)
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

    #[cfg(feature = "experimental-jit")]
    #[test]
    fn strict_basicjit_returns_to_cli_with_loaded_program_available() {
        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let mut dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender);
        let mut task = Task::new(1);
        task.loaded_tokenized_program = Some(crate::tokenized_basic::TokenizedBasicProgram {
            lines: vec![
                crate::tokenized_basic::TokenizedBasicLine {
                    number: 10,
                    bytes: vec![0xF1, b'"', b'S', b'T', b'R', b'I', b'C', b'T', b'"'],
                    line_references: Vec::new(),
                },
                crate::tokenized_basic::TokenizedBasicLine {
                    number: 20,
                    bytes: vec![0xE0],
                    line_references: Vec::new(),
                },
            ],
            record_layout: None,
        });

        dispatch_cli_line(
            &mut dispatcher,
            &mut task,
            "BASICJIT STRICT --benchmark-validation",
        )
        .unwrap();
        assert!(task.loaded_tokenized_program.is_some());
        let native_output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(byte),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(native_output.windows(6).any(|bytes| bytes == b"STRICT"));
        dispatch_cli_line(&mut dispatcher, &mut task, "HELP").unwrap();
        dispatch_cli_line(&mut dispatcher, &mut task, "QUIT").unwrap();
        assert!(dispatcher.quit_requested());
    }
}
