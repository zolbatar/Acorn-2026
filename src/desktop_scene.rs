//! Retained-in-memory scene construction for the modern Wimp shell.

use std::{
    collections::{HashMap, VecDeque},
    io::Cursor,
    sync::{Arc, OnceLock, Weak},
};

use parley::{
    Alignment, AlignmentOptions, FontContext, FontWeight, LayoutContext, PositionedLayoutItem,
    StyleProperty,
};
use vello::{
    Glyph, Scene,
    peniko::{
        Blob, Color, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality,
        kurbo::{Affine, BezPath, Rect, Stroke},
    },
};

use crate::{
    graphics::GraphicsSnapshot,
    renderer,
    wimp::{
        DESKTOP_ICONBAR_HEIGHT, DesktopIcon, DesktopIconImage, DesktopMenu, DesktopRect,
        DesktopWindow, DesktopWindowIcon, WindowFurnitureLayout, WorkArea,
    },
};

const DEFAULT_DESKTOP_OS_SIZE: (i32, i32) = (1600, 1200);
const FALLBACK_IMAGE_CACHE_MAX_BYTES: usize = 16 * 1024 * 1024;
const FALLBACK_IMAGE_CACHE_MAX_ENTRIES: usize = 1024;
const TEXT_LAYOUT_CACHE_MAX_ENTRIES: usize = 512;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum FallbackImageKind {
    Guest,
    System,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FallbackImageKey {
    kind: FallbackImageKind,
    identity: usize,
    width: u32,
    height: u32,
    selected: bool,
}

enum FallbackImageSource<'a> {
    Guest(&'a Arc<DesktopIconImage>),
    System(&'static crate::riscos_resources::RiscOsSprite),
}

impl FallbackImageSource<'_> {
    fn key(&self, selected: bool) -> FallbackImageKey {
        match self {
            Self::Guest(image) => FallbackImageKey {
                kind: FallbackImageKind::Guest,
                identity: Arc::as_ptr(image) as usize,
                width: image.width,
                height: image.height,
                selected,
            },
            Self::System(sprite) => FallbackImageKey {
                kind: FallbackImageKind::System,
                identity: *sprite as *const crate::riscos_resources::RiscOsSprite as usize,
                width: sprite.width,
                height: sprite.height,
                selected,
            },
        }
    }

    fn guard(&self) -> FallbackImageGuard {
        match self {
            Self::Guest(image) => FallbackImageGuard::Guest(Arc::downgrade(image)),
            Self::System(_) => FallbackImageGuard::System,
        }
    }

    fn pixels(&self) -> (&[[u8; 4]], u32, u32) {
        match self {
            Self::Guest(image) => (&image.rgba, image.width, image.height),
            Self::System(sprite) => (&sprite.rgba, sprite.width, sprite.height),
        }
    }
}

enum FallbackImageGuard {
    Guest(Weak<DesktopIconImage>),
    System,
}

impl FallbackImageGuard {
    fn matches(&self, source: &FallbackImageSource<'_>) -> bool {
        match (self, source) {
            (Self::Guest(cached), FallbackImageSource::Guest(current)) => {
                cached.ptr_eq(&Arc::downgrade(current))
            }
            (Self::System, FallbackImageSource::System(_)) => true,
            _ => false,
        }
    }
}

struct CachedFallbackImage {
    source: FallbackImageGuard,
    image: ImageData,
    bytes: usize,
}

#[derive(Default)]
struct FallbackImageCache {
    entries: HashMap<FallbackImageKey, CachedFallbackImage>,
    order: VecDeque<FallbackImageKey>,
    bytes: usize,
    #[cfg(test)]
    conversions: usize,
}

impl FallbackImageCache {
    fn guest_image(&mut self, image: &Arc<DesktopIconImage>, selected: bool) -> Option<ImageData> {
        self.image(FallbackImageSource::Guest(image), selected)
    }

    fn system_image(
        &mut self,
        sprite: &'static crate::riscos_resources::RiscOsSprite,
        selected: bool,
    ) -> Option<ImageData> {
        self.image(FallbackImageSource::System(sprite), selected)
    }

    fn image(&mut self, source: FallbackImageSource<'_>, selected: bool) -> Option<ImageData> {
        let key = source.key(selected);
        if let Some(image) = self.get(key, &source) {
            return Some(image);
        }

        let (pixels, width, height) = source.pixels();
        let image = pixels_to_image_data(width, height, pixels, selected)?;
        #[cfg(test)]
        {
            self.conversions += 1;
        }
        self.insert(key, source.guard(), image.clone());
        Some(image)
    }

    fn get(
        &mut self,
        key: FallbackImageKey,
        source: &FallbackImageSource<'_>,
    ) -> Option<ImageData> {
        if !self
            .entries
            .get(&key)
            .is_some_and(|cached| cached.source.matches(source))
        {
            self.remove(key);
            return None;
        }
        self.touch(key);
        self.entries.get(&key).map(|cached| cached.image.clone())
    }

    fn insert(&mut self, key: FallbackImageKey, source: FallbackImageGuard, image: ImageData) {
        let Some(bytes) = image_byte_len(image.width, image.height) else {
            return;
        };
        if bytes > FALLBACK_IMAGE_CACHE_MAX_BYTES {
            return;
        }
        self.remove(key);
        while self.entries.len() >= FALLBACK_IMAGE_CACHE_MAX_ENTRIES
            || self.bytes + bytes > FALLBACK_IMAGE_CACHE_MAX_BYTES
        {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.remove_entry(oldest);
        }
        self.order.push_back(key);
        self.bytes += bytes;
        self.entries.insert(
            key,
            CachedFallbackImage {
                source,
                image,
                bytes,
            },
        );
    }

    fn remove(&mut self, key: FallbackImageKey) {
        self.order.retain(|candidate| *candidate != key);
        self.remove_entry(key);
    }

    fn remove_entry(&mut self, key: FallbackImageKey) {
        if let Some(cached) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(cached.bytes);
        }
    }

    fn touch(&mut self, key: FallbackImageKey) {
        if let Some(index) = self.order.iter().position(|candidate| *candidate == key) {
            self.order.remove(index);
        }
        self.order.push_back(key);
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct TextLayoutKey {
    text: String,
    size: u32,
    weight: u32,
    max_width: Option<u32>,
}

impl TextLayoutKey {
    fn new(text: &str, size: f32, weight: f32, max_width: Option<f32>) -> Self {
        Self {
            text: text.to_owned(),
            size: size.to_bits(),
            weight: weight.to_bits(),
            max_width: max_width.map(f32::to_bits),
        }
    }
}

/// Maps Wimp OS coordinates into the host's physical backing surface, keeping
/// the selected desktop extent visible and centering any letterbox area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Viewport {
    pub scale: f64,
    pub offset_x: f64,
    pub offset_y: f64,
    desktop_width_os: i32,
    desktop_height_os: i32,
}

impl Viewport {
    /// Default 800×600 logical desktop compatibility helper for snapshots.
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self::for_desktop(
            width,
            height,
            DEFAULT_DESKTOP_OS_SIZE.0,
            DEFAULT_DESKTOP_OS_SIZE.1,
        )
    }

    pub(crate) fn for_desktop(
        width: u32,
        height: u32,
        desktop_width_os: i32,
        desktop_height_os: i32,
    ) -> Self {
        let desktop_width_os = desktop_width_os.max(1);
        let desktop_height_os = desktop_height_os.max(1);
        let desktop_width = f64::from(desktop_width_os);
        let desktop_height = f64::from(desktop_height_os);
        let scale = (f64::from(width) / desktop_width)
            .min(f64::from(height) / desktop_height)
            .max(0.0001);
        Self {
            scale,
            offset_x: (f64::from(width) - desktop_width * scale) / 2.0,
            offset_y: (f64::from(height) - desktop_height * scale) / 2.0,
            desktop_width_os,
            desktop_height_os,
        }
    }

    pub(crate) fn desktop_os_size(self) -> (i32, i32) {
        (self.desktop_width_os, self.desktop_height_os)
    }

    pub(crate) fn transform(self) -> Affine {
        Affine::translate((self.offset_x, self.offset_y)) * Affine::scale(self.scale)
    }

    pub(crate) fn desktop_point(self, physical_x: f64, physical_y: f64) -> Option<(i32, i32)> {
        let x = (physical_x - self.offset_x) / self.scale;
        let y = (physical_y - self.offset_y) / self.scale;
        if x < 0.0
            || y < 0.0
            || x >= f64::from(self.desktop_width_os)
            || y >= f64::from(self.desktop_height_os)
        {
            return None;
        }
        Some((x as i32, self.desktop_height_os - 1 - y as i32))
    }

    /// Map a physical cursor position to the nearest desktop edge. Used only
    /// while dragging, so letterbox space still lets the drag reach an edge.
    pub(crate) fn clamped_desktop_point(self, physical_x: f64, physical_y: f64) -> (i32, i32) {
        let x = ((physical_x - self.offset_x) / self.scale)
            .clamp(0.0, f64::from(self.desktop_width_os) - 1.0) as i32;
        let y = ((physical_y - self.offset_y) / self.scale)
            .clamp(0.0, f64::from(self.desktop_height_os) - 1.0) as i32;
        (x, self.desktop_height_os - 1 - y)
    }
}

pub(crate) struct DesktopSceneBuilder {
    fonts: FontContext,
    layouts: LayoutContext<[u8; 4]>,
    source_images: HashMap<(u64, Option<u32>), (u64, ImageData)>,
    fallback_images: FallbackImageCache,
    text_layouts: HashMap<TextLayoutKey, parley::Layout<[u8; 4]>>,
    text_layout_order: VecDeque<TextLayoutKey>,
    #[cfg(test)]
    text_layout_builds: usize,
    classic_image: Option<(u64, ImageData)>,
    font_error: Option<String>,
    bevelled: bool,
    desktop_width_os: i32,
    desktop_height_os: i32,
}

impl DesktopSceneBuilder {
    pub(crate) fn new() -> Self {
        let mut fonts = FontContext::new();
        let inter = Arc::new(include_bytes!("../resources/fonts/InterVariable.ttf").to_vec());
        fonts.collection.register_fonts(Blob::new(inter), None);
        Self {
            fonts,
            layouts: LayoutContext::new(),
            source_images: HashMap::new(),
            fallback_images: FallbackImageCache::default(),
            text_layouts: HashMap::new(),
            text_layout_order: VecDeque::new(),
            #[cfg(test)]
            text_layout_builds: 0,
            classic_image: None,
            font_error: None,
            bevelled: crate::configure::ConfigureStore::default()
                .load()
                .map(|c| c.bevelled_furniture)
                .unwrap_or(false),
            desktop_width_os: DEFAULT_DESKTOP_OS_SIZE.0,
            desktop_height_os: DEFAULT_DESKTOP_OS_SIZE.1,
        }
    }

    pub(crate) fn build(
        &mut self,
        windows: &[DesktopWindow],
        snapshots: &HashMap<(u64, Option<u32>), GraphicsSnapshot>,
        icons: &[DesktopIcon],
        window_icons: &[DesktopWindowIcon],
        menus: &[DesktopMenu],
        notice: Option<&str>,
        viewport: Viewport,
    ) -> Scene {
        (self.desktop_width_os, self.desktop_height_os) = viewport.desktop_os_size();
        let mut scene = Scene::new();
        let transform = viewport.transform();
        fill(
            &mut scene,
            transform,
            (184, 185, 187, 255),
            screen_rect(0, 0, self.desktop_width_os, self.desktop_height_os),
        );

        // desktop_windows is front-to-back, so scene order paints back-to-front.
        for window in windows.iter().rev() {
            let window_surface = (window.owner_task_id, Some(window.handle));
            let (surface, snapshot) = if let Some(snapshot) = snapshots.get(&window_surface) {
                (window_surface, Some(snapshot))
            } else if window.is_console_output {
                let default_surface = (window.owner_task_id, None);
                (default_surface, snapshots.get(&default_surface))
            } else {
                (window_surface, None)
            };
            self.draw_window(
                &mut scene,
                transform,
                window,
                snapshot,
                surface,
                window_icons,
            );
        }
        self.draw_icon_bar(
            &mut scene,
            transform,
            windows,
            icons,
            self.desktop_width_os,
            self.desktop_height_os,
        );
        if let Some(notice) = notice {
            self.draw_notice(
                &mut scene,
                transform,
                notice,
                self.desktop_width_os,
                self.desktop_height_os,
            );
        }
        for menu in menus {
            self.draw_menu(&mut scene, transform, menu);
        }
        scene
    }

    pub(crate) fn build_classic(
        &mut self,
        snapshot: &GraphicsSnapshot,
        width: u32,
        height: u32,
    ) -> Scene {
        let mut scene = Scene::new();
        fill(
            &mut scene,
            Affine::IDENTITY,
            (0, 0, 0, 255),
            Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        );
        let image = if let Some((revision, image)) = &self.classic_image {
            if *revision == snapshot.revision {
                image.clone()
            } else {
                self.rebuild_classic(snapshot)
            }
        } else {
            self.rebuild_classic(snapshot)
        };
        let scale = (f64::from(width) / f64::from(image.width))
            .min(f64::from(height) / f64::from(image.height));
        let offset_x = (f64::from(width) - f64::from(image.width) * scale) / 2.0;
        let offset_y = (f64::from(height) - f64::from(image.height) * scale) / 2.0;
        scene.draw_image(
            &ImageBrush::new(image).with_quality(ImageQuality::Low),
            Affine::translate((offset_x, offset_y)) * Affine::scale(scale),
        );
        scene
    }

    fn rebuild_classic(&mut self, snapshot: &GraphicsSnapshot) -> ImageData {
        let width = snapshot.mode.pixel_width;
        let height = snapshot.mode.pixel_height;
        let mut rgba = vec![0_u8; width as usize * height as usize * 4];
        renderer::render(snapshot, &mut rgba);
        let image = ImageData {
            data: Blob::new(Arc::new(rgba)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        };
        self.classic_image = Some((snapshot.revision, image.clone()));
        image
    }

    pub(crate) fn retain_active_surfaces(&mut self, surfaces: &[(u64, Option<u32>)]) {
        self.source_images
            .retain(|surface, _| surfaces.contains(surface));
    }

    fn map_rect(&self, area: DesktopRect) -> Rect {
        Rect::new(
            f64::from(area.min_x),
            f64::from(self.desktop_height_os - area.max_y),
            f64::from(area.max_x),
            f64::from(self.desktop_height_os - area.min_y),
        )
    }

    fn draw_window(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        window: &DesktopWindow,
        snapshot: Option<&GraphicsSnapshot>,
        surface: (u64, Option<u32>),
        icons: &[DesktopWindowIcon],
    ) {
        let furniture = layout(window, window.work_area, (window.scroll_x, window.scroll_y));
        let outer = self.map_rect(furniture.outer);
        if outer.width() <= 0.0 || outer.height() <= 0.0 {
            return;
        }
        // Paint the entire furniture backing first. Work areas, title bars and
        // controls cover their own regions; any remaining frame/bottom band
        // stays the same light neutral instead of exposing a dark fill.
        fill(scene, transform, (224, 224, 225, 255), outer);
        let work = self.map_rect(furniture.work_area);
        let modern_filer = window.title.starts_with("HostFS:")
            || window.title.to_ascii_lowercase().contains("filer");
        let display_manager = window.title.eq_ignore_ascii_case("Display Manager");
        let work_color = window_work_color(&window.title);
        fill(scene, transform, work_color, work);
        scene.push_clip_layer(Fill::NonZero, transform, &work);

        if let Some(snapshot) = snapshot {
            if let Some(image) = self.task_image(surface, snapshot) {
                // The raster remains at its guest mode's pixel grid and uses
                // nearest sampling; only Wimp furniture is rendered at host DPI.
                let osu_x =
                    mode_osu_per_pixel(snapshot.mode.logical_width, snapshot.mode.pixel_width);
                let osu_y =
                    mode_osu_per_pixel(snapshot.mode.logical_height, snapshot.mode.pixel_height);
                let screen_left =
                    window.work_area.min_x + window.work_extent.min_x - window.scroll_x;
                let screen_top =
                    window.work_area.max_y + window.work_extent.max_y - window.scroll_y;
                let image_width = f64::from(snapshot.mode.pixel_width) * f64::from(osu_x);
                let image_height = f64::from(snapshot.mode.pixel_height) * f64::from(osu_y);
                let image_rect = Rect::new(
                    f64::from(screen_left),
                    f64::from(self.desktop_height_os - screen_top),
                    f64::from(screen_left) + image_width,
                    f64::from(self.desktop_height_os - screen_top) + image_height,
                );
                let source_size = (image.width, image.height);
                scene.draw_image(
                    &ImageBrush::new(image).with_quality(ImageQuality::Low),
                    transform
                        * Affine::translate((image_rect.x0, image_rect.y0))
                        * Affine::scale_non_uniform(
                            image_rect.width() / f64::from(source_size.0),
                            image_rect.height() / f64::from(source_size.1),
                        ),
                );
            }
        }

        for icon in icons
            .iter()
            .filter(|icon| icon.window_handle == window.handle)
        {
            self.draw_guest_icon(scene, transform, icon, modern_filer, display_manager);
        }
        scene.pop_layer();
        if window.has_title {
            self.draw_title(scene, transform, window, furniture);
        }
        self.draw_scrollbar(scene, transform, furniture);
        if let Some(size_icon) = furniture.adjust_size_icon {
            self.draw_resize_glyph(scene, transform, size_icon);
        }
        // Separate the client area from furniture even where the scrollbar
        // track or bottom strip has no painted control.
        fill(
            scene,
            transform,
            (0, 0, 0, 255),
            Rect::new(work.x0, work.y1, work.x1, work.y1 + 2.0),
        );
        fill(
            scene,
            transform,
            (0, 0, 0, 255),
            Rect::new(work.x1, work.y0, work.x1 + 2.0, outer.y1),
        );
        if let Some(preview) = window.preview_area {
            let preview = layout(
                window,
                preview,
                window
                    .preview_scroll
                    .unwrap_or((window.scroll_x, window.scroll_y)),
            );
            outline(
                scene,
                transform,
                self.map_rect(preview.outer),
                (247, 247, 247, 210),
                2.0,
            );
        }
        outline(scene, transform, outer, (0, 0, 0, 255), 2.0);
    }

    fn task_image(
        &mut self,
        surface: (u64, Option<u32>),
        snapshot: &GraphicsSnapshot,
    ) -> Option<ImageData> {
        if let Some((revision, image)) = self.source_images.get(&surface)
            && *revision == snapshot.revision
        {
            return Some(image.clone());
        }
        let width = snapshot.mode.pixel_width;
        let height = snapshot.mode.pixel_height;
        if width == 0 || height == 0 || width.checked_mul(height)? > 4_194_304 {
            return None;
        }
        let mut rgba = vec![0_u8; width as usize * height as usize * 4];
        renderer::render_desktop_content(snapshot, &mut rgba);
        let image = ImageData {
            data: Blob::new(Arc::new(rgba)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        };
        self.source_images
            .insert(surface, (snapshot.revision, image.clone()));
        Some(image)
    }

    fn draw_title(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        window: &DesktopWindow,
        f: WindowFurnitureLayout,
    ) {
        let Some(title) = f.title_bar else { return };
        let title_rect = self.map_rect(title);
        let title_color = if window.focused {
            (239, 221, 105, 255)
        } else {
            (211, 212, 214, 255)
        };
        let title_band = Rect::new(
            f64::from(f.outer.min_x + 2),
            title_rect.y0,
            f64::from(f.outer.max_x - 2),
            title_rect.y1,
        );
        fill(scene, transform, title_color, title_band);
        if self.bevelled {
            let middle = (title_rect.y0 + title_rect.y1) / 2.0;
            tool_strip(
                scene,
                transform,
                "tbarmidt22",
                Rect::new(title_rect.x0, title_rect.y0, title_rect.x1, middle),
                true,
                window.focused,
            );
            tool_strip(
                scene,
                transform,
                "tbarmidb22",
                Rect::new(title_rect.x0, middle, title_rect.x1, title_rect.y1),
                true,
                window.focused,
            );
        }
        fill(
            scene,
            transform,
            (0, 0, 0, 255),
            Rect::new(
                title_band.x0,
                title_rect.y1 - 2.0,
                title_band.x1,
                title_rect.y1,
            ),
        );
        for control in [f.back_icon, f.close_icon].into_iter().flatten() {
            let control = self.map_rect(control);
            fill(
                scene,
                transform,
                (0, 0, 0, 255),
                Rect::new(
                    control.x1 - 1.0,
                    title_rect.y0,
                    control.x1 + 1.0,
                    title_rect.y1,
                ),
            );
        }
        if let Some(toggle) = f.toggle_size_icon {
            let toggle = self.map_rect(toggle);
            fill(
                scene,
                transform,
                (0, 0, 0, 255),
                Rect::new(
                    toggle.x0 - 1.0,
                    title_rect.y0,
                    toggle.x0 + 1.0,
                    title_rect.y1,
                ),
            );
        }
        let title_width = self.text_width(&window.title, 22.0, 600.0);
        scene.push_clip_layer(Fill::NonZero, transform, &title_rect);
        self.text(
            scene,
            &window.title,
            ((title_rect.x0 + title_rect.x1 - title_width) / 2.0).max(title_rect.x0 + 4.0),
            title_rect.y0 + 3.0,
            22.0,
            (0, 0, 0, 255),
            600.0,
            transform,
        );
        scene.pop_layer();
        if let Some(back) = f.back_icon {
            self.draw_back_glyph(scene, transform, back);
        }
        if let Some(close) = f.close_icon {
            self.draw_close_glyph(scene, transform, close);
        }
        if let Some(toggle) = f.toggle_size_icon {
            self.draw_toggle_glyph(scene, transform, toggle);
        }
    }

    fn draw_back_glyph(&self, scene: &mut Scene, transform: Affine, bounds: DesktopRect) {
        if self.bevelled {
            tool_image(scene, transform, "bicon22", self.map_rect(bounds), false);
            return;
        }
        let b = self.map_rect(bounds);
        let cx = (b.x0 + b.x1) / 2.0;
        let cy = (b.y0 + b.y1) / 2.0;
        let back = Rect::new(cx - 11.0, cy - 11.0, cx + 5.0, cy + 5.0);
        let front = Rect::new(cx - 5.0, cy - 5.0, cx + 11.0, cy + 11.0);
        fill(scene, transform, (153, 153, 153, 255), back);
        outline(scene, transform, back, (0, 0, 0, 255), 2.0);
        fill(scene, transform, (221, 221, 221, 255), front);
        outline(scene, transform, front, (0, 0, 0, 255), 2.0);
    }

    fn draw_close_glyph(&self, scene: &mut Scene, transform: Affine, bounds: DesktopRect) {
        if self.bevelled {
            tool_image(scene, transform, "cicon22", self.map_rect(bounds), false);
            return;
        }
        let b = self.map_rect(bounds);
        let cx = (b.x0 + b.x1) / 2.0;
        let cy = (b.y0 + b.y1) / 2.0;
        let mut path = BezPath::new();
        path.move_to((cx, cy - 5.5));
        path.curve_to((cx - 6.5, cy - 15.5), (cx - 15.5, cy - 6.5), (cx - 5.5, cy));
        path.curve_to((cx - 15.5, cy + 6.5), (cx - 6.5, cy + 15.5), (cx, cy + 5.5));
        path.curve_to((cx + 6.5, cy + 15.5), (cx + 15.5, cy + 6.5), (cx + 5.5, cy));
        path.curve_to((cx + 15.5, cy - 6.5), (cx + 6.5, cy - 15.5), (cx, cy - 5.5));
        path.close_path();
        stroke(scene, transform, &path, 2.0, (0, 0, 0, 255));
    }

    fn draw_toggle_glyph(&self, scene: &mut Scene, transform: Affine, bounds: DesktopRect) {
        if self.bevelled {
            tool_image(scene, transform, "ticon22", self.map_rect(bounds), false);
            return;
        }
        let b = self.map_rect(bounds);
        let cx = (b.x0 + b.x1) / 2.0;
        let cy = (b.y0 + b.y1) / 2.0;
        outline(
            scene,
            transform,
            Rect::new(cx - 11.0, cy - 11.0, cx - 2.0, cy - 2.0),
            (0, 0, 0, 255),
            2.0,
        );
        let large = Rect::new(cx - 3.0, cy - 3.0, cx + 11.0, cy + 11.0);
        fill(scene, transform, (153, 153, 153, 255), large);
        outline(scene, transform, large, (0, 0, 0, 255), 2.0);
    }

    fn draw_resize_glyph(&self, scene: &mut Scene, transform: Affine, bounds: DesktopRect) {
        if self.bevelled {
            tool_image(scene, transform, "sicon22", self.map_rect(bounds), false);
            return;
        }
        let b = self.map_rect(bounds);
        fill(scene, transform, (224, 224, 225, 255), b);
        fill(
            scene,
            transform,
            (0, 0, 0, 255),
            Rect::new(b.x0, b.y0, b.x1, b.y0 + 2.0),
        );
        let cx = (b.x0 + b.x1) / 2.0;
        let cy = (b.y0 + b.y1) / 2.0;
        let outer = Rect::new(cx - 11.0, cy - 11.0, cx + 11.0, cy + 11.0);
        let inset = Rect::new(cx - 11.0, cy - 11.0, cx - 2.0, cy - 2.0);
        outline(scene, transform, outer, (0, 0, 0, 255), 2.0);
        outline(scene, transform, inset, (0, 0, 0, 255), 2.0);
    }

    fn draw_scrollbar(&mut self, scene: &mut Scene, transform: Affine, f: WindowFurnitureLayout) {
        let Some(bar) = f.vertical_scrollbar else {
            return;
        };
        if self.bevelled {
            tool_image(
                scene,
                transform,
                "uicon22",
                self.map_rect(bar.up_arrow),
                false,
            );
            tool_image(
                scene,
                transform,
                "dicon22",
                self.map_rect(bar.down_arrow),
                false,
            );
            tool_strip(
                scene,
                transform,
                "vwellt22",
                self.map_rect(bar.track),
                false,
                false,
            );
            let slider = self.map_rect(bar.slider).inset(-2.0);
            tool_strip(scene, transform, "vbarmid22", slider, false, false);
            tool_image(
                scene,
                transform,
                "vbart22",
                Rect::new(slider.x0, slider.y0, slider.x1, slider.y0 + 3.0),
                false,
            );
            tool_image(
                scene,
                transform,
                "vbarb22",
                Rect::new(slider.x0, slider.y1 - 4.0, slider.x1, slider.y1),
                false,
            );
            return;
        }
        let bounds = self.map_rect(bar.bounds);
        fill(scene, transform, (226, 226, 227, 255), bounds);
        fill(
            scene,
            transform,
            (0, 0, 0, 255),
            Rect::new(bounds.x0, bounds.y0, bounds.x0 + 2.0, bounds.y1),
        );
        let up = self.map_rect(bar.up_arrow);
        let down = self.map_rect(bar.down_arrow);
        fill(scene, transform, (232, 232, 233, 255), up);
        fill(scene, transform, (232, 232, 233, 255), down);
        fill(
            scene,
            transform,
            (202, 202, 203, 255),
            self.map_rect(bar.track),
        );
        fill(
            scene,
            transform,
            (221, 221, 221, 255),
            self.map_rect(bar.slider).inset(-3.0),
        );
        outline(
            scene,
            transform,
            self.map_rect(bar.slider).inset(-3.0),
            (0, 0, 0, 255),
            2.0,
        );
        fill(
            scene,
            transform,
            (0, 0, 0, 255),
            Rect::new(up.x0, up.y1 - 1.0, up.x1, up.y1 + 1.0),
        );
        fill(
            scene,
            transform,
            (0, 0, 0, 255),
            Rect::new(down.x0, down.y0 - 1.0, down.x1, down.y0 + 1.0),
        );
        self.draw_scroll_arrow(scene, transform, bar.up_arrow, true);
        self.draw_scroll_arrow(scene, transform, bar.down_arrow, false);
    }

    fn draw_scroll_arrow(
        &self,
        scene: &mut Scene,
        transform: Affine,
        bounds: DesktopRect,
        up: bool,
    ) {
        let b = self.map_rect(bounds);
        let cx = (b.x0 + b.x1) / 2.0;
        let cy = (b.y0 + b.y1) / 2.0;
        let mut path = BezPath::new();
        if up {
            path.move_to((cx - 5.0, cy + 12.0));
            path.line_to((cx + 5.0, cy + 12.0));
            path.line_to((cx + 5.0, cy - 1.0));
            path.line_to((cx + 11.0, cy - 1.0));
            path.line_to((cx, cy - 12.0));
            path.line_to((cx - 11.0, cy - 1.0));
            path.line_to((cx - 5.0, cy - 1.0));
            path.close_path();
        } else {
            path.move_to((cx - 5.0, cy - 12.0));
            path.line_to((cx + 5.0, cy - 12.0));
            path.line_to((cx + 5.0, cy + 1.0));
            path.line_to((cx + 11.0, cy + 1.0));
            path.line_to((cx, cy + 12.0));
            path.line_to((cx - 11.0, cy + 1.0));
            path.line_to((cx - 5.0, cy + 1.0));
            path.close_path();
        }
        stroke(scene, transform, &path, 2.0, (0, 0, 0, 255));
    }

    fn draw_guest_icon(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        icon: &DesktopWindowIcon,
        modern_filer: bool,
        display_manager: bool,
    ) {
        let bounds = self.map_rect(icon.bounds);
        if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            return;
        }
        let selected = icon.flags & (1 << 21) != 0 && icon.flags & (1 << 22) == 0;
        let filled = icon.flags & (1 << 5) != 0;
        let pictorial = icon.high_resolution_image.is_some() || icon.sprite_name.is_some();
        let (ink, paper) = renderer::window_icon_colours(icon.flags);
        if filled || (selected && !pictorial && !modern_filer) {
            fill(scene, transform, rgba(paper), bounds);
        }
        if icon.flags & (1 << 2) != 0 {
            outline(scene, transform, bounds, (0, 0, 0, 255), 2.0);
        }

        let horizontal = pictorial && icon.flags & (1 << 3) == 0;
        let compact_size = (bounds.height() as i32 - 8).clamp(1, 28);
        let label_height = if icon.label.is_empty() { 0.0 } else { 36.0 };
        let art_bounds = if horizontal {
            let side = f64::from(compact_size);
            Rect::new(
                bounds.x0 + 8.0,
                bounds.y0 + (bounds.height() - side) / 2.0,
                bounds.x0 + 8.0 + side,
                bounds.y0 + (bounds.height() + side) / 2.0,
            )
        } else {
            Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1 - label_height)
        };

        scene.push_clip_layer(Fill::NonZero, transform, &bounds);
        if let Some(image) = icon
            .sprite_name
            .as_deref()
            .filter(|_| modern_filer)
            .and_then(modern_filer_art)
        {
            let side = if horizontal {
                f64::from(compact_size)
            } else {
                56.0
            };
            let cx = (art_bounds.x0 + art_bounds.x1) / 2.0;
            let cy = (art_bounds.y0 + art_bounds.y1) / 2.0;
            draw_image_data_fit(
                scene,
                transform,
                image.clone(),
                Rect::new(
                    cx - side / 2.0,
                    cy - side / 2.0,
                    cx + side / 2.0,
                    cy + side / 2.0,
                ),
                1.0,
                ImageQuality::High,
            );
        } else if let Some(image) = icon.high_resolution_image.as_ref() {
            self.draw_icon_image(
                scene,
                transform,
                image,
                art_bounds,
                if modern_filer { 1.0 } else { 0.5 },
                selected && !modern_filer,
            );
        } else if let Some(sprite_name) = icon.sprite_name.as_deref()
            && let Some(sprite) = renderer::desktop_system_sprite(sprite_name)
        {
            self.draw_system_sprite(
                scene,
                transform,
                sprite,
                art_bounds,
                selected && !modern_filer,
            );
        }
        scene.pop_layer();

        if !icon.label.is_empty() {
            let label_size = if modern_filer {
                22.0
            } else if display_manager {
                23.0
            } else {
                16.0
            };
            let label_weight = if modern_filer { 600.0 } else { 400.0 };
            let ink = if modern_filer && selected {
                [255, 255, 255, 255]
            } else if modern_filer {
                [0, 0, 0, 255]
            } else {
                ink
            };
            let width = self.text_width(&icon.label, label_size, label_weight);
            let max_width = (bounds.width() - 20.0).max(1.0);
            let centered = icon.flags & (1 << 3) != 0;
            let x = if width > max_width {
                bounds.x0 + 10.0
            } else if icon.flags & (1 << 9) != 0 {
                bounds.x1 - 10.0 - width
            } else if centered {
                bounds.x0 + (bounds.width() - width) / 2.0
            } else if horizontal {
                bounds.x0 + f64::from(compact_size) + 16.0
            } else {
                bounds.x0 + 10.0
            };
            let text_only = icon.high_resolution_image.is_none()
                && icon
                    .sprite_name
                    .as_deref()
                    .and_then(renderer::desktop_system_sprite)
                    .is_none();
            let baseline = if (text_only || horizontal) && icon.flags & (1 << 4) != 0 {
                (bounds.y0 + bounds.y1) / 2.0 + 7.0
            } else {
                bounds.y1 - 6.0
            };
            let y = baseline - f64::from(label_size) * 1.1;
            scene.push_clip_layer(Fill::NonZero, transform, &bounds);
            if selected && (modern_filer || pictorial) {
                let highlight_bounds = if modern_filer {
                    let (label_width, label_height) = self.text_layout_size(
                        &icon.label,
                        label_size,
                        label_weight,
                        Some(max_width as f32),
                    );
                    Rect::new(
                        x - 3.0,
                        y - 4.0,
                        x + label_width + 3.0,
                        y + label_height + 4.0,
                    )
                } else {
                    Rect::new(
                        x - 3.0,
                        baseline - f64::from(label_size) * 1.3,
                        x + width + 3.0,
                        baseline + 5.0,
                    )
                };
                fill(scene, transform, (0, 0, 0, 255), highlight_bounds);
            }
            self.text_with_width(
                scene,
                &icon.label,
                x,
                y,
                label_size,
                rgba(ink),
                label_weight,
                transform,
                Some(max_width as f32),
            );
            scene.pop_layer();
        }
    }

    fn draw_icon_bar(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        windows: &[DesktopWindow],
        icons: &[DesktopIcon],
        desktop_width_os: i32,
        desktop_height_os: i32,
    ) {
        let top = desktop_height_os - DESKTOP_ICONBAR_HEIGHT;
        fill(
            scene,
            transform,
            (224, 224, 224, 255),
            screen_rect(0, top, desktop_width_os, DESKTOP_ICONBAR_HEIGHT),
        );
        fill(
            scene,
            transform,
            (0, 0, 0, 255),
            screen_rect(0, top, desktop_width_os, 2),
        );
        let frontmost_task = windows.first().map(|window| window.owner_task_id);
        for icon in icons {
            let bounds = self.map_rect(icon.bounds);
            let artwork = Rect::new(
                bounds.x0,
                f64::from(top + 4),
                bounds.x1,
                f64::from(top + 76),
            );
            scene.push_clip_layer(Fill::NonZero, transform, &artwork);
            if icon
                .sprite_name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case("display"))
            {
                draw_image_data_fit(
                    scene,
                    transform,
                    display_manager_logo().clone(),
                    artwork,
                    1.0,
                    ImageQuality::High,
                );
            } else if icon.side == crate::wimp::IconBarSide::Devices
                && icon
                    .sprite_name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case("harddisc"))
            {
                let center_x = (artwork.x0 + artwork.x1) / 2.0;
                let harddisk_artwork =
                    Rect::new(center_x - 60.0, artwork.y0, center_x + 60.0, artwork.y1);
                draw_image_data_fit(
                    scene,
                    transform,
                    harddisc_logo().clone(),
                    harddisk_artwork,
                    1.0,
                    ImageQuality::High,
                );
            } else if let Some(image) = icon.high_resolution_image.as_ref() {
                self.draw_icon_image(scene, transform, image, artwork, 0.5, false);
            } else if let Some(sprite) = icon
                .sprite_name
                .as_deref()
                .and_then(renderer::desktop_system_sprite)
            {
                self.draw_system_sprite(scene, transform, sprite, artwork, false);
            }
            scene.pop_layer();
            let label_clip = Rect::new(
                bounds.x0 + 8.0,
                f64::from(top + 76),
                bounds.x1 - 8.0,
                f64::from(desktop_height_os - 6),
            );
            let label_width = self.text_width(&icon.label, 18.0, 600.0);
            scene.push_clip_layer(Fill::NonZero, transform, &label_clip);
            self.text(
                scene,
                &icon.label,
                (bounds.x0 + bounds.x1 - label_width) / 2.0,
                f64::from(top + 77),
                18.0,
                (0, 0, 0, 255),
                600.0,
                transform,
            );
            scene.pop_layer();
            if icon
                .activate_task_id
                .is_some_and(|task_id| Some(task_id) == frontmost_task)
            {
                let center = (bounds.x0 + bounds.x1) / 2.0;
                fill(
                    scene,
                    transform,
                    (37, 103, 202, 255),
                    Rect::new(
                        center - 20.0,
                        f64::from(desktop_height_os - 5),
                        center + 20.0,
                        f64::from(desktop_height_os - 2),
                    ),
                );
            }
        }
        let control = Rect::new(
            f64::from(desktop_width_os - 72),
            f64::from(top + (DESKTOP_ICONBAR_HEIGHT - 64) / 2),
            f64::from(desktop_width_os - 8),
            f64::from(top + (DESKTOP_ICONBAR_HEIGHT - 64) / 2 + 64),
        );
        draw_image_data_fit(
            scene,
            transform,
            acorn_logo().clone(),
            control,
            1.0,
            ImageQuality::High,
        );
    }

    fn draw_notice(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &str,
        desktop_width_os: i32,
        desktop_height_os: i32,
    ) {
        let width = f64::from((desktop_width_os - 48).clamp(120, 740));
        let height = 118.0_f64.min(f64::from((desktop_height_os - 48).max(0)));
        let left = (f64::from(desktop_width_os) - width) / 2.0;
        let bottom_margin = 142.0_f64.min((f64::from(desktop_height_os) - height).max(24.0));
        let bounds = Rect::new(
            left,
            f64::from(desktop_height_os) - bottom_margin - height,
            left + width,
            f64::from(desktop_height_os) - bottom_margin,
        );
        scene.draw_blurred_rounded_rect(transform, bounds, color((0, 0, 0, 45)), 3.0, 5.0);
        fill(scene, transform, (252, 249, 228, 255), bounds);
        outline(scene, transform, bounds, (55, 55, 56, 255), 2.0);
        self.text(
            scene,
            text,
            bounds.x0 + 22.0,
            bounds.y0 + 38.0,
            19.0,
            (30, 30, 31, 255),
            400.0,
            transform,
        );
    }

    fn draw_menu(&mut self, scene: &mut Scene, transform: Affine, menu: &DesktopMenu) {
        let bounds = self.map_rect(menu.bounds);
        scene.draw_blurred_rounded_rect(transform, bounds, color((0, 0, 0, 55)), 2.0, 4.0);
        fill(scene, transform, palette(menu.work_background), bounds);
        outline(scene, transform, bounds, (54, 54, 55, 255), 2.0);
        if let Some(title) = menu.title_bounds {
            let title = self.map_rect(title);
            fill(scene, transform, palette(menu.title_background), title);
            self.text(
                scene,
                &menu.title,
                title.x0 + 10.0,
                title.y0 + 5.0,
                18.0,
                palette_rgb(menu.title_foreground),
                500.0,
                transform,
            );
        }
        for row in &menu.rows {
            let bounds = self.map_rect(row.bounds);
            if row.separator_after {
                fill(
                    scene,
                    transform,
                    (118, 118, 119, 255),
                    self.map_rect(DesktopRect {
                        min_x: row.bounds.min_x + 4,
                        min_y: row.bounds.min_y + 4,
                        max_x: row.bounds.max_x - 4,
                        max_y: row.bounds.min_y + 5,
                    }),
                );
            }
            if row.selected {
                fill(scene, transform, (52, 94, 143, 255), bounds);
            }
            let text_color = if row.shaded {
                (135, 135, 136, 255)
            } else if row.selected {
                (255, 255, 255, 255)
            } else {
                palette_rgb(menu.work_foreground)
            };
            let label = if row.tick {
                format!("✓  {}", row.label)
            } else if row.has_submenu {
                format!("{}  ›", row.label)
            } else {
                row.label.clone()
            };
            self.text(
                scene,
                &label,
                bounds.x0 + 10.0,
                bounds.y0 + 5.0,
                17.0,
                text_color,
                400.0,
                transform,
            );
        }
    }

    fn text(
        &mut self,
        scene: &mut Scene,
        text: &str,
        x: f64,
        y: f64,
        size: f32,
        rgba: (u8, u8, u8, u8),
        weight: f32,
        transform: Affine,
    ) {
        self.text_with_width(scene, text, x, y, size, rgba, weight, transform, None);
    }

    fn text_with_width(
        &mut self,
        scene: &mut Scene,
        text: &str,
        x: f64,
        y: f64,
        size: f32,
        rgba: (u8, u8, u8, u8),
        weight: f32,
        transform: Affine,
        max_width: Option<f32>,
    ) {
        let Some(key) = self.ensure_text_layout(text, size, weight, max_width) else {
            return;
        };
        self.touch_text_layout(&key);
        let Some(layout) = self.text_layouts.get(&key) else {
            return;
        };
        draw_parley_layout(scene, layout, x, y, rgba, transform);
    }

    fn text_width(&mut self, text: &str, size: f32, weight: f32) -> f64 {
        let Some(key) = self.ensure_text_layout(text, size, weight, None) else {
            return 0.0;
        };
        self.touch_text_layout(&key);
        self.text_layouts
            .get(&key)
            .map(|layout| f64::from(layout.width()))
            .unwrap_or(0.0)
    }

    fn text_layout_size(
        &mut self,
        text: &str,
        size: f32,
        weight: f32,
        max_width: Option<f32>,
    ) -> (f64, f64) {
        let Some(key) = self.ensure_text_layout(text, size, weight, max_width) else {
            return (0.0, 0.0);
        };
        self.touch_text_layout(&key);
        self.text_layouts
            .get(&key)
            .map(|layout| (f64::from(layout.width()), f64::from(layout.height())))
            .unwrap_or((0.0, 0.0))
    }

    fn ensure_text_layout(
        &mut self,
        text: &str,
        size: f32,
        weight: f32,
        max_width: Option<f32>,
    ) -> Option<TextLayoutKey> {
        if text.is_empty() || self.font_error.is_some() {
            return None;
        }
        let key = TextLayoutKey::new(text, size, weight, max_width);
        if self.text_layouts.contains_key(&key) {
            self.touch_text_layout(&key);
            return Some(key);
        }
        let base_key = TextLayoutKey::new(text, size, weight, None);
        if !self.text_layouts.contains_key(&base_key) {
            let layout = self.build_text_layout(text, size, weight);
            self.insert_text_layout(base_key.clone(), layout);
        } else {
            self.touch_text_layout(&base_key);
        }

        if key != base_key {
            // Keep shaping and measurement in one cached unwrapped layout.
            // Width-constrained variants only redo line breaking.
            let mut layout = self.text_layouts.get(&base_key)?.clone();
            layout.break_all_lines(max_width);
            layout.align(Alignment::Start, AlignmentOptions::default());
            self.insert_text_layout(key.clone(), layout);
        }
        Some(key)
    }

    fn build_text_layout(&mut self, text: &str, size: f32, weight: f32) -> parley::Layout<[u8; 4]> {
        #[cfg(test)]
        {
            self.text_layout_builds += 1;
        }
        let mut builder = self
            .layouts
            .ranged_builder(&mut self.fonts, text, 1.0, true);
        builder.push_default(StyleProperty::FontFamily(parley::FontFamily::named(
            "Inter",
        )));
        builder.push_default(StyleProperty::FontSize(size));
        builder.push_default(StyleProperty::FontWeight(FontWeight::new(weight)));
        builder.push_default(StyleProperty::Brush([0, 0, 0, 0]));
        let mut layout = builder.build(text);
        layout.break_all_lines(None);
        layout.align(Alignment::Start, AlignmentOptions::default());
        layout
    }

    fn insert_text_layout(&mut self, key: TextLayoutKey, layout: parley::Layout<[u8; 4]>) {
        if let Some(index) = self
            .text_layout_order
            .iter()
            .position(|candidate| *candidate == key)
        {
            self.text_layout_order.remove(index);
            self.text_layout_order.push_back(key.clone());
            self.text_layouts.insert(key.clone(), layout);
            return;
        }
        while self.text_layouts.len() >= TEXT_LAYOUT_CACHE_MAX_ENTRIES {
            let Some(oldest) = self.text_layout_order.pop_front() else {
                break;
            };
            self.text_layouts.remove(&oldest);
        }
        self.text_layout_order.push_back(key.clone());
        self.text_layouts.insert(key, layout);
    }

    fn touch_text_layout(&mut self, key: &TextLayoutKey) {
        if let Some(index) = self
            .text_layout_order
            .iter()
            .position(|candidate| candidate == key)
        {
            self.text_layout_order.remove(index);
        }
        self.text_layout_order.push_back(key.clone());
    }

    fn draw_icon_image(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        image: &Arc<DesktopIconImage>,
        bounds: Rect,
        source_pixel_scale: f64,
        selected: bool,
    ) {
        let Some(image) = self.fallback_images.guest_image(image, selected) else {
            return;
        };
        draw_image_data_fit(
            scene,
            transform,
            image,
            bounds,
            source_pixel_scale,
            ImageQuality::High,
        );
    }

    fn draw_system_sprite(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        sprite: &'static crate::riscos_resources::RiscOsSprite,
        bounds: Rect,
        selected: bool,
    ) {
        let Some(image) = self.fallback_images.system_image(sprite, selected) else {
            return;
        };
        draw_image_data_fit(scene, transform, image, bounds, 2.0, ImageQuality::Low);
    }
}

fn draw_image_data_fit(
    scene: &mut Scene,
    transform: Affine,
    image: ImageData,
    bounds: Rect,
    source_pixel_scale: f64,
    quality: ImageQuality,
) {
    if image.width == 0
        || image.height == 0
        || bounds.width() <= 0.0
        || bounds.height() <= 0.0
        || source_pixel_scale <= 0.0
    {
        return;
    }
    let source_width = f64::from(image.width) * source_pixel_scale;
    let source_height = f64::from(image.height) * source_pixel_scale;
    let fit = (bounds.width() / source_width)
        .min(bounds.height() / source_height)
        .min(1.0);
    let scale = source_pixel_scale * fit;
    let drawn_width = f64::from(image.width) * scale;
    let drawn_height = f64::from(image.height) * scale;
    let x = bounds.x0 + (bounds.width() - drawn_width) / 2.0;
    let y = bounds.y0 + (bounds.height() - drawn_height) / 2.0;
    scene.draw_image(
        &ImageBrush::new(image).with_quality(quality),
        transform * Affine::translate((x, y)) * Affine::scale(scale),
    );
}

fn draw_parley_layout(
    scene: &mut Scene,
    layout: &parley::Layout<[u8; 4]>,
    x: f64,
    y: f64,
    rgba: (u8, u8, u8, u8),
    transform: Affine,
) {
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(run) = item else {
                continue;
            };
            let coords: Vec<_> = run.run().normalized_coords().iter().copied().collect();
            let glyphs = run.positioned_glyphs().map(|glyph| Glyph {
                id: glyph.id,
                x: glyph.x,
                y: glyph.y,
            });
            scene
                .draw_glyphs(run.run().font())
                .font_size(run.run().font_size())
                .normalized_coords(&coords)
                .brush(color(rgba))
                .transform(transform * Affine::translate((x, y)))
                .draw(Fill::NonZero, glyphs);
        }
    }
}

fn image_byte_len(width: u32, height: u32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
}

fn pixels_to_image_data(
    width: u32,
    height: u32,
    pixels: &[[u8; 4]],
    selected: bool,
) -> Option<ImageData> {
    let pixel_count = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?;
    if width == 0 || height == 0 || pixels.len() != pixel_count {
        return None;
    }
    let mut rgba = Vec::with_capacity(image_byte_len(width, height)?);
    for pixel in pixels {
        let pixel = if selected {
            renderer::selected_sprite_colour(*pixel)
        } else {
            *pixel
        };
        rgba.extend_from_slice(&pixel);
    }
    rgba_image_data(width, height, rgba)
}

fn rgba_image_data(width: u32, height: u32, rgba: Vec<u8>) -> Option<ImageData> {
    let expected = image_byte_len(width, height)?;
    if width == 0 || height == 0 || rgba.len() != expected {
        return None;
    }
    Some(ImageData {
        data: Blob::new(Arc::new(rgba)),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    })
}

fn tool_art(name: &str, yellow: bool) -> ImageData {
    static IMAGES: OnceLock<HashMap<(String, bool), ImageData>> = OnceLock::new();
    IMAGES
        .get_or_init(|| {
            let sprites = crate::riscos_resources::builtin_sprite_set(
                crate::riscos_resources::SpriteSet::Tools3d,
            )
            .expect("bundled tools");
            let mut images = HashMap::new();
            for name in [
                "bicon22",
                "cicon22",
                "ticon22",
                "sicon22",
                "uicon22",
                "dicon22",
                "tbarmidt22",
                "tbarmidb22",
                "vwellt22",
                "vbarmid22",
                "vbart22",
                "vbarb22",
            ] {
                let sprite = sprites.get(name).expect("bundled tool");
                for yellow in [false, true] {
                    let pixels = sprite
                        .rgba
                        .iter()
                        .flat_map(|p| {
                            if yellow {
                                [
                                    ((u16::from(p[0]) * 239) / 255) as u8,
                                    ((u16::from(p[1]) * 221) / 255) as u8,
                                    ((u16::from(p[2]) * 105) / 255) as u8,
                                    p[3],
                                ]
                            } else {
                                *p
                            }
                        })
                        .collect();
                    images.insert(
                        (name.to_owned(), yellow),
                        rgba_image_data(sprite.width, sprite.height, pixels).unwrap(),
                    );
                }
            }
            images
        })
        .get(&(name.to_owned(), yellow))
        .unwrap()
        .clone()
}

fn tool_image(scene: &mut Scene, transform: Affine, name: &str, bounds: Rect, yellow: bool) {
    let image = tool_art(name, yellow);
    let scale = Affine::scale_non_uniform(
        bounds.width() / f64::from(image.width),
        bounds.height() / f64::from(image.height),
    );
    scene.draw_image(
        &ImageBrush::new(image).with_quality(ImageQuality::Low),
        transform * Affine::translate((bounds.x0, bounds.y0)) * scale,
    );
}

fn tool_strip(
    scene: &mut Scene,
    transform: Affine,
    name: &str,
    bounds: Rect,
    horizontal: bool,
    yellow: bool,
) {
    let image = tool_art(name, yellow);
    scene.push_clip_layer(Fill::NonZero, transform, &bounds);
    let scale = if horizontal {
        bounds.height() / f64::from(image.height)
    } else {
        bounds.width() / f64::from(image.width)
    };
    let step = if horizontal {
        f64::from(image.width) * scale
    } else {
        f64::from(image.height) * scale
    };
    let length = if horizontal {
        bounds.width()
    } else {
        bounds.height()
    };
    let mut offset = 0.0;
    while offset < length {
        let (x, y) = if horizontal {
            (bounds.x0 + offset, bounds.y0)
        } else {
            (bounds.x0, bounds.y0 + offset)
        };
        scene.draw_image(
            &ImageBrush::new(image.clone()).with_quality(ImageQuality::Low),
            transform * Affine::translate((x, y)) * Affine::scale(scale),
        );
        offset += step;
    }
    scene.pop_layer();
}

fn acorn_logo() -> &'static ImageData {
    static IMAGE: OnceLock<ImageData> = OnceLock::new();
    IMAGE.get_or_init(|| {
        decode_branding_png(include_bytes!(
            "../resources/branding/acorn-glossy-v2-1024.png"
        ))
    })
}

fn display_manager_logo() -> &'static ImageData {
    static IMAGE: OnceLock<ImageData> = OnceLock::new();
    IMAGE.get_or_init(|| {
        decode_branding_png(include_bytes!(
            "../resources/branding/display-glossy-v1-1024.png"
        ))
    })
}

// Standard RISC OS sprite names carry the catalogue filetype. Unknown types
// retain the caller's supplied art rather than being mislabelled as Text.
fn modern_filer_art(sprite: &str) -> Option<&'static ImageData> {
    match sprite {
        "directory" => Some(directory_logo()),
        "file_fff" => Some(text_file_logo()),
        "file_ffb" => Some(basic_file_logo()),
        "file_064" => Some(basic64_file_logo()),
        _ => None,
    }
}

fn text_file_logo() -> &'static ImageData {
    static IMAGE: OnceLock<ImageData> = OnceLock::new();
    IMAGE.get_or_init(|| {
        decode_branding_png(include_bytes!(
            "../resources/branding/text-glossy-v1-1024.png"
        ))
    })
}

fn basic64_file_logo() -> &'static ImageData {
    static IMAGE: OnceLock<ImageData> = OnceLock::new();
    IMAGE.get_or_init(|| {
        decode_branding_png(include_bytes!(
            "../resources/branding/basic64-glossy-v1-1024.png"
        ))
    })
}

fn basic_file_logo() -> &'static ImageData {
    static IMAGE: OnceLock<ImageData> = OnceLock::new();
    IMAGE.get_or_init(|| {
        decode_branding_png(include_bytes!(
            "../resources/branding/basic-glossy-v1-1024.png"
        ))
    })
}

fn directory_logo() -> &'static ImageData {
    static IMAGE: OnceLock<ImageData> = OnceLock::new();
    IMAGE.get_or_init(|| {
        decode_branding_png(include_bytes!(
            "../resources/branding/directory-glossy-v1-1024.png"
        ))
    })
}

fn harddisc_logo() -> &'static ImageData {
    static IMAGE: OnceLock<ImageData> = OnceLock::new();
    IMAGE.get_or_init(|| {
        decode_branding_png(include_bytes!(
            "../resources/branding/harddisc-glossy-v2-1024.png"
        ))
    })
}

fn decode_branding_png(bytes: &[u8]) -> ImageData {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .expect("bundled branding PNG has valid metadata");
    let mut rgba = vec![
        0;
        reader
            .output_buffer_size()
            .expect("branding size is bounded")
    ];
    let info = reader
        .next_frame(&mut rgba)
        .expect("bundled branding PNG has valid pixels");
    assert_eq!(info.color_type, png::ColorType::Rgba);
    assert_eq!(info.bit_depth, png::BitDepth::Eight);
    rgba.truncate(info.buffer_size());
    let (width, height, rgba) = trim_transparent_border(info.width, info.height, rgba);
    rgba_image_data(width, height, rgba).expect("bundled branding PNG is non-empty RGBA")
}

fn trim_transparent_border(width: u32, height: u32, rgba: Vec<u8>) -> (u32, u32, Vec<u8>) {
    let (Ok(width_usize), Ok(height_usize)) = (usize::try_from(width), usize::try_from(height))
    else {
        return (width, height, rgba);
    };
    if rgba.len() != width_usize.saturating_mul(height_usize).saturating_mul(4) {
        return (width, height, rgba);
    }
    let mut min_x = width_usize;
    let mut min_y = height_usize;
    let mut max_x = 0;
    let mut max_y = 0;
    for y in 0..height_usize {
        for x in 0..width_usize {
            // Ignore the nearly invisible ambient fringe around the supplied
            // logo canvases. This keeps the disk's broad 3:1 silhouette when
            // the icon is fitted into its icon-bar artwork slot.
            if rgba[(y * width_usize + x) * 4 + 3] > 8 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x + 1);
                max_y = max_y.max(y + 1);
            }
        }
    }
    if max_x <= min_x || max_y <= min_y {
        return (width, height, rgba);
    }
    let cropped_width = max_x - min_x;
    let cropped_height = max_y - min_y;
    if cropped_width == width_usize && cropped_height == height_usize {
        return (width, height, rgba);
    }
    let mut cropped = Vec::with_capacity(cropped_width * cropped_height * 4);
    for y in min_y..max_y {
        let start = (y * width_usize + min_x) * 4;
        let end = start + cropped_width * 4;
        cropped.extend_from_slice(&rgba[start..end]);
    }
    (cropped_width as u32, cropped_height as u32, cropped)
}

fn rgba(rgba: [u8; 4]) -> (u8, u8, u8, u8) {
    (rgba[0], rgba[1], rgba[2], rgba[3])
}

fn fill(scene: &mut Scene, transform: Affine, rgba: (u8, u8, u8, u8), bounds: Rect) {
    if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return;
    }
    scene.fill(Fill::NonZero, transform, color(rgba), None, &bounds);
}

fn stroke(
    scene: &mut Scene,
    transform: Affine,
    path: &BezPath,
    width: f64,
    rgba: (u8, u8, u8, u8),
) {
    scene.stroke(&Stroke::new(width), transform, color(rgba), None, path);
}

fn outline(scene: &mut Scene, transform: Affine, bounds: Rect, rgba: (u8, u8, u8, u8), width: f64) {
    fill(
        scene,
        transform,
        rgba,
        Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y0 + width),
    );
    fill(
        scene,
        transform,
        rgba,
        Rect::new(bounds.x0, bounds.y1 - width, bounds.x1, bounds.y1),
    );
    fill(
        scene,
        transform,
        rgba,
        Rect::new(
            bounds.x0,
            bounds.y0 + width,
            bounds.x0 + width,
            bounds.y1 - width,
        ),
    );
    fill(
        scene,
        transform,
        rgba,
        Rect::new(
            bounds.x1 - width,
            bounds.y0 + width,
            bounds.x1,
            bounds.y1 - width,
        ),
    );
}

fn screen_rect(x: i32, top: i32, width: i32, height: i32) -> Rect {
    Rect::new(
        f64::from(x),
        f64::from(top),
        f64::from(x + width),
        f64::from(top + height),
    )
}

fn layout(window: &DesktopWindow, area: WorkArea, scroll: (i32, i32)) -> WindowFurnitureLayout {
    WindowFurnitureLayout::new(
        area,
        window.work_extent,
        scroll.0,
        scroll.1,
        window.has_back_icon,
        window.has_title,
        window.closable,
        window.has_toggle_size_icon,
        window.has_vertical_scrollbar,
        window.resizable,
    )
}

fn mode_osu_per_pixel(logical_extent: i32, pixel_extent: u32) -> u32 {
    if logical_extent <= 0 || pixel_extent == 0 {
        1
    } else {
        (logical_extent / pixel_extent as i32).max(1) as u32
    }
}

fn color(rgba: (u8, u8, u8, u8)) -> Color {
    Color::from_rgba8(rgba.0, rgba.1, rgba.2, rgba.3)
}

fn window_work_color(title: &str) -> (u8, u8, u8, u8) {
    if title.starts_with("HostFS:") || title.to_ascii_lowercase().contains("filer") {
        (238, 238, 238, 255)
    } else if title.eq_ignore_ascii_case("Display Manager") {
        (224, 224, 225, 255)
    } else {
        (255, 255, 255, 255)
    }
}

fn palette(index: u8) -> (u8, u8, u8, u8) {
    let [r, g, b] = match index & 15 {
        0 => [255, 255, 255],
        1 => [221, 221, 221],
        2 => [187, 187, 187],
        3 => [153, 153, 153],
        4 => [119, 119, 119],
        5 => [85, 85, 85],
        6 => [51, 51, 51],
        7 => [0, 0, 0],
        8 => [0, 68, 153],
        9 => [238, 238, 0],
        10 => [0, 204, 0],
        11 => [221, 0, 0],
        12 => [238, 238, 187],
        13 => [85, 136, 0],
        14 => [255, 187, 0],
        _ => [0, 187, 255],
    };
    (r, g, b, 255)
}

fn palette_rgb(index: u8) -> (u8, u8, u8, u8) {
    palette(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_branding_assets_decode_once_as_rgba() {
        let basic64 = basic64_file_logo();
        assert!(basic64.width > 0 && basic64.height > 0);
        assert_eq!(basic64.alpha_type, ImageAlphaType::Alpha);
        let acorn = acorn_logo();
        let harddisc = harddisc_logo();
        let display = display_manager_logo();
        assert_eq!((acorn.width, acorn.height), (556, 872));
        assert_eq!((harddisc.width, harddisc.height), (954, 346));
        assert!(display.width > 0 && display.height > 0);
        assert!(display.width <= 1024 && display.height <= 1024);
        assert_eq!(acorn.alpha_type, ImageAlphaType::Alpha);
        assert_eq!(harddisc.alpha_type, ImageAlphaType::Alpha);
        assert_eq!(display.alpha_type, ImageAlphaType::Alpha);
        assert!(
            display
                .data
                .data()
                .chunks_exact(4)
                .any(|pixel| pixel[3] == 0)
        );
        assert!(
            display
                .data
                .data()
                .chunks_exact(4)
                .any(|pixel| pixel[3] == 255)
        );
    }

    #[test]
    fn viewport_keeps_letterbox_and_pointer_mappings_aligned() {
        let viewport = Viewport::new(2400, 1200);
        assert_eq!(viewport.scale, 1.0);
        assert_eq!(viewport.offset_x, 400.0);
        assert_eq!(viewport.desktop_point(400.0, 0.0), Some((0, 1199)));
        assert_eq!(viewport.desktop_point(399.0, 0.0), None);
        assert_eq!(viewport.desktop_point(1999.0, 1199.0), Some((1599, 0)));
    }

    #[test]
    fn viewport_maps_physical_cursor_positions_equally_at_one_and_two_times() {
        let one_x = Viewport::for_desktop(800, 600, 1600, 1200);
        let two_x = Viewport::for_desktop(1600, 1200, 1600, 1200);
        assert_eq!(one_x.desktop_point(100.0, 200.0), Some((200, 799)));
        assert_eq!(two_x.desktop_point(200.0, 400.0), Some((200, 799)));
    }

    #[test]
    fn viewport_tracks_window_metrics_and_letterboxes_fixed_extents() {
        let window_mode = Viewport::for_desktop(1920, 1440, 1920, 1440);
        assert_eq!(window_mode.scale, 1.0);
        assert_eq!(window_mode.desktop_os_size(), (1920, 1440));
        assert_eq!(window_mode.desktop_point(640.0, 480.0), Some((640, 959)));

        let fixed_mode = Viewport::for_desktop(2400, 1200, 1280, 960);
        assert_eq!(fixed_mode.scale, 1.25);
        assert_eq!(fixed_mode.offset_x, 400.0);
        assert_eq!(fixed_mode.desktop_os_size(), (1280, 960));
        assert_eq!(fixed_mode.desktop_point(400.0, 0.0), Some((0, 959)));
        assert_eq!(fixed_mode.desktop_point(399.0, 0.0), None);
        assert_eq!(fixed_mode.desktop_point(1999.0, 1199.0), Some((1279, 0)));
    }

    #[test]
    fn display_manager_work_area_uses_furniture_grey_without_changing_filer_or_guest_paper() {
        assert_eq!(window_work_color("Display Manager"), (224, 224, 225, 255));
        assert_eq!(window_work_color("HostFS: Filer"), (238, 238, 238, 255));
        assert_eq!(window_work_color("BASIC window"), (255, 255, 255, 255));
    }

    #[test]
    fn drag_mapping_clamps_to_desktop_edges_through_letterbox_space() {
        let viewport = Viewport::new(2400, 1200);
        assert_eq!(viewport.clamped_desktop_point(0.0, 0.0), (0, 1199));
        assert_eq!(viewport.clamped_desktop_point(2399.0, 1199.0), (1599, 0));
    }

    #[test]
    fn fallback_image_cache_reuses_and_invalidates_by_source_identity() {
        let source = test_icon_image(2, 2, [220, 80, 30, 255]);
        let mut cache = FallbackImageCache::default();

        let first = cache.guest_image(&source, false).unwrap();
        let repeated = cache.guest_image(&source, false).unwrap();
        assert_eq!(cache.conversions, 1);
        assert_eq!(first.data.id(), repeated.data.id());

        let replacement = test_icon_image(2, 2, [30, 180, 90, 255]);
        let replacement_data = cache.guest_image(&replacement, false).unwrap();
        assert_eq!(cache.conversions, 2);
        assert_ne!(first.data.id(), replacement_data.data.id());
        assert_eq!(replacement_data.data.data()[..4], [30, 180, 90, 255]);

        let selected = cache.guest_image(&source, true).unwrap();
        assert_eq!(cache.conversions, 3);
        assert_eq!(
            selected.data.data()[..4],
            renderer::selected_sprite_colour([220, 80, 30, 255])
        );
        assert_eq!(cache.entries.len(), 3);
    }

    #[test]
    fn fallback_image_cache_enforces_byte_and_entry_limits_with_eviction() {
        let mut byte_limited = FallbackImageCache::default();
        let large_images: Vec<_> = (0..17)
            .map(|index| test_icon_image(512, 512, [index as u8, 80, 30, 255]))
            .collect();
        for image in &large_images {
            byte_limited.guest_image(image, false).unwrap();
        }
        assert!(byte_limited.bytes <= FALLBACK_IMAGE_CACHE_MAX_BYTES);
        assert!(byte_limited.entries.len() < large_images.len());
        let conversions = byte_limited.conversions;
        byte_limited.guest_image(&large_images[0], false).unwrap();
        assert_eq!(byte_limited.conversions, conversions + 1);

        let mut entry_limited = FallbackImageCache::default();
        let small_images: Vec<_> = (0..=FALLBACK_IMAGE_CACHE_MAX_ENTRIES)
            .map(|index| test_icon_image(1, 1, [index as u8, 80, 30, 255]))
            .collect();
        for image in &small_images {
            entry_limited.guest_image(image, false).unwrap();
        }
        assert_eq!(
            entry_limited.entries.len(),
            FALLBACK_IMAGE_CACHE_MAX_ENTRIES
        );
        assert_eq!(entry_limited.order.len(), FALLBACK_IMAGE_CACHE_MAX_ENTRIES);
        let conversions = entry_limited.conversions;
        entry_limited.guest_image(&small_images[0], false).unwrap();
        assert_eq!(entry_limited.conversions, conversions + 1);
    }

    #[test]
    fn text_layout_cache_reuses_shaping_for_width_limited_paint() {
        let mut builder = DesktopSceneBuilder::new();
        let label = "A long Filer entry that wraps over multiple lines";
        let measured_width = builder.text_width(label, 22.0, 600.0);
        assert!(measured_width > 140.0);
        assert_eq!(builder.text_layout_builds, 1);

        let (wrapped_width, wrapped_height) =
            builder.text_layout_size(label, 22.0, 600.0, Some(140.0));
        assert!(wrapped_width <= 140.0);
        assert!(wrapped_height > 22.0);
        assert_eq!(builder.text_layout_builds, 1);

        let mut scene = Scene::new();
        builder.text_with_width(
            &mut scene,
            label,
            0.0,
            0.0,
            22.0,
            (255, 255, 255, 255),
            600.0,
            Affine::IDENTITY,
            Some(140.0),
        );
        let cached_entries = builder.text_layouts.len();
        builder.text_with_width(
            &mut scene,
            label,
            0.0,
            0.0,
            22.0,
            (0, 0, 0, 255),
            600.0,
            Affine::IDENTITY,
            Some(140.0),
        );
        assert_eq!(builder.text_layout_builds, 1);
        assert_eq!(builder.text_layouts.len(), cached_entries);
        assert_eq!(cached_entries, 2);
    }

    #[test]
    fn text_layout_cache_evicts_old_entries() {
        let mut builder = DesktopSceneBuilder::new();
        let first = TextLayoutKey::new("label-0", 18.0, 400.0, None);
        for index in 0..=TEXT_LAYOUT_CACHE_MAX_ENTRIES {
            let key = TextLayoutKey::new(&format!("label-{index}"), 18.0, 400.0, None);
            builder.insert_text_layout(key, parley::Layout::default());
        }
        assert_eq!(builder.text_layouts.len(), TEXT_LAYOUT_CACHE_MAX_ENTRIES);
        assert_eq!(
            builder.text_layout_order.len(),
            TEXT_LAYOUT_CACHE_MAX_ENTRIES
        );
        assert!(!builder.text_layouts.contains_key(&first));
    }

    #[test]
    #[ignore = "requires a GPU; run with ACORN_VELLO_SNAPSHOT=1 and --ignored"]
    fn gpu_modern_selection_changes_only_the_label_and_legacy_still_tints_art() {
        assert!(
            std::env::var_os("ACORN_VELLO_SNAPSHOT").is_some(),
            "set ACORN_VELLO_SNAPSHOT=1 to opt into real GPU rendering"
        );

        let source_image = test_icon_image(64, 64, [220, 80, 30, 255]);
        let mut modern_builder = DesktopSceneBuilder::new();
        let modern_before =
            selection_test_scene(&mut modern_builder, "HostFS: Filer", false, &source_image);
        let before = crate::vello_backend::snapshot_scene(
            &modern_before,
            DEFAULT_DESKTOP_OS_SIZE.0 as u32,
            DEFAULT_DESKTOP_OS_SIZE.1 as u32,
        )
        .expect("GPU renders the unselected Filer icon");
        assert_eq!(modern_builder.fallback_images.conversions, 1);

        let modern_after =
            selection_test_scene(&mut modern_builder, "HostFS: Filer", true, &source_image);
        let after = crate::vello_backend::snapshot_scene(
            &modern_after,
            DEFAULT_DESKTOP_OS_SIZE.0 as u32,
            DEFAULT_DESKTOP_OS_SIZE.1 as u32,
        )
        .expect("GPU renders the selected Filer icon");
        assert_eq!(modern_builder.fallback_images.conversions, 1);
        assert_region_equal(&before, &after, 1600, (225, 857, 275, 907));
        assert!(region_difference_count(&before, &after, 1600, (100, 1058, 400, 1100)) > 0);

        let mut legacy_builder = DesktopSceneBuilder::new();
        let legacy_before_scene =
            selection_test_scene(&mut legacy_builder, "Viewer", false, &source_image);
        let legacy_before = crate::vello_backend::snapshot_scene(
            &legacy_before_scene,
            DEFAULT_DESKTOP_OS_SIZE.0 as u32,
            DEFAULT_DESKTOP_OS_SIZE.1 as u32,
        )
        .expect("GPU renders the unselected compatibility icon");
        assert_eq!(legacy_builder.fallback_images.conversions, 1);

        let legacy_after_scene =
            selection_test_scene(&mut legacy_builder, "Viewer", true, &source_image);
        let legacy_after = crate::vello_backend::snapshot_scene(
            &legacy_after_scene,
            DEFAULT_DESKTOP_OS_SIZE.0 as u32,
            DEFAULT_DESKTOP_OS_SIZE.1 as u32,
        )
        .expect("GPU renders the selected compatibility icon");
        assert_eq!(legacy_builder.fallback_images.conversions, 2);
        write_gpu_selection_artifacts(&before, &after, &legacy_before, &legacy_after)
            .expect("optional GPU selection PNGs are written");
        assert!(
            region_difference_count(&legacy_before, &legacy_after, 1600, (240, 872, 260, 892)) > 0
        );
    }

    fn test_icon_image(width: u32, height: u32, rgba: [u8; 4]) -> Arc<DesktopIconImage> {
        Arc::new(DesktopIconImage {
            width,
            height,
            rgba: vec![rgba; width as usize * height as usize],
        })
    }

    fn selection_test_scene(
        builder: &mut DesktopSceneBuilder,
        title: &str,
        selected: bool,
        image: &Arc<DesktopIconImage>,
    ) -> Scene {
        let window = DesktopWindow {
            handle: 1,
            owner_task_id: 7,
            is_console_output: false,
            title: title.to_owned(),
            work_area: WorkArea {
                min_x: 0,
                min_y: 0,
                max_x: 800,
                max_y: 800,
            },
            work_extent: WorkArea {
                min_x: 0,
                min_y: 0,
                max_x: 800,
                max_y: 800,
            },
            scroll_x: 0,
            scroll_y: 0,
            preview_area: None,
            preview_scroll: None,
            has_back_icon: false,
            has_title: true,
            has_vertical_scrollbar: false,
            has_toggle_size_icon: false,
            maximized: false,
            closable: true,
            movable: true,
            resizable: false,
            focused: true,
        };
        let icon = DesktopWindowIcon {
            handle: 4,
            owner_task_id: 7,
            window_handle: 1,
            label: "Launch Me".to_owned(),
            sprite_name: None,
            high_resolution_image: Some(Arc::clone(image)),
            bounds: DesktopRect {
                min_x: 100,
                min_y: 100,
                max_x: 400,
                max_y: 500,
            },
            flags: (1 << 3) | if selected { 1 << 21 } else { 0 },
        };
        builder.build(
            &[window],
            &HashMap::new(),
            &[],
            &[icon],
            &[],
            None,
            Viewport::new(
                DEFAULT_DESKTOP_OS_SIZE.0 as u32,
                DEFAULT_DESKTOP_OS_SIZE.1 as u32,
            ),
        )
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let offset = ((y * width + x) * 4) as usize;
        rgba[offset..offset + 4].try_into().unwrap()
    }

    fn assert_region_equal(left: &[u8], right: &[u8], width: u32, bounds: (u32, u32, u32, u32)) {
        for y in bounds.1..bounds.3 {
            for x in bounds.0..bounds.2 {
                assert_eq!(
                    pixel(left, width, x, y),
                    pixel(right, width, x, y),
                    "pixel ({x}, {y})"
                );
            }
        }
    }

    fn region_difference_count(
        left: &[u8],
        right: &[u8],
        width: u32,
        bounds: (u32, u32, u32, u32),
    ) -> usize {
        (bounds.1..bounds.3)
            .flat_map(|y| (bounds.0..bounds.2).map(move |x| (x, y)))
            .filter(|(x, y)| pixel(left, width, *x, *y) != pixel(right, width, *x, *y))
            .count()
    }

    fn write_gpu_selection_artifacts(
        modern_before: &[u8],
        modern_after: &[u8],
        legacy_before: &[u8],
        legacy_after: &[u8],
    ) -> std::io::Result<()> {
        let Some(directory) = std::env::var_os("ACORN_VELLO_SELECTION_ARTIFACT_DIR") else {
            return Ok(());
        };
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory)?;
        for (name, rgba) in [
            ("modern-unselected.png", modern_before),
            ("modern-selected.png", modern_after),
            ("legacy-unselected.png", legacy_before),
            ("legacy-selected.png", legacy_after),
        ] {
            let file = std::fs::File::create(directory.join(name))?;
            let mut encoder = png::Encoder::new(
                file,
                DEFAULT_DESKTOP_OS_SIZE.0 as u32,
                DEFAULT_DESKTOP_OS_SIZE.1 as u32,
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
            writer
                .write_image_data(rgba)
                .map_err(std::io::Error::other)?;
        }
        Ok(())
    }
}
