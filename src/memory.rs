use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use crate::filesystem::OpenFile;

pub const GUEST_MEMORY_BASE: u32 = 0x1000;
// Room for BASIC64 desktop artwork, menu trees and catalogue state. Addresses
// remain caller-scoped and every access still passes the same bounds checks.
pub const GUEST_MEMORY_SIZE: usize = 1024 * 1024;
/// Per-task compatibility error block used by X-form SWIs. It is a real
/// caller-scoped logical address, never a Rust pointer, and is overwritten by
/// the next X-form error in that task.
pub const SWI_ERROR_BLOCK_SIZE: usize = 256;
pub const MAX_DYNAMIC_AREA_SIZE: u32 = 16 * 1024 * 1024;
pub const DYNAMIC_AREA_PAGE_SIZE: u32 = 4096;
const MAX_DYNAMIC_AREA_RESERVATION: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub enum MemoryError {
    AddressOutsideSpace(u32),
    AddressOverflow,
    MissingNullTerminator(u32),
    InvalidDynamicArea(String),
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddressOutsideSpace(address) => {
                write!(f, "logical address &{address:08X} is outside this task")
            }
            Self::AddressOverflow => write!(f, "logical address range overflowed"),
            Self::MissingNullTerminator(address) => {
                write!(
                    f,
                    "string at logical address &{address:08X} is not terminated"
                )
            }
            Self::InvalidDynamicArea(message) => write!(f, "dynamic area error: {message}"),
        }
    }
}

impl Error for MemoryError {}

#[derive(Debug)]
pub struct GuestMemory {
    bytes: Vec<u8>,
    dynamic_areas: BTreeMap<u32, DynamicArea>,
    command_scratch_areas: BTreeSet<u32>,
    system_variable_context_areas: BTreeSet<u32>,
    retired_dynamic_ranges: Vec<(u32, u32)>,
    next_dynamic_area_number: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DynamicArea {
    pub number: u32,
    pub current_size: u32,
    pub base_address: u32,
    pub flags: u32,
    pub maximum_size: u32,
    pub handler_address: u32,
    pub workspace_address: u32,
    /// The supplied name is copied for diagnostics. `name_address` is retained
    /// only as a caller-scoped logical address for the hosted OS_DynamicArea
    /// register contract; it is never dereferenced after creation.
    pub name_address: u32,
    pub name: String,
}

impl Default for GuestMemory {
    fn default() -> Self {
        Self {
            // Keep the compatibility zero page task-local. Legacy BASIC may
            // use zero-valued variables with an indirection operator during
            // initialisation, while managed allocations still begin at
            // GUEST_MEMORY_BASE.
            bytes: vec![0; GUEST_MEMORY_BASE as usize + GUEST_MEMORY_SIZE + SWI_ERROR_BLOCK_SIZE],
            dynamic_areas: BTreeMap::new(),
            command_scratch_areas: BTreeSet::new(),
            system_variable_context_areas: BTreeSet::new(),
            retired_dynamic_ranges: Vec::new(),
            next_dynamic_area_number: 256,
        }
    }
}

impl GuestMemory {
    /// Reserve a bounded, task-local dynamic area for short-lived system-command
    /// scratch. The returned address is a logical address in this GuestMemory;
    /// it never exposes the backing Vec pointer.
    pub fn acquire_command_scratch(&mut self) -> Result<DynamicArea, MemoryError> {
        let area = self.create_dynamic_area(
            u32::MAX,
            4096,
            u32::MAX,
            0,
            4096,
            0,
            0,
            0,
            "command scratch".into(),
        )?;
        self.command_scratch_areas.insert(area.number);
        Ok(area)
    }

    /// Release only an area returned by `acquire_command_scratch`.
    pub fn release_command_scratch(&mut self, number: u32) -> Result<(), MemoryError> {
        if !self.command_scratch_areas.contains(&number) {
            return Err(MemoryError::InvalidDynamicArea(
                "area is not a command scratch allocation".into(),
            ));
        }
        self.remove_dynamic_area(number)
    }

    pub(crate) fn release_all_command_scratch(&mut self) -> Result<(), MemoryError> {
        let areas = self
            .command_scratch_areas
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for number in areas {
            if let Err(error) = self.remove_dynamic_area(number) {
                self.command_scratch_areas.remove(&number);
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(crate) fn command_scratch_snapshot(&self) -> BTreeSet<u32> {
        self.command_scratch_areas.clone()
    }

    pub(crate) fn release_command_scratch_after(
        &mut self,
        existing: &BTreeSet<u32>,
    ) -> Result<(), MemoryError> {
        let areas = self
            .command_scratch_areas
            .difference(existing)
            .copied()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for number in areas {
            if let Err(error) = self.remove_dynamic_area(number) {
                self.command_scratch_areas.remove(&number);
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Reserve caller-owned logical memory for a live OS_ReadVarVal wildcard
    /// context. These areas are intentionally separate from command scratch:
    /// an R3 context returned to guest code remains valid across later OS_CLI
    /// calls until that enumeration is exhausted or explicitly abandoned.
    pub(crate) fn acquire_system_variable_context(&mut self) -> Result<DynamicArea, MemoryError> {
        let area = self.create_dynamic_area(
            u32::MAX,
            4096,
            u32::MAX,
            0,
            4096,
            0,
            0,
            0,
            "system-variable context".into(),
        )?;
        self.system_variable_context_areas.insert(area.number);
        Ok(area)
    }

    pub(crate) fn release_system_variable_context(
        &mut self,
        number: u32,
    ) -> Result<(), MemoryError> {
        if !self.system_variable_context_areas.contains(&number) {
            return Err(MemoryError::InvalidDynamicArea(
                "area is not a system-variable enumeration context".into(),
            ));
        }
        self.remove_dynamic_area(number)
    }

    pub fn read_byte(&self, address: u32) -> Result<u8, MemoryError> {
        let index = self.index(address, 1)?;
        Ok(self.bytes[index])
    }

    pub fn write_byte(&mut self, address: u32, byte: u8) -> Result<(), MemoryError> {
        let index = self.index(address, 1)?;
        self.bytes[index] = byte;
        Ok(())
    }

    pub fn write_bytes(&mut self, address: u32, bytes: &[u8]) -> Result<(), MemoryError> {
        let index = self.index(address, bytes.len())?;
        self.bytes[index..index + bytes.len()].copy_from_slice(bytes);
        Ok(())
    }

    pub fn read_bytes(&self, address: u32, length: usize) -> Result<Vec<u8>, MemoryError> {
        let index = self.index(address, length)?;
        Ok(self.bytes[index..index + length].to_vec())
    }

    /// Read caller-visible guest data while keeping the reserved X-form SWI
    /// error block private from ordinary public memory services.
    pub(crate) fn read_caller_data_bytes(
        &self,
        address: u32,
        length: usize,
    ) -> Result<Vec<u8>, MemoryError> {
        self.reject_swi_error_block_overlap(address, length)?;
        self.read_bytes(address, length)
    }

    /// Write caller-visible guest data atomically, excluding the runtime's
    /// reserved X-form SWI error block from guest write access.
    pub(crate) fn write_caller_data_bytes(
        &mut self,
        address: u32,
        bytes: &[u8],
    ) -> Result<(), MemoryError> {
        self.reject_swi_error_block_overlap(address, bytes.len())?;
        self.write_bytes(address, bytes)
    }

    pub(crate) fn validate_caller_data_span(
        &self,
        address: u32,
        length: usize,
    ) -> Result<(), MemoryError> {
        self.reject_swi_error_block_overlap(address, length)?;
        self.index(address, length).map(|_| ())
    }

    pub fn read_c_string(&self, address: u32, max_bytes: usize) -> Result<Vec<u8>, MemoryError> {
        let mut result = Vec::new();
        for offset in 0..max_bytes {
            let offset = u32::try_from(offset).map_err(|_| MemoryError::AddressOverflow)?;
            let current = address
                .checked_add(offset)
                .ok_or(MemoryError::AddressOverflow)?;
            let byte = self.read_byte(current)?;
            if byte == 0 {
                return Ok(result);
            }
            result.push(byte);
        }
        Err(MemoryError::MissingNullTerminator(address))
    }

    pub fn create_dynamic_area(
        &mut self,
        requested_number: u32,
        initial_size: u32,
        requested_base: u32,
        flags: u32,
        maximum_size: u32,
        handler_address: u32,
        workspace_address: u32,
        name_address: u32,
        name: String,
    ) -> Result<DynamicArea, MemoryError> {
        if requested_number != u32::MAX {
            return Err(MemoryError::InvalidDynamicArea(
                "the hosted profile requires automatic area numbering".into(),
            ));
        }
        if requested_base != u32::MAX {
            return Err(MemoryError::InvalidDynamicArea(
                "the hosted profile requires automatic logical placement".into(),
            ));
        }
        if handler_address != 0 || workspace_address != 0 {
            return Err(MemoryError::InvalidDynamicArea(
                "dynamic-area callbacks are not supported".into(),
            ));
        }
        if flags & (1 << 6) != 0 || flags & (1 << 8) != 0 || flags >> 9 != 0 {
            return Err(MemoryError::InvalidDynamicArea(
                "doubly mapped, physical-page, and reserved flags are not supported".into(),
            ));
        }
        let maximum_size = if maximum_size == u32::MAX {
            MAX_DYNAMIC_AREA_SIZE
        } else {
            maximum_size
        };
        if maximum_size == 0 || maximum_size > MAX_DYNAMIC_AREA_SIZE || initial_size > maximum_size
        {
            return Err(MemoryError::InvalidDynamicArea(format!(
                "requested initial/maximum sizes {initial_size}/{maximum_size} exceed the hosted bounds"
            )));
        }

        let reusable_range = self
            .retired_dynamic_ranges
            .iter()
            .position(|(_, length)| *length >= maximum_size);
        let newly_reserved = reusable_range.is_none();
        let active_reserved = self
            .dynamic_areas
            .values()
            .try_fold(0_usize, |sum, area| {
                sum.checked_add(area.maximum_size as usize)
            })
            .ok_or(MemoryError::AddressOverflow)?;
        let retired_reserved = self
            .retired_dynamic_ranges
            .iter()
            .try_fold(0_usize, |sum, (_, size)| sum.checked_add(*size as usize))
            .ok_or(MemoryError::AddressOverflow)?;
        let total_reserved = active_reserved
            .checked_add(retired_reserved)
            .ok_or(MemoryError::AddressOverflow)?;
        let additional = if newly_reserved {
            maximum_size as usize
        } else {
            0
        };
        if total_reserved
            .checked_add(additional)
            .is_none_or(|total| total > MAX_DYNAMIC_AREA_RESERVATION)
        {
            return Err(MemoryError::InvalidDynamicArea(
                "the task dynamic-area reservation limit was reached".into(),
            ));
        }

        let number = self.next_dynamic_area_number;
        self.next_dynamic_area_number =
            number
                .checked_add(1)
                .ok_or(MemoryError::InvalidDynamicArea(
                    "dynamic-area number space is exhausted".into(),
                ))?;
        let base_address = if let Some(range_index) = reusable_range {
            let (base, reserved_length) = self.retired_dynamic_ranges.remove(range_index);
            if reserved_length > maximum_size {
                self.retired_dynamic_ranges
                    .push((base + maximum_size, reserved_length - maximum_size));
            }
            base
        } else {
            let current_end =
                u32::try_from(self.bytes.len()).map_err(|_| MemoryError::AddressOverflow)?;
            let base = current_end
                .checked_add(DYNAMIC_AREA_PAGE_SIZE - 1)
                .map(|end| end & !(DYNAMIC_AREA_PAGE_SIZE - 1))
                .ok_or(MemoryError::AddressOverflow)?;
            let end = base
                .checked_add(maximum_size)
                .ok_or(MemoryError::AddressOverflow)?;
            let end = usize::try_from(end).map_err(|_| MemoryError::AddressOverflow)?;
            self.bytes.resize(end, 0);
            base
        };
        let area = DynamicArea {
            number,
            current_size: initial_size,
            base_address,
            flags,
            maximum_size,
            handler_address,
            workspace_address,
            name_address,
            name,
        };
        self.dynamic_areas.insert(number, area.clone());
        Ok(area)
    }

    pub fn dynamic_area(&self, number: u32) -> Result<&DynamicArea, MemoryError> {
        self.dynamic_areas
            .get(&number)
            .ok_or_else(|| MemoryError::InvalidDynamicArea(format!("area {number} does not exist")))
    }

    pub fn change_dynamic_area(
        &mut self,
        number: u32,
        change: i32,
    ) -> Result<(u32, u32), MemoryError> {
        let area = self.dynamic_areas.get_mut(&number).ok_or_else(|| {
            MemoryError::InvalidDynamicArea(format!("area {number} does not exist"))
        })?;
        let requested = i64::from(area.current_size) + i64::from(change);
        if requested < 0 || requested > i64::from(area.maximum_size) {
            return Err(MemoryError::InvalidDynamicArea(format!(
                "resize of area {number} exceeds its 0..={} byte range",
                area.maximum_size
            )));
        }
        let next = requested as u32;
        if next < area.current_size {
            let start = usize::try_from(area.base_address + next)
                .map_err(|_| MemoryError::AddressOverflow)?;
            let end = usize::try_from(area.base_address + area.current_size)
                .map_err(|_| MemoryError::AddressOverflow)?;
            self.bytes[start..end].fill(0);
        }
        area.current_size = next;
        Ok((change.unsigned_abs(), next))
    }

    pub fn remove_dynamic_area(&mut self, number: u32) -> Result<(), MemoryError> {
        let area = self.dynamic_areas.remove(&number).ok_or_else(|| {
            MemoryError::InvalidDynamicArea(format!("area {number} does not exist"))
        })?;
        let start = usize::try_from(area.base_address).map_err(|_| MemoryError::AddressOverflow)?;
        let end = usize::try_from(area.base_address + area.maximum_size)
            .map_err(|_| MemoryError::AddressOverflow)?;
        self.bytes[start..end].fill(0);
        self.retired_dynamic_ranges
            .push((area.base_address, area.maximum_size));
        self.command_scratch_areas.remove(&number);
        self.system_variable_context_areas.remove(&number);
        Ok(())
    }

    pub fn next_dynamic_area(&self, after: u32) -> Option<u32> {
        if after == u32::MAX {
            self.dynamic_areas.keys().next().copied()
        } else {
            self.dynamic_areas
                .range((std::ops::Bound::Excluded(after), std::ops::Bound::Unbounded))
                .next()
                .map(|(number, _)| *number)
        }
    }

    pub fn dynamic_area_count(&self) -> usize {
        self.dynamic_areas.len()
    }

    pub fn logical_size(&self) -> usize {
        self.bytes.len().saturating_sub(GUEST_MEMORY_BASE as usize)
    }

    pub fn swi_error_block_address(&self) -> u32 {
        GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32
    }

    /// Stores a standard little-endian RISC OS-style error block in this
    /// task's reserved scratch slot and returns its logical address.
    pub fn write_swi_error_block(&mut self, code: u32, message: &str) -> u32 {
        let address = self.swi_error_block_address();
        let start = address as usize;
        let slot = &mut self.bytes[start..start + SWI_ERROR_BLOCK_SIZE];
        slot.fill(0);
        slot[..4].copy_from_slice(&code.to_le_bytes());
        let message = message.as_bytes();
        let length = message.len().min(SWI_ERROR_BLOCK_SIZE - 5);
        slot[4..4 + length].copy_from_slice(&message[..length]);
        address
    }

    fn index(&self, address: u32, length: usize) -> Result<usize, MemoryError> {
        let start = usize::try_from(address).map_err(|_| MemoryError::AddressOverflow)?;
        let end = start
            .checked_add(length)
            .ok_or(MemoryError::AddressOverflow)?;
        if end > self.bytes.len() {
            return Err(MemoryError::AddressOutsideSpace(address));
        }
        let end_address = u32::try_from(end).map_err(|_| MemoryError::AddressOverflow)?;
        for area in self.dynamic_areas.values() {
            let reserved_end = area
                .base_address
                .checked_add(area.maximum_size)
                .ok_or(MemoryError::AddressOverflow)?;
            let committed_end = area
                .base_address
                .checked_add(area.current_size)
                .ok_or(MemoryError::AddressOverflow)?;
            if address < reserved_end && end_address > committed_end {
                return Err(MemoryError::AddressOutsideSpace(address));
            }
        }
        if self.retired_dynamic_ranges.iter().any(|(base, size)| {
            base.checked_add(*size)
                .is_some_and(|reserved_end| address < reserved_end && end_address > *base)
        }) {
            return Err(MemoryError::AddressOutsideSpace(address));
        }
        Ok(start)
    }

    fn reject_swi_error_block_overlap(
        &self,
        address: u32,
        length: usize,
    ) -> Result<(), MemoryError> {
        let end = u64::from(address)
            .checked_add(u64::try_from(length).map_err(|_| MemoryError::AddressOverflow)?)
            .ok_or(MemoryError::AddressOverflow)?;
        let reserved_start = u64::from(self.swi_error_block_address());
        let reserved_end = reserved_start + SWI_ERROR_BLOCK_SIZE as u64;
        if u64::from(address) < reserved_end && end > reserved_start {
            return Err(MemoryError::AddressOutsideSpace(address));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct Task {
    pub id: u64,
    pub memory: GuestMemory,
    pub file_system: FileSystemContext,
    // Privileges belong to the task object, never to its public numeric ID.
    // BASIC64 and guest module data cannot create or alter these grants.
    authority: TaskAuthority,
    system_variable_read_cursors: Vec<SystemVariableReadCursor>,
    exec_input: Option<ExecInputSource>,
    exec_input_provenance: Option<(String, u32)>,
}

#[derive(Debug)]
struct ExecInputSource {
    guest_path: String,
    bytes: Vec<u8>,
    offset: usize,
    line: u32,
    line_has_data: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ExecInputRead {
    Inactive,
    Byte {
        byte: u8,
        guest_path: String,
        line: u32,
    },
    UnterminatedLineEnd,
}

#[derive(Clone, Debug)]
pub(crate) struct SystemVariableReadCursor {
    pub(crate) pattern: String,
    pub(crate) after_key: Option<String>,
    pub(crate) scratch_number: u32,
    pub(crate) scratch_base: u32,
}

#[derive(Clone, Copy, Debug, Default)]
struct TaskAuthority(u8);

impl TaskAuthority {
    const SOURCE_READ: u8 = 1 << 0;
    const MODULE_MANAGEMENT: u8 = 1 << 1;
    const CONFIGURATION_WRITE: u8 = 1 << 2;
    const SYSTEM_VARIABLE_WRITE: u8 = 1 << 3;

    const fn source_inspector() -> Self {
        Self(Self::SOURCE_READ)
    }

    const fn module_manager() -> Self {
        Self(Self::MODULE_MANAGEMENT)
    }

    const fn trusted_mos_session() -> Self {
        Self(
            Self::SOURCE_READ
                | Self::MODULE_MANAGEMENT
                | Self::CONFIGURATION_WRITE
                | Self::SYSTEM_VARIABLE_WRITE,
        )
    }

    const fn configuration_manager() -> Self {
        Self(Self::CONFIGURATION_WRITE)
    }

    const fn allows(self, authority: u8) -> bool {
        self.0 & authority == authority
    }
}

#[derive(Debug)]
pub struct FileSystemContext {
    pub current_file_system: String,
    pub temporary_file_system: String,
    pub current_directory: Vec<String>,
    pub user_root: Vec<String>,
    pub library_directory: Vec<String>,
    pub previous_directory: Vec<String>,
    pub open_files: BTreeMap<u32, OpenFile>,
    pub gbpb_directory_snapshot: Vec<crate::filesystem::GuestObject>,
    pub gbpb_directory_snapshot_id: u32,
    next_handle: u32,
}

impl Default for FileSystemContext {
    fn default() -> Self {
        Self {
            current_file_system: "HostFS".to_string(),
            temporary_file_system: "HostFS".to_string(),
            current_directory: Vec::new(),
            user_root: Vec::new(),
            library_directory: Vec::new(),
            previous_directory: Vec::new(),
            open_files: BTreeMap::new(),
            gbpb_directory_snapshot: Vec::new(),
            gbpb_directory_snapshot_id: 0,
            // FileSwitch file handles are conventionally returned in the
            // upper half of the byte range.
            next_handle: 0x80,
        }
    }
}

impl FileSystemContext {
    pub fn has_file_slot(&self) -> bool {
        self.open_files.len() < 0x80
    }

    pub fn insert_file(&mut self, open_file: OpenFile) -> Option<u32> {
        if !self.has_file_slot() {
            return None;
        }
        let mut handle = self.next_handle.clamp(0x80, 0xFF);
        while self.open_files.contains_key(&handle) {
            handle = if handle == 0xFF { 0x80 } else { handle + 1 };
        }
        self.next_handle = if handle == 0xFF { 0x80 } else { handle + 1 };
        self.open_files.insert(handle, open_file);
        Some(handle)
    }
}

impl Task {
    pub fn new(id: u64) -> Self {
        Self::with_authority(id, TaskAuthority::default())
    }

    /// Construct a host-authorized interactive MOS session.
    ///
    /// This is an explicit host bootstrap path; it is not exposed to BASIC64
    /// or guest modules. It includes source-read, module-management,
    /// configuration-write, and session system-variable-write authority.
    /// Spawned desktop tasks must use `Task::new` instead.
    pub fn trusted_mos_session(id: u64) -> Self {
        Self::with_authority(id, TaskAuthority::trusted_mos_session())
    }

    /// Construct a host-side task that may inspect retained definition source
    /// but cannot load, replace, or unload modules.
    pub fn trusted_source_inspector(id: u64) -> Self {
        Self::with_authority(id, TaskAuthority::source_inspector())
    }

    /// Construct a host-side task that may manage modules but cannot inspect
    /// retained definition source.
    pub fn trusted_module_manager(id: u64) -> Self {
        Self::with_authority(id, TaskAuthority::module_manager())
    }

    /// Construct a host-side task that may persist user configuration but
    /// has neither retained-source visibility nor module-management rights.
    pub fn trusted_configuration_manager(id: u64) -> Self {
        Self::with_authority(id, TaskAuthority::configuration_manager())
    }

    pub(crate) fn require_source_read(&self) -> Result<(), crate::error::RuntimeError> {
        if self.authority.allows(TaskAuthority::SOURCE_READ) {
            Ok(())
        } else {
            Err(Self::authorization_denied(
                1,
                "caller task lacks definition-source read authority",
            ))
        }
    }

    pub(crate) fn require_module_management(&self) -> Result<(), crate::error::RuntimeError> {
        if self.authority.allows(TaskAuthority::MODULE_MANAGEMENT) {
            Ok(())
        } else {
            Err(Self::authorization_denied(
                2,
                "caller task lacks module-management authority",
            ))
        }
    }

    pub(crate) fn require_configuration_write(&self) -> Result<(), crate::error::RuntimeError> {
        if self.authority.allows(TaskAuthority::CONFIGURATION_WRITE) {
            Ok(())
        } else {
            Err(Self::authorization_denied(
                4,
                "caller task lacks configuration-write authority",
            ))
        }
    }

    pub(crate) fn require_system_variable_write(&self) -> Result<(), crate::error::RuntimeError> {
        if self.authority.allows(TaskAuthority::SYSTEM_VARIABLE_WRITE) {
            Ok(())
        } else {
            Err(Self::authorization_denied(
                8,
                "caller task lacks system-variable write authority",
            ))
        }
    }

    pub(crate) fn set_system_variable_read_cursor(&mut self, cursor: SystemVariableReadCursor) {
        self.system_variable_read_cursors.push(cursor);
    }

    pub(crate) fn system_variable_read_cursor(
        &self,
        context_address: u32,
    ) -> Option<&SystemVariableReadCursor> {
        self.system_variable_read_cursors
            .iter()
            .find(|cursor| cursor.scratch_base == context_address)
    }

    pub(crate) fn update_system_variable_read_cursor_after_key(
        &mut self,
        context_address: u32,
        after_key: String,
    ) -> Result<(), crate::error::RuntimeError> {
        let cursor = self
            .system_variable_read_cursors
            .iter_mut()
            .find(|cursor| cursor.scratch_base == context_address)
            .ok_or_else(|| {
                crate::error::RuntimeError::Program(
                    "system-variable enumeration context disappeared".into(),
                )
            })?;
        cursor.after_key = Some(after_key);
        Ok(())
    }

    pub(crate) fn system_variable_read_context_addresses(&self) -> BTreeSet<u32> {
        self.system_variable_read_cursors
            .iter()
            .map(|cursor| cursor.scratch_base)
            .collect()
    }

    pub(crate) fn clear_new_system_variable_read_cursors(
        &mut self,
        existing_contexts: &BTreeSet<u32>,
    ) {
        let released = self
            .system_variable_read_cursors
            .iter()
            .filter(|cursor| !existing_contexts.contains(&cursor.scratch_base))
            .map(|cursor| cursor.scratch_number)
            .collect::<Vec<_>>();
        self.system_variable_read_cursors
            .retain(|cursor| existing_contexts.contains(&cursor.scratch_base));
        for number in released {
            let _ = self.memory.release_system_variable_context(number);
        }
    }

    pub(crate) fn remove_system_variable_read_cursor(&mut self, context_address: u32) {
        if let Some(index) = self
            .system_variable_read_cursors
            .iter()
            .position(|cursor| cursor.scratch_base == context_address)
        {
            let cursor = self.system_variable_read_cursors.remove(index);
            let _ = self
                .memory
                .release_system_variable_context(cursor.scratch_number);
        }
    }

    fn authorization_denied(code: u32, message: &str) -> crate::error::RuntimeError {
        crate::error::RuntimeError::Structured {
            type_name: "TaskAuthorizationDenied".into(),
            code,
            message: message.into(),
        }
    }

    fn with_authority(id: u64, authority: TaskAuthority) -> Self {
        Self {
            id,
            memory: GuestMemory::default(),
            file_system: FileSystemContext::default(),
            authority,
            system_variable_read_cursors: Vec::new(),
            exec_input: None,
            exec_input_provenance: None,
        }
    }

    pub(crate) fn install_exec_input(&mut self, guest_path: String, bytes: Vec<u8>) {
        self.exec_input = Some(ExecInputSource {
            guest_path,
            bytes,
            offset: 0,
            line: 1,
            line_has_data: false,
        });
        self.exec_input_provenance = None;
    }

    pub(crate) fn close_exec_input(&mut self) {
        self.exec_input = None;
        self.exec_input_provenance = None;
    }

    pub(crate) fn has_exec_input(&self) -> bool {
        self.exec_input.is_some()
    }

    pub(crate) fn exec_input_provenance(&self) -> Option<(&str, u32)> {
        self.exec_input_provenance
            .as_ref()
            .map(|(path, line)| (path.as_str(), *line))
    }

    pub(crate) fn read_exec_input(&mut self) -> ExecInputRead {
        let Some(source) = self.exec_input.as_mut() else {
            return ExecInputRead::Inactive;
        };
        if source.offset >= source.bytes.len() {
            let partial_line = source.line_has_data;
            self.exec_input = None;
            return if partial_line {
                ExecInputRead::UnterminatedLineEnd
            } else {
                ExecInputRead::Inactive
            };
        }
        let byte = source.bytes[source.offset];
        source.offset += 1;
        let line = source.line;
        if byte == b'\r' {
            source.line = source.line.saturating_add(1);
            source.line_has_data = false;
        } else {
            source.line_has_data = true;
        }
        let guest_path = source.guest_path.clone();
        self.exec_input_provenance = Some((guest_path.clone(), line));
        ExecInputRead::Byte {
            byte,
            guest_path,
            line,
        }
    }

    pub(crate) fn set_exec_input_provenance(&mut self, provenance: Option<(String, u32)>) {
        self.exec_input_provenance = provenance;
    }
}
