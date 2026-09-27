//! Persistent host-backed equivalents of MOS `*CONFIGURE` settings.

use std::{
    collections::BTreeSet,
    env, fs,
    io::Write,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::graphics::GraphicsProfile;

const CONFIG_HEADER: &str = "# Acorn-2026 MOS configuration v1";
static CONFIGURE_LOCK: Mutex<()> = Mutex::new(());
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BasicLanguageMode {
    Classic,
    Basic64,
    #[default]
    Hybrid,
}

impl BasicLanguageMode {
    fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("CLASSIC") {
            Some(Self::Classic)
        } else if value.eq_ignore_ascii_case("BASIC64") {
            Some(Self::Basic64)
        } else if value.eq_ignore_ascii_case("HYBRID") {
            Some(Self::Hybrid)
        } else {
            None
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Classic => "CLASSIC",
            Self::Basic64 => "BASIC64",
            Self::Hybrid => "HYBRID",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BasicEngine {
    #[default]
    Interpreter,
    HybridJit,
    StrictJit,
}

impl BasicEngine {
    fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("INTERPRETER") {
            Some(Self::Interpreter)
        } else if value.eq_ignore_ascii_case("HYBRID")
            || value.eq_ignore_ascii_case("HYBRIDJIT")
            || value.eq_ignore_ascii_case("HYBRID-JIT")
        {
            Some(Self::HybridJit)
        } else if value.eq_ignore_ascii_case("STRICT")
            || value.eq_ignore_ascii_case("STRICTJIT")
            || value.eq_ignore_ascii_case("STRICT-JIT")
        {
            Some(Self::StrictJit)
        } else {
            None
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Interpreter => "INTERPRETER",
            Self::HybridJit => "HYBRID-JIT",
            Self::StrictJit => "STRICT-JIT",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BasicConfiguration {
    /// `None` means use a source `REM @BASIC64` field, then the runtime default.
    pub(crate) language: Option<BasicLanguageMode>,
    pub(crate) profile: Option<String>,
    pub(crate) target: Option<GraphicsProfile>,
    pub(crate) engine: BasicEngine,
}

impl BasicConfiguration {
    pub(crate) fn set(&mut self, option: &str, value: &str) -> Result<&'static str, String> {
        if option.eq_ignore_ascii_case("BASICMODE") {
            self.language = if value.eq_ignore_ascii_case("AUTO") {
                None
            } else {
                Some(BasicLanguageMode::parse(value).ok_or_else(|| {
                    "BASICMode must be Auto, Classic, BASIC64, or Hybrid".to_string()
                })?)
            };
            Ok("BASICMode")
        } else if option.eq_ignore_ascii_case("BASICPROFILE") {
            self.profile = if value.eq_ignore_ascii_case("AUTO") {
                None
            } else {
                validate_profile(value)?;
                Some(value.to_string())
            };
            Ok("BASICProfile")
        } else if option.eq_ignore_ascii_case("BASICTARGET") {
            self.target = if value.eq_ignore_ascii_case("AUTO") {
                None
            } else if value.eq_ignore_ascii_case("HOSTED") || value.eq_ignore_ascii_case("RISCOS") {
                Some(GraphicsProfile::Hosted)
            } else if value.eq_ignore_ascii_case("AGON") {
                Some(GraphicsProfile::Agon)
            } else {
                return Err("BASICTarget must be Auto, Hosted, RISCOS, or Agon".into());
            };
            Ok("BASICTarget")
        } else if option.eq_ignore_ascii_case("BASICENGINE") {
            self.engine = BasicEngine::parse(value).ok_or_else(|| {
                "BASICEngine must be Interpreter, HybridJIT, or StrictJIT".to_string()
            })?;
            Ok("BASICEngine")
        } else {
            Err(format!(
                "unknown *CONFIGURE option '{option}'; use *CONFIGURE to list BASIC settings"
            ))
        }
    }

    pub(crate) fn status_value(&self, option: &str) -> Option<(&'static str, String)> {
        if option.eq_ignore_ascii_case("BASICMODE") {
            Some((
                "BASICMode",
                self.language
                    .map_or("AUTO", BasicLanguageMode::as_str)
                    .to_string(),
            ))
        } else if option.eq_ignore_ascii_case("BASICPROFILE") {
            Some((
                "BASICProfile",
                self.profile.clone().unwrap_or_else(|| "AUTO".into()),
            ))
        } else if option.eq_ignore_ascii_case("BASICTARGET") {
            Some((
                "BASICTarget",
                match self.target {
                    None => "AUTO",
                    Some(GraphicsProfile::Hosted) => "HOSTED",
                    Some(GraphicsProfile::Agon) => "AGON",
                }
                .into(),
            ))
        } else if option.eq_ignore_ascii_case("BASICENGINE") {
            Some(("BASICEngine", self.engine.as_str().into()))
        } else {
            None
        }
    }

    pub(crate) fn status_entries(&self) -> [(&'static str, String); 4] {
        [
            self.status_value("BASICMODE").expect("known option"),
            self.status_value("BASICPROFILE").expect("known option"),
            self.status_value("BASICTARGET").expect("known option"),
            self.status_value("BASICENGINE").expect("known option"),
        ]
    }

    fn serialized(&self) -> String {
        let mut contents = format!("{CONFIG_HEADER}\n");
        for (name, value) in self.status_entries() {
            contents.push_str(name);
            contents.push('=');
            contents.push_str(&value);
            contents.push('\n');
        }
        contents
    }

    fn parse(contents: &str) -> Result<Self, String> {
        let mut configuration = Self::default();
        let mut seen = BTreeSet::new();
        for (line_index, line) in contents.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (option, value) = line
                .split_once('=')
                .ok_or_else(|| format!("invalid BASIC configuration at line {}", line_index + 1))?;
            let option = option.trim();
            let key = option.to_ascii_uppercase();
            if !seen.insert(key) {
                return Err(format!(
                    "duplicate BASIC configuration option at line {}",
                    line_index + 1
                ));
            }
            configuration
                .set(option, value.trim())
                .map_err(|error| format!("configuration line {}: {error}", line_index + 1))?;
        }
        Ok(configuration)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ConfigureStore {
    path: Option<PathBuf>,
}

impl Default for ConfigureStore {
    fn default() -> Self {
        Self {
            path: default_config_path(),
        }
    }
}

impl ConfigureStore {
    #[cfg(test)]
    pub(crate) fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
        }
    }

    pub(crate) fn load(&self) -> Result<BasicConfiguration, String> {
        let Some(path) = &self.path else {
            return Ok(BasicConfiguration::default());
        };
        match fs::read_to_string(path) {
            Ok(contents) => BasicConfiguration::parse(&contents)
                .map_err(|error| format!("could not read {}: {error}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(BasicConfiguration::default())
            }
            Err(error) => Err(format!("could not read {}: {error}", path.display())),
        }
    }

    pub(crate) fn set(&self, option: &str, value: &str) -> Result<BasicConfiguration, String> {
        let _lock = CONFIGURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut configuration = self.load()?;
        configuration.set(option, value)?;
        self.save(&configuration)?;
        Ok(configuration)
    }

    pub(crate) fn reset(&self) -> Result<BasicConfiguration, String> {
        let _lock = CONFIGURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let configuration = BasicConfiguration::default();
        self.save(&configuration)?;
        Ok(configuration)
    }

    fn save(&self, configuration: &BasicConfiguration) -> Result<(), String> {
        let path = self.path.as_ref().ok_or_else(|| {
            "no user configuration directory is available; set ACORN_CONFIG_PATH".to_string()
        })?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }

        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = path.as_os_str().to_os_string();
        temporary_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
        let temporary_path = PathBuf::from(temporary_name);
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary_path)
                .map_err(|error| {
                    format!("could not create {}: {error}", temporary_path.display())
                })?;
            file.write_all(configuration.serialized().as_bytes())
                .map_err(|error| {
                    format!("could not write {}: {error}", temporary_path.display())
                })?;
            file.sync_all()
                .map_err(|error| format!("could not sync {}: {error}", temporary_path.display()))?;
            fs::rename(&temporary_path, path)
                .map_err(|error| format!("could not replace {}: {error}", path.display()))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary_path);
        }
        result
    }
}

fn default_config_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("ACORN_CONFIG_PATH") {
        return Some(PathBuf::from(path));
    }

    #[cfg(target_os = "macos")]
    {
        return env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Acorn-2026")
                .join("configure")
        });
    }

    #[cfg(target_os = "windows")]
    {
        return env::var_os("APPDATA")
            .map(|app_data| PathBuf::from(app_data).join("Acorn-2026").join("configure"));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
            return Some(
                PathBuf::from(config_home)
                    .join("acorn-2026")
                    .join("configure"),
            );
        }
        env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join(".config")
                .join("acorn-2026")
                .join("configure")
        })
    }
}

fn validate_profile(profile: &str) -> Result<(), String> {
    if profile.is_empty()
        || !profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("BASICProfile must be a single name or Auto".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BasicConfiguration, ConfigureStore};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEMP_PATH: AtomicU64 = AtomicU64::new(0);

    fn temporary_path() -> PathBuf {
        let sequence = NEXT_TEMP_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "acorn-2026-configure-test-{}-{sequence}",
            std::process::id()
        ))
    }

    #[test]
    fn configure_store_persists_supported_options_and_defaults() {
        let path = temporary_path();
        let store = ConfigureStore::with_path(&path);

        let configured = store.set("BASICEngine", "Strict").unwrap();
        assert_eq!(configured.engine.as_str(), "STRICT-JIT");
        store.set("BASICMode", "Classic").unwrap();
        store.set("BASICProfile", "BBCV-1.05").unwrap();
        store.set("BASICTarget", "Agon").unwrap();

        let reloaded = store.load().unwrap();
        assert_eq!(reloaded, store.load().unwrap());
        assert_eq!(
            reloaded.status_value("BASICEngine").unwrap().1,
            "STRICT-JIT"
        );
        assert_eq!(reloaded.status_value("BASICMode").unwrap().1, "CLASSIC");
        assert_eq!(
            reloaded.status_value("BASICProfile").unwrap().1,
            "BBCV-1.05"
        );
        assert_eq!(reloaded.status_value("BASICTarget").unwrap().1, "AGON");

        store.reset().unwrap();
        assert_eq!(store.load().unwrap(), BasicConfiguration::default());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn configure_store_rejects_invalid_or_ambiguous_values() {
        let path = temporary_path();
        let store = ConfigureStore::with_path(&path);
        assert!(store.set("BASICEngine", "maybe").is_err());
        assert!(store.set("BASICMode", "Strict").is_err());
        assert!(store.set("BASICProfile", "bad profile").is_err());
        assert!(store.set("SomethingElse", "value").is_err());
        assert!(!path.exists());
    }
}
