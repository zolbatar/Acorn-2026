//! Native RISC OS outline-font decoding and cached glyph rasterization.
//!
//! The source files remain in the original `FONT` format. This module reads
//! their 1000-em metrics and outline chunks directly; it does not depend on a
//! TrueType conversion step.

use crate::riscos_resources::{FontName, NativeFontMetrics, ResourceError, font_outline_bytes};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Point {
    x: i32,
    y: i32,
}

#[derive(Clone, Debug)]
enum Segment {
    Move(Point),
    Line(Point),
    Curve(Point, Point, Point),
}

#[derive(Clone, Debug, Default)]
struct OutlineGlyph {
    bbox: Option<[i32; 4]>,
    contours: Vec<Vec<Segment>>,
    includes: Vec<GlyphInclude>,
}

#[derive(Clone, Copy, Debug)]
struct GlyphInclude {
    slot: usize,
    offset: Point,
}

#[derive(Clone, Debug)]
struct OutlineFile {
    version: u8,
    design_size: u16,
    glyphs: HashMap<usize, OutlineGlyph>,
}

/// One cached grayscale glyph bitmap in top-to-bottom row order.
#[derive(Clone, Debug, PartialEq)]
pub struct RasterizedGlyph {
    pub width: u32,
    pub height: u32,
    /// Distance from the baseline to the left edge, in display pixels.
    pub bearing_x: i32,
    /// Distance from the baseline to the top edge, in display pixels.
    pub bearing_y: i32,
    /// Advance from the current pen position, in display pixels.
    pub advance_px: f32,
    /// Coverage values; zero is transparent and 255 is fully covered.
    pub coverage: Vec<u8>,
}

/// A native RISC OS outline font at one requested em size.
///
/// Glyph bitmaps are rasterized on first use and then shared from this cache
/// for both drawing and measurement.
pub struct NativeRasterFont {
    name: FontName,
    pixel_size: u16,
    metrics: NativeFontMetrics,
    outlines: OutlineFile,
    cache: Mutex<HashMap<char, Arc<RasterizedGlyph>>>,
}

impl NativeRasterFont {
    /// Load one of the pinned Homerton, Corpus, or Trinity ROM fonts.
    pub fn builtin(name: FontName, pixel_size: u16) -> Result<Self, ResourceError> {
        if pixel_size == 0 || pixel_size > 256 {
            return Err(ResourceError::InvalidFont(
                "requested outline-font size is outside 1..=256 pixels",
            ));
        }
        let metrics = NativeFontMetrics::builtin(name)?;
        let outlines = OutlineFile::parse(font_outline_bytes(name)?)?;
        Ok(Self {
            name,
            pixel_size,
            metrics,
            outlines,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn name(&self) -> FontName {
        self.name
    }

    pub fn pixel_size(&self) -> u16 {
        self.pixel_size
    }

    pub fn outline_version(&self) -> u8 {
        self.outlines.version
    }

    pub fn design_size(&self) -> u16 {
        self.outlines.design_size
    }

    /// The authoritative native horizontal advance in display pixels.
    pub fn advance_px(&self, character: char) -> f32 {
        let measured_character = self.fallback_character(character);
        f32::from(self.metrics.advance_1000em(measured_character)) * f32::from(self.pixel_size)
            / 1000.0
    }

    fn fallback_character(&self, character: char) -> char {
        if self.metrics.base0_slot(character).is_some() {
            character
        } else {
            '?'
        }
    }

    /// Measure a string on the same native metric path used by `draw_text`.
    pub fn measure_text_px(&self, text: &str) -> f32 {
        text.chars()
            .map(|character| self.advance_px(character))
            .sum()
    }

    /// Return a cached native raster glyph, or `None` for an undefined code.
    pub fn rasterize_glyph(&self, character: char) -> Option<Arc<RasterizedGlyph>> {
        if let Some(glyph) = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&character)
            .cloned()
        {
            return Some(glyph);
        }

        let slot = self.metrics.base0_slot(character)?;
        let outline = self.outlines.glyphs.get(&slot);
        let raster = rasterize_outline(
            outline,
            &self.outlines,
            slot,
            f32::from(self.pixel_size) / f32::from(self.outlines.design_size),
            self.advance_px(character),
        );
        let raster = Arc::new(raster);
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(character, Arc::clone(&raster));
        Some(raster)
    }

    /// Paint UTF-8 text with native metrics and cached outlines into an RGBA
    /// buffer. `baseline_y` is measured downward from the buffer's top edge.
    pub fn draw_text(
        &self,
        frame: &mut [u8],
        width: u32,
        height: u32,
        x: i32,
        baseline_y: i32,
        text: &str,
        color: [u8; 4],
    ) {
        if width == 0 || height == 0 || frame.len() < width as usize * height as usize * 4 {
            return;
        }
        let mut pen_x = 0.0_f32;
        for character in text.chars() {
            let painted_character = self.fallback_character(character);
            let Some(glyph) = self.rasterize_glyph(painted_character) else {
                pen_x += self.advance_px(character);
                continue;
            };
            let left = x + pen_x.round() as i32 + glyph.bearing_x;
            let top = baseline_y - glyph.bearing_y;
            for gy in 0..glyph.height {
                let py = top + gy as i32;
                if py < 0 || py >= height as i32 {
                    continue;
                }
                for gx in 0..glyph.width {
                    let px = left + gx as i32;
                    if px < 0 || px >= width as i32 {
                        continue;
                    }
                    let coverage = glyph.coverage[(gy * glyph.width + gx) as usize];
                    if coverage == 0 {
                        continue;
                    }
                    blend_pixel(
                        frame,
                        (py as usize * width as usize + px as usize) * 4,
                        color,
                        coverage,
                    );
                }
            }
            pen_x += glyph.advance_px;
        }
    }
}

/// Render a simple three-row specimen from the original ROM outline families.
pub fn render_font_specimen(
    frame: &mut [u8],
    width: u32,
    height: u32,
) -> Result<(), ResourceError> {
    let bytes = width as usize * height as usize * 4;
    if frame.len() < bytes {
        return Err(ResourceError::InvalidFont(
            "font specimen framebuffer is too small",
        ));
    }
    for pixel in frame[..bytes].chunks_exact_mut(4) {
        pixel.copy_from_slice(&[211, 211, 211, 255]);
    }
    let specimens = [
        (
            FontName::Homerton,
            "HOMERTON  AaBbCc  0123456789",
            "AgjpQéö£",
        ),
        (
            FontName::Corpus,
            "CORPUS  RISC OS desktop typography",
            "AgjpQéö£",
        ),
        (FontName::Trinity, "TRINITY  RISC OS 3.71", "AgjpQéö£"),
    ];
    for (index, (name, text, secondary)) in specimens.into_iter().enumerate() {
        let font = NativeRasterFont::builtin(name, 30)?;
        let baseline = 130 + index as i32 * 170;
        font.draw_text(frame, width, height, 36, baseline, text, [30, 30, 30, 255]);
        font.draw_text(
            frame,
            width,
            height,
            36,
            baseline + 42,
            secondary,
            [30, 30, 30, 255],
        );
    }
    Ok(())
}

impl OutlineFile {
    fn parse(bytes: &[u8]) -> Result<Self, ResourceError> {
        if bytes.len() < 20 || bytes.get(..4) != Some(b"FONT") {
            return Err(ResourceError::InvalidFont(
                "outline header is truncated or invalid",
            ));
        }
        if bytes[4] != 0 {
            return Err(ResourceError::InvalidFont(
                "resource is not an outline font",
            ));
        }
        let version = bytes[5];
        if version != 8 {
            return Err(ResourceError::InvalidFont(
                "this rasterizer supports the pinned version-8 ROM outline format",
            ));
        }
        let design_size = read_u16(bytes, 6)?;
        if design_size == 0 {
            return Err(ResourceError::InvalidFont("outline design size is zero"));
        }
        let table = read_u32(bytes, 16)? as usize;
        let chunks = read_u32(bytes, 20)? as usize;
        let scaffold_flags = read_u32(bytes, 28)?;
        if scaffold_flags & 4 != 0 {
            return Err(ResourceError::InvalidFont(
                "non-zero outline winding is not supported",
            ));
        }
        if chunks == 0 || chunks > 24 {
            return Err(ResourceError::InvalidFont("outline chunk count is invalid"));
        }
        let count = chunks + 1;
        let table_bytes = count
            .checked_mul(4)
            .ok_or(ResourceError::InvalidFont("outline offset table overflow"))?;
        if table
            .checked_add(table_bytes)
            .is_none_or(|end| end > bytes.len())
        {
            return Err(ResourceError::InvalidFont(
                "outline offset table is truncated",
            ));
        }
        let offsets = (0..count)
            .map(|index| read_u32(bytes, table + index * 4).map(|offset| offset as usize))
            .collect::<Result<Vec<_>, _>>()?;

        let mut glyphs = HashMap::new();
        for (chunk, pair) in offsets.windows(2).enumerate() {
            let start = pair[0];
            let end = pair[1];
            if start == 0 || start == end {
                continue;
            }
            if start > end || end > bytes.len() {
                return Err(ResourceError::InvalidFont(
                    "outline chunk offsets are invalid",
                ));
            }
            if end - start < 4 {
                return Err(ResourceError::InvalidFont("outline chunk is truncated"));
            }
            let chunk_flags = read_u32(bytes, start)?;
            let index = start + 4;
            let per_character = (if chunk_flags & 1 != 0 { 4 } else { 1 })
                * (if chunk_flags & 2 != 0 { 4 } else { 1 });
            let index_bytes = 32_usize
                .checked_mul(per_character)
                .and_then(|size| size.checked_mul(4))
                .ok_or(ResourceError::InvalidFont("outline index size overflow"))?;
            if index
                .checked_add(index_bytes)
                .is_none_or(|limit| limit > end)
            {
                return Err(ResourceError::InvalidFont(
                    "outline chunk index is truncated",
                ));
            }
            for character in 0..32 {
                let relative = read_u32(bytes, index + character * per_character * 4)? as usize;
                if relative == 0 {
                    continue;
                }
                let glyph_start = index
                    .checked_add(relative)
                    .ok_or(ResourceError::InvalidFont("outline glyph offset overflow"))?;
                if glyph_start < index + index_bytes || glyph_start >= end {
                    return Err(ResourceError::InvalidFont(
                        "outline glyph lies outside its chunk",
                    ));
                }
                if let Some(glyph) = parse_glyph(bytes, glyph_start, end)? {
                    glyphs.insert(chunk * 32 + character, glyph);
                }
            }
        }
        Ok(Self {
            version,
            design_size,
            glyphs,
        })
    }
}

fn parse_glyph(
    bytes: &[u8],
    mut cursor: usize,
    end: usize,
) -> Result<Option<OutlineGlyph>, ResourceError> {
    let flags = read_byte(bytes, &mut cursor, end)?;
    if flags & 8 == 0 {
        return Ok(None);
    }
    if flags & 0x80 != 0 {
        return Err(ResourceError::InvalidFont(
            "reserved outline glyph flag is set",
        ));
    }
    if flags & 0x40 != 0 {
        return Err(ResourceError::InvalidFont(
            "16-bit outline character codes are unsupported",
        ));
    }
    let wide_coords = flags & 1 != 0;
    let mut glyph = OutlineGlyph::default();
    if flags & 16 != 0 {
        let base = read_byte(bytes, &mut cursor, end)? as usize;
        glyph.includes.push(GlyphInclude {
            slot: base,
            offset: Point::default(),
        });
        if flags & 32 != 0 {
            let accent = read_byte(bytes, &mut cursor, end)? as usize;
            glyph.includes.push(GlyphInclude {
                slot: accent,
                offset: read_xy(bytes, &mut cursor, end, wide_coords)?,
            });
        }
        return Ok(Some(glyph));
    }

    let origin = read_xy(bytes, &mut cursor, end, wide_coords)?;
    let span = read_xy(bytes, &mut cursor, end, wide_coords)?;
    let max_x = origin
        .x
        .checked_add(span.x)
        .ok_or(ResourceError::InvalidFont("outline bounding box overflow"))?;
    let max_y = origin
        .y
        .checked_add(span.y)
        .ok_or(ResourceError::InvalidFont("outline bounding box overflow"))?;
    glyph.bbox = Some([origin.x, origin.y, max_x, max_y]);
    let (contours, terminator) = parse_paths(bytes, &mut cursor, end, wide_coords)?;
    glyph.contours = contours;
    let mut terminator = terminator;
    if terminator & 4 != 0 {
        let (_, stroke_terminator) = parse_paths(bytes, &mut cursor, end, wide_coords)?;
        terminator |= stroke_terminator;
    }
    if terminator & 8 != 0 {
        loop {
            let code = read_byte(bytes, &mut cursor, end)? as usize;
            if code == 0 {
                break;
            }
            let offset = read_xy(bytes, &mut cursor, end, wide_coords)?;
            glyph.includes.push(GlyphInclude { slot: code, offset });
        }
    }
    Ok(Some(glyph))
}

fn parse_paths(
    bytes: &[u8],
    cursor: &mut usize,
    end: usize,
    wide_coords: bool,
) -> Result<(Vec<Vec<Segment>>, u8), ResourceError> {
    let mut contours = Vec::<Vec<Segment>>::new();
    let mut current = false;
    loop {
        let command = read_byte(bytes, cursor, end)?;
        match command & 3 {
            0 => return Ok((contours, command)),
            1 => {
                contours.push(vec![Segment::Move(read_xy(
                    bytes,
                    cursor,
                    end,
                    wide_coords,
                )?)]);
                current = true;
            }
            2 => {
                let point = read_xy(bytes, cursor, end, wide_coords)?;
                if !current {
                    contours.push(vec![Segment::Move(point)]);
                    current = true;
                } else {
                    contours
                        .last_mut()
                        .expect("current contour exists")
                        .push(Segment::Line(point));
                }
            }
            _ => {
                if !current {
                    return Err(ResourceError::InvalidFont("curve precedes move in outline"));
                }
                let first = read_xy(bytes, cursor, end, wide_coords)?;
                let second = read_xy(bytes, cursor, end, wide_coords)?;
                let third = read_xy(bytes, cursor, end, wide_coords)?;
                contours
                    .last_mut()
                    .expect("current contour exists")
                    .push(Segment::Curve(first, second, third));
            }
        }
    }
}

fn read_xy(
    bytes: &[u8],
    cursor: &mut usize,
    end: usize,
    wide: bool,
) -> Result<Point, ResourceError> {
    if wide {
        let raw = take(bytes, cursor, end, 3)?;
        let mut x = i32::from(raw[0]) | (i32::from(raw[1] & 0x0f) << 8);
        let mut y = i32::from(raw[1] >> 4) | (i32::from(raw[2]) << 4);
        if x & 0x800 != 0 {
            x -= 0x1000;
        }
        if y & 0x800 != 0 {
            y -= 0x1000;
        }
        Ok(Point { x, y })
    } else {
        let raw = take(bytes, cursor, end, 2)?;
        Ok(Point {
            x: i32::from(raw[0] as i8),
            y: i32::from(raw[1] as i8),
        })
    }
}

fn rasterize_outline(
    glyph: Option<&OutlineGlyph>,
    file: &OutlineFile,
    slot: usize,
    scale: f32,
    advance_px: f32,
) -> RasterizedGlyph {
    let mut contours = Vec::new();
    let mut bounds = None;
    collect_glyph(file, slot, Point::default(), 0, &mut contours, &mut bounds);
    if contours.is_empty() {
        return RasterizedGlyph {
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
            advance_px,
            coverage: Vec::new(),
        };
    }
    if bounds.is_none() {
        bounds = glyph.and_then(|glyph| glyph.bbox);
    }
    let Some([x0, y0, x1, y1]) = bounds else {
        return RasterizedGlyph {
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
            advance_px,
            coverage: Vec::new(),
        };
    };
    if x1 <= x0 || y1 <= y0 {
        return RasterizedGlyph {
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
            advance_px,
            coverage: Vec::new(),
        };
    }
    let left = (x0 as f32 * scale).floor() as i32;
    let right = (x1 as f32 * scale).ceil() as i32;
    let top = (y1 as f32 * scale).ceil() as i32;
    let bottom = (y0 as f32 * scale).floor() as i32;
    let width = (right - left).clamp(0, 512) as u32;
    let height = (top - bottom).clamp(0, 512) as u32;
    let mut coverage = vec![0_u8; width as usize * height as usize];
    const SAMPLES: i32 = 4;
    let total = (SAMPLES * SAMPLES) as usize;
    for py in 0..height as i32 {
        for px in 0..width as i32 {
            let mut inside_count = 0;
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let screen_x =
                        left as f32 + px as f32 + (sample_x as f32 + 0.5) / SAMPLES as f32;
                    let screen_y =
                        top as f32 - py as f32 - (sample_y as f32 + 0.5) / SAMPLES as f32;
                    let point = (screen_x / scale, screen_y / scale);
                    if inside_even_odd(point, &contours) {
                        inside_count += 1;
                    }
                }
            }
            coverage[(py as usize * width as usize) + px as usize] =
                ((inside_count * 255 + total / 2) / total) as u8;
        }
    }
    RasterizedGlyph {
        width,
        height,
        bearing_x: left,
        bearing_y: top,
        advance_px,
        coverage,
    }
}

type Polyline = Vec<(f32, f32)>;

fn collect_glyph(
    file: &OutlineFile,
    slot: usize,
    offset: Point,
    depth: usize,
    contours: &mut Vec<Polyline>,
    bounds: &mut Option<[i32; 4]>,
) {
    if depth > 12 {
        return;
    }
    let Some(glyph) = file.glyphs.get(&slot) else {
        return;
    };
    if let Some([x0, y0, x1, y1]) = glyph.bbox {
        union_bounds(
            bounds,
            [x0 + offset.x, y0 + offset.y, x1 + offset.x, y1 + offset.y],
        );
    }
    for contour in &glyph.contours {
        let flattened = flatten_contour(contour, offset);
        if !flattened.is_empty() {
            contours.push(flattened);
        }
    }
    for include in &glyph.includes {
        collect_glyph(
            file,
            include.slot,
            Point {
                x: offset.x + include.offset.x,
                y: offset.y + include.offset.y,
            },
            depth + 1,
            contours,
            bounds,
        );
    }
}

fn union_bounds(bounds: &mut Option<[i32; 4]>, next: [i32; 4]) {
    *bounds = Some(match *bounds {
        Some([x0, y0, x1, y1]) => [
            x0.min(next[0]),
            y0.min(next[1]),
            x1.max(next[2]),
            y1.max(next[3]),
        ],
        None => next,
    });
}

fn flatten_contour(contour: &[Segment], offset: Point) -> Polyline {
    let mut result = Vec::new();
    let mut current = None::<(f32, f32)>;
    for segment in contour {
        match segment {
            Segment::Move(point) => {
                let point = ((point.x + offset.x) as f32, (point.y + offset.y) as f32);
                result.push(point);
                current = Some(point);
            }
            Segment::Line(point) => {
                let point = ((point.x + offset.x) as f32, (point.y + offset.y) as f32);
                if current.is_none() {
                    result.push(point);
                } else {
                    result.push(point);
                }
                current = Some(point);
            }
            Segment::Curve(first, second, third) => {
                let Some(start) = current else { continue };
                let p1 = ((first.x + offset.x) as f32, (first.y + offset.y) as f32);
                let p2 = ((second.x + offset.x) as f32, (second.y + offset.y) as f32);
                let end = ((third.x + offset.x) as f32, (third.y + offset.y) as f32);
                for step in 1..=16 {
                    let t = step as f32 / 16.0;
                    let inv = 1.0 - t;
                    let x = inv.powi(3) * start.0
                        + 3.0 * inv.powi(2) * t * p1.0
                        + 3.0 * inv * t.powi(2) * p2.0
                        + t.powi(3) * end.0;
                    let y = inv.powi(3) * start.1
                        + 3.0 * inv.powi(2) * t * p1.1
                        + 3.0 * inv * t.powi(2) * p2.1
                        + t.powi(3) * end.1;
                    result.push((x, y));
                }
                current = Some(end);
            }
        }
    }
    result
}

fn inside_even_odd(point: (f32, f32), contours: &[Polyline]) -> bool {
    let mut inside = false;
    for contour in contours {
        if contour.len() < 3 {
            continue;
        }
        let mut previous = contour[contour.len() - 1];
        for current in contour.iter().copied() {
            if (current.1 > point.1) != (previous.1 > point.1) {
                let crossing_x = (previous.0 - current.0) * (point.1 - current.1)
                    / (previous.1 - current.1)
                    + current.0;
                if point.0 < crossing_x {
                    inside = !inside;
                }
            }
            previous = current;
        }
    }
    inside
}

fn blend_pixel(frame: &mut [u8], offset: usize, color: [u8; 4], coverage: u8) {
    let alpha = u32::from(coverage) * u32::from(color[3]) / 255;
    let inverse = 255 - alpha;
    for channel in 0..3 {
        frame[offset + channel] = ((u32::from(color[channel]) * alpha
            + u32::from(frame[offset + channel]) * inverse
            + 127)
            / 255) as u8;
    }
    frame[offset + 3] = 255;
}

fn read_byte(bytes: &[u8], cursor: &mut usize, end: usize) -> Result<u8, ResourceError> {
    let byte = *bytes
        .get(*cursor)
        .filter(|_| *cursor < end)
        .ok_or(ResourceError::InvalidFont("outline glyph is truncated"))?;
    *cursor += 1;
    Ok(byte)
}

fn take<'a>(
    bytes: &'a [u8],
    cursor: &mut usize,
    end: usize,
    count: usize,
) -> Result<&'a [u8], ResourceError> {
    let limit = cursor
        .checked_add(count)
        .filter(|limit| *limit <= end && *limit <= bytes.len())
        .ok_or(ResourceError::InvalidFont("outline glyph is truncated"))?;
    let result = &bytes[*cursor..limit];
    *cursor = limit;
    Ok(result)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, ResourceError> {
    let raw = bytes
        .get(offset..offset + 2)
        .ok_or(ResourceError::InvalidFont("outline field is truncated"))?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, ResourceError> {
    let raw = bytes
        .get(offset..offset + 4)
        .ok_or(ResourceError::InvalidFont("outline field is truncated"))?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_outline_families_load_and_rasterize_distinct_glyphs() {
        let homerton = NativeRasterFont::builtin(FontName::Homerton, 24).unwrap();
        let corpus = NativeRasterFont::builtin(FontName::Corpus, 24).unwrap();
        let trinity = NativeRasterFont::builtin(FontName::Trinity, 24).unwrap();
        for font in [&homerton, &corpus, &trinity] {
            assert_eq!(font.outline_version(), 8);
            assert!(font.design_size() > 0);
            let glyph = font.rasterize_glyph('A').unwrap();
            assert!(glyph.width > 8 && glyph.height > 8);
            assert!(glyph.coverage.iter().any(|pixel| *pixel > 0));
            assert!(
                glyph
                    .coverage
                    .iter()
                    .any(|pixel| *pixel > 0 && *pixel < 255)
            );
            let again = font.rasterize_glyph('A').unwrap();
            assert!(Arc::ptr_eq(&glyph, &again));
            assert_eq!(font.measure_text_px("WW"), font.advance_px('W') * 2.0);
            assert_eq!(font.measure_text_px("🙂A"), font.measure_text_px("?A"));
            let mut fallback = vec![211; 80 * 40 * 4];
            let mut question = fallback.clone();
            font.draw_text(&mut fallback, 80, 40, 4, 30, "🙂A", [30, 30, 30, 255]);
            font.draw_text(&mut question, 80, 40, 4, 30, "?A", [30, 30, 30, 255]);
            assert_eq!(fallback, question);
        }
        assert_ne!(
            homerton.rasterize_glyph('A').unwrap().coverage,
            corpus.rasterize_glyph('A').unwrap().coverage,
        );
        assert_ne!(
            corpus.rasterize_glyph('A').unwrap().coverage,
            trinity.rasterize_glyph('A').unwrap().coverage,
        );
    }

    #[test]
    fn specimen_paints_native_rom_outlines_into_a_real_rgba_surface() {
        let mut frame = vec![0; 800 * 600 * 4];
        render_font_specimen(&mut frame, 800, 600).unwrap();
        assert_eq!(&frame[..4], &[211, 211, 211, 255]);
        assert!(frame.chunks_exact(4).any(|pixel| pixel[0] < 100));
    }
}
