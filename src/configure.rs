//! Persistent host-backed equivalents of MOS `*CONFIGURE` settings.

use std::{
    collections::BTreeSet,
    env, fs,
    io::{ErrorKind, Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::display::{DesktopResolution, DisplayColour, DisplaySettings};
use crate::graphics::GraphicsProfile;

const CONFIG_HEADER: &str = "# Ricochet MOS configuration v3";
const CONFIG_V1_HEADER: &str = "# Ricochet MOS configuration v1";
const CONFIG_V2_HEADER: &str = "# Ricochet MOS configuration v2";
const LEGACY_CONFIG_V1_HEADER: &str = "# Acorn-2026 MOS configuration v1";
const LEGACY_CONFIG_V2_HEADER: &str = "# Acorn-2026 MOS configuration v2";
const LEGACY_CONFIG_V3_HEADER: &str = "# Acorn-2026 MOS configuration v3";
/// Keep startup parsing and recovery memory bounded even if the selected file
/// is corrupt or unexpectedly large. Valid configuration rows need far less.
const MAX_STORED_CONFIGURATION_BYTES: usize = 64 * 1024;
static CONFIGURE_LOCK: Mutex<()> = Mutex::new(());
static TEMPORARY_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Cause class for a persisted file that could not be used at startup. These
/// numeric values cross only the private BASIC64/host primitive boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConfigurationRecoveryKind {
    Malformed = 1,
    UnsupportedVersion = 2,
    UnsupportedSchema = 3,
    InvalidUtf8 = 4,
    Unreadable = 5,
    Oversized = 6,
}

impl ConfigurationRecoveryKind {
    pub(crate) fn code(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Debug)]
struct ConfigurationRecovery {
    kind: ConfigurationRecoveryKind,
    effective: BasicConfiguration,
    original_bytes: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default)]
struct ConfigureSession {
    recovery: Option<ConfigurationRecovery>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum StartupLanguage {
    #[default]
    Mos,
    Desktop,
}

impl StartupLanguage {
    fn module_number(self) -> &'static str {
        match self {
            Self::Mos => "0",
            Self::Desktop => "3",
        }
    }
}

/// The hosted subset of RISC OS `*Configure WimpMode` / `*Configure Mode`.
/// `Auto` follows the host content size. Fixed selectors are limited to the
/// logical desktop sizes and output palettes supported by this runtime.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum WimpMode {
    #[default]
    Auto,
    Fixed {
        resolution: DesktopResolution,
        colour: DisplayColour,
    },
}

impl WimpMode {
    fn parse(value: &str) -> Result<Self, String> {
        if value.trim().eq_ignore_ascii_case("AUTO") {
            return Ok(Self::Auto);
        }

        let fields = value.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err("WimpMode must be Auto or `X<width> Y<height> C/G<depth>`; numeric modes, EX/EY scaling, and frame-rate selectors are unsupported".into());
        }

        let width = parse_mode_dimension(fields[0], 'X');
        let height = parse_mode_dimension(fields[1], 'Y');
        let (Some(width), Some(height)) = (width, height) else {
            return Err("WimpMode requires X<width> followed by Y<height>".into());
        };
        let resolution = DesktopResolution::ALL
            .into_iter()
            .find(|resolution| resolution.fixed_size() == Some((width, height)))
            .ok_or_else(|| {
                format!(
                    "WimpMode size X{width} Y{height} is unsupported; available sizes are 640x480, 800x600, 1024x768, 1152x864, 1280x1024, and 1600x1200"
                )
            })?;
        let colour = parse_mode_colour(fields[2]).ok_or_else(|| {
            "WimpMode colour must be C2, C16, C256, C32K, C16M, G4, G16, or G256; this hosted renderer has no C4, C64, or C32T mode".to_string()
        })?;
        Ok(Self::Fixed { resolution, colour })
    }

    fn as_value(self) -> String {
        match self {
            Self::Auto => "AUTO".into(),
            Self::Fixed { resolution, colour } => {
                let (width, height) = resolution
                    .fixed_size()
                    .expect("fixed WimpMode has a fixed resolution");
                format!("X{width} Y{height} {}", mode_colour_token(colour))
            }
        }
    }
}

fn parse_mode_dimension(value: &str, prefix: char) -> Option<u32> {
    let (actual_prefix, digits) = value.split_at_checked(1)?;
    if !actual_prefix.eq_ignore_ascii_case(&prefix.to_string())
        || !(3..=4).contains(&digits.len())
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    digits.parse().ok()
}

fn parse_mode_colour(value: &str) -> Option<DisplayColour> {
    match value.to_ascii_uppercase().as_str() {
        "C2" => Some(DisplayColour::BW),
        "C16" => Some(DisplayColour::Colour16),
        "C256" => Some(DisplayColour::Colour256),
        "C32K" => Some(DisplayColour::Rgb555),
        "C16M" => Some(DisplayColour::Rgb888),
        "G4" => Some(DisplayColour::Grey4),
        "G16" => Some(DisplayColour::Grey16),
        "G256" => Some(DisplayColour::Grey256),
        _ => None,
    }
}

fn mode_colour_token(colour: DisplayColour) -> &'static str {
    match colour {
        DisplayColour::BW => "C2",
        DisplayColour::Grey4 => "G4",
        DisplayColour::Grey16 => "G16",
        DisplayColour::Colour16 => "C16",
        DisplayColour::Grey256 => "G256",
        DisplayColour::Colour256 => "C256",
        DisplayColour::Rgb555 => "C32K",
        DisplayColour::Rgb888 => "C16M",
    }
}

fn parse_riscos_number(value: &str) -> Option<u32> {
    if let Some(hex) = value.strip_prefix('&') {
        if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        return u32::from_str_radix(hex, 16).ok();
    }

    if let Some((base, digits)) = value.split_once('_') {
        if base.is_empty() || digits.is_empty() || !base.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let base = base.parse::<u32>().ok()?;
        if !(2..=36).contains(&base) {
            return None;
        }
        let mut result = 0_u32;
        for digit in digits.chars() {
            let digit = digit.to_digit(36)?;
            if digit >= base {
                return None;
            }
            result = result.checked_mul(base)?.checked_add(digit)?;
        }
        return Some(result);
    }

    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

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
        } else if value.eq_ignore_ascii_case("HYBRID") {
            Some(Self::HybridJit)
        } else if value.eq_ignore_ascii_case("STRICT") {
            Some(Self::StrictJit)
        } else {
            None
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Interpreter => "INTERPRETER",
            Self::HybridJit => "HYBRID",
            Self::StrictJit => "STRICT",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct BasicConfiguration {
    /// RISC OS `Language` module number: 0 selects the MOS prompt, 3 the desktop.
    pub(crate) startup_language: StartupLanguage,
    /// `None` means use a source `REM @BASIC64` field, then the runtime default.
    pub(crate) language: Option<BasicLanguageMode>,
    pub(crate) profile: Option<String>,
    pub(crate) target: Option<GraphicsProfile>,
    pub(crate) engine: BasicEngine,
    pub(crate) display: DisplaySettings,
    wimp_mode: WimpMode,
}

impl BasicConfiguration {
    pub(crate) fn set(&mut self, option: &str, value: &str) -> Result<&'static str, String> {
        if option.eq_ignore_ascii_case("LANGUAGE") {
            self.startup_language = match parse_riscos_number(value) {
                Some(0) => StartupLanguage::Mos,
                Some(3) => StartupLanguage::Desktop,
                _ => return Err("Language must select module 0 (MOS prompt) or 3 (desktop)".into()),
            };
            Ok("Language")
        } else if option.eq_ignore_ascii_case("WIMPMODE") || option.eq_ignore_ascii_case("MODE") {
            self.wimp_mode = WimpMode::parse(value)?;
            self.display.resolution = match self.wimp_mode {
                WimpMode::Auto => DesktopResolution::Window,
                WimpMode::Fixed { resolution, .. } => resolution,
            };
            self.display.colour = match self.wimp_mode {
                WimpMode::Auto => DisplayColour::Rgb888,
                WimpMode::Fixed { colour, .. } => colour,
            };
            Ok("WimpMode")
        } else if option.eq_ignore_ascii_case("BASICMODE") {
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
            self.engine = BasicEngine::parse(value)
                .ok_or_else(|| "BASICEngine must be Interpreter, Hybrid, or Strict".to_string())?;
            Ok("BASICEngine")
        } else {
            Err(format!(
                "unknown *CONFIGURE option '{option}'; use *CONFIGURE to list supported settings"
            ))
        }
    }

    pub(crate) fn status_value(&self, option: &str) -> Option<(&'static str, String)> {
        if option.eq_ignore_ascii_case("LANGUAGE") {
            Some(("Language", self.startup_language.module_number().into()))
        } else if option.eq_ignore_ascii_case("WIMPMODE") || option.eq_ignore_ascii_case("MODE") {
            Some(("WimpMode", self.wimp_mode.as_value()))
        } else if option.eq_ignore_ascii_case("BASICMODE") {
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

    pub(crate) fn status_entries(&self) -> [(&'static str, String); 6] {
        [
            self.status_value("LANGUAGE").expect("known option"),
            self.status_value("BASICMODE").expect("known option"),
            self.status_value("BASICPROFILE").expect("known option"),
            self.status_value("BASICTARGET").expect("known option"),
            self.status_value("BASICENGINE").expect("known option"),
            self.status_value("WIMPMODE").expect("known option"),
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
        Self::parse_with_completeness(contents, false)
    }

    fn parse_stored(contents: &str) -> Result<Self, (ConfigurationRecoveryKind, String)> {
        let mut headers = contents.lines().filter_map(|line| {
            let line = line.trim();
            let lowered = line.to_ascii_lowercase();
            (lowered.starts_with("# ricochet mos configuration")
                || lowered.starts_with("# acorn-2026 mos configuration"))
            .then_some(line)
        });
        let first_header = headers.next();
        if headers.next().is_some() {
            return Err((
                ConfigurationRecoveryKind::Malformed,
                "configuration file contains conflicting or duplicate version headers".into(),
            ));
        }
        let version = match first_header {
            None => None,
            Some(header)
                if header.eq_ignore_ascii_case(CONFIG_V1_HEADER)
                    || header.eq_ignore_ascii_case(LEGACY_CONFIG_V1_HEADER) =>
            {
                Some(1)
            }
            Some(header)
                if header.eq_ignore_ascii_case(CONFIG_V2_HEADER)
                    || header.eq_ignore_ascii_case(LEGACY_CONFIG_V2_HEADER) =>
            {
                Some(2)
            }
            Some(header)
                if header.eq_ignore_ascii_case(CONFIG_HEADER)
                    || header.eq_ignore_ascii_case(LEGACY_CONFIG_V3_HEADER) =>
            {
                Some(3)
            }
            Some(_) => {
                return Err((
                    ConfigurationRecoveryKind::UnsupportedVersion,
                    "configuration file declares an unsupported format version".into(),
                ));
            }
        };

        let has_setting = contents.lines().any(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with('#')
        });
        if !has_setting {
            return Err((
                ConfigurationRecoveryKind::Malformed,
                "configuration file contains no settings; it may be truncated".into(),
            ));
        }

        let parsed = if version == Some(3) {
            Self::parse_complete(contents)
        } else {
            Self::parse(contents)
        };
        parsed.map_err(|diagnostic| {
            let kind = if diagnostic.contains("unknown *CONFIGURE option")
                || diagnostic.contains("obsolete option")
            {
                ConfigurationRecoveryKind::UnsupportedSchema
            } else {
                ConfigurationRecoveryKind::Malformed
            };
            (kind, diagnostic)
        })
    }

    fn parse_complete(contents: &str) -> Result<Self, String> {
        Self::parse_with_completeness(contents, true)
    }

    fn parse_with_completeness(contents: &str, require_all: bool) -> Result<Self, String> {
        let mut configuration = Self::default();
        let mut seen = BTreeSet::new();
        let mut legacy_resolution = None;
        let mut legacy_colour = None;
        let mut configured_wimp_mode = None;
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
            let canonical_key = if key == "MODE" {
                "WIMPMODE".to_string()
            } else {
                key.clone()
            };
            if !seen.insert(canonical_key) {
                return Err(format!(
                    "duplicate BASIC configuration option at line {}",
                    line_index + 1
                ));
            }
            let value = value.trim();
            if require_all
                && matches!(
                    key.as_str(),
                    "WINDOWFURNITURE"
                        | "DISPLAYRESOLUTION"
                        | "DISPLAYCOLOUR"
                        | "RICOCHETOUTPUTPROFILE"
                        | "TRELLISOUTPUTPROFILE"
                )
            {
                return Err(format!(
                    "configuration reset payload contains obsolete option at line {}",
                    line_index + 1
                ));
            }
            match key.as_str() {
                // Migration-only legacy option. Both old values are discarded;
                // no runtime or rendering path consults this key anymore.
                "WINDOWFURNITURE" => {}
                "DISPLAYRESOLUTION" => {
                    legacy_resolution = Some(DesktopResolution::parse(value).ok_or_else(|| {
                        format!(
                            "configuration line {}: invalid legacy DisplayResolution",
                            line_index + 1
                        )
                    })?);
                }
                "DISPLAYCOLOUR" => {
                    legacy_colour = Some(DisplayColour::parse(value).ok_or_else(|| {
                        format!(
                            "configuration line {}: invalid legacy DisplayColour",
                            line_index + 1
                        )
                    })?);
                }
                "WIMPMODE" | "MODE" => configured_wimp_mode = Some(value.to_string()),
                // v2 renderer-profile rows are accepted only to migrate old
                // files. A WimpMode selector (or a v1 display pair) is the
                // single source of truth for both size and colour depth.
                "RICOCHETOUTPUTPROFILE" | "TRELLISOUTPUTPROFILE" => {}
                _ => {
                    configuration.set(option, value).map_err(|error| {
                        format!("configuration line {}: {error}", line_index + 1)
                    })?;
                }
            }
        }
        if let Some(value) = configured_wimp_mode {
            configuration
                .set("WimpMode", &value)
                .map_err(|error| format!("invalid configured WimpMode: {error}"))?;
        } else if legacy_resolution.is_some() || legacy_colour.is_some() {
            let resolution = legacy_resolution.unwrap_or_default();
            configuration.set_display_settings(DisplaySettings {
                resolution,
                colour: if resolution == DesktopResolution::Window {
                    DisplayColour::Rgb888
                } else {
                    legacy_colour.unwrap_or_default()
                },
            })?;
        }
        if require_all {
            for option in [
                "LANGUAGE",
                "BASICMODE",
                "BASICPROFILE",
                "BASICTARGET",
                "BASICENGINE",
                "WIMPMODE",
            ] {
                if !seen.contains(option) {
                    return Err(format!("configuration reset payload is missing {option}"));
                }
            }
        }
        Ok(configuration)
    }

    fn set_display_settings(&mut self, settings: DisplaySettings) -> Result<(), String> {
        if settings.resolution == DesktopResolution::Window
            && settings.colour != DisplayColour::Rgb888
        {
            return Err("WimpMode Auto uses the full-colour C16M/Rgb888 desktop palette".into());
        }
        self.display = DisplaySettings {
            resolution: settings.resolution,
            colour: if settings.resolution == DesktopResolution::Window {
                DisplayColour::Rgb888
            } else {
                settings.colour
            },
        };
        self.wimp_mode = match settings.resolution {
            DesktopResolution::Window => WimpMode::Auto,
            resolution => WimpMode::Fixed {
                resolution,
                colour: settings.colour,
            },
        };
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ConfigureStore {
    path: Option<PathBuf>,
    session: Arc<Mutex<ConfigureSession>>,
}

impl Default for ConfigureStore {
    fn default() -> Self {
        Self {
            path: default_config_path(),
            session: Arc::new(Mutex::new(ConfigureSession::default())),
        }
    }
}

impl ConfigureStore {
    pub(crate) fn with_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            session: Arc::new(Mutex::new(ConfigureSession::default())),
        }
    }

    pub(crate) fn load(&self) -> Result<BasicConfiguration, String> {
        let _lock = CONFIGURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.load_locked()
    }

    fn load_locked(&self) -> Result<BasicConfiguration, String> {
        if let Some(recovery) = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recovery
            .clone()
        {
            return Ok(recovery.effective);
        }

        let Some(path) = &self.path else {
            return Ok(BasicConfiguration::default());
        };
        match read_configuration_file(path) {
            Ok(Some(bytes)) => {
                let original_bytes = bytes.clone();
                let contents = match String::from_utf8(bytes) {
                    Ok(contents) => contents,
                    Err(_) => {
                        self.latch_recovery(ConfigurationRecovery {
                            kind: ConfigurationRecoveryKind::InvalidUtf8,
                            effective: BasicConfiguration::default(),
                            original_bytes: Some(original_bytes),
                        });
                        return Ok(BasicConfiguration::default());
                    }
                };
                match BasicConfiguration::parse_stored(&contents) {
                    Ok(configuration) => Ok(configuration),
                    Err((kind, _diagnostic)) => {
                        self.latch_recovery(ConfigurationRecovery {
                            kind,
                            effective: BasicConfiguration::default(),
                            original_bytes: Some(original_bytes),
                        });
                        Ok(BasicConfiguration::default())
                    }
                }
            }
            Ok(None) => {
                self.latch_recovery(ConfigurationRecovery {
                    kind: ConfigurationRecoveryKind::Oversized,
                    effective: BasicConfiguration::default(),
                    original_bytes: None,
                });
                Ok(BasicConfiguration::default())
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(BasicConfiguration::default()),
            Err(_error) => {
                self.latch_recovery(ConfigurationRecovery {
                    kind: ConfigurationRecoveryKind::Unreadable,
                    effective: BasicConfiguration::default(),
                    original_bytes: None,
                });
                Ok(BasicConfiguration::default())
            }
        }
    }

    fn latch_recovery(&self, recovery: ConfigurationRecovery) {
        let mut session = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        session.recovery.get_or_insert(recovery);
    }

    pub(crate) fn recovery_code(&self) -> u32 {
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recovery
            .as_ref()
            .map_or(0, |recovery| recovery.kind.code())
    }

    pub(crate) fn set(&self, option: &str, value: &str) -> Result<BasicConfiguration, String> {
        self.set_with_recovery(option, value)
            .map(|(configuration, _)| configuration)
    }

    pub(crate) fn set_with_recovery(
        &self,
        option: &str,
        value: &str,
    ) -> Result<(BasicConfiguration, bool), String> {
        let _lock = CONFIGURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut configuration = self.load_locked()?;
        configuration.set(option, value)?;
        self.save_and_clear_recovery(&configuration)
    }

    pub(crate) fn set_display_settings(
        &self,
        settings: DisplaySettings,
    ) -> Result<BasicConfiguration, String> {
        self.set_display_settings_with_recovery(settings)
            .map(|(configuration, _)| configuration)
    }

    pub(crate) fn set_display_settings_with_recovery(
        &self,
        settings: DisplaySettings,
    ) -> Result<(BasicConfiguration, bool), String> {
        let _lock = CONFIGURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut configuration = self.load_locked()?;
        configuration.set_display_settings(settings)?;
        self.save_and_clear_recovery(&configuration)
    }

    /// Atomically replace all settings from BASIC64's explicit reset payload.
    /// The parser is a defensive schema boundary: callers own policy and must
    /// provide every supported key exactly once before anything is persisted.
    pub(crate) fn replace_from_payload(&self, payload: &str) -> Result<BasicConfiguration, String> {
        self.replace_from_payload_with_recovery(payload)
            .map(|(configuration, _)| configuration)
    }

    pub(crate) fn replace_from_payload_with_recovery(
        &self,
        payload: &str,
    ) -> Result<(BasicConfiguration, bool), String> {
        let _lock = CONFIGURE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // An explicit DEFAULTS operation must not bypass damaged-source
        // detection merely because STATUS has not run in this session.
        let _ = self.load_locked()?;
        let configuration = BasicConfiguration::parse_complete(payload)?;
        self.save_and_clear_recovery(&configuration)
    }

    fn save_and_clear_recovery(
        &self,
        configuration: &BasicConfiguration,
    ) -> Result<(BasicConfiguration, bool), String> {
        let recovery = self
            .session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recovery
            .clone();
        let recovery_copy_created = self.save(configuration, recovery.as_ref())?;
        self.session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recovery = None;
        Ok((configuration.clone(), recovery_copy_created))
    }

    fn save(
        &self,
        configuration: &BasicConfiguration,
        recovery: Option<&ConfigurationRecovery>,
    ) -> Result<bool, String> {
        let path = self.path.as_ref().ok_or_else(|| {
            "no user configuration directory is available; set RICOCHET_CONFIG_PATH".to_string()
        })?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create configuration directory: {error}"))?;
        }

        let recovery_copy_created = if let Some(recovery) = recovery {
            self.preserve_recovery_source(path, recovery)?
        } else {
            false
        };

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
                    format!("could not create configuration temporary file: {error}")
                })?;
            file.write_all(configuration.serialized().as_bytes())
                .map_err(|error| {
                    format!("could not write configuration temporary file: {error}")
                })?;
            file.sync_all()
                .map_err(|error| format!("could not sync configuration temporary file: {error}"))?;
            fs::rename(&temporary_path, path)
                .map_err(|error| format!("could not replace configuration file: {error}"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary_path);
        }
        result.map(|()| recovery_copy_created)
    }

    fn preserve_recovery_source(
        &self,
        path: &std::path::Path,
        recovery: &ConfigurationRecovery,
    ) -> Result<bool, String> {
        let mut current_is_oversized = false;
        let current_bytes = match read_configuration_file(path) {
            Ok(Some(bytes)) => Some(bytes),
            Ok(None) => {
                current_is_oversized = true;
                None
            }
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                return Err(format!(
                    "cannot safely repair configuration while its current contents are unreadable ({:?}); no settings were changed",
                    error.kind()
                ));
            }
        };

        let mut to_preserve = Vec::new();
        if let Some(original) = &recovery.original_bytes {
            if current_bytes.as_ref() != Some(original) {
                to_preserve.push(original.as_slice());
            }
        }
        if let Some(current) = current_bytes.as_deref()
            && !to_preserve.iter().any(|bytes| *bytes == current)
        {
            to_preserve.push(current);
        }
        let mut copy_created = false;
        for bytes in to_preserve {
            create_recovery_copy(path, bytes)?;
            copy_created = true;
        }
        if current_is_oversized {
            create_recovery_file_copy(path)?;
            copy_created = true;
        }
        Ok(copy_created)
    }
}

/// `Some(bytes)` is a complete bounded file; `None` means it exceeded the
/// configured limit. The reader consumes at most one byte beyond the limit.
fn read_configuration_file(path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
    configuration_file_metadata(path)?;
    let file = fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(MAX_STORED_CONFIGURATION_BYTES + 1);
    file.take((MAX_STORED_CONFIGURATION_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_STORED_CONFIGURATION_BYTES {
        Ok(None)
    } else {
        Ok(Some(bytes))
    }
}

fn configuration_file_metadata(path: &std::path::Path) -> std::io::Result<std::fs::Metadata> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            // A dangling symlink is not the same as an absent first-run file:
            // do not silently replace an unexpected filesystem entry.
            if fs::symlink_metadata(path).is_ok() {
                return Err(std::io::Error::new(
                    ErrorKind::InvalidInput,
                    "configuration path is not a readable regular file",
                ));
            }
            return Err(error);
        }
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "configuration path is not a regular file",
        ));
    }
    Ok(metadata)
}

fn create_recovery_copy(path: &std::path::Path, bytes: &[u8]) -> Result<PathBuf, String> {
    for _ in 0..128 {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".recovery-{}-{sequence}", std::process::id()));
        let backup_path = PathBuf::from(name);
        let mut backup = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "cannot preserve damaged configuration before repair ({:?}); no settings were changed",
                    error.kind()
                ));
            }
        };
        if let Err(error) = backup.write_all(bytes).and_then(|_| backup.sync_all()) {
            let _ = fs::remove_file(&backup_path);
            return Err(format!(
                "cannot finish preserving damaged configuration ({:?}); no settings were changed",
                error.kind()
            ));
        }
        return Ok(backup_path);
    }
    Err("cannot allocate a unique recovery-copy name; no settings were changed".into())
}

fn create_recovery_file_copy(path: &std::path::Path) -> Result<PathBuf, String> {
    for _ in 0..128 {
        let sequence = TEMPORARY_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut name = path.as_os_str().to_os_string();
        name.push(format!(".recovery-{}-{sequence}", std::process::id()));
        let backup_path = PathBuf::from(name);
        configuration_file_metadata(path).map_err(|error| {
            format!(
                "cannot safely preserve oversized configuration before repair ({:?}); no settings were changed",
                error.kind()
            )
        })?;
        let mut source = fs::File::open(path).map_err(|error| {
            format!(
                "cannot safely preserve oversized configuration before repair ({:?}); no settings were changed",
                error.kind()
            )
        })?;
        let mut backup = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "cannot preserve oversized configuration before repair ({:?}); no settings were changed",
                    error.kind()
                ));
            }
        };
        if let Err(error) = std::io::copy(&mut source, &mut backup).and_then(|_| backup.sync_all())
        {
            let _ = fs::remove_file(&backup_path);
            return Err(format!(
                "cannot finish preserving oversized configuration ({:?}); no settings were changed",
                error.kind()
            ));
        }
        return Ok(backup_path);
    }
    Err("cannot allocate a unique recovery-copy name; no settings were changed".into())
}

fn default_config_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RICOCHET_CONFIG_PATH")
        .or_else(|| env::var_os("ACORN_CONFIG_PATH"))
    {
        return Some(PathBuf::from(path));
    }

    #[cfg(target_os = "macos")]
    {
        return env::var_os("HOME").map(|home| {
            let support = PathBuf::from(home).join("Library").join("Application Support");
            migrate_config_path(
                support.join("Ricochet").join("configure"),
                support.join("Acorn-2026").join("configure"),
            )
        });
    }

    #[cfg(target_os = "windows")]
    {
        return env::var_os("APPDATA").map(|app_data| {
            let app_data = PathBuf::from(app_data);
            migrate_config_path(
                app_data.join("Ricochet").join("configure"),
                app_data.join("Acorn-2026").join("configure"),
            )
        });
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
            let config_home = PathBuf::from(config_home);
            return Some(migrate_config_path(
                config_home.join("ricochet").join("configure"),
                config_home.join("acorn-2026").join("configure"),
            ));
        }
        env::var_os("HOME").map(|home| {
            let config_home = PathBuf::from(home).join(".config");
            migrate_config_path(
                config_home.join("ricochet").join("configure"),
                config_home.join("acorn-2026").join("configure"),
            )
        })
    }
}

fn migrate_config_path(current: PathBuf, legacy: PathBuf) -> PathBuf {
    if current.exists() || !legacy.exists() {
        return current;
    }
    let Some(parent) = current.parent() else {
        return legacy;
    };
    if fs::create_dir_all(parent).is_ok() && fs::rename(&legacy, &current).is_ok() {
        current
    } else {
        legacy
    }
}

fn validate_profile(profile: &str) -> Result<(), String> {
    if profile.is_empty()
        || profile.len() > 232
        || !profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("BASICProfile must be a single name of at most 232 bytes or Auto".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        BasicConfiguration, BasicEngine, ConfigurationRecoveryKind, ConfigureStore, StartupLanguage,
    };
    use crate::display::{DesktopResolution, DisplayColour, DisplaySettings};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEMP_PATH: AtomicU64 = AtomicU64::new(0);

    fn temporary_path() -> PathBuf {
        let sequence = NEXT_TEMP_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "ricochet-configure-test-{}-{sequence}",
            std::process::id()
        ))
    }

    fn recovery_copies(path: &std::path::Path) -> Vec<PathBuf> {
        let Some(parent) = path.parent() else {
            return Vec::new();
        };
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Vec::new();
        };
        let prefix = format!("{name}.recovery-");
        fs::read_dir(parent)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix))
            })
            .collect()
    }

    #[test]
    fn old_display_keys_migrate_to_one_wimp_mode_setting() {
        let mut old = BasicConfiguration::parse(
            "Language=0\nWindowFurniture=Bevelled\nDisplayResolution=800x600\nDisplayColour=32KRGB555\n",
        )
        .unwrap();
        assert_eq!(old.status_value("WindowFurniture"), None);
        assert_eq!(old.status_value("WimpMode").unwrap().1, "X800 Y600 C32K");
        assert!(!old.serialized().contains("WindowFurniture"));
        assert!(!old.serialized().contains("DisplayResolution"));
        assert!(!old.serialized().contains("DisplayColour"));
        assert!(old.set("WindowFurniture", "Flat").is_err());
        assert!(old.set("RicochetOutputProfile", "BW").is_err());

        let auto = BasicConfiguration::parse(
            "DisplayResolution=Window\nDisplayColour=BW\nWindowFurniture=Bevelled\n",
        )
        .unwrap();
        assert_eq!(auto.status_value("WimpMode").unwrap().1, "AUTO");
        assert_eq!(auto.display.colour, DisplayColour::Rgb888);
    }

    #[test]
    fn wimp_mode_auto_always_means_host_size_and_full_colour() {
        let mut configuration = BasicConfiguration::default();
        configuration.set("WimpMode", "X800 Y600 C2").unwrap();
        configuration.set("Mode", "Auto").unwrap();
        assert_eq!(configuration.status_value("Mode").unwrap().1, "AUTO");
        assert_eq!(configuration.display.resolution, DesktopResolution::Window);
        assert_eq!(configuration.display.colour, DisplayColour::Rgb888);
    }

    #[test]
    fn configure_store_persists_supported_options_and_defaults() {
        let path = temporary_path();
        let store = ConfigureStore::with_path(&path);

        let hybrid = store.set("BASICEngine", "Hybrid").unwrap();
        assert_eq!(hybrid.engine.as_str(), "HYBRID");
        let configured = store.set("BASICEngine", "Strict").unwrap();
        assert_eq!(configured.engine.as_str(), "STRICT");
        store.set("BASICMode", "Classic").unwrap();
        store.set("BASICProfile", "BBCV-1.05").unwrap();
        store.set("BASICTarget", "Agon").unwrap();
        store.set("Language", "3").unwrap();
        let display = DisplaySettings {
            resolution: DesktopResolution::R1280x1024,
            colour: DisplayColour::Grey16,
        };
        store.set_display_settings(display).unwrap();

        let reloaded = store.load().unwrap();
        assert_eq!(reloaded, store.load().unwrap());
        assert_eq!(reloaded.status_value("BASICEngine").unwrap().1, "STRICT");
        assert_eq!(reloaded.status_value("BASICMode").unwrap().1, "CLASSIC");
        assert_eq!(
            reloaded.status_value("BASICProfile").unwrap().1,
            "BBCV-1.05"
        );
        assert_eq!(reloaded.status_value("BASICTarget").unwrap().1, "AGON");
        assert_eq!(reloaded.status_value("Language").unwrap().1, "3");
        assert_eq!(reloaded.startup_language, StartupLanguage::Desktop);
        assert_eq!(reloaded.display, display);
        assert_eq!(
            reloaded.status_value("WimpMode").unwrap().1,
            "X1280 Y1024 G16"
        );
        store
            .replace_from_payload(
                "Language=0\nBASICMode=Auto\nBASICProfile=Auto\nBASICTarget=Auto\nBASICEngine=Interpreter\nWimpMode=Auto\n",
            )
            .unwrap();
        assert_eq!(store.load().unwrap(), BasicConfiguration::default());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn complete_reset_payload_rejects_migration_only_keys_without_overwriting() {
        let path = temporary_path();
        let store = ConfigureStore::with_path(&path);
        store.set("Language", "3").unwrap();
        let original = fs::read(&path).unwrap();
        let base = "Language=0\nBASICMode=Auto\nBASICProfile=Auto\nBASICTarget=Auto\nBASICEngine=Interpreter\nWimpMode=Auto\n";

        for obsolete in [
            "WindowFurniture=Flat\n",
            "DisplayResolution=Window\n",
            "DisplayColour=16MRGB888\n",
            "RicochetOutputProfile=16MRGB888\n",
        ] {
            let result = store.replace_from_payload(&format!("{base}{obsolete}"));
            assert!(
                result.is_err(),
                "reset accepted obsolete field {obsolete:?}"
            );
            assert_eq!(fs::read(&path).unwrap(), original);
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn configure_store_rejects_invalid_or_ambiguous_values() {
        let path = temporary_path();
        let store = ConfigureStore::with_path(&path);
        for value in [
            "maybe",
            "HybridJIT",
            "Hybrid-JIT",
            "StrictJIT",
            "Strict-JIT",
        ] {
            assert!(store.set("BASICEngine", value).is_err(), "accepted {value}");
        }
        assert!(store.set("BASICMode", "Strict").is_err());
        assert!(store.set("BASICProfile", "bad profile").is_err());
        assert!(store.set("RicochetOutputProfile", "BW").is_err());
        for value in ["MOS", "Desktop", "1", "4", "2_100", "&4"] {
            assert!(store.set("Language", value).is_err(), "accepted {value}");
        }
        for value in [
            "X640 Y480 C4",
            "X640 Y512 C16M",
            "20",
            "X640 Y480 C16M F060",
        ] {
            assert!(store.set("WimpMode", value).is_err(), "accepted {value}");
        }
        assert!(store.set("DisplayResolution", "640x480").is_err());
        assert!(store.set("DisplayColour", "32KRGB555").is_err());
        assert!(store.set("SomethingElse", "value").is_err());
        assert!(!path.exists());
    }

    #[test]
    fn legacy_configuration_defaults_to_mos_and_language_only_accepts_standard_modules() {
        let legacy = BasicConfiguration::parse("BASICEngine=INTERPRETER\n").unwrap();
        assert_eq!(legacy.startup_language, StartupLanguage::Mos);
        assert_eq!(legacy.status_value("Language").unwrap().1, "0");

        for (value, expected) in [
            ("0", StartupLanguage::Mos),
            ("&3", StartupLanguage::Desktop),
            ("2_11", StartupLanguage::Desktop),
            ("03", StartupLanguage::Desktop),
        ] {
            let mut configuration = BasicConfiguration::default();
            configuration.set("Language", value).unwrap();
            assert_eq!(configuration.startup_language, expected);
            assert_eq!(
                BasicConfiguration::parse(&configuration.serialized())
                    .unwrap()
                    .startup_language,
                expected
            );
        }
    }

    #[test]
    fn old_configuration_files_migrate_to_authoritative_wimp_mode() {
        let legacy = BasicConfiguration::parse("Language=3\nBASICEngine=INTERPRETER\n").unwrap();
        assert_eq!(
            legacy.display,
            DisplaySettings {
                resolution: DesktopResolution::Window,
                colour: DisplayColour::Rgb888,
            }
        );

        let mut configuration = BasicConfiguration::default();
        configuration.set("WimpMode", "X640 Y480 C32K").unwrap();
        let parsed = BasicConfiguration::parse(&configuration.serialized()).unwrap();
        assert_eq!(parsed.display.resolution, DesktopResolution::R640x480);
        assert_eq!(parsed.display.colour, DisplayColour::Rgb555);
        assert_eq!(parsed.status_value("Mode").unwrap().1, "X640 Y480 C32K");

        let windowed = BasicConfiguration::parse(
            "Language=3\nDisplayResolution=Window\nDisplayColour=BW\nWindowFurniture=garbage\n",
        )
        .unwrap();
        assert_eq!(windowed.status_value("WimpMode").unwrap().1, "AUTO");
        assert_eq!(windowed.display.resolution, DesktopResolution::Window);
        assert_eq!(windowed.display.colour, DisplayColour::Rgb888);

        let v2_conflict = BasicConfiguration::parse(
            "# Ricochet MOS configuration v2\nWimpMode=X800 Y600 C16\nRicochetOutputProfile=BW\nDisplayResolution=640x480\nDisplayColour=BW\n",
        )
        .unwrap();
        assert_eq!(
            v2_conflict.status_value("WimpMode").unwrap().1,
            "X800 Y600 C16"
        );
        assert_eq!(v2_conflict.display.colour, DisplayColour::Colour16);
    }

    #[test]
    fn malformed_stored_configuration_uses_defaults_until_explicit_save_and_keeps_bytes() {
        let path = temporary_path();
        let original = b"# Ricochet MOS configuration v3\nLanguage=3\n";
        fs::write(&path, original).unwrap();
        let store = ConfigureStore::with_path(&path);

        assert_eq!(store.load().unwrap(), BasicConfiguration::default());
        assert_eq!(
            store.recovery_code(),
            ConfigurationRecoveryKind::Malformed.code()
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        // A later reader in the same session sees the latched effective value
        // rather than reparsing or changing the damaged file.
        assert_eq!(store.load().unwrap(), BasicConfiguration::default());
        assert_eq!(fs::read(&path).unwrap(), original);

        // Direct DEFAULTS must detect and preserve the damaged input even when
        // it is the first write and no preceding STATUS call occurred.
        let defaults = "Language=0\nBASICMode=Auto\nBASICProfile=Auto\nBASICTarget=Auto\nBASICEngine=Interpreter\nWimpMode=Auto\n";
        let (_, copy_created) = store.replace_from_payload_with_recovery(defaults).unwrap();
        assert!(copy_created);
        assert_eq!(store.recovery_code(), 0);
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .starts_with("# Ricochet MOS configuration v3\n")
        );
        let copies = recovery_copies(&path);
        assert_eq!(copies.len(), 1);
        assert_eq!(fs::read(&copies[0]).unwrap(), original);
        let _ = fs::remove_file(path);
        for copy in copies {
            let _ = fs::remove_file(copy);
        }
    }

    #[test]
    fn stored_configuration_distinguishes_unsupported_headers_utf8_and_io() {
        let rows = "Language=0\nBASICMode=Auto\nBASICProfile=Auto\nBASICTarget=Auto\nBASICEngine=Interpreter\nWimpMode=Auto\n";
        for (contents, expected_kind) in [
            (
                format!("# Ricochet MOS configuration v99\n{rows}"),
                ConfigurationRecoveryKind::UnsupportedVersion,
            ),
            (
                format!(
                    "# Ricochet MOS configuration v3\n# Ricochet MOS configuration v3\n{rows}"
                ),
                ConfigurationRecoveryKind::Malformed,
            ),
            (
                format!(
                    "# Ricochet MOS configuration v3\n# Ricochet MOS configuration v99\n{rows}"
                ),
                ConfigurationRecoveryKind::Malformed,
            ),
            (
                format!("# Ricochet MOS configuration\n{rows}"),
                ConfigurationRecoveryKind::UnsupportedVersion,
            ),
        ] {
            let path = temporary_path();
            fs::write(&path, contents.as_bytes()).unwrap();
            let store = ConfigureStore::with_path(&path);
            assert_eq!(store.load().unwrap(), BasicConfiguration::default());
            assert_eq!(store.recovery_code(), expected_kind.code());
            let _ = fs::remove_file(path);
        }

        let utf8_path = temporary_path();
        let invalid_utf8 = [0xFF, 0xFE, 0x00];
        fs::write(&utf8_path, invalid_utf8).unwrap();
        let utf8_store = ConfigureStore::with_path(&utf8_path);
        assert_eq!(utf8_store.load().unwrap(), BasicConfiguration::default());
        assert_eq!(
            utf8_store.recovery_code(),
            ConfigurationRecoveryKind::InvalidUtf8.code()
        );
        assert_eq!(fs::read(&utf8_path).unwrap(), invalid_utf8);
        let _ = fs::remove_file(utf8_path);

        let unreadable_path = temporary_path();
        fs::create_dir(&unreadable_path).unwrap();
        let unreadable_store = ConfigureStore::with_path(&unreadable_path);
        assert_eq!(
            unreadable_store.load().unwrap(),
            BasicConfiguration::default()
        );
        assert_eq!(
            unreadable_store.recovery_code(),
            ConfigurationRecoveryKind::Unreadable.code()
        );
        let error = unreadable_store.set("Language", "3").unwrap_err();
        assert!(!error.contains(&unreadable_path.display().to_string()));
        assert_eq!(
            unreadable_store.recovery_code(),
            ConfigurationRecoveryKind::Unreadable.code()
        );
        assert!(unreadable_path.is_dir());
        let _ = fs::remove_dir(unreadable_path);
    }

    #[test]
    fn oversized_configuration_is_bounded_and_streamed_to_recovery_copy() {
        let path = temporary_path();
        let original = vec![b'X'; super::MAX_STORED_CONFIGURATION_BYTES + 4096];
        fs::write(&path, &original).unwrap();
        let store = ConfigureStore::with_path(&path);

        assert_eq!(store.load().unwrap(), BasicConfiguration::default());
        assert_eq!(
            store.recovery_code(),
            ConfigurationRecoveryKind::Oversized.code()
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), original.len() as u64);

        let (_, copy_created) = store.set_with_recovery("Language", "3").unwrap();
        assert!(copy_created);
        assert_eq!(store.recovery_code(), 0);
        let copies = recovery_copies(&path);
        assert_eq!(copies.len(), 1);
        assert_eq!(fs::read(&copies[0]).unwrap(), original);
        let _ = fs::remove_file(path);
        for copy in copies {
            let _ = fs::remove_file(copy);
        }
    }

    #[test]
    fn removed_unreadable_path_does_not_claim_a_recovery_copy() {
        let path = temporary_path();
        fs::create_dir(&path).unwrap();
        let store = ConfigureStore::with_path(&path);
        assert_eq!(store.load().unwrap(), BasicConfiguration::default());
        assert_eq!(
            store.recovery_code(),
            ConfigurationRecoveryKind::Unreadable.code()
        );
        fs::remove_dir(&path).unwrap();

        let (_, copy_created) = store.set_with_recovery("Language", "3").unwrap();
        assert!(!copy_created);
        assert_eq!(store.recovery_code(), 0);
        assert_eq!(
            store.load().unwrap().startup_language,
            StartupLanguage::Desktop
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn persisted_engine_values_accept_only_canonical_names() {
        for (value, expected) in [
            ("INTERPRETER", BasicEngine::Interpreter),
            ("HYBRID", BasicEngine::HybridJit),
            ("STRICT", BasicEngine::StrictJit),
        ] {
            let contents = format!("BASICEngine={value}\n");
            assert_eq!(
                BasicConfiguration::parse(&contents).unwrap().engine,
                expected
            );
        }
        for value in ["HybridJIT", "Hybrid-JIT", "StrictJIT", "Strict-JIT"] {
            let contents = format!("BASICEngine={value}\n");
            assert!(
                BasicConfiguration::parse(&contents).is_err(),
                "accepted {value}"
            );
        }
    }
}
