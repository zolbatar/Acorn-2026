//! Host-directory-backed filing system with RISC OS-style guest paths.
//!
//! File contents remain ordinary host files. Per-file catalogue attributes are
//! kept in checked-in `*.acornmeta` sidecars and are never stored in host
//! extended attributes.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use crate::{error::RuntimeError, memory::FileSystemContext};

pub const HOST_FS_NAME: &str = "HOSTFS";
pub const DEFAULT_VOLUME_NAME: &str = "DemoDisk";
pub const VOLUME_DESCRIPTOR: &str = ".acorn-volume";
pub const METADATA_SUFFIX: &str = ".acornmeta";
pub const FILETYPE_TEXT: u32 = 0xFFF;
pub const FILETYPE_BASIC: u32 = 0xFFB;

const METADATA_MAGIC: &str = "Acorn-2026 file metadata v1";
const VOLUME_MAGIC: &str = "Acorn-2026 folder volume v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileMetadata {
    pub guest_name: String,
    pub file_type: u32,
    pub load_address: u32,
    pub execution_address: u32,
    pub attributes: u32,
}

impl FileMetadata {
    fn plain_file(name: String) -> Self {
        Self {
            guest_name: name,
            file_type: FILETYPE_TEXT,
            load_address: 0,
            execution_address: 0,
            attributes: 0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct GuestObject {
    pub guest_name: String,
    pub host_path: PathBuf,
    pub metadata: FileMetadata,
    pub is_directory: bool,
    pub length: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedPath {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub metadata: Option<FileMetadata>,
    pub is_directory: bool,
}

#[derive(Debug)]
pub struct OpenFile {
    pub file: File,
    pub can_read: bool,
    pub can_write: bool,
    pub guest_path: String,
}

#[derive(Debug)]
pub struct HostFileSystem {
    root: PathBuf,
    volume_name: String,
}

impl HostFileSystem {
    pub fn demo_default() -> Self {
        let root = std::env::var_os("ACORN_DEMO_VOLUME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .join("demo-volume")
            });
        Self::new(root)
    }

    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let volume_name =
            read_volume_name(&root).unwrap_or_else(|| DEFAULT_VOLUME_NAME.to_string());
        Self { root, volume_name }
    }

    pub fn file_system_name(&self) -> &'static str {
        HOST_FS_NAME
    }

    pub fn volume_name(&self) -> &str {
        &self.volume_name
    }

    fn canonical_root(&self) -> Result<PathBuf, RuntimeError> {
        fs::canonicalize(&self.root).map_err(|error| {
            RuntimeError::Program(format!(
                "HostFS volume root '{}' is unavailable: {error}",
                self.root.display()
            ))
        })
    }

    pub fn check_file_system_name(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case(HOST_FS_NAME)
    }

    pub fn check_volume_name(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case(&self.volume_name)
    }

    pub fn canonical_guest_path(
        &self,
        context: &FileSystemContext,
        path: &str,
    ) -> Result<ResolvedPath, RuntimeError> {
        let (file_system, volume, path) = split_device_prefix(path);
        if let Some(file_system) = file_system
            && !self.check_file_system_name(file_system)
        {
            return Err(fs_error(format!(
                "filing system '{file_system}' is not present"
            )));
        }
        if let Some(volume) = volume
            && !self.check_volume_name(volume)
        {
            return Err(fs_error(format!(
                "volume '{volume}' is not present on HostFS"
            )));
        }

        let mut components = context.current_directory.clone();
        let path_components: Vec<_> = path.split('.').collect();
        for (index, component) in path_components.iter().copied().enumerate() {
            if component.is_empty() {
                if index == 0 || index + 1 == path_components.len() {
                    continue;
                }
                return Err(fs_error("empty path component"));
            }
            match component {
                "$" => components.clear(),
                "@" => components = context.current_directory.clone(),
                "&" => components = context.user_root.clone(),
                "%" => components = context.library_directory.clone(),
                "^" => {
                    components.pop();
                }
                name => {
                    if name.contains('/') || name.contains('\\') || name.contains(':') {
                        return Err(fs_error(format!("invalid RISC OS path component '{name}'")));
                    }
                    components.push(name.to_string());
                }
            }
        }

        let root = self.canonical_root()?;
        let mut host_path = root.clone();
        let mut canonical_components = Vec::new();
        for component in &components {
            let found = self.find_child(&host_path, component)?;
            let Some(found) = found else {
                host_path.push(component);
                canonical_components.push(component.clone());
                continue;
            };
            if found.file_type()?.is_symlink() {
                return Err(fs_error("HostFS does not follow symbolic links"));
            }
            if !found.file_type()?.is_dir() && canonical_components.len() + 1 < components.len() {
                return Err(fs_error(format!("'{}' is not a directory", component)));
            }
            host_path = found.path();
            canonical_components.push(found.file_name().to_string_lossy().into_owned());
        }

        if !host_path.starts_with(&root) {
            return Err(fs_error("path resolves outside the mounted HostFS volume"));
        }

        let metadata = if host_path.is_file() {
            self.metadata_for_host_file(&host_path)?
        } else {
            None
        };
        let is_directory = host_path.is_dir();
        Ok(ResolvedPath {
            host_path,
            guest_path: canonical_components.join("."),
            metadata,
            is_directory,
        })
    }

    pub fn resolve_parent_and_leaf(
        &self,
        context: &FileSystemContext,
        path: &str,
    ) -> Result<(PathBuf, String, String), RuntimeError> {
        let (file_system, volume, path) = split_device_prefix(path);
        if let Some(file_system) = file_system
            && !self.check_file_system_name(file_system)
        {
            return Err(fs_error(format!(
                "filing system '{file_system}' is not present"
            )));
        }
        if let Some(volume) = volume
            && !self.check_volume_name(volume)
        {
            return Err(fs_error(format!(
                "volume '{volume}' is not present on HostFS"
            )));
        }
        let Some((parent_path, leaf)) = path.rsplit_once('.') else {
            if path.is_empty() {
                return Err(fs_error("a file name is required"));
            }
            let full = self.canonical_guest_path(context, path)?;
            return Ok((
                full.host_path
                    .parent()
                    .unwrap_or(&full.host_path)
                    .to_path_buf(),
                full.host_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                full.guest_path,
            ));
        };
        if leaf.is_empty() {
            return Err(fs_error("a file name is required"));
        }
        let parent = self.canonical_guest_path(context, parent_path)?;
        if !parent.is_directory {
            return Err(fs_error("parent path is not a directory"));
        }
        Ok((
            parent.host_path,
            leaf.to_string(),
            if parent.guest_path.is_empty() {
                leaf.to_string()
            } else {
                format!("{}.{}", parent.guest_path, leaf)
            },
        ))
    }

    pub fn enumerate(
        &self,
        context: &FileSystemContext,
        directory: &str,
        wildcard: &str,
    ) -> Result<Vec<GuestObject>, RuntimeError> {
        let resolved = self.canonical_guest_path(context, directory)?;
        if !resolved.is_directory {
            return Err(fs_error(format!("'{}' is not a directory", directory)));
        }
        let mut objects = Vec::new();
        let mut sidecars = BTreeMap::new();
        for entry in fs::read_dir(&resolved.host_path)? {
            let entry = entry?;
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if file_name == VOLUME_DESCRIPTOR {
                continue;
            }
            if file_name.ends_with(METADATA_SUFFIX) {
                let payload_name = file_name
                    .strip_suffix(METADATA_SUFFIX)
                    .unwrap_or_default()
                    .to_string();
                sidecars.insert(payload_name, entry.path());
                continue;
            }
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if file_name.contains('.') {
                    continue;
                }
                let meta = FileMetadata::plain_file(file_name.clone());
                if wildcard_matches(wildcard, &file_name) {
                    objects.push(GuestObject {
                        guest_name: file_name,
                        host_path: entry.path(),
                        metadata: meta,
                        is_directory: true,
                        length: 0,
                    });
                }
                continue;
            }
            if !file_type.is_file() {
                continue;
            }
            let sidecar = sidecars.get(&file_name).cloned().or_else(|| {
                let sidecar = metadata_path(&entry.path());
                sidecar.is_file().then_some(sidecar)
            });
            let metadata = match sidecar {
                Some(sidecar) => read_metadata(&sidecar)?,
                None => {
                    if file_name.contains('.') {
                        continue;
                    }
                    FileMetadata::plain_file(file_name.clone())
                }
            };
            if !valid_guest_leaf(&metadata.guest_name) {
                return Err(fs_error(format!(
                    "invalid guest filename '{}' in {}",
                    metadata.guest_name,
                    metadata_path(&entry.path()).display()
                )));
            }
            if wildcard_matches(wildcard, &metadata.guest_name) {
                let length = u32::try_from(entry.metadata()?.len()).unwrap_or(u32::MAX);
                objects.push(GuestObject {
                    guest_name: metadata.guest_name.clone(),
                    host_path: entry.path(),
                    metadata,
                    is_directory: false,
                    length,
                });
            }
        }
        objects.sort_by(|left, right| {
            left.guest_name
                .to_ascii_lowercase()
                .cmp(&right.guest_name.to_ascii_lowercase())
        });
        Ok(objects)
    }

    pub fn read_file(
        &self,
        context: &FileSystemContext,
        path: &str,
    ) -> Result<(Vec<u8>, FileMetadata), RuntimeError> {
        let resolved = self.canonical_guest_path(context, path)?;
        if resolved.is_directory {
            return Err(fs_error(format!("'{}' is a directory", path)));
        }
        let metadata = resolved
            .metadata
            .ok_or_else(|| fs_error(format!("file '{}' not found", path)))?;
        Ok((fs::read(resolved.host_path)?, metadata))
    }

    pub fn create_file(
        &self,
        context: &FileSystemContext,
        path: &str,
        metadata: FileMetadata,
    ) -> Result<ResolvedPath, RuntimeError> {
        let (parent, leaf, guest_path) = self.resolve_parent_and_leaf(context, path)?;
        if !valid_guest_leaf(&leaf) || !valid_guest_leaf(&metadata.guest_name) {
            return Err(fs_error("invalid guest filename"));
        }
        if self
            .enumerate(
                context,
                &parent_guest_path(&guest_path),
                &metadata.guest_name,
            )?
            .iter()
            .any(|object| object.guest_name.eq_ignore_ascii_case(&metadata.guest_name))
        {
            return Err(fs_error(format!(
                "'{}' already exists",
                metadata.guest_name
            )));
        }
        let target = parent.join(leaf);
        let root = self.canonical_root()?;
        let canonical_parent = parent.canonicalize()?;
        if !canonical_parent.starts_with(&root) {
            return Err(fs_error("path resolves outside the mounted HostFS volume"));
        }
        let mut file = OpenOptions::new()
            .write(true)
            .read(true)
            .create_new(true)
            .open(&target)?;
        file.flush()?;
        drop(file);
        write_metadata(&metadata_path(&target), &metadata)?;
        Ok(ResolvedPath {
            host_path: target,
            guest_path,
            metadata: Some(metadata),
            is_directory: false,
        })
    }

    pub fn create_directory(
        &self,
        context: &FileSystemContext,
        path: &str,
    ) -> Result<ResolvedPath, RuntimeError> {
        let (parent, leaf, guest_path) = self.resolve_parent_and_leaf(context, path)?;
        if !valid_guest_leaf(&leaf) {
            return Err(fs_error("invalid guest directory name"));
        }
        let root = self.canonical_root()?;
        let canonical_parent = parent.canonicalize()?;
        if !canonical_parent.starts_with(&root) {
            return Err(fs_error("path resolves outside the mounted HostFS volume"));
        }
        let target = parent.join(&leaf);
        fs::create_dir(&target)?;
        Ok(ResolvedPath {
            host_path: target,
            guest_path,
            metadata: None,
            is_directory: true,
        })
    }

    pub fn open_file(
        &self,
        context: &FileSystemContext,
        path: &str,
        read: bool,
        write: bool,
        create: bool,
    ) -> Result<(File, ResolvedPath), RuntimeError> {
        let resolved = self.canonical_guest_path(context, path)?;
        if resolved.is_directory {
            return Err(fs_error(format!("'{}' is a directory", path)));
        }
        let mut options = OpenOptions::new();
        options.read(read).write(write).create(create);
        let file = options.open(&resolved.host_path)?;
        Ok((file, resolved))
    }

    pub fn delete_file(&self, context: &FileSystemContext, path: &str) -> Result<(), RuntimeError> {
        let resolved = self.canonical_guest_path(context, path)?;
        if resolved.is_directory {
            return Err(fs_error(format!("'{}' is a directory", path)));
        }
        fs::remove_file(&resolved.host_path)?;
        let sidecar = metadata_path(&resolved.host_path);
        if sidecar.exists() {
            fs::remove_file(sidecar)?;
        }
        Ok(())
    }

    pub fn remove_directory(
        &self,
        context: &FileSystemContext,
        path: &str,
    ) -> Result<(), RuntimeError> {
        let resolved = self.canonical_guest_path(context, path)?;
        if !resolved.is_directory {
            return Err(fs_error(format!("'{}' is not a directory", path)));
        }
        fs::remove_dir(&resolved.host_path)?;
        Ok(())
    }

    pub fn set_metadata(
        &self,
        context: &FileSystemContext,
        path: &str,
        metadata: &FileMetadata,
    ) -> Result<(), RuntimeError> {
        let resolved = self.canonical_guest_path(context, path)?;
        if resolved.is_directory {
            return Err(fs_error("file metadata cannot be set on a directory"));
        }
        write_metadata(&metadata_path(&resolved.host_path), metadata)
    }

    pub fn rename(
        &self,
        context: &FileSystemContext,
        from: &str,
        to: &str,
    ) -> Result<(), RuntimeError> {
        let source = self.canonical_guest_path(context, from)?;
        let (destination_parent, leaf, _) = self.resolve_parent_and_leaf(context, to)?;
        if !valid_guest_leaf(&leaf) {
            return Err(fs_error("invalid destination filename"));
        }
        let root = self.canonical_root()?;
        let source_parent = source
            .host_path
            .parent()
            .ok_or_else(|| fs_error("invalid path"))?;
        let canonical_destination_parent = destination_parent.canonicalize()?;
        if !source_parent.starts_with(&root) || !canonical_destination_parent.starts_with(&root) {
            return Err(fs_error("path resolves outside the mounted HostFS volume"));
        }
        let destination = destination_parent.join(leaf);
        if destination.exists() {
            return Err(fs_error("destination already exists"));
        }
        let sidecar = metadata_path(&source.host_path);
        if source.is_directory {
            fs::rename(source.host_path, destination)?;
        } else {
            let old_meta = if sidecar.is_file() {
                Some(read_metadata(&sidecar)?)
            } else {
                None
            };
            fs::rename(&source.host_path, &destination)?;
            if let Some(mut metadata) = old_meta {
                metadata.guest_name = leaf_for_guest(to).to_string();
                fs::remove_file(sidecar)?;
                write_metadata(&metadata_path(&destination), &metadata)?;
            }
        }
        Ok(())
    }

    pub fn file_type_for_host_path(&self, path: &Path) -> Result<u32, RuntimeError> {
        if let Some(metadata) = self.metadata_for_host_file(path)? {
            return Ok(metadata.file_type);
        }
        Ok(FILETYPE_TEXT)
    }

    pub fn metadata_for_host_file(
        &self,
        path: &Path,
    ) -> Result<Option<FileMetadata>, RuntimeError> {
        let sidecar = metadata_path(path);
        if sidecar.is_file() {
            return read_metadata(&sidecar).map(Some);
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if name.contains('.') {
            return Ok(None);
        }
        Ok(Some(FileMetadata::plain_file(name)))
    }

    pub fn set_current_directory(
        &self,
        context: &mut FileSystemContext,
        path: &str,
    ) -> Result<(), RuntimeError> {
        let resolved = self.canonical_guest_path(context, path)?;
        if !resolved.is_directory {
            return Err(fs_error(format!("'{}' is not a directory", path)));
        }
        context.previous_directory = context.current_directory.clone();
        context.current_directory = guest_parts(&resolved.guest_path);
        Ok(())
    }

    pub fn set_library_directory(
        &self,
        context: &mut FileSystemContext,
        path: &str,
    ) -> Result<(), RuntimeError> {
        let resolved = self.canonical_guest_path(context, path)?;
        if !resolved.is_directory {
            return Err(fs_error(format!("'{}' is not a directory", path)));
        }
        context.library_directory = guest_parts(&resolved.guest_path);
        Ok(())
    }

    pub fn set_user_root(
        &self,
        context: &mut FileSystemContext,
        path: &str,
    ) -> Result<(), RuntimeError> {
        let resolved = self.canonical_guest_path(context, path)?;
        if !resolved.is_directory {
            return Err(fs_error(format!("'{}' is not a directory", path)));
        }
        context.user_root = guest_parts(&resolved.guest_path);
        Ok(())
    }

    fn find_child(
        &self,
        directory: &Path,
        guest_name: &str,
    ) -> Result<Option<fs::DirEntry>, RuntimeError> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == VOLUME_DESCRIPTOR || name.ends_with(METADATA_SUFFIX) {
                continue;
            }
            if entry.file_type()?.is_symlink() {
                continue;
            }
            if name.eq_ignore_ascii_case(guest_name) {
                return Ok(Some(entry));
            }
            if entry.file_type()?.is_file() {
                let sidecar = metadata_path(&entry.path());
                if sidecar.is_file() {
                    let metadata = read_metadata(&sidecar)?;
                    if metadata.guest_name.eq_ignore_ascii_case(guest_name) {
                        return Ok(Some(entry));
                    }
                }
            }
        }
        Ok(None)
    }
}

pub fn metadata_path(file_path: &Path) -> PathBuf {
    let mut name = file_path.as_os_str().to_os_string();
    name.push(METADATA_SUFFIX);
    PathBuf::from(name)
}

pub fn read_metadata(path: &Path) -> Result<FileMetadata, RuntimeError> {
    let contents = fs::read_to_string(path)?;
    let mut lines = contents.lines();
    if lines.next() != Some(METADATA_MAGIC) {
        return Err(fs_error(format!(
            "unsupported metadata sidecar format in {}",
            path.display()
        )));
    }
    let mut fields = BTreeMap::new();
    for line in lines {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        fields.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let guest_name = fields
        .get("guest-name")
        .map(|value| percent_decode(value))
        .transpose()?
        .ok_or_else(|| fs_error(format!("missing guest-name in {}", path.display())))?;
    if fields.get("format-version").map(String::as_str) != Some("1") {
        return Err(fs_error(format!(
            "unsupported metadata version in {}",
            path.display()
        )));
    }
    Ok(FileMetadata {
        guest_name,
        file_type: parse_hex_field(&fields, "file-type", path)?,
        load_address: parse_hex_field(&fields, "load-address", path)?,
        execution_address: parse_hex_field(&fields, "execution-address", path)?,
        attributes: parse_hex_field(&fields, "attributes", path)?,
    })
}

pub fn write_metadata(path: &Path, metadata: &FileMetadata) -> Result<(), RuntimeError> {
    if !valid_guest_leaf(&metadata.guest_name) {
        return Err(fs_error("invalid guest name in metadata"));
    }
    let temporary = path.with_extension(format!("acornmeta.tmp-{}", std::process::id()));
    let contents = format!(
        "{METADATA_MAGIC}\nformat-version=1\nguest-name={}\nfile-type=0x{:08X}\nload-address=0x{:08X}\nexecution-address=0x{:08X}\nattributes=0x{:08X}\n",
        percent_encode(&metadata.guest_name),
        metadata.file_type,
        metadata.load_address,
        metadata.execution_address,
        metadata.attributes
    );
    fs::write(&temporary, contents)?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(RuntimeError::Io(error))
        }
    }
}

pub fn wildcard_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase().into_bytes();
    let value = value.to_ascii_lowercase().into_bytes();
    let mut current = vec![false; value.len() + 1];
    current[0] = true;
    for token in pattern {
        let mut next = vec![false; value.len() + 1];
        match token {
            b'*' => {
                next[0] = current[0];
                for index in 1..=value.len() {
                    next[index] = current[index] || next[index - 1];
                }
            }
            b'#' => {
                for index in 1..=value.len() {
                    next[index] = current[index - 1];
                }
            }
            literal => {
                for index in 1..=value.len() {
                    next[index] = current[index - 1] && value[index - 1] == literal;
                }
            }
        }
        current = next;
    }
    current[value.len()]
}

pub fn file_for_read(path: &Path) -> Result<File, RuntimeError> {
    Ok(File::open(path)?)
}

pub fn seek_file(file: &mut File, position: u64) -> Result<u64, RuntimeError> {
    Ok(file.seek(SeekFrom::Start(position))?)
}

pub fn read_bytes(file: &mut File, buffer: &mut [u8]) -> Result<usize, RuntimeError> {
    Ok(file.read(buffer)?)
}

pub fn write_bytes(file: &mut File, buffer: &[u8]) -> Result<usize, RuntimeError> {
    Ok(file.write(buffer)?)
}

fn split_device_prefix(path: &str) -> (Option<&str>, Option<&str>, &str) {
    let Some((prefix, remainder)) = path.split_once(':') else {
        return (None, None, path);
    };
    if prefix.is_empty() || prefix.contains('.') {
        return (None, None, path);
    }
    let (volume, remainder) = if let Some(after_colon) = remainder.strip_prefix(':') {
        let (volume, path) = after_colon.split_once('.').unwrap_or((after_colon, ""));
        (Some(volume), path)
    } else {
        (None, remainder)
    };
    (Some(prefix), volume, remainder)
}

fn read_volume_name(root: &Path) -> Option<String> {
    let descriptor = fs::read_to_string(root.join(VOLUME_DESCRIPTOR)).ok()?;
    let mut lines = descriptor.lines();
    if lines.next()? != VOLUME_MAGIC {
        return None;
    }
    lines.find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "volume-name" && !value.trim().is_empty()).then(|| value.trim().to_string())
    })
}

fn parent_guest_path(full_path: &str) -> &str {
    full_path
        .rsplit_once('.')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

fn leaf_for_guest(path: &str) -> &str {
    path.rsplit('.').next().unwrap_or(path)
}

fn guest_parts(path: &str) -> Vec<String> {
    if path.is_empty() {
        Vec::new()
    } else {
        path.split('.').map(str::to_string).collect()
    }
}

fn valid_guest_leaf(name: &str) -> bool {
    !name.is_empty()
        && name != "$"
        && name != "@"
        && name != "&"
        && name != "%"
        && name != "^"
        && !name.contains('.')
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains(':')
        && name != "."
        && name != ".."
}

fn parse_hex_field(
    fields: &BTreeMap<String, String>,
    name: &str,
    path: &Path,
) -> Result<u32, RuntimeError> {
    let value = fields
        .get(name)
        .ok_or_else(|| fs_error(format!("missing {name} in {}", path.display())))?;
    let value = value.strip_prefix("0x").unwrap_or(value);
    u32::from_str_radix(value, 16)
        .map_err(|_| fs_error(format!("invalid {name} in {}", path.display())))
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b" _-!()[]{}~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn percent_decode(value: &str) -> Result<String, RuntimeError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let digits = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| fs_error("invalid percent-escaped guest name"))?;
            let digits = std::str::from_utf8(digits)
                .map_err(|_| fs_error("invalid percent-escaped guest name"))?;
            decoded.push(
                u8::from_str_radix(digits, 16)
                    .map_err(|_| fs_error("invalid percent-escaped guest name"))?,
            );
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| fs_error("guest filename is not valid UTF-8"))
}

fn fs_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::Program(message.into())
}
