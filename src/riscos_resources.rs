//! Pinned native RISC OS 3.71 desktop resources.
//!
//! This module decodes the original sprite-file and ROM-font data used by
//! the RISC OS desktop.  It deliberately keeps this font separate from the
//! BBC Micro character set in [`crate::font`].

use std::collections::HashMap;
use std::fmt;

const SOURCE_TREE: &str = "f6c81db7e90f727f3258f874692f751b692a14cb";

const TOOLS: &[u8] = include_bytes!("../resources/riscos-3.71/sprites/Tools,ff9");
const SPRITES22: &[u8] = include_bytes!("../resources/riscos-3.71/sprites/Sprites22,ff9");
const SYSTEM_HARD_FONT: &[u8; 1792] =
    include_bytes!("../resources/riscos-3.71/fonts/System/vdufontl1.bin");
#[cfg(test)]
const SYSTEM_HARD_FONT_SOURCE: &[u8] =
    include_bytes!("../resources/riscos-3.71/os-source/vdufontl1");
const BASE0_ENCODING: &[u8] = include_bytes!("../resources/riscos-3.71/fonts/Encodings/.Base0");
const LATIN1_ENCODING: &[u8] = include_bytes!("../resources/riscos-3.71/fonts/Encodings/Latin1");
const HOMERTON_METRICS: &[u8] =
    include_bytes!("../resources/riscos-3.71/fonts/Homerton/Medium/IntMetric0,ff6");
const CORPUS_METRICS: &[u8] =
    include_bytes!("../resources/riscos-3.71/fonts/Corpus/Medium/IntMetric0,ff6");
const TRINITY_METRICS: &[u8] =
    include_bytes!("../resources/riscos-3.71/fonts/Trinity/Medium/IntMetric0,ff6");
const HOMERTON_OUTLINES: &[u8] =
    include_bytes!("../resources/riscos-3.71/fonts/Homerton/Medium/Outlines0,ff6");
const CORPUS_OUTLINES: &[u8] =
    include_bytes!("../resources/riscos-3.71/fonts/Corpus/Medium/Outlines0,ff6");
const TRINITY_OUTLINES: &[u8] =
    include_bytes!("../resources/riscos-3.71/fonts/Trinity/Medium/Outlines0,ff6");
const SYSTEM_MEDIUM_METRICS: &[u8] =
    include_bytes!("../resources/riscos-3.71/fonts/System/Medium/IntMetrics,ff6");
const SYSTEM_FIXED_METRICS: &[u8] =
    include_bytes!("../resources/riscos-3.71/fonts/System/Fixed/IntMetrics,ff6");
const PALETTE_8DESKTOP: &[u8; 1024] =
    include_bytes!("../resources/riscos-3.71/palettes/8desktop,ffd");

/// Original resource collection included in this checkout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpriteSet {
    /// Earlier flat furniture sprite set.
    Tools,
    /// System sprite catalogue. Some packed high-colour atlas modes are kept
    /// as source files but are not decoded by the small desktop slice yet.
    Sprites22,
}

/// Error raised when a pinned resource fails verification or parsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceError {
    ChecksumMismatch {
        resource: &'static str,
        expected: &'static str,
        actual: String,
    },
    InvalidFormat(&'static str),
    UnsupportedMode(u32),
    InvalidFont(&'static str),
    MissingGlyph(char),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChecksumMismatch {
                resource,
                expected,
                actual,
            } => write!(
                f,
                "pinned RISC OS resource {resource} has Git blob SHA-1 {actual}; expected {expected}"
            ),
            Self::InvalidFormat(what) => write!(f, "invalid RISC OS resource: {what}"),
            Self::UnsupportedMode(mode) => write!(f, "unsupported RISC OS sprite mode {mode:#x}"),
            Self::InvalidFont(what) => write!(f, "invalid RISC OS font resource: {what}"),
            Self::MissingGlyph(ch) => write!(f, "font has no glyph for {ch:?}"),
        }
    }
}

impl std::error::Error for ResourceError {}

/// The original two-word OS_ReadPalette values and an RGB interpretation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpritePaletteEntry {
    pub logical_word: u32,
    pub physical_word: u32,
    pub rgba: [u8; 4],
}

/// One RISC OS sprite. `rgba` and `mask` use top-to-bottom, row-major order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RiscOsSprite {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Sprite mode word copied from its native header.
    pub mode: u32,
    /// Original palette entries, empty when the file relies on the mode's
    /// default palette.
    pub palette: Vec<SpritePaletteEntry>,
    /// Decoded pixels. The mask is applied to alpha as well as retained below.
    pub rgba: Vec<[u8; 4]>,
    /// Per-pixel mask in row-major order: 255 is solid, 0 transparent. `None`
    /// means the file has no mask and the whole rectangular sprite is opaque.
    pub mask: Option<Vec<u8>>,
    /// Resource convention used by the larger 22-unit furniture artwork.
    pub scale_22: bool,
}

impl RiscOsSprite {
    /// Return an RGBA pixel, or `None` when coordinates lie outside the sprite.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.rgba.get((y * self.width + x) as usize).copied()
    }
}

/// A parsed native sprite file with name lookup.
#[derive(Clone, Debug)]
pub struct RiscOsSpriteFile {
    pub source_tree: &'static str,
    sprites: Vec<RiscOsSprite>,
    by_name: HashMap<String, usize>,
}

impl RiscOsSpriteFile {
    /// Load and verify a checked-in RISC OS 3.71 sprite resource.
    pub fn builtin(set: SpriteSet) -> Result<Self, ResourceError> {
        let (label, bytes, blob_sha): (&'static str, &[u8], &'static str) = match set {
            SpriteSet::Tools => (
                "Sources/OS_Core/Desktop/Wimp/Resources/UK/Tools,ff9",
                TOOLS,
                "298d22d65559f3ffbb73029d3c857331a75f0ce2",
            ),
            SpriteSet::Sprites22 => (
                "Sources/OS_Core/Desktop/Wimp/Resources/UK/Sprites22,ff9",
                SPRITES22,
                "c64f8aa90a6c2593f55255dd40e29e1d4a009a24",
            ),
        };
        verify_blob(label, bytes, blob_sha)?;
        Self::parse(bytes)
    }

    /// Decode an on-disk sprite file. The RISC OS saved-file form omits the
    /// first word (area size), so offsets are based at byte -4 from this slice.
    pub fn parse(bytes: &[u8]) -> Result<Self, ResourceError> {
        if bytes.len() < 12 {
            return Err(ResourceError::InvalidFormat(
                "sprite-file header is truncated",
            ));
        }
        let count = read_u32(bytes, 0)? as usize;
        let first_offset = read_u32(bytes, 4)? as usize;
        let free_offset = read_u32(bytes, 8)? as usize;
        if count > 4096 || first_offset < 16 || free_offset < first_offset {
            return Err(ResourceError::InvalidFormat(
                "sprite area offsets/count are invalid",
            ));
        }
        // Saved sprite files omit the area-size word, so in-file positions are
        // four bytes behind the area-relative offsets in the header.
        let area_end = free_offset
            .checked_sub(4)
            .filter(|end| *end <= bytes.len())
            .ok_or(ResourceError::InvalidFormat(
                "sprite free offset exceeds file",
            ))?;
        let mut pos = first_offset
            .checked_sub(4)
            .ok_or(ResourceError::InvalidFormat(
                "first sprite offset underflow",
            ))?;
        let mut sprites = Vec::with_capacity(count);
        let mut by_name = HashMap::with_capacity(count);
        for _ in 0..count {
            let sprite = parse_sprite(bytes, pos, area_end)?;
            if by_name
                .insert(sprite.name.to_ascii_lowercase(), sprites.len())
                .is_some()
            {
                return Err(ResourceError::InvalidFormat("duplicate sprite name"));
            }
            let next = read_u32(bytes, pos)? as usize;
            if next < 44 {
                return Err(ResourceError::InvalidFormat(
                    "sprite's next offset is too small",
                ));
            }
            pos = pos
                .checked_add(next)
                .ok_or(ResourceError::InvalidFormat("sprite offset overflow"))?;
            if pos > area_end {
                return Err(ResourceError::InvalidFormat(
                    "next sprite extends past file",
                ));
            }
            sprites.push(sprite);
        }
        if count != 0 && pos > bytes.len() {
            return Err(ResourceError::InvalidFormat(
                "sprite area exceeds file size",
            ));
        }
        Ok(Self {
            source_tree: SOURCE_TREE,
            sprites,
            by_name,
        })
    }

    /// Find a sprite by its case-insensitive native name.
    pub fn get(&self, name: &str) -> Option<&RiscOsSprite> {
        self.by_name
            .get(&name.to_ascii_lowercase())
            .and_then(|index| self.sprites.get(*index))
    }

    pub fn sprites(&self) -> &[RiscOsSprite] {
        &self.sprites
    }

    pub fn len(&self) -> usize {
        self.sprites.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sprites.is_empty()
    }
}

/// Original 8×8 RISC OS system hard-font bitmap (ISO 32–255).
///
/// This is independent of the BBC Micro character set. The VDU source stores
/// bit 7 at the left edge of each row.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemBitmapFont;

impl SystemBitmapFont {
    pub fn glyph(&self, codepoint: u8) -> Option<&'static [u8; 8]> {
        if !(32..=255).contains(&codepoint) {
            return None;
        }
        let start = usize::from(codepoint - 32) * 8;
        SYSTEM_HARD_FONT.get(start..start + 8)?.try_into().ok()
    }

    /// Advance at the RISC OS desktop pixel scale (2 OS units per source
    /// bitmap pixel). The bitmap itself remains 8×8 source pixels.
    pub fn advance_osu(&self, _codepoint: u8) -> i32 {
        16
    }
}

/// Return the verified native RISC OS system bitmap font.
pub fn system_bitmap_font() -> SystemBitmapFont {
    SystemBitmapFont
}

/// Native RISC OS 3.71 outline/desktop font families.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontName {
    Homerton,
    Corpus,
    Trinity,
    SystemMedium,
    SystemFixed,
}

/// A glyph metric in the font's native 1000-em coordinate system.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NativeGlyphMetric {
    /// Optional native bounding box `(x_min, y_min, x_max, y_max)`.
    /// Missing boxes are supplied by the corresponding outline record.
    pub bbox_1000em: Option<[i16; 4]>,
    /// Horizontal advance in 1000-em units. A per-glyph value overrides the
    /// font's default advance.
    pub advance_x_1000em: i16,
    /// Vertical advance in 1000-em units (normally zero for horizontal text).
    pub advance_y_1000em: i16,
}

/// Parsed native IntMetrics file plus the RISC OS Base0/Latin1 character map.
///
/// This is exposed separately from outline decoding so callers can inspect
/// authoritative advances without converting the source fonts to TTF.
#[derive(Clone, Debug)]
pub struct NativeFontMetrics {
    pub name: FontName,
    pub source_tree: &'static str,
    pub glyph_count: usize,
    pub default_advance_x_1000em: i16,
    pub default_advance_y_1000em: i16,
    metrics: Vec<NativeGlyphMetric>,
    /// Latin-1 byte -> Base0 glyph slot, or None for an undefined code.
    latin1_to_base0: Vec<Option<u16>>,
    /// Base0 slot -> zero-based metric index, or None if not represented in
    /// the IntMetrics table.
    base0_to_metric: Vec<Option<usize>>,
}

impl NativeFontMetrics {
    /// Load and checksum-verify a checked-in RISC OS 3.71 native metric file.
    pub fn builtin(name: FontName) -> Result<Self, ResourceError> {
        let (label, metrics, expected): (&'static str, &[u8], &'static str) = match name {
            FontName::Homerton => (
                "Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Homerton/Medium/IntMetric0,ff6",
                HOMERTON_METRICS,
                "0caf1960d6e777b2f1048bf1fd2b3a30c79bd4a6",
            ),
            FontName::Corpus => (
                "Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Corpus/Medium/IntMetric0,ff6",
                CORPUS_METRICS,
                "3b8cc63ba8b61c09db88899395a92bd4696350a0",
            ),
            FontName::Trinity => (
                "Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Trinity/Medium/IntMetric0,ff6",
                TRINITY_METRICS,
                "996cc38852c72ab9e5446519635771fad8c92eab",
            ),
            FontName::SystemMedium => (
                "Sources/SystemRes/Fonts/System/Medium/IntMetrics,ff6",
                SYSTEM_MEDIUM_METRICS,
                "f13ad9a354474beaf841d508c4cd878e43bec669",
            ),
            FontName::SystemFixed => (
                "Sources/SystemRes/Fonts/System/Fixed/IntMetrics,ff6",
                SYSTEM_FIXED_METRICS,
                "43f586351fb64ef2ffd6987c1032ba1b5cad254b",
            ),
        };
        verify_blob(label, metrics, expected)?;
        verify_blob(
            "Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Encodings/.Base0",
            BASE0_ENCODING,
            "b8aece8a9698b55aa02cde8193b2d1ce1195e24a",
        )?;
        verify_blob(
            "Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Encodings/Latin1",
            LATIN1_ENCODING,
            "b016454d018aac54cb158b15c620be465da8c6c6",
        )?;
        Self::parse(name, metrics, BASE0_ENCODING, LATIN1_ENCODING)
    }

    /// Parse native IntMetrics and encoding text. Primarily useful for testing
    /// malformed files and alternate pinned snapshots.
    pub fn parse(
        name: FontName,
        bytes: &[u8],
        base0_encoding: &[u8],
        latin1_encoding: &[u8],
    ) -> Result<Self, ResourceError> {
        if bytes.len() < 52 {
            return Err(ResourceError::InvalidFont("IntMetrics header is truncated"));
        }
        let count = usize::from(bytes[48]) | (usize::from(bytes[51]) << 8);
        let version = bytes[49];
        let flags = bytes[50];
        if !matches!(version, 0 | 2) || (version == 0 && (flags != 0 || bytes[51] != 0)) {
            return Err(ResourceError::InvalidFont(
                "unsupported IntMetrics version or v0 flags",
            ));
        }
        if count > 4096 {
            return Err(ResourceError::InvalidFont("glyph count is unreasonable"));
        }
        let mut cursor = 52_usize;
        let charmap_size = if flags & 0x20 != 0 {
            let size = usize::from(read_u16(bytes, cursor)?);
            cursor += 2;
            size
        } else {
            256
        };
        if charmap_size > 4096 {
            return Err(ResourceError::InvalidFont("character map is unreasonable"));
        }
        let character_map = take(bytes, &mut cursor, charmap_size)?;
        let x_min = if flags & 0x01 == 0 {
            Some(read_metric_array(bytes, &mut cursor, count)?)
        } else {
            None
        };
        let y_min = if flags & 0x01 == 0 {
            Some(read_metric_array(bytes, &mut cursor, count)?)
        } else {
            None
        };
        let x_max = if flags & 0x01 == 0 {
            Some(read_metric_array(bytes, &mut cursor, count)?)
        } else {
            None
        };
        let y_max = if flags & 0x01 == 0 {
            Some(read_metric_array(bytes, &mut cursor, count)?)
        } else {
            None
        };
        let x_advance = if flags & 0x02 == 0 {
            Some(read_metric_array(bytes, &mut cursor, count)?)
        } else {
            None
        };
        let y_advance = if flags & 0x04 == 0 {
            Some(read_metric_array(bytes, &mut cursor, count)?)
        } else {
            None
        };

        let (default_x, default_y) = if flags & 0x08 != 0 {
            let offsets_start = cursor;
            let offsets = [
                read_u16(bytes, cursor)?,
                read_u16(bytes, cursor + 2)?,
                read_u16(bytes, cursor + 4)?,
                read_u16(bytes, cursor + 6)?,
            ];
            let misc_start = offsets_start
                .checked_add(usize::from(offsets[0]))
                .ok_or(ResourceError::InvalidFont("misc offset overflow"))?;
            let misc = bytes
                .get(misc_start..misc_start.saturating_add(28))
                .ok_or(ResourceError::InvalidFont("misc metrics are truncated"))?;
            (read_i16(misc, 8)?, read_i16(misc, 10)?)
        } else {
            (0, 0)
        };

        let mut metrics = Vec::with_capacity(count);
        for index in 0..count {
            let bbox = match (&x_min, &y_min, &x_max, &y_max) {
                (Some(x0), Some(y0), Some(x1), Some(y1)) => {
                    Some([x0[index], y0[index], x1[index], y1[index]])
                }
                _ => None,
            };
            metrics.push(NativeGlyphMetric {
                bbox_1000em: bbox,
                advance_x_1000em: x_advance.as_ref().map_or(default_x, |values| values[index]),
                advance_y_1000em: y_advance.as_ref().map_or(default_y, |values| values[index]),
            });
        }

        let base0_names = parse_encoding(base0_encoding)?;
        let latin_names = parse_encoding(latin1_encoding)?;
        if base0_names.len() != 416 || latin_names.len() != 256 {
            return Err(ResourceError::InvalidFont(
                "pinned Base0 or Latin1 encoding has unexpected length",
            ));
        }
        let mut base0_name_to_slot = HashMap::new();
        for (slot, glyph_name) in base0_names.iter().enumerate() {
            base0_name_to_slot
                .entry(glyph_name.as_str())
                .or_insert(slot);
        }
        let latin1_to_base0 = latin_names
            .iter()
            .map(|glyph_name| {
                base0_name_to_slot
                    .get(glyph_name.as_str())
                    .map(|slot| *slot as u16)
            })
            .collect();

        let base0_to_metric = (0..base0_names.len())
            .map(|slot| {
                if character_map.is_empty() {
                    (slot < count).then_some(slot)
                } else {
                    character_map
                        .get(slot)
                        .copied()
                        .map(usize::from)
                        .filter(|index| *index < count)
                }
            })
            .collect();

        Ok(Self {
            name,
            source_tree: SOURCE_TREE,
            glyph_count: count,
            default_advance_x_1000em: default_x,
            default_advance_y_1000em: default_y,
            metrics,
            latin1_to_base0,
            base0_to_metric,
        })
    }

    /// Return the Base0 outline slot used for a Latin-1 character.
    pub fn base0_slot(&self, ch: char) -> Option<usize> {
        let byte = u8::try_from(u32::from(ch)).ok()?;
        self.latin1_to_base0
            .get(usize::from(byte))
            .copied()
            .flatten()
            .map(usize::from)
    }

    /// Return the zero-based metrics table index for a Latin-1 character.
    pub fn metric_index(&self, ch: char) -> Option<usize> {
        let slot = self.base0_slot(ch)?;
        self.base0_to_metric.get(slot).copied().flatten()
    }

    /// Return native metrics for a Latin-1 character, if the table defines it.
    pub fn metric(&self, ch: char) -> Option<NativeGlyphMetric> {
        self.metric_index(ch)
            .and_then(|index| self.metrics.get(index).copied())
    }

    /// Native horizontal advance in 1000-em units. If a font has no per-glyph
    /// metric table, its default advance is returned.
    pub fn advance_1000em(&self, ch: char) -> i16 {
        self.metric(ch)
            .map(|metric| metric.advance_x_1000em)
            .unwrap_or(self.default_advance_x_1000em)
    }
}

/// Return checksum-verified native outline bytes for Homerton, Corpus, or
/// Trinity. The bytes stay in RISC OS's outline format; callers need not
/// convert the resources to a modern font container.
pub fn font_outline_bytes(name: FontName) -> Result<&'static [u8], ResourceError> {
    let (label, bytes, expected) = match name {
        FontName::Homerton => (
            "Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Homerton/Medium/Outlines0,ff6",
            HOMERTON_OUTLINES,
            "6e2e72b25758a4d5880bb193d78c2df4ecf78a06",
        ),
        FontName::Corpus => (
            "Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Corpus/Medium/Outlines0,ff6",
            CORPUS_OUTLINES,
            "fd2d9eb857d8d3b2a230796517be7618bccb8a91",
        ),
        FontName::Trinity => (
            "Sources/OS_Core/Video/Render/Fonts/ROMFonts/Fonts/Trinity/Medium/Outlines0,ff6",
            TRINITY_OUTLINES,
            "c18d69dc477b6279241cd446fcb0d49dd7724b02",
        ),
        FontName::SystemMedium | FontName::SystemFixed => {
            return Err(ResourceError::InvalidFont(
                "System outline bitmaps are exposed separately from ROM outline fonts",
            ));
        }
    };
    verify_blob(label, bytes, expected)?;
    Ok(bytes)
}

fn parse_encoding(bytes: &[u8]) -> Result<Vec<String>, ResourceError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| ResourceError::InvalidFont("encoding file is not UTF-8/ASCII"))?;
    Ok(text
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let value = line.strip_prefix('/')?;
            let value = value.split_whitespace().next().unwrap_or(value);
            (!value.is_empty()).then(|| value.to_owned())
        })
        .collect())
}

fn read_metric_array(
    bytes: &[u8],
    cursor: &mut usize,
    count: usize,
) -> Result<Vec<i16>, ResourceError> {
    let byte_count = count
        .checked_mul(2)
        .ok_or(ResourceError::InvalidFont("metric array size overflow"))?;
    let raw = take(bytes, cursor, byte_count)?;
    Ok(raw
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

fn take<'a>(bytes: &'a [u8], cursor: &mut usize, count: usize) -> Result<&'a [u8], ResourceError> {
    let end = cursor
        .checked_add(count)
        .ok_or(ResourceError::InvalidFont("file offset overflow"))?;
    let result = bytes
        .get(*cursor..end)
        .ok_or(ResourceError::InvalidFont("file data is truncated"))?;
    *cursor = end;
    Ok(result)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ResourceError> {
    let raw: [u8; 2] = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or(ResourceError::InvalidFont("u16 field is truncated"))?
        .try_into()
        .map_err(|_| ResourceError::InvalidFont("u16 field is truncated"))?;
    Ok(u16::from_le_bytes(raw))
}

fn read_i16(bytes: &[u8], offset: usize) -> Result<i16, ResourceError> {
    Ok(read_u16(bytes, offset)? as i16)
}

/// Convenience loader for desktop sprite sets.
pub fn builtin_sprite_set(set: SpriteSet) -> Result<RiscOsSpriteFile, ResourceError> {
    RiscOsSpriteFile::builtin(set)
}

fn parse_sprite(bytes: &[u8], pos: usize, area_end: usize) -> Result<RiscOsSprite, ResourceError> {
    if pos.checked_add(44).filter(|end| *end <= area_end).is_none() {
        return Err(ResourceError::InvalidFormat("sprite header is truncated"));
    }
    let next = read_u32(bytes, pos)? as usize;
    let name_end = bytes[pos + 4..pos + 16]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(12);
    let name_bytes = &bytes[pos + 4..pos + 4 + name_end];
    // Some Sprites22 resources use a RISC OS control byte as their name.
    // Sprite names are byte strings in the original format. Preserve values
    // such as Sprites22 control-name bytes one-to-one instead of letting
    // replacement characters collapse distinct names.
    let name: String = name_bytes.iter().map(|byte| char::from(*byte)).collect();
    let words = read_u32(bytes, pos + 16)? as usize + 1;
    let height = read_u32(bytes, pos + 20)? as usize + 1;
    let first_bit = read_u32(bytes, pos + 24)? as usize;
    let last_bit = read_u32(bytes, pos + 28)? as usize;
    let image_offset = read_u32(bytes, pos + 32)? as usize;
    let mask_offset = read_u32(bytes, pos + 36)? as usize;
    let mode = read_u32(bytes, pos + 40)?;
    if name.is_empty() || words == 0 || height == 0 {
        return Err(ResourceError::InvalidFormat(
            "sprite dimensions or bit range are invalid",
        ));
    }
    if next < 44
        || pos
            .checked_add(next)
            .filter(|end| *end <= area_end)
            .is_none()
    {
        return Err(ResourceError::InvalidFormat(
            "sprite extent is out of bounds",
        ));
    }
    let (bits_per_pixel, new_sprite_type) = sprite_format(mode)?;
    if last_bit >= 32 || first_bit >= 32 {
        return Err(ResourceError::InvalidFormat(
            "sprite edge bit index exceeds word",
        ));
    }
    if new_sprite_type && first_bit != 0 {
        return Err(ResourceError::InvalidFormat(
            "new-format sprite has nonzero reserved left-bit field",
        ));
    }
    // These are bit positions within the first and last 32-bit words. A
    // multiword sprite's rightmost position may therefore be numerically less
    // than the leftmost position.
    let width_bits = (words - 1)
        .checked_mul(32)
        .and_then(|bits| bits.checked_add(last_bit))
        .and_then(|bits| bits.checked_sub(first_bit))
        .and_then(|bits| bits.checked_add(1))
        .ok_or(ResourceError::InvalidFormat(
            "sprite width bit count underflow",
        ))?;
    if width_bits % bits_per_pixel != 0 {
        return Err(ResourceError::InvalidFormat(
            "sprite bit range is not pixel aligned",
        ));
    }
    let width = width_bits / bits_per_pixel;
    let row_bytes = words
        .checked_mul(4)
        .ok_or(ResourceError::InvalidFormat("sprite row size overflow"))?;
    let pixel_count = width
        .checked_mul(height)
        .ok_or(ResourceError::InvalidFormat("sprite pixel count overflow"))?;
    // Guard corrupt files from requesting unreasonable allocations.
    if width == 0 || width > 16384 || height > 16384 || pixel_count > 16 * 1024 * 1024 {
        return Err(ResourceError::InvalidFormat(
            "sprite dimensions are unreasonable",
        ));
    }
    let sprite_end = pos + next;
    let image_start = pos
        .checked_add(image_offset)
        .ok_or(ResourceError::InvalidFormat("image offset overflow"))?;
    let image_len = row_bytes
        .checked_mul(height)
        .ok_or(ResourceError::InvalidFormat("image size overflow"))?;
    let image_end = image_start
        .checked_add(image_len)
        .ok_or(ResourceError::InvalidFormat("image extent overflow"))?;
    if image_offset < 44 || image_end > sprite_end {
        return Err(ResourceError::InvalidFormat(
            "sprite image is out of bounds",
        ));
    }
    let has_mask = mask_offset != image_offset;
    let mask_start = pos
        .checked_add(mask_offset)
        .ok_or(ResourceError::InvalidFormat("mask offset overflow"))?;
    let mask_row_bytes = if new_sprite_type {
        width.div_ceil(32) * 4
    } else {
        row_bytes
    };
    let mask_len = mask_row_bytes
        .checked_mul(height)
        .ok_or(ResourceError::InvalidFormat("mask size overflow"))?;
    let mask_end = mask_start
        .checked_add(mask_len)
        .ok_or(ResourceError::InvalidFormat("mask extent overflow"))?;
    if has_mask && (mask_offset < 44 || mask_end > sprite_end) {
        return Err(ResourceError::InvalidFormat("sprite mask is out of bounds"));
    }
    if has_mask && ranges_overlap(image_start..image_end, mask_start..mask_end) {
        return Err(ResourceError::InvalidFormat(
            "sprite image overlaps its mask",
        ));
    }
    let palette_end = image_start.min(mask_start);
    let palette_bytes = palette_end.saturating_sub(pos + 44);
    let palette = parse_palette(bytes, pos + 44, palette_bytes, bits_per_pixel)?;
    let fallback_palette = default_palette(bits_per_pixel)?;
    let effective_palette = if palette.is_empty() {
        fallback_palette
    } else {
        palette.iter().map(|entry| entry.rgba).collect()
    };
    if effective_palette.is_empty() && bits_per_pixel <= 8 {
        return Err(ResourceError::UnsupportedMode(mode));
    }

    let mut rgba = Vec::with_capacity(pixel_count);
    let mut mask = has_mask.then(|| Vec::with_capacity(pixel_count));
    for y in 0..height {
        let row_start = image_start + y * row_bytes;
        let mask_row_start = mask_start + y * mask_row_bytes;
        for x in 0..width {
            let bit = first_bit + x * bits_per_pixel;
            let mut pixel = match bits_per_pixel {
                16 => decode_rgb555(&bytes[row_start..row_start + row_bytes], bit)?,
                32 => decode_rgb888(&bytes[row_start..row_start + row_bytes], bit)?,
                _ => {
                    let index = get_lsb_bits(
                        &bytes[row_start..row_start + row_bytes],
                        bit,
                        bits_per_pixel,
                    )? as usize;
                    *effective_palette
                        .get(index)
                        .ok_or(ResourceError::InvalidFormat("pixel index exceeds palette"))?
                }
            };
            let solid = if let Some(decoded_mask) = mask.as_mut() {
                let (mask_bit, mask_bpp) = if new_sprite_type {
                    (x, 1)
                } else {
                    (bit, bits_per_pixel)
                };
                let mask_index = get_lsb_bits(
                    &bytes[mask_row_start..mask_row_start + mask_row_bytes],
                    mask_bit,
                    mask_bpp,
                )?;
                let solid = mask_index != 0;
                decoded_mask.push(if solid { 255 } else { 0 });
                solid
            } else {
                true
            };
            pixel[3] = if solid { 255 } else { 0 };
            rgba.push(pixel);
        }
    }
    Ok(RiscOsSprite {
        name: name.clone(),
        width: width as u32,
        height: height as u32,
        mode,
        palette,
        rgba,
        mask,
        scale_22: name.to_ascii_lowercase().ends_with("22"),
    })
}

fn sprite_format(mode: u32) -> Result<(usize, bool), ResourceError> {
    if mode >= 256 {
        let sprite_type = (mode >> 27) & 0x1f;
        let bits = match sprite_type {
            1 => 1,
            2 => 2,
            3 => 4,
            4 => 8,
            5 => 16,
            6 => 32,
            _ => return Err(ResourceError::UnsupportedMode(mode)),
        };
        return Ok((bits, true));
    }
    // The original resource sets include the classic modes 0, 8, 12, 15,
    // 18–21, and RISC OS 3.7 mode IDs 25, 27, 28, and 31. Mode 44 is the
    // 16-colour Wimp palette used by the native desktop furniture.
    match mode {
        0 | 4 | 18 | 25 => Ok((1, false)),
        1 | 8 | 19 | 26 => Ok((2, false)),
        9 | 12 | 20 | 27 | 31 | 44 => Ok((4, false)),
        13 | 15 | 21 | 28 => Ok((8, false)),
        _ => Err(ResourceError::UnsupportedMode(mode)),
    }
}

fn decode_rgb555(row: &[u8], bit: usize) -> Result<[u8; 4], ResourceError> {
    if bit % 8 != 0
        || bit
            .checked_add(16)
            .filter(|end| *end <= row.len() * 8)
            .is_none()
    {
        return Err(ResourceError::InvalidFormat("16bpp pixel is out of bounds"));
    }
    let offset = bit / 8;
    let value = u16::from_le_bytes([row[offset], row[offset + 1]]);
    let expand = |channel: u16| ((channel * 255 + 15) / 31) as u8;
    Ok([
        expand(value & 0x1f),
        expand((value >> 5) & 0x1f),
        expand((value >> 10) & 0x1f),
        255,
    ])
}

fn decode_rgb888(row: &[u8], bit: usize) -> Result<[u8; 4], ResourceError> {
    if bit % 8 != 0
        || bit
            .checked_add(32)
            .filter(|end| *end <= row.len() * 8)
            .is_none()
    {
        return Err(ResourceError::InvalidFormat("32bpp pixel is out of bounds"));
    }
    let offset = bit / 8;
    Ok([row[offset], row[offset + 1], row[offset + 2], 255])
}

fn parse_palette(
    bytes: &[u8],
    start: usize,
    palette_bytes: usize,
    bits_per_pixel: usize,
) -> Result<Vec<SpritePaletteEntry>, ResourceError> {
    if palette_bytes == 0 {
        return Ok(Vec::new());
    }
    if bits_per_pixel > 8 {
        return Err(ResourceError::InvalidFormat(
            "16/32bpp sprites cannot contain a palette",
        ));
    }
    if palette_bytes % 8 != 0
        || start
            .checked_add(palette_bytes)
            .filter(|end| *end <= bytes.len())
            .is_none()
    {
        return Err(ResourceError::InvalidFormat(
            "sprite palette has invalid size",
        ));
    }
    let count = palette_bytes / 8;
    if count > (1 << bits_per_pixel) {
        return Err(ResourceError::InvalidFormat(
            "sprite palette exceeds mode depth",
        ));
    }
    let mut palette = Vec::with_capacity(count);
    for i in 0..count {
        let p = start + i * 8;
        let logical_word = read_u32(bytes, p)?;
        let physical_word = read_u32(bytes, p + 4)?;
        palette.push(SpritePaletteEntry {
            logical_word,
            physical_word,
            rgba: physical_word_to_rgba(physical_word),
        });
    }
    Ok(palette)
}

pub(crate) fn default_palette(bits_per_pixel: usize) -> Result<Vec<[u8; 4]>, ResourceError> {
    match bits_per_pixel {
        1 => Ok(vec![wimp_palette()[0], wimp_palette()[7]]),
        2 => Ok(vec![
            wimp_palette()[0],
            wimp_palette()[2],
            wimp_palette()[4],
            wimp_palette()[7],
        ]),
        4 => Ok(wimp_palette().to_vec()),
        // Reuse the original RISC OS desktop's 256-entry palette rather than
        // inventing an RGB ramp for palette-less 8bpp sprites.
        8 => {
            verify_blob(
                "Sources/OS_Core/Video/Render/Colours/Palettes/8desktop,ffd",
                PALETTE_8DESKTOP,
                "1dc5e842239e20f3800caa9dc65deda57aac66ea",
            )?;
            Ok(PALETTE_8DESKTOP
                .chunks_exact(4)
                .map(|entry| [entry[1], entry[2], entry[3], 255])
                .collect())
        }
        _ => Ok(Vec::new()),
    }
}

fn physical_word_to_rgba(word: u32) -> [u8; 4] {
    // RISC OS palette RGB words use &BBGGRRnn. The low byte is reserved/phase
    // state; retain it in `physical_word` but use the 24-bit colour here.
    [
        ((word >> 8) & 0xff) as u8,
        ((word >> 16) & 0xff) as u8,
        (word >> 24) as u8,
        255,
    ]
}

fn wimp_palette() -> [[u8; 4]; 16] {
    // RISC OS 3 Wimp standard colours: white-to-black greys, then the eight
    // named desktop colours. Values are the standard palette from the PRM's
    // Desktop_SetPalette example (not the BBC Micro palette).
    [
        [255, 255, 255, 255],
        [221, 221, 221, 255],
        [187, 187, 187, 255],
        [153, 153, 153, 255],
        [119, 119, 119, 255],
        [85, 85, 85, 255],
        [51, 51, 51, 255],
        [0, 0, 0, 255],
        [0x00, 0x44, 0x99, 255], // dark blue
        [0xee, 0xee, 0x00, 255], // yellow
        [0x00, 0xcc, 0x00, 255], // green
        [0xdd, 0x00, 0x00, 255], // red
        [0xee, 0xee, 0xbb, 255], // cream
        [0x55, 0x88, 0x00, 255], // army green
        [0xff, 0xbb, 0x00, 255], // orange
        [0x00, 0xbb, 0xff, 255], // light blue
    ]
}

fn get_lsb_bits(row: &[u8], bit: usize, width: usize) -> Result<u16, ResourceError> {
    if width == 0
        || width > 8
        || bit
            .checked_add(width)
            .filter(|end| *end <= row.len() * 8)
            .is_none()
    {
        return Err(ResourceError::InvalidFormat("pixel bit read exceeds row"));
    }
    let mut value = 0_u16;
    for offset in 0..width {
        let source_bit = bit + offset;
        let set = (row[source_bit / 8] >> (source_bit % 8)) & 1;
        value |= u16::from(set) << offset;
    }
    Ok(value)
}

fn ranges_overlap(a: std::ops::Range<usize>, b: std::ops::Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ResourceError> {
    let raw: [u8; 4] = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or(ResourceError::InvalidFormat("u32 field is truncated"))?
        .try_into()
        .map_err(|_| ResourceError::InvalidFormat("u32 field is truncated"))?;
    Ok(u32::from_le_bytes(raw))
}

fn verify_blob(
    resource: &'static str,
    bytes: &[u8],
    expected: &'static str,
) -> Result<(), ResourceError> {
    let actual = git_blob_sha1(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(ResourceError::ChecksumMismatch {
            resource,
            expected,
            actual,
        })
    }
}

fn git_blob_sha1(bytes: &[u8]) -> String {
    // Minimal SHA-1 implementation for verifying source-control blob IDs. This
    // is used only to pin checked-in data, not for security decisions.
    let mut message = Vec::with_capacity(bytes.len() + 64);
    message.extend_from_slice(format!("blob {}\0", bytes.len()).as_bytes());
    message.extend_from_slice(bytes);
    let bit_length = (message.len() as u64) * 8;
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());
    let mut h = [
        0x67452301_u32,
        0xefcdab89,
        0x98badcfe,
        0x10325476,
        0xc3d2e1f0,
    ];
    for block in message.chunks_exact(64) {
        let mut w = [0_u32; 80];
        for (i, word) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(word.try_into().expect("four-byte chunk"));
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    format!(
        "{:08x}{:08x}{:08x}{:08x}{:08x}",
        h[0], h[1], h[2], h[3], h[4]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_pinned_sprite_sets_parse_with_native_depths() {
        for set in [SpriteSet::Tools, SpriteSet::Sprites22] {
            let file = RiscOsSpriteFile::builtin(set).unwrap();
            assert!(!file.is_empty());
        }
        let atlas = RiscOsSpriteFile::builtin(SpriteSet::Sprites22).unwrap();
        assert_eq!(atlas.get("tile_1").unwrap().width, 210);
        assert_eq!(atlas.get("tile_1-8").unwrap().width, 210);
        assert_eq!(atlas.get("tile_1-16").unwrap().width, 210);
        assert_eq!(atlas.get("tile_1-16").unwrap().rgba[0][3], 255);
        assert_eq!(
            atlas.get("tile_1-16").unwrap().rgba[0],
            [189, 189, 189, 255]
        );
    }

    #[test]
    fn native_intmetrics_and_latin1_map_produce_real_advances() {
        let homerton = NativeFontMetrics::builtin(FontName::Homerton).unwrap();
        let corpus = NativeFontMetrics::builtin(FontName::Corpus).unwrap();
        let trinity = NativeFontMetrics::builtin(FontName::Trinity).unwrap();
        assert_eq!(homerton.base0_slot('A'), Some(65));
        assert!(homerton.metric_index('A').is_some());
        for (character, expected) in [
            ('H', 722),
            ('o', 556),
            ('m', 833),
            ('r', 333),
            ('t', 278),
            (' ', 278),
            ('A', 667),
            ('i', 222),
        ] {
            assert_eq!(
                homerton.advance_1000em(character),
                expected,
                "Homerton {character:?}"
            );
        }
        assert_eq!(corpus.advance_1000em('W'), 600);
        for (character, expected) in [(' ', 250), ('A', 722), ('i', 278)] {
            assert_eq!(
                trinity.advance_1000em(character),
                expected,
                "Trinity {character:?}"
            );
        }
    }

    #[test]
    fn system_rom_font_is_not_the_bbc_font() {
        let font = system_bitmap_font();
        assert_eq!(
            font.glyph(b'A').unwrap(),
            &[0x3c, 0x66, 0x66, 0x7e, 0x66, 0x66, 0x66, 0x00]
        );
        assert_eq!(font.advance_osu(b'W'), 16);
        assert!(font.glyph(31).is_none());
    }

    #[test]
    fn system_font_source_and_extraction_are_pinned_git_blobs() {
        verify_blob(
            "Sources/OS_Core/Kernel/s/vdu/vdufontl1",
            SYSTEM_HARD_FONT_SOURCE,
            "2faac185375d69d00f9780e155ccaf9c74788143",
        )
        .unwrap();
        verify_blob(
            "derived ISO 32–255 bitmap from Sources/OS_Core/Kernel/s/vdu/vdufontl1",
            SYSTEM_HARD_FONT,
            "53a8ac732144f02835829c3d514767b2bcfd14cb",
        )
        .unwrap();
    }

    #[test]
    fn git_blob_sha1_matches_known_vectors() {
        assert_eq!(
            git_blob_sha1(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        assert_eq!(
            git_blob_sha1(b"abc"),
            "f2ba8f84ab5c1bce84a7b441cb1959cfc7093b7f"
        );
    }
}
