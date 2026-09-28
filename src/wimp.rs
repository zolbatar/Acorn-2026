//! Shared hosted Wimp service for the first two-task desktop slice.
//!
//! The SWI numbers and parameter blocks implemented here follow the RISC OS
//! Programmer's Reference Manual. The hosted service deliberately implements
//! only window registration, opening/closing, polling, basic mouse/key events,
//! and desktop stacking. It does not claim the complete Wimp API.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex, OnceLock, mpsc::Sender},
    time::Instant,
};

use crate::{
    configure::ConfigureStore,
    error::RuntimeError,
    memory::Task,
    riscos_resources::{RiscOsSpriteFile, SpriteSet, builtin_sprite_set},
    swi::SwiContext,
};

pub const WIMP_INITIALISE: u32 = 0x400C0;
pub const WIMP_CREATE_WINDOW: u32 = 0x400C1;
pub const WIMP_REDRAW_WINDOW: u32 = 0x400C8;
pub const WIMP_UPDATE_WINDOW: u32 = 0x400C9;
pub const WIMP_GET_RECTANGLE: u32 = 0x400CA;
pub const WIMP_CREATE_ICON: u32 = 0x400C2;
pub const WIMP_DELETE_ICON: u32 = 0x400C4;
pub const WIMP_OPEN_WINDOW: u32 = 0x400C5;
pub const WIMP_CLOSE_WINDOW: u32 = 0x400C6;
pub const WIMP_POLL: u32 = 0x400C7;
pub const WIMP_GET_WINDOW_STATE: u32 = 0x400CB;
pub const WIMP_SET_ICON_STATE: u32 = 0x400CD;
pub const WIMP_GET_POINTER_INFO: u32 = 0x400CF;
pub const WIMP_CREATE_MENU: u32 = 0x400D4;
pub const WIMP_SET_EXTENT: u32 = 0x400D7;
pub const WIMP_FORCE_REDRAW: u32 = 0x400D1;
pub const WIMP_CLOSE_DOWN: u32 = 0x400DD;
pub const WIMP_START_TASK: u32 = 0x400DE;
/// Acorn-2026 extension SWI: Wimp_CreateIcon with an optional 2× RGBA asset.
pub const WIMP_CREATE_ICON_EX: u32 = 0x4FF02;

const TASK_MAGIC: u32 = 0x4B53_4154;
const WIMP_VERSION: u32 = 310;
const WINDOW_BLOCK_SIZE: usize = 88;
const OPEN_BLOCK_SIZE: usize = 32;
const WINDOW_STATE_BLOCK_SIZE: usize = 36;
const POLL_BLOCK_SIZE: usize = 256;
const POINTER_INFO_BLOCK_SIZE: usize = 20;
const MENU_HEADER_SIZE: usize = 28;
const MENU_ITEM_SIZE: usize = 24;
const MAX_MENU_ITEMS: usize = 64;
const MAX_MENU_DEPTH: usize = 8;
const MAX_MENU_TREE_ITEMS: usize = 1024;
pub(crate) const MENU_SEPARATOR_HEIGHT: i32 = 12;
const MENU_TITLE_HEIGHT: i32 = 32;
const MENU_BORDER: i32 = 2;
const MENU_GUTTER_WIDTH: i32 = 24;
const DEFAULT_MENU_HOVER_DELAY: std::time::Duration = std::time::Duration::from_millis(120);
const MAX_EVENT_QUEUE: usize = 256;
/// Hosted desktop framebuffer samples. The guest-visible desktop remains
/// 800×600 logical pixels; this 2× surface supplies crisp modern shell art.
pub const DESKTOP_PIXEL_WIDTH: u32 = 1600;
pub const DESKTOP_PIXEL_HEIGHT: u32 = 1200;
/// Guest Wimp coordinates remain in OS units. Each framebuffer sample spans
/// one OS unit so that the logical desktop keeps the same size at 2× density.
pub const DESKTOP_OS_UNITS_PER_PIXEL_X: i32 = 1;
pub const DESKTOP_OS_UNITS_PER_PIXEL_Y: i32 = 1;
pub const DESKTOP_WIDTH: i32 = DESKTOP_PIXEL_WIDTH as i32 * DESKTOP_OS_UNITS_PER_PIXEL_X;
pub const DESKTOP_HEIGHT: i32 = DESKTOP_PIXEL_HEIGHT as i32 * DESKTOP_OS_UNITS_PER_PIXEL_Y;
pub const SYSTEM_FONT_WIDTH: i32 = 16;
pub const SYSTEM_FONT_HEIGHT: i32 = 32;
pub(crate) const FRAME_BORDER: i32 = 2;
const TITLE_HEIGHT: i32 = 32;
const VERTICAL_SCROLLBAR_WIDTH: i32 = TITLE_HEIGHT;
const SCROLL_ARROW_SIZE: i32 = TITLE_HEIGHT;
const MIN_SLIDER_SIZE: i32 = 28;
const SIZE_ICON_HEIGHT: i32 = TITLE_HEIGHT;
const MAX_DESKTOP_COORDINATE: i32 = 16_384;
const SCROLL_ARROW_STEP: i32 = 32;
/// The hosted icon bar occupies the bottom 50 logical pixels (100 OS units).
pub const DESKTOP_ICONBAR_HEIGHT: i32 = 100;
pub(crate) const ICONBAR_SYSTEM_AREA_OS: i32 = 72;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopRect {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

impl DesktopRect {
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.min_x && x < self.max_x && y >= self.min_y && y < self.max_y
    }

    pub fn is_empty(self) -> bool {
        self.max_x <= self.min_x || self.max_y <= self.min_y
    }
}

impl WindowFurnitureLayout {
    pub fn new(
        work_area: WorkArea,
        work_extent: WorkArea,
        _scroll_x: i32,
        scroll_y: i32,
        has_back_icon: bool,
        has_title: bool,
        closable: bool,
        has_toggle_size_icon: bool,
        has_vertical_scrollbar: bool,
        resizable: bool,
    ) -> Self {
        let work = DesktopRect {
            min_x: work_area.min_x,
            min_y: work_area.min_y,
            max_x: work_area.max_x,
            max_y: work_area.max_y,
        };
        let outer = DesktopRect {
            min_x: work.min_x - FRAME_BORDER,
            min_y: work.min_y
                - if resizable && !has_vertical_scrollbar {
                    SIZE_ICON_HEIGHT
                } else {
                    0
                }
                - FRAME_BORDER,
            max_x: work.max_x
                + if has_vertical_scrollbar {
                    VERTICAL_SCROLLBAR_WIDTH
                } else {
                    0
                }
                + FRAME_BORDER,
            max_y: work.max_y
                + if has_title || has_toggle_size_icon {
                    TITLE_HEIGHT
                } else {
                    0
                }
                + FRAME_BORDER,
        };

        let mut left = work.min_x;
        let header_bottom = work.max_y;
        let header_top = header_bottom + TITLE_HEIGHT;
        let back_icon = if has_title && has_back_icon {
            let icon = DesktopRect {
                min_x: left,
                min_y: header_bottom,
                max_x: left + TITLE_HEIGHT,
                max_y: header_top,
            };
            left += TITLE_HEIGHT;
            Some(icon)
        } else {
            None
        };
        let close_icon = if has_title && closable {
            let icon = DesktopRect {
                min_x: left,
                min_y: header_bottom,
                max_x: left + TITLE_HEIGHT,
                max_y: header_top,
            };
            left += TITLE_HEIGHT;
            Some(icon)
        } else {
            None
        };
        let right = outer.max_x - FRAME_BORDER;
        let toggle_size_icon = if has_toggle_size_icon {
            Some(DesktopRect {
                min_x: right - TITLE_HEIGHT,
                min_y: header_bottom,
                max_x: right,
                max_y: header_top,
            })
        } else {
            None
        };
        let title_bar = if has_title {
            let title_right = toggle_size_icon.map_or(right, |icon| icon.min_x);
            Some(DesktopRect {
                min_x: left,
                min_y: header_bottom,
                max_x: title_right.max(left),
                max_y: header_top,
            })
        } else {
            None
        };

        let vertical_scrollbar = has_vertical_scrollbar.then(|| {
            let bounds = DesktopRect {
                min_x: work.max_x,
                min_y: if resizable {
                    (work.min_y + SIZE_ICON_HEIGHT).min(work.max_y)
                } else {
                    work.min_y
                },
                max_x: work.max_x + VERTICAL_SCROLLBAR_WIDTH,
                max_y: work.max_y,
            };
            let up_arrow = DesktopRect {
                min_x: bounds.min_x,
                min_y: (bounds.max_y - SCROLL_ARROW_SIZE).max(bounds.min_y),
                max_x: bounds.max_x,
                max_y: bounds.max_y,
            };
            let down_arrow = DesktopRect {
                min_x: bounds.min_x,
                min_y: bounds.min_y,
                max_x: bounds.max_x,
                max_y: (bounds.min_y + SCROLL_ARROW_SIZE).min(bounds.max_y),
            };
            let track = DesktopRect {
                min_x: bounds.min_x,
                min_y: down_arrow.max_y.min(up_arrow.min_y),
                max_x: bounds.max_x,
                max_y: up_arrow.min_y.max(down_arrow.max_y),
            };
            let track_height = (track.max_y - track.min_y).max(0);
            let visible_height = (work.max_y - work.min_y).max(0);
            let extent_height = (work_extent.max_y - work_extent.min_y).max(visible_height);
            let slider_height = if extent_height == 0 {
                track_height
            } else {
                (track_height.saturating_mul(visible_height) / extent_height)
                    .clamp(track_height.min(MIN_SLIDER_SIZE), track_height)
            };
            let scroll_range = (extent_height - visible_height).max(0);
            let top_scroll = work_extent.max_y - scroll_y;
            let scroll_position = top_scroll.clamp(0, scroll_range);
            let slider_travel = (track_height - slider_height).max(0);
            let slider_top = track.max_y
                - if scroll_range == 0 {
                    0
                } else {
                    (i64::from(slider_travel) * i64::from(scroll_position)
                        / i64::from(scroll_range)) as i32
                };
            let slider = DesktopRect {
                min_x: bounds.min_x,
                min_y: slider_top - slider_height,
                max_x: bounds.max_x,
                max_y: slider_top,
            };
            VerticalScrollbarLayout {
                bounds,
                up_arrow,
                down_arrow,
                track,
                slider,
            }
        });
        let adjust_size_icon = resizable.then(|| DesktopRect {
            min_x: outer.max_x - VERTICAL_SCROLLBAR_WIDTH - FRAME_BORDER,
            min_y: if has_vertical_scrollbar {
                work.min_y
            } else {
                work.min_y - SIZE_ICON_HEIGHT
            },
            max_x: outer.max_x - FRAME_BORDER,
            max_y: if has_vertical_scrollbar {
                work.min_y + SIZE_ICON_HEIGHT
            } else {
                work.min_y
            },
        });
        Self {
            outer,
            work_area: work,
            back_icon,
            close_icon,
            title_bar,
            toggle_size_icon,
            vertical_scrollbar,
            adjust_size_icon,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerticalScrollbarLayout {
    pub bounds: DesktopRect,
    pub up_arrow: DesktopRect,
    pub down_arrow: DesktopRect,
    pub track: DesktopRect,
    pub slider: DesktopRect,
}

/// One source of truth for Wimp furniture drawing and pointer hit testing.
/// Rectangles use inclusive-minimum/exclusive-maximum RISC OS screen OS units.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowFurnitureLayout {
    pub outer: DesktopRect,
    pub work_area: DesktopRect,
    pub back_icon: Option<DesktopRect>,
    pub close_icon: Option<DesktopRect>,
    pub title_bar: Option<DesktopRect>,
    pub toggle_size_icon: Option<DesktopRect>,
    pub vertical_scrollbar: Option<VerticalScrollbarLayout>,
    pub adjust_size_icon: Option<DesktopRect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkArea {
    pub min_x: i32,
    pub min_y: i32,
    pub max_x: i32,
    pub max_y: i32,
}

impl WorkArea {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.min_x && x < self.max_x && y >= self.min_y && y < self.max_y
    }

    fn is_empty(self) -> bool {
        self.max_x <= self.min_x || self.max_y <= self.min_y
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopWindow {
    pub handle: u32,
    pub owner_task_id: u64,
    pub is_console_output: bool,
    pub title: String,
    pub work_area: WorkArea,
    pub work_extent: WorkArea,
    pub scroll_x: i32,
    pub scroll_y: i32,
    /// A move/resize preview remains an outline until the owner accepts its
    /// Open_Window_Request with Wimp_OpenWindow.
    pub preview_area: Option<WorkArea>,
    pub preview_scroll: Option<(i32, i32)>,
    pub has_back_icon: bool,
    pub has_title: bool,
    pub has_vertical_scrollbar: bool,
    pub has_toggle_size_icon: bool,
    pub maximized: bool,
    pub closable: bool,
    pub movable: bool,
    pub resizable: bool,
    pub focused: bool,
}

/// A text icon currently shown on the hosted Wimp icon bar. Icon placement is
/// owned by the Wimp; BASIC clients provide the label and the parent side.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopIcon {
    pub handle: u32,
    pub owner_task_id: u64,
    pub owner_task_handle: u32,
    pub label: String,
    pub sprite_name: Option<String>,
    pub high_resolution_image: Option<Arc<DesktopIconImage>>,
    pub bounds: DesktopRect,
    pub side: IconBarSide,
    pub button_type: u32,
    pub activate_task_id: Option<u64>,
}

/// A guest-defined icon in an open Wimp window, in desktop screen coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopWindowIcon {
    pub handle: u32,
    pub owner_task_id: u64,
    pub window_handle: u32,
    pub label: String,
    pub sprite_name: Option<String>,
    pub high_resolution_image: Option<Arc<DesktopIconImage>>,
    pub bounds: DesktopRect,
    pub flags: u32,
}

/// Caller-owned 2× RGBA artwork copied into the hosted icon record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopIconImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<[u8; 4]>,
}

/// One currently visible Wimp menu panel, in desktop screen OS coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopMenu {
    pub title: String,
    pub bounds: DesktopRect,
    pub title_bounds: Option<DesktopRect>,
    pub rows: Vec<DesktopMenuItem>,
    pub title_foreground: u8,
    pub title_background: u8,
    pub work_foreground: u8,
    pub work_background: u8,
    pub reverse: bool,
}

/// A menu row as presented by the shared Wimp renderer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopMenuItem {
    pub index: usize,
    pub label: String,
    /// Full row rectangle; selection art leaves the tick and arrow gutters clear.
    pub bounds: DesktopRect,
    pub tick: bool,
    pub separator_after: bool,
    pub has_submenu: bool,
    pub shaded: bool,
    pub selected: bool,
    pub icon_flags: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IconBarSide {
    Devices,
    Applications,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DesktopTaskKind {
    File,
    Commands,
    BasicWindow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopTaskRequest {
    pub kind: DesktopTaskKind,
    pub task_id: u64,
    pub guest_path: String,
    pub title: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowDragKind {
    Move,
    Resize,
    ScrollSlider,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowDrag {
    pub handle: u32,
    pub kind: WindowDragKind,
    pub start_x: i32,
    pub start_y: i32,
    pub original: WorkArea,
    pub original_scroll: (i32, i32),
    pub slider_grab_offset: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct QueuedEvent {
    reason: u32,
    block: [u8; POLL_BLOCK_SIZE],
    pointer_button_state: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
struct RedrawRectangle {
    work: WorkArea,
    screen: DesktopRect,
}

#[derive(Debug)]
struct RedrawLoop {
    window_handle: u32,
    rectangles: VecDeque<RedrawRectangle>,
    clears_background: bool,
    current_work: Option<WorkArea>,
}

#[derive(Clone, Debug)]
struct WimpMenuRow {
    label: String,
    flags: u32,
    icon_flags: u32,
    submenu: Option<Box<WimpMenu>>,
}

#[derive(Clone, Debug)]
struct WimpMenu {
    address: u32,
    title: String,
    title_foreground: u8,
    title_background: u8,
    work_foreground: u8,
    work_background: u8,
    width: i32,
    row_height: i32,
    gap: i32,
    rows: Vec<WimpMenuRow>,
    reverse: bool,
}

#[derive(Clone, Copy, Debug)]
struct MenuHoverCandidate {
    level: usize,
    row: usize,
    since: Instant,
}

#[derive(Clone, Debug)]
struct ActiveMenu {
    owner: u32,
    root: WimpMenu,
    root_x: i32,
    root_top: i32,
    /// Item index selected at each visible level. The final element owns the
    /// selected parent row for the deepest open submenu.
    open_path: Vec<usize>,
    selected_path: Vec<usize>,
    hover: Option<MenuHoverCandidate>,
    awaiting_adjust_reopen: bool,
    adjust_event_delivered: bool,
    reopened_after_adjust: bool,
    hover_delay: std::time::Duration,
    drag: Option<MenuDrag>,
}

#[derive(Clone, Copy, Debug)]
struct MenuDrag {
    start_x: i32,
    start_y: i32,
    root_x: i32,
    root_top: i32,
}

#[derive(Clone, Copy, Debug, Default)]
struct PointerState {
    x: i32,
    y: i32,
    buttons: u32,
}

#[derive(Debug, Default)]
struct WimpTask {
    events: VecDeque<QueuedEvent>,
    initialised: bool,
    started_by_wimp: bool,
    input: Option<Sender<u8>>,
    last_event_button_state: Option<u32>,
    redraw_event_pending: Option<u32>,
    redraw_loop: Option<RedrawLoop>,
}

#[derive(Clone, Debug)]
struct WimpIcon {
    handle: u32,
    owner_task_handle: u32,
    owner_task_id: u64,
    side: IconBarSide,
    label: String,
    sprite_name: Option<String>,
    high_resolution_image: Option<Arc<DesktopIconImage>>,
    width: i32,
    flags: u32,
    button_type: u32,
    activate_task_id: Option<u64>,
}

#[derive(Clone, Debug)]
struct WimpWindowIcon {
    handle: u32,
    window_handle: u32,
    owner_task_id: u64,
    bounds: DesktopRect,
    flags: u32,
    label: String,
    sprite_name: Option<String>,
    high_resolution_image: Option<Arc<DesktopIconImage>>,
}

#[derive(Clone, Copy, Debug)]
struct RecentClick {
    at: Instant,
    owner: u32,
    window: u32,
    icon: i32,
    x: i32,
    y: i32,
    buttons: u32,
}

#[derive(Clone, Debug)]
struct WimpWindow {
    handle: u32,
    owner_task_handle: u32,
    owner_task_id: u64,
    title: String,
    flags: u32,
    work_area_flags: u32,
    work_area_background: u8,
    work_area: WorkArea,
    work_extent: WorkArea,
    invalid_regions: Vec<WorkArea>,
    min_width: i32,
    min_height: i32,
    scroll_x: i32,
    scroll_y: i32,
    has_title: bool,
    has_back_icon: bool,
    has_vertical_scrollbar: bool,
    has_toggle_size_icon: bool,
    closable: bool,
    movable: bool,
    resizable: bool,
    open: bool,
    has_opened: bool,
    preview_area: Option<WorkArea>,
    preview_scroll: Option<(i32, i32)>,
    last_user_area: WorkArea,
    last_user_scroll: (i32, i32),
    maximized: bool,
    toggle_request_pending: bool,
    restore_behind: i32,
    console_window: bool,
}

#[derive(Debug, Default)]
struct WimpState {
    next_task_handle: u32,
    next_window_handle: u32,
    next_icon_handle: u32,
    next_guest_task_id: u64,
    pending_launches: VecDeque<DesktopTaskRequest>,
    guest_to_task: HashMap<u64, u32>,
    tasks: HashMap<u32, WimpTask>,
    windows: HashMap<u32, WimpWindow>,
    icons: Vec<WimpIcon>,
    window_icons: Vec<WimpWindowIcon>,
    /// Front to back, as in the Wimp's active-window list.
    stacking: Vec<u32>,
    keyboard_focus: Option<u32>,
    notice: Option<String>,
    last_click: Option<RecentClick>,
    pointer: PointerState,
    active_menu: Option<ActiveMenu>,
    system_menu_owner: Option<u32>,
    stopped: bool,
}

/// One Wimp namespace shared by every guest task in a hosted desktop.
pub struct WimpServer {
    state: Mutex<WimpState>,
    changed: Condvar,
    desktop_updates: Sender<()>,
    configure: Mutex<Option<ConfigureStore>>,
}

impl WimpServer {
    pub fn new(desktop_updates: Sender<()>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(WimpState {
                next_task_handle: 1,
                next_window_handle: 1,
                next_icon_handle: 1,
                next_guest_task_id: 10_000,
                ..WimpState::default()
            }),
            changed: Condvar::new(),
            desktop_updates,
            configure: Mutex::new(None),
        })
    }

    pub(crate) fn set_configure_store(&self, configure: ConfigureStore) {
        *self
            .configure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(configure);
    }

    pub(crate) fn configure_store(&self) -> Option<ConfigureStore> {
        self.configure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn dispatch(
        &self,
        swi: u32,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let redraw_state = {
            let state = self.lock_state()?;
            state
                .guest_to_task
                .get(&task.id)
                .and_then(|owner| state.tasks.get(owner))
                .map(|task| (task.redraw_event_pending, task.redraw_loop.is_some()))
        };
        if let Some((redraw_pending, redraw_active)) = redraw_state {
            if redraw_pending.is_some() && swi != WIMP_REDRAW_WINDOW {
                return Err(program_error(
                    "Wimp_RedrawWindow must be the first Wimp operation after a redraw event",
                ));
            }
            if redraw_active && !matches!(swi, WIMP_GET_RECTANGLE) {
                return Err(program_error(
                    "Wimp_GetRectangle must finish the current redraw/update loop before another Wimp operation",
                ));
            }
        }
        match swi {
            WIMP_INITIALISE => self.initialise(task, context),
            WIMP_CREATE_WINDOW => self.create_window(task, context),
            WIMP_REDRAW_WINDOW => self.redraw_window(task, context),
            WIMP_UPDATE_WINDOW => self.update_window(task, context),
            WIMP_GET_RECTANGLE => self.get_rectangle(task, context),
            WIMP_FORCE_REDRAW => self.force_redraw(task, context),
            WIMP_CREATE_ICON => self.create_icon(task, context),
            WIMP_CREATE_ICON_EX => self.create_icon_ex(task, context),
            WIMP_DELETE_ICON => self.delete_icon(task, context),
            WIMP_OPEN_WINDOW => self.open_window(task, context),
            WIMP_CLOSE_WINDOW => self.close_window(task, context),
            WIMP_POLL => self.poll(task, context),
            WIMP_GET_WINDOW_STATE => self.get_window_state(task, context),
            WIMP_SET_ICON_STATE => self.set_icon_state(task, context),
            WIMP_GET_POINTER_INFO => self.get_pointer_info(task, context),
            WIMP_CREATE_MENU => self.create_menu(task, context),
            WIMP_SET_EXTENT => self.set_extent(task, context),
            WIMP_CLOSE_DOWN => self.close_down(task, context),
            WIMP_START_TASK => self.start_task(task, context),
            _ => Err(RuntimeError::InvalidSwi(swi)),
        }
    }

    pub fn desktop_windows(&self) -> Vec<DesktopWindow> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        state
            .stacking
            .iter()
            .filter_map(|handle| state.windows.get(handle))
            .filter(|window| window.open)
            .map(|window| DesktopWindow {
                handle: window.handle,
                owner_task_id: window.owner_task_id,
                is_console_output: window.console_window,
                title: window.title.clone(),
                work_area: window.work_area,
                work_extent: window.work_extent,
                scroll_x: window.scroll_x,
                scroll_y: window.scroll_y,
                preview_area: window.preview_area,
                preview_scroll: window.preview_scroll,
                has_back_icon: window.has_back_icon,
                has_title: window.has_title,
                has_vertical_scrollbar: window.has_vertical_scrollbar,
                has_toggle_size_icon: window.has_toggle_size_icon,
                maximized: window.maximized,
                closable: window.closable,
                movable: window.movable,
                resizable: window.resizable,
                focused: state.keyboard_focus == Some(window.handle),
            })
            .collect()
    }

    pub fn desktop_icons(&self) -> Vec<DesktopIcon> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        layout_icon_bar(&state.icons, &state.windows)
    }

    pub fn desktop_window_icons(&self) -> Vec<DesktopWindowIcon> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        state
            .window_icons
            .iter()
            .filter(|icon| icon.flags & (1 << 23) == 0)
            .filter_map(|icon| {
                let window = state.windows.get(&icon.window_handle)?;
                if !window.open {
                    return None;
                }
                Some(DesktopWindowIcon {
                    handle: icon.handle,
                    owner_task_id: icon.owner_task_id,
                    window_handle: icon.window_handle,
                    label: icon.label.clone(),
                    sprite_name: icon.sprite_name.clone(),
                    high_resolution_image: icon.high_resolution_image.clone(),
                    bounds: icon_screen_bounds(icon, window),
                    flags: icon.flags,
                })
            })
            .collect()
    }

    /// Return visible menu panels in root-to-leaf paint order.
    pub fn desktop_menus(&self) -> Vec<DesktopMenu> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        state
            .active_menu
            .as_ref()
            .map(active_menu_panels)
            .unwrap_or_default()
    }

    /// Route pointer motion to the active Wimp menu. Returns true when the
    /// menu's highlight, cascade, or position changed and needs repainting.
    pub fn mouse_move(&self, x: i32, y: i32) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        state.pointer.x = x;
        state.pointer.y = y;
        let pointer_buttons = state.pointer.buttons;
        let Some(active) = state.active_menu.as_mut() else {
            return false;
        };
        let changed = if let Some(drag) = active.drag
            && pointer_buttons & 4 != 0
        {
            let dx = x - drag.start_x;
            let dy = y - drag.start_y;
            active.root_x = clamp_menu_x(&active.root, drag.root_x + dx);
            active.root_top = clamp_menu_top(&active.root, drag.root_top + dy);
            active.hover = None;
            true
        } else {
            let before_selected = active.selected_path.clone();
            let before_open = active.open_path.clone();
            let before_hover = active
                .hover
                .map(|candidate| (candidate.level, candidate.row));
            update_menu_hover(active, x, y, Instant::now());
            before_selected != active.selected_path
                || before_open != active.open_path
                || before_hover
                    != active
                        .hover
                        .map(|candidate| (candidate.level, candidate.row))
        };
        drop(state);
        if changed {
            let _ = self.desktop_updates.send(());
        }
        changed
    }

    /// Deadline for the currently armed body-row submenu hover.
    pub fn next_menu_hover_deadline(&self) -> Option<Instant> {
        let state = self.state.lock().ok()?;
        let active = state.active_menu.as_ref()?;
        Some(active.hover?.since + active.hover_delay)
    }

    /// Advance a delayed submenu hover against an explicit clock, allowing
    /// deterministic desktop interaction tests.
    pub fn advance_menu_hover_at(&self, now: Instant) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let (pointer_x, pointer_y) = (state.pointer.x, state.pointer.y);
        let Some(active) = state.active_menu.as_mut() else {
            return false;
        };
        let Some(candidate) = active.hover else {
            return false;
        };
        if now.saturating_duration_since(candidate.since) < active.hover_delay
            || !menu_candidate_is_still_hovered(active, candidate, pointer_x, pointer_y)
        {
            return false;
        }
        if let Some(row) = menu_row_at_level(active, candidate.level, candidate.row)
            && row.submenu.is_some()
            && (row.icon_flags & (1 << 22) == 0 || row.flags & (1 << 4) != 0)
        {
            active.open_path.truncate(candidate.level);
            active.open_path.push(candidate.row);
            set_path_value(&mut active.selected_path, candidate.level, candidate.row);
            active.hover = None;
            drop(state);
            self.changed.notify_all();
            let _ = self.desktop_updates.send(());
            return true;
        }
        false
    }

    /// Close the current menu, typically for Escape from the host keyboard.
    pub fn dismiss_menu(&self) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let was_open = state.active_menu.take().is_some();
        if was_open {
            drop(state);
            self.changed.notify_all();
            let _ = self.desktop_updates.send(());
        }
        was_open
    }

    /// Update the live button state when the host releases a mouse button.
    pub fn mouse_button_up(&self, buttons: u32) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.pointer.buttons &= !buttons;
        if state.pointer.buttons & 4 == 0
            && let Some(active) = state.active_menu.as_mut()
        {
            active.drag = None;
        }
    }

    pub fn active_guest_task_ids(&self) -> Vec<u64> {
        let Ok(state) = self.state.lock() else {
            return Vec::new();
        };
        state.guest_to_task.keys().copied().collect()
    }

    pub fn is_guest_task_active(&self, task_id: u64) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| state.guest_to_task.contains_key(&task_id))
    }

    pub fn desktop_notice(&self) -> Option<String> {
        self.state.lock().ok()?.notice.clone()
    }

    pub fn post_notice(&self, text: impl Into<String>) {
        if let Ok(mut state) = self.state.lock() {
            state.notice = Some(text.into());
        }
        let _ = self.desktop_updates.send(());
    }

    pub fn set_task_input(&self, guest_task_id: u64, input: Sender<u8>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let handle = state.guest_to_task.get(&guest_task_id).copied();
        if let Some(handle) = handle
            && let Some(task) = state.tasks.get_mut(&handle)
        {
            task.input = Some(input);
        }
    }

    /// Register the BASIC desktop policy as recipient of OS-icon Menu clicks.
    pub(crate) fn register_system_menu(&self, task_id: u64) -> Result<(), RuntimeError> {
        let mut state = self.lock_state()?;
        let owner = *state
            .guest_to_task
            .get(&task_id)
            .ok_or_else(|| program_error("system menu requires a Wimp task"))?;
        state.system_menu_owner = Some(owner);
        Ok(())
    }

    pub fn take_pending_launches(&self) -> Vec<DesktopTaskRequest> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        state.pending_launches.drain(..).collect()
    }

    /// Bootstrap a desktop component through the same isolated guest-task
    /// path used by Wimp_StartTask, without exposing it as a user application.
    pub fn start_system_task(&self, guest_path: &str, title: &str) -> Result<u64, RuntimeError> {
        let mut state = self.lock_state()?;
        let task_id = state.next_guest_task_id;
        state.next_guest_task_id = state
            .next_guest_task_id
            .checked_add(1)
            .ok_or_else(|| program_error("hosted task id space is exhausted"))?;
        let task_handle = allocate_handle(&mut state.next_task_handle)?;
        state.guest_to_task.insert(task_id, task_handle);
        state.tasks.insert(
            task_handle,
            WimpTask {
                events: VecDeque::new(),
                initialised: false,
                input: None,
                last_event_button_state: None,
                ..WimpTask::default()
            },
        );
        state.pending_launches.push_back(DesktopTaskRequest {
            kind: DesktopTaskKind::File,
            task_id,
            guest_path: guest_path.to_string(),
            title: title.to_string(),
        });
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(task_id)
    }

    /// Register a guest BASIC task as a running application. Called by the
    /// frontend at task start and removed by `task_exited` on every exit path.
    pub fn task_started(&self, guest_task_id: u64, label: &str) -> Result<(), RuntimeError> {
        let mut state = self.lock_state()?;
        if state.guest_to_task.contains_key(&guest_task_id) {
            return Err(program_error("guest task is already registered with Wimp"));
        }
        let task_handle = allocate_handle(&mut state.next_task_handle)?;
        state.guest_to_task.insert(guest_task_id, task_handle);
        state.tasks.insert(
            task_handle,
            WimpTask {
                events: VecDeque::new(),
                initialised: false,
                input: None,
                last_event_button_state: None,
                ..WimpTask::default()
            },
        );
        insert_console_window(&mut state, task_handle, guest_task_id, label)?;
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    /// Host-owned console surfaces follow their guest MODE. Ordinary Wimp
    /// applications keep control of their own extents and redraw protocol.
    pub(crate) fn sync_console_mode(
        &self,
        task_id: u64,
        handle: Option<u32>,
        mode: crate::graphics::ScreenMode,
    ) {
        let Ok(mut state) = self.lock_state() else { return };
        let mut changed = false;
        for window in state.windows.values_mut().filter(|window| {
            window.console_window && window.owner_task_id == task_id
                && handle.is_none_or(|handle| handle == window.handle)
        }) {
            // Match the renderer's integer guest-pixel scale, including modes
            // whose logical dimensions are not exact multiples of pixel size.
            let width = mode.pixel_width as i32
                * (mode.logical_width / mode.pixel_width.max(1) as i32).max(1);
            let height = mode.pixel_height as i32
                * (mode.logical_height / mode.pixel_height.max(1) as i32).max(1);
            let extent = WorkArea { min_x: 0, min_y: -height, max_x: width, max_y: 0 };
            if window.work_extent == extent { continue; }
            window.work_extent = extent;
            window.work_area.max_x = (window.work_area.min_x + width)
                .min(DESKTOP_WIDTH - VERTICAL_SCROLLBAR_WIDTH - FRAME_BORDER);
            window.work_area.min_y = (window.work_area.max_y - height)
                .max(DESKTOP_ICONBAR_HEIGHT + FRAME_BORDER);
            window.scroll_x = 0;
            window.scroll_y = 0;
            window.last_user_area = window.work_area;
            window.last_user_scroll = (0, 0);
            window.preview_area = None;
            window.preview_scroll = None;
            window.maximized = false;
            changed = true;
        }
        drop(state);
        if changed { let _ = self.desktop_updates.send(()); }
    }

    pub fn furniture_layout(&self) -> Vec<(DesktopWindow, WindowFurnitureLayout)> {
        self.desktop_windows()
            .into_iter()
            .map(|window| {
                let area = window.preview_area.unwrap_or(window.work_area);
                let (scroll_x, scroll_y) = window
                    .preview_scroll
                    .unwrap_or((window.scroll_x, window.scroll_y));
                let layout = WindowFurnitureLayout::new(
                    area,
                    window.work_extent,
                    scroll_x,
                    scroll_y,
                    window.has_back_icon,
                    window.has_title,
                    window.closable,
                    window.has_toggle_size_icon,
                    window.has_vertical_scrollbar,
                    window.resizable,
                );
                (window, layout)
            })
            .collect()
    }

    /// Route a desktop click. `x` and `y` use Wimp screen OS units with a
    /// bottom-left origin; host display pixels are converted at the boundary.
    pub fn mouse_down(&self, x: i32, y: i32, buttons: u32) -> Option<WindowDrag> {
        let mut state = self.state.lock().ok()?;
        state.notice = None;
        state.pointer.x = x;
        state.pointer.y = y;
        state.pointer.buttons |= buttons;
        if state.active_menu.is_some() {
            if active_menu_contains(&state.active_menu, x, y) {
                menu_mouse_down(&mut state, x, y, buttons);
                drop(state);
                self.changed.notify_all();
                let _ = self.desktop_updates.send(());
                return None;
            }
            // RISC OS removes the menu tree and passes an outside click
            // through as though the menu had not intercepted it.
            state.active_menu = None;
        }
        if (0..DESKTOP_ICONBAR_HEIGHT).contains(&y) {
            if x >= DESKTOP_WIDTH - ICONBAR_SYSTEM_AREA_OS && buttons & 2 != 0 {
                if let Some(owner) = state.system_menu_owner {
                    enqueue_for_owner(
                        &mut state,
                        owner,
                        mouse_event_with_icon(x, y, 2, u32::MAX, -1),
                    );
                }
                drop(state);
                self.changed.notify_all();
                let _ = self.desktop_updates.send(());
                return None;
            }

            let icons = layout_icon_bar(&state.icons, &state.windows);
            if let Some(icon) = icons
                .iter()
                .find(|icon| icon.bounds.contains(x, y))
                .cloned()
                && buttons & (1 | 2 | 4) != 0
            {
                let iconbar_parent = match icon.side {
                    IconBarSide::Devices => -2,
                    IconBarSide::Applications => -1,
                };
                if let Some(task_id) = icon.activate_task_id {
                    let handles = state
                        .windows
                        .values()
                        .filter(|window| window.owner_task_id == task_id && window.open)
                        .map(|window| window.handle)
                        .collect::<Vec<_>>();
                    for handle in handles.into_iter().rev() {
                        bring_to_front(&mut state, handle);
                    }
                    if let Some(handle) = state.stacking.iter().copied().find(|handle| {
                        state
                            .windows
                            .get(handle)
                            .is_some_and(|w| w.owner_task_id == task_id)
                    }) {
                        state.keyboard_focus = Some(handle);
                    }
                }
                let event = mouse_event_with_icon(
                    x,
                    y,
                    event_button_state(
                        &mut state,
                        icon.owner_task_handle,
                        iconbar_parent as u32,
                        icon.handle as i32,
                        x,
                        y,
                        buttons,
                        icon.button_type,
                    ),
                    iconbar_parent as u32,
                    icon.handle as i32,
                );
                let _ = enqueue_for_owner(&mut state, icon.owner_task_handle, event);
                drop(state);
                self.changed.notify_all();
                let _ = self.desktop_updates.send(());
                return None;
            }
            return None;
        }
        let handle = state.stacking.iter().copied().find(|handle| {
            state
                .windows
                .get(handle)
                .is_some_and(|window| window.open && point_in_window(window, x, y))
        });
        let Some(handle) = handle else {
            return None;
        };
        let window = state.windows.get(&handle)?.clone();

        let layout = window_furniture(&window);
        let hovered_icon = system_icon_at(layout, x, y);
        if buttons & 2 != 0 && hovered_icon.is_some_and(|icon| icon != -1) {
            // Wimp03 bypasses system furniture for Menu; negative system
            // handles are pointer-query results, never Mouse_Click icons.
            drop(state);
            return None;
        }
        state.keyboard_focus = Some(handle);
        if let Some(icon) = hovered_icon.filter(|icon| *icon != -1) {
            match icon {
                -2 if buttons & 4 != 0 || buttons & 1 != 0 => {
                    send_to_back(&mut state, handle);
                }
                -3 if buttons & (4 | 1) != 0 => {
                    let mut close_request = event_with_word(3, 0, window.handle);
                    close_request.pointer_button_state = Some(state.pointer.buttons);
                    let _ = enqueue_for_owner(&mut state, window.owner_task_handle, close_request);
                    close_window_in_state(&mut state, handle);
                }
                -5 if buttons & (4 | 1) != 0 => {
                    let select = buttons & 4 != 0;
                    toggle_window_size(&mut state, handle, select);
                }
                -6 | -8 => {
                    let direction = if icon == -6 { 1 } else { -1 };
                    request_or_apply_scroll(&mut state, handle, buttons, 0, direction);
                }
                -7 => {
                    if let Some(bar) = layout.vertical_scrollbar {
                        if bar.slider.contains(x, y) {
                            let drag = WindowDrag {
                                handle,
                                kind: WindowDragKind::ScrollSlider,
                                start_x: x,
                                start_y: y,
                                original: window.work_area,
                                original_scroll: (window.scroll_x, window.scroll_y),
                                // Wimp keeps the pointer's offset from the
                                // thumb's top edge while dragging. This makes
                                // a press/release without movement a no-op.
                                slider_grab_offset: y - bar.slider.max_y,
                            };
                            drop(state);
                            self.changed.notify_all();
                            let _ = self.desktop_updates.send(());
                            return Some(drag);
                        }
                        let direction = if y >= bar.slider.max_y { 2 } else { -2 };
                        request_or_apply_scroll(&mut state, handle, buttons, 0, direction);
                    }
                }
                -9 => {
                    let drag = WindowDrag {
                        handle,
                        kind: WindowDragKind::Resize,
                        start_x: x,
                        start_y: y,
                        original: window.work_area,
                        original_scroll: (window.scroll_x, window.scroll_y),
                        slider_grab_offset: 0,
                    };
                    drop(state);
                    self.changed.notify_all();
                    let _ = self.desktop_updates.send(());
                    return Some(drag);
                }
                -4 => {
                    if buttons & 4 != 0 && window.movable {
                        bring_to_front(&mut state, handle);
                        let drag = WindowDrag {
                            handle,
                            kind: WindowDragKind::Move,
                            start_x: x,
                            start_y: y,
                            original: window.work_area,
                            original_scroll: (window.scroll_x, window.scroll_y),
                            slider_grab_offset: 0,
                        };
                        drop(state);
                        self.changed.notify_all();
                        let _ = self.desktop_updates.send(());
                        return Some(drag);
                    } else if buttons & 1 != 0 && window.movable {
                        let drag = WindowDrag {
                            handle,
                            kind: WindowDragKind::Move,
                            start_x: x,
                            start_y: y,
                            original: window.work_area,
                            original_scroll: (window.scroll_x, window.scroll_y),
                            slider_grab_offset: 0,
                        };
                        drop(state);
                        self.changed.notify_all();
                        let _ = self.desktop_updates.send(());
                        return Some(drag);
                    }
                }
                _ => {}
            }
            drop(state);
            self.changed.notify_all();
            let _ = self.desktop_updates.send(());
            return None;
        }

        let window_icon = if layout.work_area.contains(x, y) {
            state
                .window_icons
                .iter()
                .rev()
                .find(|icon| {
                    icon.window_handle == window.handle
                        && icon.flags & ((1 << 22) | (1 << 23)) == 0
                        && clipped_icon_screen_bounds(icon, &window)
                            .is_some_and(|bounds| bounds.contains(x, y))
                })
                .map(|icon| (icon.handle as i32, (icon.flags >> 12) & 0xF))
        } else {
            None
        };
        if let Some((icon_handle, button_type)) = window_icon {
            if buttons & 2 != 0 || button_type != 0 {
                let owner_task_handle = window.owner_task_handle;
                let window_handle = window.handle;
                let event_buttons = event_button_state(
                    &mut state,
                    owner_task_handle,
                    window_handle,
                    icon_handle,
                    x,
                    y,
                    buttons,
                    button_type,
                );
                let _ = enqueue_for_owner(
                    &mut state,
                    owner_task_handle,
                    mouse_event_with_icon(x, y, event_buttons, window_handle, icon_handle),
                );
                drop(state);
                self.changed.notify_all();
                let _ = self.desktop_updates.send(());
                return None;
            }
        }

        if buttons & 2 != 0 {
            if hovered_icon == Some(-1) {
                let _ = enqueue_for_owner(
                    &mut state,
                    window.owner_task_handle,
                    mouse_event_with_icon(x, y, buttons, window.handle, -1),
                );
            }
            drop(state);
            self.changed.notify_all();
            return None;
        }

        let drag = if layout.title_bar.is_some_and(|rect| rect.contains(x, y))
            && window.movable
            && buttons & (4 | 1) != 0
        {
            if buttons & 4 != 0 {
                bring_to_front(&mut state, handle);
            }
            Some(WindowDrag {
                handle,
                kind: WindowDragKind::Move,
                start_x: x,
                start_y: y,
                original: window.work_area,
                original_scroll: (window.scroll_x, window.scroll_y),
                slider_grab_offset: 0,
            })
        } else if layout.work_area.contains(x, y) {
            let button_type = ((window.work_area_flags >> 12) & 0xF) as u32;
            if matches!(button_type, 3 | 10) {
                let event_buttons = event_button_state(
                    &mut state,
                    window.owner_task_handle,
                    window.handle,
                    -1,
                    x,
                    y,
                    buttons,
                    button_type,
                );
                let _ = enqueue_for_owner(
                    &mut state,
                    window.owner_task_handle,
                    mouse_event_with_icon(x, y, event_buttons, window.handle, -1),
                );
            }
            None
        } else {
            None
        };
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        drag
    }

    pub fn drag_to(&self, drag: WindowDrag, x: i32, y: i32) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(window) = state.windows.get_mut(&drag.handle) else {
            return;
        };
        let dx = x.saturating_sub(drag.start_x);
        let dy = y.saturating_sub(drag.start_y);
        let layout = WindowFurnitureLayout::new(
            drag.original,
            window.work_extent,
            drag.original_scroll.0,
            drag.original_scroll.1,
            window.has_back_icon,
            window.has_title,
            window.closable,
            window.has_toggle_size_icon,
            window.has_vertical_scrollbar,
            window.resizable,
        );
        let max_width = (window.work_extent.max_x - drag.original_scroll.0)
            .min(DESKTOP_WIDTH - drag.original.min_x - window_outer_extra_x(window))
            .max(1);
        let max_height = (drag.original_scroll.1 - window.work_extent.min_y)
            .min(drag.original.max_y - DESKTOP_ICONBAR_HEIGHT - window_outer_bottom_extra_y(window))
            .max(1);
        let mut scroll = drag.original_scroll;
        let area = match drag.kind {
            WindowDragKind::Move => {
                let width = drag.original.max_x - drag.original.min_x;
                let height = drag.original.max_y - drag.original.min_y;
                let min_x = drag.original.min_x.saturating_add(dx).clamp(
                    FRAME_BORDER,
                    DESKTOP_WIDTH - width - window_outer_extra_x(window),
                );
                let min_y = drag.original.min_y.saturating_add(dy).clamp(
                    window_outer_bottom_extra_y(window),
                    DESKTOP_HEIGHT - height - window_outer_top_extra_y(window),
                );
                WorkArea {
                    min_x,
                    max_x: min_x + width,
                    min_y,
                    max_y: min_y + height,
                }
            }
            WindowDragKind::Resize => {
                let min_width = window.min_width.max(48).min(max_width);
                let min_height = window.min_height.max(48).min(max_height);
                let width = drag
                    .original
                    .max_x
                    .saturating_add(dx)
                    .saturating_sub(drag.original.min_x)
                    .clamp(min_width, max_width);
                let height = drag
                    .original
                    .max_y
                    .saturating_sub(drag.original.min_y.saturating_add(dy))
                    .clamp(min_height, max_height);
                WorkArea {
                    max_x: drag.original.min_x + width,
                    min_y: drag.original.max_y - height,
                    ..drag.original
                }
            }
            WindowDragKind::ScrollSlider => {
                if let Some(bar) = layout.vertical_scrollbar {
                    let track_height = bar.track.max_y - bar.track.min_y;
                    let slider_height = bar.slider.max_y - bar.slider.min_y;
                    let slider_travel = (track_height - slider_height).max(0);
                    let scroll_range = (window.work_extent.max_y
                        - window.work_extent.min_y
                        - (drag.original.max_y - drag.original.min_y))
                        .max(0);
                    if slider_travel > 0 && scroll_range > 0 {
                        let slider_top = y.saturating_sub(drag.slider_grab_offset);
                        if slider_top != bar.slider.max_y {
                            let slider_offset =
                                (bar.track.max_y - slider_top).clamp(0, slider_travel);
                            let scrolled = i64::from(slider_offset) * i64::from(scroll_range)
                                / i64::from(slider_travel);
                            scroll.1 = window.work_extent.max_y - scrolled as i32;
                        }
                    }
                }
                drag.original
            }
        };
        let valid = validate_visible_area(window.work_extent, area, scroll.0, scroll.1)
            .and_then(|()| validate_screen_area(window, area))
            .is_ok();
        window.preview_area = valid.then_some(area);
        window.preview_scroll = valid.then_some(scroll);
        drop(state);
        let _ = self.desktop_updates.send(());
    }

    pub fn finish_drag(&self, drag: WindowDrag) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let behind = window_behind(&state, drag.handle);
        let Some(window) = state.windows.get_mut(&drag.handle) else {
            return;
        };
        let area = window.preview_area.unwrap_or(window.work_area);
        let scroll = window
            .preview_scroll
            .unwrap_or((window.scroll_x, window.scroll_y));
        if window.maximized && drag.kind != WindowDragKind::ScrollSlider {
            window.maximized = false;
            window.last_user_area = area;
            window.last_user_scroll = scroll;
            window.restore_behind = behind;
        }
        let event = open_request_event(window, area, scroll, behind, 0, 0);
        let owner = window.owner_task_handle;
        let _ = enqueue_for_owner(&mut state, owner, event);
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
    }

    pub fn key_pressed(&self, key_code: u32) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if key_code == 27 && state.active_menu.take().is_some() {
            drop(state);
            self.changed.notify_all();
            let _ = self.desktop_updates.send(());
            return;
        }
        state.notice = None;
        let Some(handle) = state.keyboard_focus else {
            return;
        };
        let Some(window) = state.windows.get(&handle) else {
            return;
        };
        let (window_handle, owner_task_handle) = (window.handle, window.owner_task_handle);
        if let Some(task) = state.tasks.get(&owner_task_handle)
            && !task.initialised
        {
            if let Some(input) = &task.input {
                let character = if key_code == 13 {
                    b'\r'
                } else {
                    key_code as u8
                };
                let _ = input.send(character);
            }
            return;
        }
        let mut event = QueuedEvent {
            reason: 8,
            block: [0; POLL_BLOCK_SIZE],
            pointer_button_state: None,
        };
        put_word(&mut event.block, 0, window_handle);
        put_word(&mut event.block, 4, u32::MAX); // no caret icon
        put_word(&mut event.block, 24, key_code);
        let _ = enqueue_for_owner(&mut state, owner_task_handle, event);
        drop(state);
        self.changed.notify_all();
    }

    pub fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.stopped = true;
        }
        self.changed.notify_all();
    }

    /// Block the hosted command task until the host closes the Wimp session.
    pub fn wait_until_stopped(&self) -> Result<(), RuntimeError> {
        let mut state = self.lock_state()?;
        while !state.stopped {
            state = self
                .changed
                .wait(state)
                .map_err(|_| program_error("Wimp session lock was poisoned"))?;
        }
        Ok(())
    }

    /// The Wimp closes a task's windows if it exits without Wimp_CloseDown.
    pub fn task_exited(&self, guest_task_id: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        remove_task(&mut state, guest_task_id);
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
    }

    fn initialise(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        if !matches!(context.registers[0], 200 | 300 | 310) {
            return Err(program_error(
                "Wimp_Initialise version must be 200, 300 or 310",
            ));
        }
        if context.registers[1] != TASK_MAGIC {
            return Err(program_error("Wimp_Initialise R1 must contain 'TASK'"));
        }
        let _description = read_control_string(task, context.registers[2], 128)?;
        if context.registers[0] >= 300 && context.registers[3] != 0 {
            let mut address = context.registers[3];
            let mut terminated = false;
            for _ in 0..128 {
                if read_word(&task.memory.read_bytes(address, 4)?, 0) == 0 {
                    terminated = true;
                    break;
                }
                address = address
                    .checked_add(4)
                    .ok_or(crate::memory::MemoryError::AddressOverflow)?;
            }
            if !terminated {
                return Err(program_error(
                    "Wimp_Initialise message list is not terminated",
                ));
            }
        }

        let mut state = self.lock_state()?;
        let handle = if let Some(handle) = state.guest_to_task.get(&task.id).copied() {
            let registered = state
                .tasks
                .get_mut(&handle)
                .ok_or_else(|| program_error("registered task has no Wimp state"))?;
            if registered.initialised {
                return Err(program_error("task has already called Wimp_Initialise"));
            }
            registered.initialised = true;
            state
                .windows
                .retain(|_, window| !(window.owner_task_handle == handle && window.console_window));
            let existing_windows = state.windows.keys().copied().collect::<Vec<_>>();
            state
                .stacking
                .retain(|window| existing_windows.contains(window));
            if state
                .keyboard_focus
                .is_some_and(|focused| !state.windows.contains_key(&focused))
            {
                state.keyboard_focus = state.stacking.first().copied();
            }
            handle
        } else {
            let handle = allocate_handle(&mut state.next_task_handle)?;
            state.guest_to_task.insert(task.id, handle);
            state.tasks.insert(
                handle,
                WimpTask {
                    events: VecDeque::new(),
                    initialised: true,
                    input: None,
                    last_event_button_state: None,
                    ..WimpTask::default()
                },
            );
            handle
        };
        context.registers[0] = WIMP_VERSION;
        context.registers[1] = handle;
        Ok(())
    }

    fn create_window(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let address = context.registers[1];
        let block = task.memory.read_bytes(address, WINDOW_BLOCK_SIZE)?;
        let icon_count = read_word(&block, 84);
        if icon_count != 0 {
            return Err(program_error(
                "hosted Wimp_CreateWindow currently requires zero initial icons",
            ));
        }
        let flags = read_word(&block, 28);
        let title_flags = read_word(&block, 56);
        let modern_controls = flags & (1 << 31) != 0;
        let supported_flags = if modern_controls {
            (1 << 31)
                | (1 << 29)
                | (1 << 28)
                | (1 << 27)
                | (1 << 26)
                | (1 << 25)
                | (1 << 24)
                | (1 << 9)
                | (1 << 8)
                | (1 << 1)
        } else {
            (1 << 7) | (1 << 2) | (1 << 1) | (1 << 0)
        };
        if flags & !supported_flags != 0 {
            return Err(program_error(
                "Wimp_CreateWindow requested unsupported window flags",
            ));
        }
        let has_vertical_scrollbar = if modern_controls {
            flags & (1 << 28) != 0
        } else {
            flags & (1 << 2) != 0
        };
        let resizable = if modern_controls {
            flags & (1 << 29) != 0
        } else {
            has_vertical_scrollbar
        };
        if resizable && !has_vertical_scrollbar {
            return Err(program_error(
                "Wimp_CreateWindow size icon requires a supported scroll bar",
            ));
        }
        let has_title = if modern_controls {
            flags & (1 << 26) != 0
        } else {
            flags & 1 != 0
        };
        let has_back_icon = if modern_controls {
            flags & (1 << 24) != 0
        } else {
            has_title && flags & (1 << 7) == 0
        };
        let closable = if modern_controls {
            flags & (1 << 25) != 0
        } else {
            flags & (1 << 7) == 0
        };
        if closable && !has_title {
            return Err(program_error(
                "Wimp_CreateWindow Close icon requires a Title Bar",
            ));
        }
        let has_toggle_size_icon = modern_controls && flags & (1 << 27) != 0;
        if has_back_icon && !has_title {
            return Err(program_error(
                "Wimp_CreateWindow Back icon requires a Title Bar",
            ));
        }
        if has_toggle_size_icon && !(has_title || has_vertical_scrollbar) {
            return Err(program_error(
                "Wimp_CreateWindow Toggle Size icon requires a Title Bar or vertical scroll bar",
            ));
        }
        if title_flags & !((1 << 0) | (1 << 8)) != 0 {
            return Err(program_error(
                "hosted Wimp_CreateWindow supports text titles only",
            ));
        }
        let title = if has_title && title_flags & 1 != 0 {
            if title_flags & (1 << 8) != 0 {
                let length = read_word(&block, 80) as usize;
                if length == 0 || length > 4096 {
                    return Err(program_error("Wimp title buffer length is invalid"));
                }
                read_control_string(task, read_word(&block, 72), length)?
            } else {
                control_terminated(&block[72..84])
            }
        } else {
            String::new()
        };
        let work_extent = WorkArea {
            min_x: read_word(&block, 40) as i32,
            min_y: read_word(&block, 44) as i32,
            max_x: read_word(&block, 48) as i32,
            max_y: read_word(&block, 52) as i32,
        };
        validate_geometry(work_extent)?;
        let declared_min_width = read_halfword(&block, 68) as i32;
        let declared_min_height = read_halfword(&block, 70) as i32;
        let min_width = minimum_work_width(
            declared_min_width,
            has_title,
            has_back_icon,
            closable,
            has_toggle_size_icon,
            has_vertical_scrollbar,
        );
        let min_height = minimum_work_height(declared_min_height, has_vertical_scrollbar);
        let work_area_flags = read_word(&block, 60);
        let button_type = (work_area_flags >> 12) & 0xF;
        if !matches!(button_type, 0 | 3 | 10) {
            return Err(program_error(
                "hosted Wimp_CreateWindow supports ignored, once-only, and double-click work areas (types 0, 3 and 10)",
            ));
        }
        let initial_area = WorkArea {
            min_x: read_word(&block, 0) as i32,
            min_y: read_word(&block, 4) as i32,
            max_x: read_word(&block, 8) as i32,
            max_y: read_word(&block, 12) as i32,
        };
        let initial_scroll_x = read_word(&block, 16) as i32;
        let initial_scroll_y = read_word(&block, 20) as i32;
        if initial_area
            != (WorkArea {
                min_x: 0,
                min_y: 0,
                max_x: 0,
                max_y: 0,
            })
        {
            validate_visible_area(
                work_extent,
                initial_area,
                initial_scroll_x,
                initial_scroll_y,
            )?;
            validate_min_dimensions(initial_area, min_width, min_height)?;
            validate_screen_area_parts(
                initial_area,
                has_title || has_toggle_size_icon,
                has_vertical_scrollbar,
                resizable,
            )?;
        }

        let mut state = self.lock_state()?;
        let owner_task_handle = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_CreateWindow called before Wimp_Initialise"))?;
        let handle = allocate_handle(&mut state.next_window_handle)?;
        state.windows.insert(
            handle,
            WimpWindow {
                handle,
                owner_task_handle,
                owner_task_id: task.id,
                title,
                flags,
                work_area_flags,
                work_area_background: block[35],
                work_area: initial_area,
                work_extent,
                invalid_regions: Vec::new(),
                min_width,
                min_height,
                scroll_x: initial_scroll_x,
                scroll_y: initial_scroll_y,
                has_title,
                has_back_icon,
                has_vertical_scrollbar,
                has_toggle_size_icon,
                closable,
                movable: flags & (1 << 1) != 0,
                resizable,
                open: false,
                has_opened: false,
                preview_area: None,
                preview_scroll: None,
                last_user_area: initial_area,
                last_user_scroll: (initial_scroll_x, initial_scroll_y),
                maximized: false,
                toggle_request_pending: false,
                restore_behind: -1,
                console_window: false,
            },
        );
        context.registers[0] = handle;
        drop(state);
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn open_window(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let address = context.registers[1];
        let block = task.memory.read_bytes(address, OPEN_BLOCK_SIZE)?;
        let handle = read_word(&block, 0);
        let area = WorkArea {
            min_x: read_word(&block, 4) as i32,
            min_y: read_word(&block, 8) as i32,
            max_x: read_word(&block, 12) as i32,
            max_y: read_word(&block, 16) as i32,
        };
        validate_geometry(area)?;
        let scroll_x = read_word(&block, 20) as i32;
        let scroll_y = read_word(&block, 24) as i32;
        let behind = read_word(&block, 28) as i32;
        let mut state = self.lock_state()?;
        let caller = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_OpenWindow called before Wimp_Initialise"))?;
        let window = state
            .windows
            .get(&handle)
            .ok_or_else(|| program_error("Wimp_OpenWindow received an unknown window handle"))?;
        if window.owner_task_handle != caller {
            return Err(program_error("window handle belongs to another Wimp task"));
        }
        validate_visible_area(window.work_extent, area, scroll_x, scroll_y)?;
        validate_min_dimensions(area, window.min_width, window.min_height)?;
        validate_screen_area(window, area)?;
        if behind != -1 && behind != -2 && behind <= 0 {
            return Err(program_error(
                "Wimp_OpenWindow has an unsupported stack position",
            ));
        }
        if behind > 0 {
            let target = behind as u32;
            if target == handle || !state.stacking.contains(&target) {
                return Err(program_error(
                    "Wimp_OpenWindow stack handle is unknown, closed or self",
                ));
            }
        }
        let next_stacking = stacking_after_open(&state, handle, behind)?;
        let window = state.windows.get_mut(&handle).expect("window was checked");
        let first_open = !window.has_opened;
        let full_extent = window.work_extent;
        if !window.maximized && !window.toggle_request_pending {
            window.last_user_area = area;
            window.last_user_scroll = (scroll_x, scroll_y);
        }
        window.work_area = area;
        window.scroll_x = scroll_x;
        window.scroll_y = scroll_y;
        window.open = true;
        window.has_opened = true;
        window.preview_area = None;
        window.preview_scroll = None;
        window.toggle_request_pending = false;
        state.stacking = next_stacking;
        if first_open {
            add_invalid_region(
                state
                    .windows
                    .get_mut(&handle)
                    .expect("window remains registered"),
                full_extent,
            );
        }
        queue_visible_invalid_redraws(&mut state);
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn close_window(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let address = context.registers[1];
        let block = task.memory.read_bytes(address, 4)?;
        let handle = read_word(&block, 0);
        let mut state = self.lock_state()?;
        let caller = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_CloseWindow called before Wimp_Initialise"))?;
        let window = state
            .windows
            .get_mut(&handle)
            .ok_or_else(|| program_error("Wimp_CloseWindow received an unknown window handle"))?;
        if window.owner_task_handle != caller {
            return Err(program_error("window handle belongs to another Wimp task"));
        }
        close_window_in_state(&mut state, handle);
        drop(state);
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn redraw_window(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        const REDRAW_BLOCK_SIZE: usize = 44;
        let address = context.registers[1];
        let mut block = task.memory.read_bytes(address, REDRAW_BLOCK_SIZE)?;
        let handle = read_word(&block, 0);
        let mut state = self.lock_state()?;
        let owner = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_RedrawWindow called before Wimp_Initialise"))?;
        let task_state = state
            .tasks
            .get_mut(&owner)
            .ok_or_else(|| program_error("registered Wimp task has no state"))?;
        if task_state.redraw_event_pending != Some(handle) {
            return Err(program_error(
                "Wimp_RedrawWindow does not match the preceding Redraw_Window_Request",
            ));
        }
        task_state.redraw_event_pending = None;
        let window =
            state.windows.get(&handle).cloned().ok_or_else(|| {
                program_error("Wimp_RedrawWindow received an unknown window handle")
            })?;
        if window.owner_task_handle != owner {
            return Err(program_error("window handle belongs to another Wimp task"));
        }
        let mut rectangles = VecDeque::new();
        for invalid in &window.invalid_regions {
            rectangles.extend(visible_redraw_rectangles(&state, handle, *invalid));
        }
        let mut redraw = RedrawLoop {
            window_handle: handle,
            rectangles,
            clears_background: window.work_area_background != 0xFF,
            current_work: None,
        };
        let first = redraw.rectangles.pop_front();
        if let Some(rectangle) = first {
            redraw.current_work = Some(rectangle.work);
            subtract_invalid_region(
                state
                    .windows
                    .get_mut(&handle)
                    .expect("window remains registered"),
                rectangle.work,
            );
            write_redraw_block(&mut block, &window, rectangle);
            state
                .tasks
                .get_mut(&owner)
                .expect("task remains registered")
                .redraw_loop = Some(redraw);
            context.registers[0] = 1;
        } else {
            state
                .tasks
                .get_mut(&owner)
                .expect("task remains registered")
                .redraw_loop = None;
            context.registers[0] = 0;
        }
        drop(state);
        task.memory.write_bytes(address, &block)?;
        Ok(())
    }

    fn update_window(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        const REDRAW_BLOCK_SIZE: usize = 44;
        let address = context.registers[1];
        let input = task.memory.read_bytes(address, REDRAW_BLOCK_SIZE)?;
        let handle = read_word(&input, 0);
        let region = WorkArea {
            min_x: read_word(&input, 4) as i32,
            min_y: read_word(&input, 8) as i32,
            max_x: read_word(&input, 12) as i32,
            max_y: read_word(&input, 16) as i32,
        };
        validate_geometry(region)?;
        let mut state = self.lock_state()?;
        let owner = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_UpdateWindow called before Wimp_Initialise"))?;
        let window =
            state.windows.get(&handle).cloned().ok_or_else(|| {
                program_error("Wimp_UpdateWindow received an unknown window handle")
            })?;
        if window.owner_task_handle != owner {
            return Err(program_error("window handle belongs to another Wimp task"));
        }
        let mut redraw = RedrawLoop {
            window_handle: handle,
            rectangles: visible_redraw_rectangles(&state, handle, region).into(),
            clears_background: false,
            current_work: None,
        };
        let mut block = vec![0_u8; REDRAW_BLOCK_SIZE];
        put_word(&mut block, 0, handle);
        if let Some(rectangle) = redraw.rectangles.pop_front() {
            redraw.current_work = Some(rectangle.work);
            write_redraw_block(&mut block, &window, rectangle);
            state
                .tasks
                .get_mut(&owner)
                .expect("task remains registered")
                .redraw_loop = Some(redraw);
            context.registers[0] = 1;
        } else {
            state
                .tasks
                .get_mut(&owner)
                .expect("task remains registered")
                .redraw_loop = None;
            context.registers[0] = 0;
        }
        drop(state);
        task.memory.write_bytes(address, &block)?;
        Ok(())
    }

    fn get_rectangle(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        const REDRAW_BLOCK_SIZE: usize = 44;
        let address = context.registers[1];
        let mut block = task.memory.read_bytes(address, REDRAW_BLOCK_SIZE)?;
        let handle = read_word(&block, 0);
        let mut state = self.lock_state()?;
        let owner = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_GetRectangle called before Wimp_Initialise"))?;
        let window =
            state.windows.get(&handle).cloned().ok_or_else(|| {
                program_error("Wimp_GetRectangle received an unknown window handle")
            })?;
        if window.owner_task_handle != owner {
            return Err(program_error("window handle belongs to another Wimp task"));
        }
        let (rectangle, clears_background) = {
            let task_state = state
                .tasks
                .get_mut(&owner)
                .expect("task remains registered");
            let redraw = task_state
                .redraw_loop
                .as_mut()
                .filter(|redraw| redraw.window_handle == handle)
                .ok_or_else(|| {
                    program_error("Wimp_GetRectangle called without an active redraw/update")
                })?;
            let rectangle = redraw.rectangles.pop_front();
            if let Some(rectangle) = rectangle {
                redraw.current_work = Some(rectangle.work);
            }
            (rectangle, redraw.clears_background)
        };
        if let Some(rectangle) = rectangle {
            write_redraw_block(&mut block, &window, rectangle);
            if clears_background {
                subtract_invalid_region(
                    state
                        .windows
                        .get_mut(&handle)
                        .expect("window remains registered"),
                    rectangle.work,
                );
            }
            context.registers[0] = 1;
        } else {
            state
                .tasks
                .get_mut(&owner)
                .expect("task remains registered")
                .redraw_loop = None;
            context.registers[0] = 0;
        }
        drop(state);
        task.memory.write_bytes(address, &block)?;
        Ok(())
    }

    fn force_redraw(&self, task: &Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let handle = context.registers[0] as i32;
        let region = WorkArea {
            min_x: context.registers[1] as i32,
            min_y: context.registers[2] as i32,
            max_x: context.registers[3] as i32,
            max_y: context.registers[4] as i32,
        };
        validate_geometry(region)?;
        let mut state = self.lock_state()?;
        let owner = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_ForceRedraw called before Wimp_Initialise"))?;
        match handle {
            -2 => {
                drop(state);
                let _ = self.desktop_updates.send(());
                return Ok(());
            }
            -1 => {
                for window_handle in state.stacking.clone() {
                    let window = state
                        .windows
                        .get(&window_handle)
                        .expect("stacked window exists")
                        .clone();
                    let screen = DesktopRect {
                        min_x: region.min_x,
                        min_y: region.min_y,
                        max_x: region.max_x,
                        max_y: region.max_y,
                    };
                    let visible = visible_screen_work_area(&window);
                    if let Some(screen_part) = intersect_desktop_rect(screen, visible) {
                        let work = screen_rect_to_work(&window, screen_part);
                        if !work.is_empty() {
                            add_invalid_region(
                                state
                                    .windows
                                    .get_mut(&window_handle)
                                    .expect("window remains registered"),
                                work,
                            );
                            queue_window_redraw(&mut state, window_handle);
                        }
                    }
                }
            }
            value if value > 0 => {
                let handle = value as u32;
                let window = state.windows.get(&handle).cloned().ok_or_else(|| {
                    program_error("Wimp_ForceRedraw received an unknown window handle")
                })?;
                if window.owner_task_handle != owner {
                    return Err(program_error("window handle belongs to another Wimp task"));
                }
                let Some(region) = intersect_work_area(region, window.work_extent) else {
                    return Ok(());
                };
                add_invalid_region(
                    state
                        .windows
                        .get_mut(&handle)
                        .expect("window remains registered"),
                    region,
                );
                queue_window_redraw(&mut state, handle);
            }
            _ => {
                return Err(program_error(
                    "Wimp_ForceRedraw received an invalid window handle",
                ));
            }
        }
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn current_graphics_clip(
        &self,
        task_id: u64,
    ) -> Option<crate::graphics::GraphicsWindow> {
        let state = self.state.lock().ok()?;
        let owner = state.guest_to_task.get(&task_id)?;
        let redraw = state.tasks.get(owner)?.redraw_loop.as_ref()?;
        let work = redraw.current_work?;
        Some(crate::graphics::GraphicsWindow {
            left: work.min_x,
            bottom: work.min_y,
            right: work.max_x,
            top: work.max_y,
        })
    }

    pub(crate) fn current_graphics_context(
        &self,
        task_id: u64,
    ) -> Option<(u32, WorkArea, WorkArea)> {
        let state = self.state.lock().ok()?;
        let owner = state.guest_to_task.get(&task_id)?;
        let redraw = state.tasks.get(owner)?.redraw_loop.as_ref()?;
        let work = redraw.current_work?;
        let window = state.windows.get(&redraw.window_handle)?;
        Some((redraw.window_handle, work, window.work_extent))
    }

    pub(crate) fn current_redraw_clears_background(&self, task_id: u64) -> bool {
        let Ok(state) = self.state.lock() else {
            return false;
        };
        let Some(owner) = state.guest_to_task.get(&task_id) else {
            return false;
        };
        state
            .tasks
            .get(owner)
            .and_then(|task| task.redraw_loop.as_ref())
            .is_some_and(|redraw| redraw.clears_background)
    }

    pub(crate) fn current_redraw_background_colour(&self, task_id: u64) -> Option<u32> {
        let state = self.state.lock().ok()?;
        let owner = state.guest_to_task.get(&task_id)?;
        let redraw = state.tasks.get(owner)?.redraw_loop.as_ref()?;
        let window = state.windows.get(&redraw.window_handle)?;
        let colour = window.work_area_background;
        if window.flags & (1 << 10) != 0 {
            return Some(u32::from(colour));
        }
        Some(match colour {
            // The hosted BASIC raster palette is black through white (0–7),
            // while the standard Wimp greys run white through black.
            0..=7 => 7 - u32::from(colour),
            8 => 4,  // dark blue
            9 => 3,  // yellow
            10 => 2, // green
            11 => 1, // red
            12 => 7, // cream, approximated by white in the classic palette
            13 => 2, // army green
            14 => 3, // orange
            15 => 6, // light blue
            _ => 7,
        })
    }

    fn create_menu(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let address = context.registers[1];
        let caller = {
            let state = self.lock_state()?;
            *state
                .guest_to_task
                .get(&task.id)
                .ok_or_else(|| program_error("Wimp_CreateMenu called before Wimp_Initialise"))?
        };
        let mut state = self.lock_state()?;
        if address == u32::MAX {
            if state
                .active_menu
                .as_ref()
                .is_some_and(|menu| menu.owner == caller)
            {
                state.active_menu = None;
                drop(state);
                self.changed.notify_all();
                let _ = self.desktop_updates.send(());
            }
            return Ok(());
        }
        let mut total_items = 0;
        let root = parse_menu(task, address, false, &mut Vec::new(), 0, &mut total_items)?;
        let previous_root = state
            .active_menu
            .as_ref()
            .filter(|old| old.owner == caller && old.root.address == address)
            .map(|old| (old.root_x, old.root_top, old.awaiting_adjust_reopen));
        let (root_x, root_top) = if let Some((x, top, true)) = previous_root {
            (x, top)
        } else {
            (
                clamp_menu_x(&root, context.registers[2] as i32),
                clamp_menu_top(&root, context.registers[3] as i32),
            )
        };
        let (open_path, selected_path, preserved_adjust) = state
            .active_menu
            .as_ref()
            .filter(|old| old.owner == caller && old.root.address == address)
            .map(|old| {
                let open_path = valid_menu_path(&root, &old.open_path);
                let selected_path = valid_selection_path(&root, &old.selected_path, &open_path);
                (open_path, selected_path, old.awaiting_adjust_reopen)
            })
            .unwrap_or_default();
        let mut active = ActiveMenu {
            owner: caller,
            root,
            root_x,
            root_top,
            open_path,
            selected_path,
            hover: None,
            awaiting_adjust_reopen: false,
            adjust_event_delivered: false,
            reopened_after_adjust: preserved_adjust,
            hover_delay: DEFAULT_MENU_HOVER_DELAY,
            drag: None,
        };
        if preserved_adjust {
            active.reopened_after_adjust = true;
        }
        state.active_menu = Some(active);
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn get_pointer_info(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let address = context.registers[1];
        task.memory.read_bytes(address, POINTER_INFO_BLOCK_SIZE)?;
        let mut state = self.lock_state()?;
        let caller = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_GetPointerInfo called before Wimp_Initialise"))?;
        let pointer = state.pointer;
        let selection_buttons = state
            .tasks
            .get_mut(&caller)
            .and_then(|task| task.last_event_button_state.take());
        let (window, icon) = pointer_window_and_icon(&state, pointer.x, pointer.y);
        let buttons = selection_buttons.unwrap_or(pointer.buttons);
        drop(state);
        let mut block = [0u8; POINTER_INFO_BLOCK_SIZE];
        put_word(&mut block, 0, pointer.x as u32);
        put_word(&mut block, 4, pointer.y as u32);
        put_word(&mut block, 8, buttons);
        put_word(&mut block, 12, window as u32);
        put_word(&mut block, 16, icon as u32);
        task.memory.write_bytes(address, &block)?;
        Ok(())
    }

    fn poll(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let mask = context.registers[0];
        let address = context.registers[1];
        // Validate the entire caller-owned block before waiting. A bad pointer
        // must not suspend the task or consume a queued event.
        task.memory.read_bytes(address, POLL_BLOCK_SIZE)?;
        let reserved =
            (1 << 2) | (1 << 3) | (1 << 7) | (1 << 9) | (0x7 << 14) | (0x3 << 20) | (0x7F << 25);
        if mask & reserved != 0 {
            return Err(program_error("Wimp_Poll has a non-zero reserved mask bit"));
        }
        if mask & ((1 << 22) | (1 << 23)) != 0 {
            return Err(program_error(
                "Wimp_Poll poll-word support is not implemented",
            ));
        }
        let mut state = self.lock_state()?;
        let handle = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_Poll called before Wimp_Initialise"))?;
        if state
            .active_menu
            .as_ref()
            .is_some_and(|menu| menu.owner == handle && menu.awaiting_adjust_reopen)
        {
            let should_close = state
                .active_menu
                .as_ref()
                .is_some_and(|menu| !menu.reopened_after_adjust && menu.adjust_event_delivered);
            if should_close {
                state.active_menu = None;
                let _ = self.desktop_updates.send(());
            } else if let Some(menu) = state.active_menu.as_mut() {
                if menu.reopened_after_adjust {
                    menu.awaiting_adjust_reopen = false;
                    menu.reopened_after_adjust = false;
                    menu.adjust_event_delivered = false;
                } else {
                    menu.adjust_event_delivered = true;
                }
            }
        }
        let event = loop {
            let Some(owner) = state.tasks.get_mut(&handle) else {
                return Err(program_error("Wimp task has exited"));
            };
            if let Some(index) = owner
                .events
                .iter()
                .position(|event| event.reason == 0 || mask & (1 << event.reason) == 0)
            {
                break owner.events.remove(index).expect("event index exists");
            }
            if state.stopped {
                return Err(RuntimeError::EndOfInput);
            }
            if mask & 1 == 0 {
                break null_event();
            }
            state = self
                .changed
                .wait(state)
                .map_err(|_| program_error("Wimp event queue lock was poisoned"))?;
        };
        if let Some(owner) = state.tasks.get_mut(&handle) {
            owner.last_event_button_state = event.pointer_button_state;
        }
        if event.reason == 1 {
            if let Some(owner) = state.tasks.get_mut(&handle) {
                owner.redraw_event_pending = Some(read_word(&event.block, 0));
            }
        }
        let mut block = event.block;
        if event.reason == 0 {
            block.fill(0);
        }
        task.memory.write_bytes(address, &block)?;
        context.registers[0] = event.reason;
        context.registers[1] = address;
        Ok(())
    }

    fn get_window_state(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let address = context.registers[1];
        let block = task.memory.read_bytes(address, WINDOW_STATE_BLOCK_SIZE)?;
        let handle = read_word(&block, 0);
        let state = self.lock_state()?;
        let caller = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_GetWindowState called before Wimp_Initialise"))?;
        let window = state.windows.get(&handle).ok_or_else(|| {
            program_error("Wimp_GetWindowState received an unknown window handle")
        })?;
        if window.owner_task_handle != caller {
            return Err(program_error("window handle belongs to another Wimp task"));
        }
        let front = state
            .stacking
            .iter()
            .position(|item| *item == handle)
            .and_then(|index| {
                index
                    .checked_sub(1)
                    .and_then(|front| state.stacking.get(front).copied())
            })
            .map(|front| front as i32)
            .unwrap_or(-1);
        let fully_visible = window.open
            && !state
                .stacking
                .iter()
                .take_while(|item| **item != handle)
                .any(|other| {
                    state.windows.get(other).is_some_and(|front_window| {
                        rects_overlap(
                            window_furniture(front_window).outer,
                            window_furniture(window).outer,
                        )
                    })
                });
        let dynamic_mask = (1 << 16) | (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20);
        let control_mask = (1 << 24) | (1 << 25) | (1 << 26) | (1 << 27) | (1 << 28) | (1 << 29);
        let mut flags = window.flags & !(dynamic_mask | control_mask);
        if window.has_back_icon {
            flags |= 1 << 24;
        }
        if window.closable {
            flags |= 1 << 25;
        }
        if window.has_title {
            flags |= 1 << 26;
        }
        if window.has_toggle_size_icon {
            flags |= 1 << 27;
        }
        if window.has_vertical_scrollbar {
            flags |= 1 << 28;
        }
        if window.resizable {
            flags |= 1 << 29;
        }
        if window.open {
            flags |= 1 << 16;
        }
        if fully_visible {
            flags |= 1 << 17;
        }
        if state.keyboard_focus == Some(handle) {
            flags |= 1 << 20;
        }
        if window.maximized {
            flags |= 1 << 18;
        }
        if window.toggle_request_pending {
            flags |= 1 << 19;
        }
        let mut result = [0; WINDOW_STATE_BLOCK_SIZE];
        put_word(&mut result, 0, handle);
        put_word(&mut result, 4, window.work_area.min_x as u32);
        put_word(&mut result, 8, window.work_area.min_y as u32);
        put_word(&mut result, 12, window.work_area.max_x as u32);
        put_word(&mut result, 16, window.work_area.max_y as u32);
        put_word(&mut result, 20, window.scroll_x as u32);
        put_word(&mut result, 24, window.scroll_y as u32);
        put_word(&mut result, 28, front as u32);
        put_word(&mut result, 32, flags);
        drop(state);
        task.memory.write_bytes(address, &result)?;
        context.registers[1] = address;
        Ok(())
    }

    fn close_down(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        if context.registers[1] != TASK_MAGIC {
            return Err(program_error("Wimp_CloseDown R1 must contain 'TASK'"));
        }
        let mut state = self.lock_state()?;
        let handle = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_CloseDown called before Wimp_Initialise"))?;
        if context.registers[0] != handle {
            return Err(program_error(
                "Wimp_CloseDown task handle does not belong to caller",
            ));
        }
        if state
            .active_menu
            .as_ref()
            .is_some_and(|menu| menu.owner == handle)
        {
            state.active_menu = None;
        }
        let started_by_wimp = state
            .tasks
            .get(&handle)
            .is_some_and(|registered| registered.started_by_wimp);
        if started_by_wimp {
            if let Some(registered) = state.tasks.get_mut(&handle) {
                registered.initialised = false;
                registered.events.clear();
            }
            state.icons.retain(|icon| icon.owner_task_id != task.id);
            state
                .windows
                .retain(|_, window| window.owner_task_handle != handle || window.console_window);
            state
                .window_icons
                .retain(|icon| icon.owner_task_id != task.id);
            let existing_windows = state.windows.keys().copied().collect::<Vec<_>>();
            state
                .stacking
                .retain(|window| existing_windows.contains(window));
            let console_exists = state
                .windows
                .values()
                .any(|window| window.owner_task_handle == handle && window.console_window);
            if !console_exists {
                insert_console_window(&mut state, handle, task.id, "BASIC")?;
            }
            if state
                .keyboard_focus
                .is_none_or(|focused| !state.windows.contains_key(&focused))
            {
                state.keyboard_focus = state
                    .stacking
                    .iter()
                    .copied()
                    .find(|window| {
                        state
                            .windows
                            .get(window)
                            .is_some_and(|w| w.owner_task_handle == handle)
                    })
                    .or_else(|| state.stacking.first().copied());
            }
        } else {
            remove_task(&mut state, task.id);
        }
        queue_visible_invalid_redraws(&mut state);
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn create_icon(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        self.create_icon_with_extension(task, context, false)
    }

    fn create_icon_ex(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        self.create_icon_with_extension(task, context, true)
    }

    fn create_icon_with_extension(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
        extended: bool,
    ) -> Result<(), RuntimeError> {
        let address = context.registers[1];
        let block = task
            .memory
            .read_bytes(address, if extended { 56 } else { 36 })?;
        let high_resolution_image = if extended {
            read_high_resolution_icon_image(task, &block)?
        } else {
            None
        };
        let legacy_block = &block[..36];
        let flags = read_word(legacy_block, 20);
        let button_type = (flags >> 12) & 0xF;
        let parent = read_word(legacy_block, 0) as i32;
        let (text, sprite_name) = read_icon_contents(task, legacy_block, flags)?;
        let mut state = self.lock_state()?;
        let owner = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_CreateIcon called before Wimp_Initialise"))?;
        let handle = allocate_handle(&mut state.next_icon_handle)?;
        if matches!(parent, -1 | -2) {
            if !matches!(button_type, 3 | 10) || flags & 1 == 0 {
                return Err(program_error(
                    "hosted icon-bar icons require text and button type 3 or 10",
                ));
            }
            if text.is_empty() {
                return Err(program_error("icon-bar text must not be empty"));
            }
            let requested_width = (read_word(legacy_block, 12) as i32)
                .saturating_sub(read_word(legacy_block, 4) as i32)
                .max(120);
            state.icons.push(WimpIcon {
                handle,
                owner_task_handle: owner,
                owner_task_id: task.id,
                side: if parent == -2 {
                    IconBarSide::Devices
                } else {
                    IconBarSide::Applications
                },
                label: icon_label(&text),
                sprite_name,
                high_resolution_image,
                width: requested_width,
                flags,
                button_type,
                activate_task_id: None,
            });
        } else {
            let window_handle = parent as u32;
            let window = state.windows.get(&window_handle).ok_or_else(|| {
                program_error("Wimp_CreateIcon received an unknown window handle")
            })?;
            if window.owner_task_handle != owner {
                return Err(program_error("window handle belongs to another Wimp task"));
            }
            if !matches!(button_type, 0 | 3 | 4 | 5 | 10 | 11) {
                return Err(program_error(
                    "hosted Wimp_CreateIcon does not support this icon button type",
                ));
            }
            let bounds = DesktopRect {
                min_x: read_word(legacy_block, 4) as i32,
                min_y: read_word(legacy_block, 8) as i32,
                max_x: read_word(legacy_block, 12) as i32,
                max_y: read_word(legacy_block, 16) as i32,
            };
            if bounds.is_empty()
                || bounds.min_x < window.work_extent.min_x
                || bounds.max_x > window.work_extent.max_x
                || bounds.min_y < window.work_extent.min_y
                || bounds.max_y > window.work_extent.max_y
            {
                return Err(program_error(
                    "Wimp_CreateIcon bounding box lies outside the window work extent",
                ));
            }
            state.window_icons.push(WimpWindowIcon {
                handle,
                window_handle,
                owner_task_id: task.id,
                bounds,
                flags,
                label: icon_label(&text),
                sprite_name,
                high_resolution_image,
            });
        }
        context.registers[0] = handle;
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn delete_icon(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let block = task.memory.read_bytes(context.registers[1], 8)?;
        let parent = read_word(&block, 0) as i32;
        let icon_handle = read_word(&block, 4);
        let mut state = self.lock_state()?;
        if matches!(parent, -1 | -2) {
            let side = if parent == -2 {
                IconBarSide::Devices
            } else {
                IconBarSide::Applications
            };
            let Some(index) = state.icons.iter().position(|icon| {
                icon.handle == icon_handle && icon.owner_task_id == task.id && icon.side == side
            }) else {
                return Err(program_error("icon-bar icon does not belong to caller"));
            };
            state.icons.remove(index);
        } else {
            let Some(window) = state.windows.get(&(parent as u32)) else {
                return Err(program_error(
                    "Wimp_DeleteIcon received an unknown window handle",
                ));
            };
            let owner = *state
                .guest_to_task
                .get(&task.id)
                .ok_or_else(|| program_error("Wimp_DeleteIcon called before Wimp_Initialise"))?;
            if window.owner_task_handle != owner {
                return Err(program_error("window handle belongs to another Wimp task"));
            }
            let Some(index) = state.window_icons.iter().position(|icon| {
                icon.handle == icon_handle
                    && icon.window_handle == parent as u32
                    && icon.owner_task_id == task.id
            }) else {
                return Err(program_error("window icon does not belong to caller"));
            };
            state.window_icons.remove(index);
        }
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn set_icon_state(
        &self,
        task: &mut Task,
        context: &mut SwiContext,
    ) -> Result<(), RuntimeError> {
        let block = task.memory.read_bytes(context.registers[1], 16)?;
        let parent = read_word(&block, 0) as i32;
        let icon_handle = read_word(&block, 4);
        let eor = read_word(&block, 8);
        let clear = read_word(&block, 12);
        let mut state = self.lock_state()?;
        if matches!(parent, -1 | -2) {
            let side = if parent == -2 {
                IconBarSide::Devices
            } else {
                IconBarSide::Applications
            };
            let Some(icon) = state.icons.iter_mut().find(|icon| {
                icon.handle == icon_handle && icon.owner_task_id == task.id && icon.side == side
            }) else {
                return Err(program_error("icon-bar icon does not belong to caller"));
            };
            icon.flags = (icon.flags & !clear) ^ eor;
        } else {
            let owner = *state
                .guest_to_task
                .get(&task.id)
                .ok_or_else(|| program_error("Wimp_SetIconState called before Wimp_Initialise"))?;
            let Some(window) = state.windows.get(&(parent as u32)) else {
                return Err(program_error(
                    "Wimp_SetIconState received an unknown window handle",
                ));
            };
            if window.owner_task_handle != owner {
                return Err(program_error("window handle belongs to another Wimp task"));
            }
            let Some(icon) = state.window_icons.iter_mut().find(|icon| {
                icon.handle == icon_handle
                    && icon.window_handle == parent as u32
                    && icon.owner_task_id == task.id
            }) else {
                return Err(program_error("window icon does not belong to caller"));
            };
            icon.flags = (icon.flags & !clear) ^ eor;
        }
        drop(state);
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn set_extent(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let handle = context.registers[0];
        let block = task.memory.read_bytes(context.registers[1], 16)?;
        let extent = WorkArea {
            min_x: read_word(&block, 0) as i32,
            min_y: read_word(&block, 4) as i32,
            max_x: read_word(&block, 8) as i32,
            max_y: read_word(&block, 12) as i32,
        };
        validate_geometry(extent)?;
        let mut state = self.lock_state()?;
        let owner = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_SetExtent called before Wimp_Initialise"))?;
        {
            let window = state
                .windows
                .get_mut(&handle)
                .ok_or_else(|| program_error("Wimp_SetExtent received an unknown window handle"))?;
            if window.owner_task_handle != owner {
                return Err(program_error("window handle belongs to another Wimp task"));
            }
            validate_visible_area(extent, window.work_area, window.scroll_x, window.scroll_y)?;
            let previous_extent = window.work_extent;
            window.invalid_regions = window
                .invalid_regions
                .drain(..)
                .filter_map(|region| intersect_work_area(region, extent))
                .collect();
            window.work_extent = extent;
            for newly_added in subtract_work_area(extent, previous_extent) {
                add_invalid_region(window, newly_added);
            }
        }
        queue_visible_invalid_redraws(&mut state);
        context.registers[0] = 0;
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn start_task(&self, task: &mut Task, context: &mut SwiContext) -> Result<(), RuntimeError> {
        let command = read_control_string(task, context.registers[0], 256)?;
        let command = command.trim().trim_start_matches('*');
        let (verb, path) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        let guest_path = path.trim().trim_matches('"').trim_matches('\'');
        let kind = if verb.eq_ignore_ascii_case("Commands") && guest_path.is_empty() {
            DesktopTaskKind::Commands
        } else if verb.eq_ignore_ascii_case("BASIC") {
            if guest_path.is_empty() {
                DesktopTaskKind::BasicWindow
            } else {
                DesktopTaskKind::File
            }
        } else {
            return Err(program_error(
                "Wimp_StartTask supports Commands, BASIC, or BASIC <guest-path>",
            ));
        };
        if guest_path.contains('\n') || guest_path.contains('\r') {
            return Err(program_error("Wimp_StartTask path is invalid"));
        }
        let mut state = self.lock_state()?;
        let _owner = *state
            .guest_to_task
            .get(&task.id)
            .ok_or_else(|| program_error("Wimp_StartTask called by a non-Wimp task"))?;
        let task_id = state.next_guest_task_id;
        state.next_guest_task_id = state
            .next_guest_task_id
            .checked_add(1)
            .ok_or_else(|| program_error("hosted task id space is exhausted"))?;
        let task_handle = allocate_handle(&mut state.next_task_handle)?;
        let label = match kind {
            DesktopTaskKind::Commands => "*Commands",
            DesktopTaskKind::BasicWindow => "BASIC window",
            DesktopTaskKind::File => guest_path.rsplit('.').next().unwrap_or(guest_path),
        };
        state.guest_to_task.insert(task_id, task_handle);
        state.tasks.insert(
            task_handle,
            WimpTask {
                events: VecDeque::new(),
                initialised: false,
                started_by_wimp: true,
                input: None,
                last_event_button_state: None,
                ..WimpTask::default()
            },
        );
        insert_console_window(&mut state, task_handle, task_id, label)?;
        state.pending_launches.push_back(DesktopTaskRequest {
            kind,
            task_id,
            guest_path: guest_path.to_string(),
            title: label.to_string(),
        });
        context.registers[0] = task_handle;
        drop(state);
        self.changed.notify_all();
        let _ = self.desktop_updates.send(());
        Ok(())
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, WimpState>, RuntimeError> {
        self.state
            .lock()
            .map_err(|_| program_error("Wimp state lock was poisoned"))
    }
}

pub fn desktop_window_furniture(window: &DesktopWindow) -> WindowFurnitureLayout {
    let area = window.preview_area.unwrap_or(window.work_area);
    let (scroll_x, scroll_y) = window
        .preview_scroll
        .unwrap_or((window.scroll_x, window.scroll_y));
    WindowFurnitureLayout::new(
        area,
        window.work_extent,
        scroll_x,
        scroll_y,
        window.has_back_icon,
        window.has_title,
        window.closable,
        window.has_toggle_size_icon,
        window.has_vertical_scrollbar,
        window.resizable,
    )
}

fn window_furniture(window: &WimpWindow) -> WindowFurnitureLayout {
    WindowFurnitureLayout::new(
        window.preview_area.unwrap_or(window.work_area),
        window.work_extent,
        window
            .preview_scroll
            .map_or(window.scroll_x, |scroll| scroll.0),
        window
            .preview_scroll
            .map_or(window.scroll_y, |scroll| scroll.1),
        window.has_back_icon,
        window.has_title,
        window.closable,
        window.has_toggle_size_icon,
        window.has_vertical_scrollbar,
        window.resizable,
    )
}

fn point_in_window(window: &WimpWindow, x: i32, y: i32) -> bool {
    window_furniture(window).outer.contains(x, y)
}

fn icon_screen_bounds(icon: &WimpWindowIcon, window: &WimpWindow) -> DesktopRect {
    DesktopRect {
        min_x: window
            .work_area
            .min_x
            .saturating_add(icon.bounds.min_x)
            .saturating_sub(window.scroll_x),
        min_y: window
            .work_area
            .max_y
            .saturating_add(icon.bounds.min_y)
            .saturating_sub(window.scroll_y),
        max_x: window
            .work_area
            .min_x
            .saturating_add(icon.bounds.max_x)
            .saturating_sub(window.scroll_x),
        max_y: window
            .work_area
            .max_y
            .saturating_add(icon.bounds.max_y)
            .saturating_sub(window.scroll_y),
    }
}

fn clipped_icon_screen_bounds(icon: &WimpWindowIcon, window: &WimpWindow) -> Option<DesktopRect> {
    let bounds = icon_screen_bounds(icon, window);
    let work = window.work_area;
    let clipped = DesktopRect {
        min_x: bounds.min_x.max(work.min_x),
        min_y: bounds.min_y.max(work.min_y),
        max_x: bounds.max_x.min(work.max_x),
        max_y: bounds.max_y.min(work.max_y),
    };
    (!clipped.is_empty()).then_some(clipped)
}

fn system_icon_at(layout: WindowFurnitureLayout, x: i32, y: i32) -> Option<i32> {
    if layout.back_icon.is_some_and(|rect| rect.contains(x, y)) {
        return Some(-2);
    }
    if layout.close_icon.is_some_and(|rect| rect.contains(x, y)) {
        return Some(-3);
    }
    if layout
        .toggle_size_icon
        .is_some_and(|rect| rect.contains(x, y))
    {
        return Some(-5);
    }
    if layout
        .adjust_size_icon
        .is_some_and(|rect| rect.contains(x, y))
    {
        return Some(-9);
    }
    if let Some(bar) = layout.vertical_scrollbar {
        if bar.up_arrow.contains(x, y) {
            return Some(-6);
        }
        if bar.down_arrow.contains(x, y) {
            return Some(-8);
        }
        if bar.bounds.contains(x, y) {
            return Some(-7);
        }
    }
    if layout.title_bar.is_some_and(|rect| rect.contains(x, y)) {
        return Some(-4);
    }
    if layout.work_area.contains(x, y) {
        return Some(-1);
    }
    layout.outer.contains(x, y).then_some(-13)
}

fn window_outer_extra_x(window: &WimpWindow) -> i32 {
    if window.has_vertical_scrollbar {
        VERTICAL_SCROLLBAR_WIDTH + FRAME_BORDER
    } else {
        FRAME_BORDER
    }
}

fn window_outer_top_extra_y(window: &WimpWindow) -> i32 {
    if window.has_title || window.has_toggle_size_icon {
        TITLE_HEIGHT + FRAME_BORDER
    } else {
        FRAME_BORDER
    }
}

fn window_outer_bottom_extra_y(window: &WimpWindow) -> i32 {
    if window.resizable && !window.has_vertical_scrollbar {
        SIZE_ICON_HEIGHT + FRAME_BORDER
    } else {
        FRAME_BORDER
    }
}

fn validate_geometry(area: WorkArea) -> Result<(), RuntimeError> {
    if area.min_x < -MAX_DESKTOP_COORDINATE
        || area.min_y < -MAX_DESKTOP_COORDINATE
        || area.max_x > MAX_DESKTOP_COORDINATE
        || area.max_y > MAX_DESKTOP_COORDINATE
        || area.max_x <= area.min_x
        || area.max_y <= area.min_y
    {
        return Err(program_error(
            "window coordinates are empty or outside the hosted desktop range",
        ));
    }
    Ok(())
}

fn validate_visible_area(
    extent: WorkArea,
    area: WorkArea,
    scroll_x: i32,
    scroll_y: i32,
) -> Result<(), RuntimeError> {
    validate_geometry(area)?;
    let width = area.max_x - area.min_x;
    let height = area.max_y - area.min_y;
    let visible_max_x = scroll_x
        .checked_add(width)
        .ok_or_else(|| program_error("visible work area coordinate overflowed"))?;
    let visible_min_y = scroll_y
        .checked_sub(height)
        .ok_or_else(|| program_error("visible work area coordinate overflowed"))?;
    if scroll_x < extent.min_x
        || visible_max_x > extent.max_x
        || visible_min_y < extent.min_y
        || scroll_y > extent.max_y
    {
        return Err(program_error(
            "Wimp_OpenWindow visible work area lies outside the window extent",
        ));
    }
    Ok(())
}

fn validate_min_dimensions(
    area: WorkArea,
    minimum_width: i32,
    minimum_height: i32,
) -> Result<(), RuntimeError> {
    if area.max_x - area.min_x < minimum_width || area.max_y - area.min_y < minimum_height {
        return Err(program_error(
            "Wimp window is smaller than its minimum control size",
        ));
    }
    Ok(())
}

fn validate_screen_area(window: &WimpWindow, area: WorkArea) -> Result<(), RuntimeError> {
    validate_screen_area_parts(
        area,
        window.has_title || window.has_toggle_size_icon,
        window.has_vertical_scrollbar,
        window.resizable,
    )
}

fn minimum_work_width(
    declared: i32,
    has_title: bool,
    has_back_icon: bool,
    closable: bool,
    has_toggle_size_icon: bool,
    has_vertical_scrollbar: bool,
) -> i32 {
    let left_icons = if has_title {
        i32::from(has_back_icon) + i32::from(closable)
    } else {
        0
    };
    let right_icons = i32::from(has_toggle_size_icon);
    let title_text = if has_title { SYSTEM_FONT_WIDTH } else { 0 };
    let furniture_width = left_icons * TITLE_HEIGHT + right_icons * TITLE_HEIGHT + title_text
        - if has_vertical_scrollbar {
            VERTICAL_SCROLLBAR_WIDTH
        } else {
            0
        };
    declared.max(48).max(furniture_width.max(1))
}

fn minimum_work_height(declared: i32, has_vertical_scrollbar: bool) -> i32 {
    // Preserve both arrow cells plus enough page track for a minimum-size
    // thumb and non-zero slider travel.
    let control_height = if has_vertical_scrollbar {
        SCROLL_ARROW_SIZE * 2 + MIN_SLIDER_SIZE * 2
    } else {
        48
    };
    declared.max(48).max(control_height)
}

fn validate_screen_area_parts(
    area: WorkArea,
    has_title: bool,
    has_vertical_scrollbar: bool,
    resizable: bool,
) -> Result<(), RuntimeError> {
    let left = area
        .min_x
        .checked_sub(FRAME_BORDER)
        .ok_or_else(|| program_error("window system area coordinate overflowed"))?;
    let right = area
        .max_x
        .checked_add(if has_vertical_scrollbar {
            VERTICAL_SCROLLBAR_WIDTH
        } else {
            0
        })
        .and_then(|right| right.checked_add(FRAME_BORDER))
        .ok_or_else(|| program_error("window system area coordinate overflowed"))?;
    let top = area
        .max_y
        .checked_add(if has_title { TITLE_HEIGHT } else { 0 })
        .and_then(|top| top.checked_add(FRAME_BORDER))
        .ok_or_else(|| program_error("window system area coordinate overflowed"))?;
    let bottom = area
        .min_y
        .checked_sub(if resizable && !has_vertical_scrollbar {
            SIZE_ICON_HEIGHT + FRAME_BORDER
        } else {
            FRAME_BORDER
        })
        .ok_or_else(|| program_error("window system area coordinate overflowed"))?;
    if left < 0 || bottom < DESKTOP_ICONBAR_HEIGHT || right > DESKTOP_WIDTH || top > DESKTOP_HEIGHT
    {
        return Err(program_error(
            "window lies outside the hosted screen (off-screen windows are unsupported)",
        ));
    }
    Ok(())
}

fn rects_overlap(a: DesktopRect, b: DesktopRect) -> bool {
    a.min_x < b.max_x && a.max_x > b.min_x && a.min_y < b.max_y && a.max_y > b.min_y
}

#[derive(Clone, Debug)]
struct MenuGeometry {
    bounds: DesktopRect,
    title_bounds: Option<DesktopRect>,
    rows: Vec<DesktopRect>,
}

fn menu_geometry(menu: &WimpMenu, x: i32, top: i32) -> MenuGeometry {
    let width = menu_panel_width(menu);
    let title_height = if menu.title.is_empty() {
        0
    } else {
        MENU_TITLE_HEIGHT
    };
    let body_height = menu
        .rows
        .len()
        .saturating_mul(menu.row_height.max(1) as usize)
        .saturating_add(
            menu.rows
                .len()
                .saturating_sub(1)
                .saturating_mul(menu.gap.max(0) as usize),
        ) as i32;
    let separators = menu.rows.iter().filter(|row| row.flags & 2 != 0).count() as i32;
    let height = body_height + separators * MENU_SEPARATOR_HEIGHT + title_height + MENU_BORDER * 2;
    let bounds = DesktopRect {
        min_x: x,
        min_y: top - height,
        max_x: x + width,
        max_y: top,
    };
    let title_bounds = (title_height != 0).then_some(DesktopRect {
        min_x: x + MENU_BORDER,
        min_y: top - title_height,
        max_x: x + width - MENU_BORDER,
        max_y: top,
    });
    let body_top = top - title_height - MENU_BORDER;
    let inner_left = x + MENU_BORDER;
    let inner_right = x + width - MENU_BORDER;
    let mut row_top = body_top;
    let rows = menu
        .rows
        .iter()
        .map(|item| {
            let rect = DesktopRect {
                min_x: inner_left,
                min_y: row_top - menu.row_height,
                max_x: inner_right,
                max_y: row_top,
            };
            row_top -= menu.row_height + menu.gap;
            if item.flags & 2 != 0 {
                row_top -= MENU_SEPARATOR_HEIGHT;
            }
            rect
        })
        .collect();
    MenuGeometry {
        bounds,
        title_bounds,
        rows,
    }
}

fn menu_panel_width(menu: &WimpMenu) -> i32 {
    menu.width + MENU_GUTTER_WIDTH * 2 + MENU_BORDER * 2
}

fn menu_panel_height(menu: &WimpMenu) -> i32 {
    menu_geometry(menu, 0, 0).bounds.max_y - menu_geometry(menu, 0, 0).bounds.min_y
}

fn clamp_menu_x(menu: &WimpMenu, x: i32) -> i32 {
    x.clamp(0, (DESKTOP_WIDTH - menu_panel_width(menu)).max(0))
}

fn clamp_menu_top(menu: &WimpMenu, top: i32) -> i32 {
    top.clamp(menu_panel_height(menu).min(DESKTOP_HEIGHT), DESKTOP_HEIGHT)
}

fn menu_at_level(active: &ActiveMenu, level: usize) -> Option<&WimpMenu> {
    let mut menu = &active.root;
    for depth in 0..level {
        let row = active.open_path.get(depth).copied()?;
        menu = menu.rows.get(row)?.submenu.as_deref()?;
    }
    Some(menu)
}

fn menu_row_at_level(active: &ActiveMenu, level: usize, row: usize) -> Option<&WimpMenuRow> {
    menu_at_level(active, level)?.rows.get(row)
}

fn set_path_value(path: &mut Vec<usize>, level: usize, value: usize) {
    path.truncate(level);
    if path.len() <= level {
        path.resize(level + 1, usize::MAX);
    }
    path[level] = value;
}

fn menu_level_geometry(active: &ActiveMenu, level: usize) -> Option<MenuGeometry> {
    let menu = menu_at_level(active, level)?;
    if level == 0 {
        return Some(menu_geometry(menu, active.root_x, active.root_top));
    }
    let parent_row_index = *active.open_path.get(level - 1)?;
    let parent_geometry = menu_level_geometry(active, level - 1)?;
    let parent_row = *parent_geometry.rows.get(parent_row_index)?;
    let child_width = menu_panel_width(menu);
    let right_x = parent_geometry.bounds.max_x - MENU_BORDER;
    let left_x = parent_geometry.bounds.min_x - child_width + MENU_BORDER;
    let x = if right_x + child_width <= DESKTOP_WIDTH {
        right_x
    } else if left_x >= 0 {
        left_x
    } else {
        clamp_menu_x(menu, right_x)
    };
    let top = parent_row.max_y
        + MENU_BORDER
        + if menu.title.is_empty() {
            0
        } else {
            MENU_TITLE_HEIGHT
        };
    Some(menu_geometry(
        menu,
        clamp_menu_x(menu, x),
        clamp_menu_top(menu, top),
    ))
}

fn active_menu_panels(active: &ActiveMenu) -> Vec<DesktopMenu> {
    let mut panels = Vec::new();
    for level in 0..=active.open_path.len() {
        let (Some(menu), Some(geometry)) = (
            menu_at_level(active, level),
            menu_level_geometry(active, level),
        ) else {
            break;
        };
        let rows = menu
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let bounds = *geometry.rows.get(index)?;
                Some(DesktopMenuItem {
                    index,
                    label: row.label.clone(),
                    bounds,
                    tick: row.flags & 1 != 0,
                    separator_after: row.flags & 2 != 0,
                    has_submenu: row.submenu.is_some(),
                    shaded: row.icon_flags & (1 << 22) != 0,
                    selected: active.selected_path.get(level) == Some(&index)
                        && row.icon_flags & (1 << 22) == 0,
                    icon_flags: row.icon_flags,
                })
            })
            .collect();
        panels.push(DesktopMenu {
            title: menu.title.clone(),
            bounds: geometry.bounds,
            title_bounds: geometry.title_bounds,
            rows,
            title_foreground: menu.title_foreground,
            title_background: menu.title_background,
            work_foreground: menu.work_foreground,
            work_background: menu.work_background,
            reverse: menu.reverse,
        });
    }
    panels
}

fn active_menu_contains(active: &Option<ActiveMenu>, x: i32, y: i32) -> bool {
    active.as_ref().is_some_and(|menu| {
        active_menu_panels(menu)
            .iter()
            .any(|panel| panel.bounds.contains(x, y))
    })
}

fn menu_row_arrow_contains(panel: &DesktopMenu, row: &DesktopMenuItem, x: i32, y: i32) -> bool {
    if !row.bounds.contains(x, y) {
        return false;
    }
    if panel.reverse {
        x < row.bounds.min_x + MENU_GUTTER_WIDTH
    } else {
        x >= row.bounds.max_x - MENU_GUTTER_WIDTH
    }
}

fn update_menu_hover(active: &mut ActiveMenu, x: i32, y: i32, now: Instant) {
    let panels = active_menu_panels(active);
    let Some((level, panel)) = panels
        .iter()
        .enumerate()
        .rev()
        .find(|(_, panel)| panel.bounds.contains(x, y))
    else {
        active.hover = None;
        return;
    };
    if panel.title_bounds.is_some_and(|title| title.contains(x, y)) {
        active.hover = None;
        return;
    }
    let Some(row) = panel.rows.iter().find(|row| row.bounds.contains(x, y)) else {
        active.hover = None;
        return;
    };
    let parent_branch_stays_open = active.open_path.get(level) == Some(&row.index);
    if !parent_branch_stays_open {
        active.open_path.truncate(level);
    }
    set_path_value(&mut active.selected_path, level, row.index);
    let Some(internal_row) = menu_row_at_level(active, level, row.index) else {
        active.hover = None;
        return;
    };
    let can_open = internal_row.submenu.is_some()
        && (internal_row.icon_flags & (1 << 22) == 0 || internal_row.flags & (1 << 4) != 0);
    if row.shaded && !can_open {
        active.open_path.truncate(level);
        active.hover = None;
        return;
    }
    if can_open {
        if menu_row_arrow_contains(panel, row, x, y) {
            // The arrow is the explicit submenu affordance in RISC OS. Once
            // the pointer reaches it, open immediately so the pointer can
            // cross directly into the adjacent panel.
            active.open_path.truncate(level);
            active.open_path.push(row.index);
            active.hover = None;
        } else if active.open_path.get(level) != Some(&row.index) {
            let same_candidate = active
                .hover
                .is_some_and(|candidate| candidate.level == level && candidate.row == row.index);
            if !same_candidate {
                active.hover = Some(MenuHoverCandidate {
                    level,
                    row: row.index,
                    since: now,
                });
            }
        } else {
            active.hover = None;
        }
    } else {
        active.hover = None;
        if !parent_branch_stays_open {
            active.open_path.truncate(level);
        }
    }
}

fn menu_candidate_is_still_hovered(
    active: &ActiveMenu,
    candidate: MenuHoverCandidate,
    x: i32,
    y: i32,
) -> bool {
    let Some(panel) = active_menu_panels(active).get(candidate.level).cloned() else {
        return false;
    };
    let Some(row) = panel.rows.get(candidate.row) else {
        return false;
    };
    row.bounds.contains(x, y) && !menu_row_arrow_contains(&panel, row, x, y)
}

fn valid_menu_path(root: &WimpMenu, old_path: &[usize]) -> Vec<usize> {
    let mut valid = Vec::new();
    let mut menu = root;
    for index in old_path {
        let Some(row) = menu.rows.get(*index) else {
            break;
        };
        let Some(submenu) = row.submenu.as_deref() else {
            break;
        };
        valid.push(*index);
        menu = submenu;
    }
    valid
}

fn valid_selection_path(root: &WimpMenu, old_path: &[usize], open_path: &[usize]) -> Vec<usize> {
    let mut selected = Vec::new();
    let mut menu = root;
    for level in 0..=open_path.len() {
        let Some(index) = old_path.get(level).copied() else {
            break;
        };
        let Some(row) = menu.rows.get(index) else {
            break;
        };
        selected.push(index);
        if level == open_path.len() {
            break;
        }
        let Some(submenu) = row.submenu.as_deref() else {
            break;
        };
        menu = submenu;
    }
    selected
}

fn menu_mouse_down(state: &mut WimpState, x: i32, y: i32, buttons: u32) {
    let selection = {
        let Some(active) = state.active_menu.as_mut() else {
            return;
        };
        let panels = active_menu_panels(active);
        let Some((level, panel)) = panels
            .iter()
            .enumerate()
            .rev()
            .find(|(_, panel)| panel.bounds.contains(x, y))
            .map(|(level, panel)| (level, panel.clone()))
        else {
            return;
        };
        if let Some(title) = panel.title_bounds
            && title.contains(x, y)
            && buttons & 4 != 0
        {
            active.drag = Some(MenuDrag {
                start_x: x,
                start_y: y,
                root_x: active.root_x,
                root_top: active.root_top,
            });
            return;
        }
        let Some(row) = panel.rows.iter().find(|row| row.bounds.contains(x, y)) else {
            return;
        };
        let Some(internal) = menu_row_at_level(active, level, row.index) else {
            return;
        };
        let can_open_submenu =
            internal.submenu.is_some() && (!row.shaded || internal.flags & (1 << 4) != 0);
        if can_open_submenu {
            active.open_path.truncate(level);
            active.open_path.push(row.index);
            set_path_value(&mut active.selected_path, level, row.index);
            active.hover = None;
            return;
        }
        if row.shaded || internal.submenu.is_some() || buttons & (1 | 2 | 4) == 0 {
            return;
        }
        set_path_value(&mut active.selected_path, level, row.index);
        let mut path = active
            .open_path
            .iter()
            .take(level)
            .copied()
            .collect::<Vec<_>>();
        path.push(row.index);
        let mut event = QueuedEvent {
            reason: 9,
            block: [0; POLL_BLOCK_SIZE],
            pointer_button_state: Some(buttons),
        };
        for (index, selection) in path.iter().take(MAX_MENU_DEPTH).enumerate() {
            put_word(&mut event.block, index * 4, *selection as u32);
        }
        put_word(
            &mut event.block,
            path.len().min(MAX_MENU_DEPTH) * 4,
            u32::MAX,
        );
        let adjust = buttons & 1 != 0;
        active.awaiting_adjust_reopen = adjust;
        active.adjust_event_delivered = false;
        active.reopened_after_adjust = false;
        Some((active.owner, event, adjust))
    };
    if let Some((owner, event, adjust)) = selection {
        let _ = enqueue_for_owner(state, owner, event);
        if !adjust {
            state.active_menu = None;
        }
    }
}

fn pointer_window_and_icon(state: &WimpState, x: i32, y: i32) -> (i32, i32) {
    if active_menu_contains(&state.active_menu, x, y) {
        return (-1, -1);
    }
    if (0..DESKTOP_ICONBAR_HEIGHT).contains(&y) {
        let icon = layout_icon_bar(&state.icons, &state.windows)
            .into_iter()
            .find(|icon| icon.bounds.contains(x, y));
        return icon.map_or((-2, -1), |icon| {
            let parent = match icon.side {
                IconBarSide::Devices => -2,
                IconBarSide::Applications => -1,
            };
            (parent, icon.handle as i32)
        });
    }
    let Some(window) = state.stacking.iter().copied().find_map(|handle| {
        state
            .windows
            .get(&handle)
            .filter(|window| window.open && point_in_window(window, x, y))
    }) else {
        return (-1, -1);
    };
    if let Some(system_icon) = system_icon_at(window_furniture(window), x, y)
        && system_icon != -1
    {
        return (window.handle as i32, system_icon);
    }
    let icon = if window.work_area.contains(x, y) {
        state
            .window_icons
            .iter()
            .rev()
            .find(|icon| {
                icon.window_handle == window.handle
                    && icon.flags & ((1 << 22) | (1 << 23)) == 0
                    && clipped_icon_screen_bounds(icon, window)
                        .is_some_and(|bounds| bounds.contains(x, y))
            })
            .map(|icon| icon.handle as i32)
            .unwrap_or(-1)
    } else {
        -1
    };
    (window.handle as i32, icon)
}

fn enqueue_for_owner(state: &mut WimpState, owner: u32, event: QueuedEvent) -> bool {
    // Host-owned BASIC consoles never poll Wimp. Apply their geometry on the
    // UI thread instead of waiting for a guest OpenWindow acknowledgement.
    let handle = read_word(&event.block, 0);
    if matches!(event.reason, 2 | 10)
        && state
            .windows
            .get(&handle)
            .is_some_and(|w| w.console_window && w.owner_task_handle == owner)
    {
        let area = WorkArea {
            min_x: read_word(&event.block, 4) as i32,
            min_y: read_word(&event.block, 8) as i32,
            max_x: read_word(&event.block, 12) as i32,
            max_y: read_word(&event.block, 16) as i32,
        };
        let scroll = (
            read_word(&event.block, 20) as i32,
            read_word(&event.block, 24) as i32,
        );
        let behind = read_word(&event.block, 28) as i32;
        let Ok(stacking) = stacking_after_open(state, handle, behind) else {
            return false;
        };
        let window = state.windows.get_mut(&handle).unwrap();
        if validate_visible_area(window.work_extent, area, scroll.0, scroll.1).is_err()
            || validate_screen_area(window, area).is_err()
        {
            return false;
        }
        if !window.maximized && !window.toggle_request_pending {
            window.last_user_area = area;
            window.last_user_scroll = scroll;
        }
        window.work_area = area;
        window.scroll_x = scroll.0;
        window.scroll_y = scroll.1;
        window.preview_area = None;
        window.preview_scroll = None;
        window.toggle_request_pending = false;
        state.stacking = stacking;
        queue_visible_invalid_redraws(state);
        return true;
    }
    let Some(task) = state.tasks.get_mut(&owner) else {
        return false;
    };
    if task.events.len() == MAX_EVENT_QUEUE {
        // Preserve queued key events when a burst of pointer clicks fills the
        // bounded queue. If it contains no click, discard the newest event.
        if let Some(index) = task.events.iter().position(|queued| queued.reason == 6) {
            task.events.remove(index);
        } else {
            return false;
        }
    }
    task.events.push_back(event);
    true
}

fn event_with_word(reason: u32, offset: usize, value: u32) -> QueuedEvent {
    let mut event = QueuedEvent {
        reason,
        block: [0; POLL_BLOCK_SIZE],
        pointer_button_state: None,
    };
    put_word(&mut event.block, offset, value);
    event
}

fn mouse_event_with_icon(x: i32, y: i32, buttons: u32, window: u32, icon: i32) -> QueuedEvent {
    let mut event = QueuedEvent {
        reason: 6,
        block: [0; POLL_BLOCK_SIZE],
        pointer_button_state: None,
    };
    put_word(&mut event.block, 0, x as u32);
    put_word(&mut event.block, 4, y as u32);
    put_word(&mut event.block, 8, buttons);
    put_word(&mut event.block, 12, window);
    put_word(&mut event.block, 16, icon as u32);
    event
}

fn event_button_state(
    state: &mut WimpState,
    owner: u32,
    window: u32,
    icon: i32,
    x: i32,
    y: i32,
    buttons: u32,
    button_type: u32,
) -> u32 {
    // PRM Mouse_Click: Menu always reports 2, including double-click icons.
    if buttons & 2 != 0 {
        return 2;
    }
    if button_type != 10 {
        return buttons;
    }
    let is_double = state.last_click.is_some_and(|previous| {
        previous.owner == owner
            && previous.window == window
            && previous.icon == icon
            && previous.buttons == buttons
            && previous.at.elapsed().as_millis() <= 1000
            && (previous.x - x).abs() < 16
            && (previous.y - y).abs() < 16
    });
    if is_double {
        state.last_click = None;
        buttons
    } else {
        state.last_click = Some(RecentClick {
            at: Instant::now(),
            owner,
            window,
            icon,
            x,
            y,
            buttons,
        });
        buttons.saturating_mul(256)
    }
}

fn null_event() -> QueuedEvent {
    QueuedEvent {
        reason: 0,
        block: [0; POLL_BLOCK_SIZE],
        pointer_button_state: None,
    }
}

fn open_request_event(
    window: &WimpWindow,
    area: WorkArea,
    scroll: (i32, i32),
    behind: i32,
    scroll_x_direction: i32,
    scroll_y_direction: i32,
) -> QueuedEvent {
    let mut event = QueuedEvent {
        reason: 2,
        block: [0; POLL_BLOCK_SIZE],
        pointer_button_state: None,
    };
    put_word(&mut event.block, 0, window.handle);
    put_word(&mut event.block, 4, area.min_x as u32);
    put_word(&mut event.block, 8, area.min_y as u32);
    put_word(&mut event.block, 12, area.max_x as u32);
    put_word(&mut event.block, 16, area.max_y as u32);
    put_word(&mut event.block, 20, scroll.0 as u32);
    put_word(&mut event.block, 24, scroll.1 as u32);
    put_word(&mut event.block, 28, behind as u32);
    if scroll_x_direction != 0 || scroll_y_direction != 0 {
        event.reason = 10;
        put_word(&mut event.block, 32, scroll_x_direction as u32);
        put_word(&mut event.block, 36, scroll_y_direction as u32);
    }
    event
}

fn window_behind(state: &WimpState, handle: u32) -> i32 {
    state
        .stacking
        .iter()
        .position(|stacked| *stacked == handle)
        .and_then(|index| {
            index
                .checked_sub(1)
                .and_then(|front| state.stacking.get(front).copied())
        })
        .map(|front| front as i32)
        .unwrap_or(-1)
}

fn valid_restore_depth(state: &WimpState, behind: i32) -> i32 {
    if behind > 0 && !state.stacking.contains(&(behind as u32)) {
        -1
    } else {
        behind
    }
}

fn send_to_back(state: &mut WimpState, handle: u32) {
    state.stacking.retain(|item| *item != handle);
    state.stacking.push(handle);
    if state.keyboard_focus == Some(handle) {
        state.keyboard_focus = state.stacking.first().copied();
    }
    queue_visible_invalid_redraws(state);
}

fn maximum_window_area(window: &WimpWindow) -> WorkArea {
    let min_x = FRAME_BORDER;
    let min_y = if window.resizable && !window.has_vertical_scrollbar {
        DESKTOP_ICONBAR_HEIGHT + SIZE_ICON_HEIGHT + FRAME_BORDER
    } else {
        DESKTOP_ICONBAR_HEIGHT + FRAME_BORDER
    };
    let width = (window.work_extent.max_x - window.work_extent.min_x)
        .min(DESKTOP_WIDTH - min_x - window_outer_extra_x(window))
        .max(1);
    let height = (window.work_extent.max_y - window.work_extent.min_y)
        .min(DESKTOP_HEIGHT - min_y - window_outer_top_extra_y(window))
        .max(1);
    WorkArea {
        min_x,
        min_y,
        max_x: min_x + width,
        max_y: min_y + height,
    }
}

fn toggle_window_size(state: &mut WimpState, handle: u32, select: bool) {
    let Some(snapshot) = state.windows.get(&handle).cloned() else {
        return;
    };
    let (area, scroll, behind, is_maximized, restore_behind) = if snapshot.maximized {
        let restore = valid_restore_depth(state, snapshot.restore_behind);
        (
            snapshot.last_user_area,
            snapshot.last_user_scroll,
            restore,
            false,
            restore,
        )
    } else {
        let current_behind = window_behind(state, handle);
        let area = maximum_window_area(&snapshot);
        (
            area,
            (snapshot.work_extent.min_x, snapshot.work_extent.max_y),
            if select { -1 } else { current_behind },
            true,
            current_behind,
        )
    };
    let event = open_request_event(&snapshot, area, scroll, behind, 0, 0);
    let Some(window) = state.windows.get_mut(&handle) else {
        return;
    };
    if !snapshot.maximized {
        window.last_user_area = snapshot.work_area;
        window.last_user_scroll = (snapshot.scroll_x, snapshot.scroll_y);
    }
    window.restore_behind = restore_behind;
    window.maximized = is_maximized;
    window.toggle_request_pending = true;
    window.preview_area = Some(area);
    window.preview_scroll = Some(scroll);
    let owner = window.owner_task_handle;
    let _ = enqueue_for_owner(state, owner, event);
    if select && is_maximized {
        bring_to_front(state, handle);
    }
}

fn request_or_apply_scroll(
    state: &mut WimpState,
    handle: u32,
    buttons: u32,
    scroll_x_direction: i32,
    scroll_y_direction: i32,
) {
    let Some(snapshot) = state.windows.get(&handle).cloned() else {
        return;
    };
    let mut x_direction = scroll_x_direction;
    let mut y_direction = scroll_y_direction;
    if buttons & 1 != 0 {
        x_direction = -x_direction;
        y_direction = -y_direction;
    }
    let mut scroll = (snapshot.scroll_x, snapshot.scroll_y);
    if snapshot.flags & ((1 << 8) | (1 << 9)) != 0 {
        let event = open_request_event(
            &snapshot,
            snapshot.work_area,
            scroll,
            window_behind(state, handle),
            x_direction,
            y_direction,
        );
        let _ = enqueue_for_owner(state, snapshot.owner_task_handle, event);
        return;
    }

    let visible_width = snapshot.work_area.max_x - snapshot.work_area.min_x;
    let visible_height = snapshot.work_area.max_y - snapshot.work_area.min_y;
    let x_step = if x_direction.abs() == 1 {
        SCROLL_ARROW_STEP
    } else {
        page_scroll_step(visible_width)
    };
    let y_step = if y_direction.abs() == 1 {
        SCROLL_ARROW_STEP
    } else {
        page_scroll_step(visible_height)
    };
    let min_x = snapshot.work_extent.min_x;
    let max_x = (snapshot.work_extent.max_x - visible_width).max(min_x);
    let min_y = (snapshot.work_extent.min_y + visible_height).min(snapshot.work_extent.max_y);
    let max_y = snapshot.work_extent.max_y;
    scroll.0 = scroll
        .0
        .saturating_add(x_direction.signum() * x_step)
        .clamp(min_x, max_x);
    scroll.1 = scroll
        .1
        .saturating_add(y_direction.signum() * y_step)
        .clamp(min_y, max_y);
    if scroll == (snapshot.scroll_x, snapshot.scroll_y) {
        return;
    }
    let event = open_request_event(
        &snapshot,
        snapshot.work_area,
        scroll,
        window_behind(state, handle),
        0,
        0,
    );
    if let Some(window) = state.windows.get_mut(&handle) {
        window.preview_area = Some(window.work_area);
        window.preview_scroll = Some(scroll);
    }
    let _ = enqueue_for_owner(state, snapshot.owner_task_handle, event);
}

fn page_scroll_step(visible_size: i32) -> i32 {
    // Keep one third of the viewport visible after a page move so adjacent
    // Filer rows stay reachable instead of jumping over a row at the boundary.
    (visible_size.saturating_mul(2) / 3).max(SCROLL_ARROW_STEP)
}

fn remove_task(state: &mut WimpState, guest_task_id: u64) {
    let Some(handle) = state.guest_to_task.remove(&guest_task_id) else {
        return;
    };
    if state
        .active_menu
        .as_ref()
        .is_some_and(|menu| menu.owner == handle)
    {
        state.active_menu = None;
    }
    if state.system_menu_owner == Some(handle) {
        state.system_menu_owner = None;
    }
    state.tasks.remove(&handle);
    state
        .icons
        .retain(|icon| icon.owner_task_id != guest_task_id);
    state
        .pending_launches
        .retain(|launch| launch.task_id != guest_task_id);
    state
        .windows
        .retain(|_, window| window.owner_task_handle != handle);
    state
        .window_icons
        .retain(|icon| icon.owner_task_id != guest_task_id);
    state
        .stacking
        .retain(|window| state.windows.contains_key(window));
    if state
        .keyboard_focus
        .is_some_and(|focused| !state.windows.contains_key(&focused))
    {
        state.keyboard_focus = state.stacking.first().copied();
    }
    queue_visible_invalid_redraws(state);
}

fn close_window_in_state(state: &mut WimpState, handle: u32) {
    if let Some(window) = state.windows.get_mut(&handle) {
        window.open = false;
        if window.console_window {
            if let Some(owner) = state.tasks.get_mut(&window.owner_task_handle) {
                owner.input.take(); // unblock an idle host console's ReadLine
            }
        }
    }
    state.stacking.retain(|item| *item != handle);
    if state.keyboard_focus == Some(handle) {
        state.keyboard_focus = state.stacking.first().copied();
    }
    queue_visible_invalid_redraws(state);
}

fn queue_window_redraw(state: &mut WimpState, handle: u32) {
    let Some(window) = state.windows.get(&handle) else {
        return;
    };
    if !window.open
        || window.flags & (1 << 4) != 0
        || window.invalid_regions.is_empty()
        || window
            .invalid_regions
            .iter()
            .all(|region| visible_redraw_rectangles(state, handle, *region).is_empty())
    {
        return;
    }
    let owner = window.owner_task_handle;
    let already_queued = state.tasks.get(&owner).is_some_and(|task| {
        task.events
            .iter()
            .any(|event| event.reason == 1 && read_word(&event.block, 0) == handle)
    });
    if !already_queued {
        enqueue_for_owner(state, owner, event_with_word(1, 0, handle));
    }
}

fn queue_visible_invalid_redraws(state: &mut WimpState) {
    let handles = state.stacking.clone();
    for handle in handles {
        queue_window_redraw(state, handle);
    }
}

fn add_invalid_region(window: &mut WimpWindow, region: WorkArea) {
    if region.max_x <= region.min_x || region.max_y <= region.min_y {
        return;
    }
    let mut pending = vec![region];
    for existing in &window.invalid_regions {
        pending = pending
            .into_iter()
            .flat_map(|part| subtract_work_area(part, *existing))
            .collect();
        if pending.is_empty() {
            return;
        }
    }
    window.invalid_regions.extend(pending);
}

fn subtract_invalid_region(window: &mut WimpWindow, drawn: WorkArea) {
    window.invalid_regions = window
        .invalid_regions
        .drain(..)
        .flat_map(|region| subtract_work_area(region, drawn))
        .collect();
}

fn intersect_work_area(a: WorkArea, b: WorkArea) -> Option<WorkArea> {
    let result = WorkArea {
        min_x: a.min_x.max(b.min_x),
        min_y: a.min_y.max(b.min_y),
        max_x: a.max_x.min(b.max_x),
        max_y: a.max_y.min(b.max_y),
    };
    (result.min_x < result.max_x && result.min_y < result.max_y).then_some(result)
}

fn subtract_work_area(rect: WorkArea, cut: WorkArea) -> Vec<WorkArea> {
    let Some(intersection) = intersect_work_area(rect, cut) else {
        return vec![rect];
    };
    let mut output = Vec::with_capacity(4);
    let mut add = |min_x, min_y, max_x, max_y| {
        if min_x < max_x && min_y < max_y {
            output.push(WorkArea {
                min_x,
                min_y,
                max_x,
                max_y,
            });
        }
    };
    add(rect.min_x, rect.min_y, rect.max_x, intersection.min_y);
    add(rect.min_x, intersection.max_y, rect.max_x, rect.max_y);
    add(
        rect.min_x,
        intersection.min_y,
        intersection.min_x,
        intersection.max_y,
    );
    add(
        intersection.max_x,
        intersection.min_y,
        rect.max_x,
        intersection.max_y,
    );
    output
}

fn visible_screen_work_area(window: &WimpWindow) -> DesktopRect {
    DesktopRect {
        min_x: window.work_area.min_x,
        min_y: window.work_area.min_y,
        max_x: window.work_area.max_x,
        max_y: window.work_area.max_y,
    }
}

fn work_to_screen(window: &WimpWindow, work: WorkArea) -> DesktopRect {
    DesktopRect {
        min_x: window.work_area.min_x + work.min_x - window.scroll_x,
        min_y: window.work_area.max_y + work.min_y - window.scroll_y,
        max_x: window.work_area.min_x + work.max_x - window.scroll_x,
        max_y: window.work_area.max_y + work.max_y - window.scroll_y,
    }
}

fn screen_rect_to_work(window: &WimpWindow, screen: DesktopRect) -> WorkArea {
    WorkArea {
        min_x: screen.min_x - window.work_area.min_x + window.scroll_x,
        min_y: screen.min_y - window.work_area.max_y + window.scroll_y,
        max_x: screen.max_x - window.work_area.min_x + window.scroll_x,
        max_y: screen.max_y - window.work_area.max_y + window.scroll_y,
    }
}

fn intersect_desktop_rect(a: DesktopRect, b: DesktopRect) -> Option<DesktopRect> {
    let result = DesktopRect {
        min_x: a.min_x.max(b.min_x),
        min_y: a.min_y.max(b.min_y),
        max_x: a.max_x.min(b.max_x),
        max_y: a.max_y.min(b.max_y),
    };
    (!result.is_empty()).then_some(result)
}

fn subtract_desktop_rect(rect: DesktopRect, cut: DesktopRect) -> Vec<DesktopRect> {
    let Some(intersection) = intersect_desktop_rect(rect, cut) else {
        return vec![rect];
    };
    let mut output = Vec::with_capacity(4);
    let mut add = |min_x, min_y, max_x, max_y| {
        if min_x < max_x && min_y < max_y {
            output.push(DesktopRect {
                min_x,
                min_y,
                max_x,
                max_y,
            });
        }
    };
    add(rect.min_x, rect.min_y, rect.max_x, intersection.min_y);
    add(rect.min_x, intersection.max_y, rect.max_x, rect.max_y);
    add(
        rect.min_x,
        intersection.min_y,
        intersection.min_x,
        intersection.max_y,
    );
    add(
        intersection.max_x,
        intersection.min_y,
        rect.max_x,
        intersection.max_y,
    );
    output
}

fn window_outer_rect(window: &WimpWindow) -> DesktopRect {
    WindowFurnitureLayout::new(
        window.work_area,
        window.work_extent,
        window.scroll_x,
        window.scroll_y,
        window.has_back_icon,
        window.has_title,
        window.closable,
        window.has_toggle_size_icon,
        window.has_vertical_scrollbar,
        window.resizable,
    )
    .outer
}

fn visible_redraw_rectangles(
    state: &WimpState,
    handle: u32,
    requested: WorkArea,
) -> Vec<RedrawRectangle> {
    let Some(window) = state.windows.get(&handle).filter(|window| window.open) else {
        return Vec::new();
    };
    let area = window.work_area;
    let visible_work = WorkArea {
        min_x: window.scroll_x,
        min_y: window.scroll_y - (area.max_y - area.min_y),
        max_x: window.scroll_x + (area.max_x - area.min_x),
        max_y: window.scroll_y,
    };
    let Some(visible) = intersect_work_area(requested, visible_work)
        .and_then(|visible| intersect_work_area(visible, window.work_extent))
    else {
        return Vec::new();
    };
    let screen = work_to_screen(window, visible);
    let Some(stack_index) = state.stacking.iter().position(|stacked| *stacked == handle) else {
        return Vec::new();
    };
    let mut visible_rects = vec![screen];
    for front_handle in state.stacking.iter().take(stack_index) {
        let Some(front) = state.windows.get(front_handle).filter(|front| front.open) else {
            continue;
        };
        let obstacle = window_outer_rect(front);
        visible_rects = visible_rects
            .into_iter()
            .flat_map(|part| subtract_desktop_rect(part, obstacle))
            .collect();
        if visible_rects.is_empty() {
            break;
        }
    }
    visible_rects
        .into_iter()
        .map(|screen| RedrawRectangle {
            work: screen_rect_to_work(window, screen),
            screen,
        })
        .collect()
}

fn write_redraw_block(block: &mut [u8], window: &WimpWindow, rectangle: RedrawRectangle) {
    put_word(block, 0, window.handle);
    put_word(block, 4, window.work_area.min_x as u32);
    put_word(block, 8, window.work_area.min_y as u32);
    put_word(block, 12, window.work_area.max_x as u32);
    put_word(block, 16, window.work_area.max_y as u32);
    put_word(block, 20, window.scroll_x as u32);
    put_word(block, 24, window.scroll_y as u32);
    put_word(block, 28, rectangle.screen.min_x as u32);
    put_word(block, 32, rectangle.screen.min_y as u32);
    put_word(block, 36, rectangle.screen.max_x as u32);
    put_word(block, 40, rectangle.screen.max_y as u32);
}

fn layout_icon_bar(icons: &[WimpIcon], windows: &HashMap<u32, WimpWindow>) -> Vec<DesktopIcon> {
    let mut output = Vec::with_capacity(icons.len());
    let mut left = 8;
    // Keep the single OS control clear of application icons.
    let mut right = DESKTOP_WIDTH - 8 - ICONBAR_SYSTEM_AREA_OS;
    for icon in icons
        .iter()
        .filter(|icon| icon.side == IconBarSide::Devices)
    {
        let width = icon.width.clamp(120, DESKTOP_WIDTH / 2);
        let bounds = DesktopRect {
            min_x: left,
            min_y: 4,
            max_x: left + width,
            max_y: DESKTOP_ICONBAR_HEIGHT - 4,
        };
        left = bounds.max_x + 4;
        output.push(DesktopIcon {
            handle: icon.handle,
            owner_task_id: icon.owner_task_id,
            owner_task_handle: icon.owner_task_handle,
            label: icon.label.clone(),
            sprite_name: icon.sprite_name.clone(),
            high_resolution_image: icon.high_resolution_image.clone(),
            bounds,
            side: icon.side,
            button_type: icon.button_type,
            activate_task_id: icon.activate_task_id,
        });
    }
    for icon in icons.iter().rev().filter(|icon| {
        icon.side == IconBarSide::Applications
            && icon.activate_task_id.is_none_or(|task_id| {
                windows
                    .values()
                    .any(|window| window.owner_task_id == task_id && window.open)
            })
    }) {
        let width = icon.width.clamp(120, DESKTOP_WIDTH / 2);
        let bounds = DesktopRect {
            min_x: right - width,
            min_y: 4,
            max_x: right,
            max_y: DESKTOP_ICONBAR_HEIGHT - 4,
        };
        right = bounds.min_x - 4;
        output.push(DesktopIcon {
            handle: icon.handle,
            owner_task_id: icon.owner_task_id,
            owner_task_handle: icon.owner_task_handle,
            label: icon.label.clone(),
            sprite_name: icon.sprite_name.clone(),
            high_resolution_image: icon.high_resolution_image.clone(),
            bounds,
            side: icon.side,
            button_type: icon.button_type,
            activate_task_id: icon.activate_task_id,
        });
    }
    output
}

fn parse_menu(
    task: &Task,
    address: u32,
    inherited_reverse: bool,
    ancestors: &mut Vec<u32>,
    depth: usize,
    total_items: &mut usize,
) -> Result<WimpMenu, RuntimeError> {
    if depth >= MAX_MENU_DEPTH {
        return Err(program_error(
            "Wimp menu tree exceeds the hosted depth limit",
        ));
    }
    if address < 0x8000 || address & 3 != 0 {
        return Err(program_error(
            "Wimp_CreateMenu requires an aligned menu block pointer",
        ));
    }
    if ancestors.contains(&address) {
        return Err(program_error("Wimp_CreateMenu menu tree contains a cycle"));
    }
    ancestors.push(address);
    let header = task.memory.read_bytes(address, MENU_HEADER_SIZE)?;
    let first_row_flags = read_word(
        &task.memory.read_bytes(
            address
                .checked_add(MENU_HEADER_SIZE as u32)
                .ok_or(crate::memory::MemoryError::AddressOverflow)?,
            4,
        )?,
        0,
    );
    let indirect_title = first_row_flags & (1 << 8) != 0;
    let mut title = if indirect_title {
        let text_address = read_word(&header, 0);
        let length = read_word(&header, 8) as usize;
        if length == 0 || length > 4096 {
            return Err(program_error("Wimp menu title buffer length is invalid"));
        }
        read_control_string(task, text_address, length)?
    } else {
        control_terminated(&header[..12])
    };
    let reverse = inherited_reverse || title.starts_with('\\');
    if title.starts_with('\\') {
        title.remove(0);
    }
    let width = read_word(&header, 16) as i32;
    let row_height = read_word(&header, 20) as i32;
    let gap = read_word(&header, 24) as i32;
    if width <= 0 || width > 4096 || row_height <= 0 || row_height > 4096 || gap < 0 || gap > 256 {
        return Err(program_error("Wimp_CreateMenu has invalid item dimensions"));
    }
    let mut rows = Vec::new();
    let mut terminated = false;
    for index in 0..MAX_MENU_ITEMS {
        *total_items += 1;
        if *total_items > MAX_MENU_TREE_ITEMS {
            return Err(program_error(
                "Wimp menu tree exceeds the hosted item limit",
            ));
        }
        let row_offset = MENU_HEADER_SIZE
            .checked_add(index * MENU_ITEM_SIZE)
            .ok_or_else(|| program_error("Wimp menu block size overflowed"))?;
        let row_address = address
            .checked_add(row_offset as u32)
            .ok_or(crate::memory::MemoryError::AddressOverflow)?;
        let block = task.memory.read_bytes(row_address, MENU_ITEM_SIZE)?;
        let flags = read_word(&block, 0);
        let allowed_flags = 1 | 2 | 4 | 8 | 16 | 128 | if index == 0 { 1 << 8 } else { 0 };
        if flags & !allowed_flags != 0 {
            return Err(program_error("Wimp menu item contains unsupported flags"));
        }
        if flags & (1 << 2) != 0 {
            return Err(program_error(
                "writable Wimp menu entries are not implemented",
            ));
        }
        if flags & (1 << 3) != 0 {
            return Err(program_error(
                "lazy Wimp MenuWarning submenus are not implemented",
            ));
        }
        let icon_flags = read_word(&block, 8);
        if icon_flags & 1 == 0 {
            return Err(program_error("hosted Wimp menus require text items"));
        }
        let mut icon_block = [0u8; 36];
        put_word(&mut icon_block, 20, icon_flags);
        icon_block[24..36].copy_from_slice(&block[12..24]);
        let (label, _) = read_menu_icon_contents(task, &icon_block, icon_flags)?;
        let submenu_pointer = read_word(&block, 4);
        let submenu = if submenu_pointer == u32::MAX {
            None
        } else if submenu_pointer >= 0x8000 {
            Some(Box::new(parse_menu(
                task,
                submenu_pointer,
                reverse,
                ancestors,
                depth + 1,
                total_items,
            )?))
        } else {
            return Err(program_error(
                "Wimp menu dialogue-box window handles are not implemented",
            ));
        };
        rows.push(WimpMenuRow {
            label,
            flags,
            icon_flags,
            submenu,
        });
        if flags & (1 << 7) != 0 {
            terminated = true;
            break;
        }
    }
    ancestors.pop();
    if !terminated {
        return Err(program_error(
            "Wimp menu has no last-item marker within 64 rows",
        ));
    }
    Ok(WimpMenu {
        address,
        title,
        title_foreground: header[12],
        title_background: header[13],
        work_foreground: header[14],
        work_background: header[15],
        width,
        row_height,
        gap,
        rows,
        reverse,
    })
}

fn read_menu_icon_contents(
    task: &Task,
    block: &[u8],
    flags: u32,
) -> Result<(String, Option<String>), RuntimeError> {
    let indirect = flags & (1 << 8) != 0;
    if indirect {
        let address = read_word(block, 24);
        let length = read_word(block, 32) as usize;
        if length == 0 || length > 4096 {
            return Err(program_error("Wimp menu text buffer length is invalid"));
        }
        Ok((read_control_string(task, address, length)?, None))
    } else {
        let label = control_terminated(&block[24..36]);
        Ok((label, None))
    }
}

fn icon_label(label: &str) -> String {
    label
        .chars()
        .filter(|character| character.is_ascii() && !character.is_ascii_control())
        .take(32)
        .collect()
}

fn read_high_resolution_icon_image(
    task: &Task,
    block: &[u8],
) -> Result<Option<Arc<DesktopIconImage>>, RuntimeError> {
    let version = read_word(block, 36);
    if version != 1 {
        return Err(program_error(
            "Wimp_CreateIconEx supports extension block version 1",
        ));
    }
    let address = read_word(block, 40);
    let width = read_word(block, 44);
    let height = read_word(block, 48);
    let source_scale = read_word(block, 52);
    if address == 0 && width == 0 && height == 0 {
        if source_scale != 0 && source_scale != 2 {
            return Err(program_error(
                "Wimp_CreateIconEx high-resolution scale must be 2",
            ));
        }
        return Ok(None);
    }
    if address == 0 || width == 0 || height == 0 || source_scale != 2 {
        return Err(program_error(
            "Wimp_CreateIconEx needs a 2× RGBA image address and non-zero dimensions",
        ));
    }
    if width > 512 || height > 512 {
        return Err(program_error(
            "Wimp_CreateIconEx image dimensions exceed 512×512 pixels",
        ));
    }
    let byte_count = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| program_error("Wimp_CreateIconEx image size overflowed"))?;
    let bytes = task.memory.read_bytes(address, byte_count)?;
    let rgba = bytes
        .chunks_exact(4)
        .map(|channels| [channels[0], channels[1], channels[2], channels[3]])
        .collect();
    Ok(Some(Arc::new(DesktopIconImage {
        width,
        height,
        rgba,
    })))
}

fn read_icon_contents(
    task: &Task,
    block: &[u8],
    flags: u32,
) -> Result<(String, Option<String>), RuntimeError> {
    let has_text = flags & 1 != 0;
    let has_sprite = flags & (1 << 1) != 0;
    let indirected = flags & (1 << 8) != 0;
    let (text, sprite_name) = if indirected {
        let text_address = read_word(block, 24);
        let validation_address = read_word(block, 28);
        let buffer_length = read_word(block, 32) as usize;
        if buffer_length == 0 || buffer_length > 4096 {
            return Err(program_error("Wimp icon buffer length is invalid"));
        }
        let text = if has_text {
            read_control_string(task, text_address, buffer_length)?
        } else {
            String::new()
        };
        let sprite_name = if has_sprite {
            let validation = read_control_string(task, validation_address, 256)?;
            validation
                .split(';')
                .find_map(|command| command.strip_prefix('S'))
                .and_then(|names| names.split(',').next())
                .filter(|name| !name.is_empty() && name.len() <= 12)
                .map(str::to_string)
        } else {
            None
        };
        (text, sprite_name)
    } else {
        let data = &block[24..36];
        let direct = control_terminated(data);
        let text = if has_text {
            direct.clone()
        } else {
            String::new()
        };
        let sprite_name = has_sprite.then_some(direct);
        (text, sprite_name)
    };

    if has_text && text.is_empty() {
        return Err(program_error("Wimp icon text must not be empty"));
    }
    if has_sprite {
        let Some(sprite_name) = &sprite_name else {
            return Err(program_error("Wimp sprite icon has no sprite name"));
        };
        static SYSTEM_SPRITES: OnceLock<RiscOsSpriteFile> = OnceLock::new();
        let sprites = SYSTEM_SPRITES.get_or_init(|| {
            builtin_sprite_set(SpriteSet::Sprites22)
                .expect("pinned RISC OS 3.71 system sprites parse and verify")
        });
        // Local BASIC64 type has modern art and a classic BASIC fallback.
        if sprite_name != "file_064" && sprites.get(sprite_name).is_none() {
            return Err(program_error(format!(
                "Wimp sprite {sprite_name:?} is not in the hosted RISC OS 3.71 sprite pool"
            )));
        }
    }
    Ok((text, sprite_name))
}

fn insert_console_window(
    state: &mut WimpState,
    owner_task_handle: u32,
    owner_task_id: u64,
    title: &str,
) -> Result<(), RuntimeError> {
    let handle = allocate_handle(&mut state.next_window_handle)?;
    let area = WorkArea {
        min_x: 48,
        min_y: DESKTOP_HEIGHT - 72 - 1024,
        max_x: 48 + 1280,
        max_y: DESKTOP_HEIGHT - 72,
    };
    let extent = WorkArea {
        min_x: 0,
        min_y: -1024,
        max_x: 1280,
        max_y: 0,
    };
    state.windows.insert(
        handle,
        WimpWindow {
            handle,
            owner_task_handle,
            owner_task_id,
            title: if matches!(title, "*Commands" | "BASIC window") {
                title.to_owned()
            } else {
                format!("BASIC Output: {}", icon_label(title))
            },
            flags: 0xB600_0002,
            work_area_flags: 3 << 12,
            work_area_background: 0,
            work_area: area,
            work_extent: extent,
            invalid_regions: Vec::new(),
            min_width: 48,
            min_height: 176,
            scroll_x: 0,
            scroll_y: 0,
            has_title: true,
            has_back_icon: true,
            has_vertical_scrollbar: true,
            has_toggle_size_icon: true,
            closable: true,
            movable: true,
            resizable: true,
            open: true,
            has_opened: true,
            preview_area: None,
            preview_scroll: None,
            last_user_area: area,
            last_user_scroll: (0, 0),
            maximized: false,
            toggle_request_pending: false,
            restore_behind: -1,
            console_window: true,
        },
    );
    bring_to_front(state, handle);
    state.keyboard_focus = Some(handle);
    Ok(())
}

fn bring_to_front(state: &mut WimpState, handle: u32) {
    state.stacking.retain(|item| *item != handle);
    state.stacking.insert(0, handle);
}

fn stacking_after_open(
    state: &WimpState,
    handle: u32,
    behind: i32,
) -> Result<Vec<u32>, RuntimeError> {
    let mut stacking = state
        .stacking
        .iter()
        .copied()
        .filter(|existing| *existing != handle)
        .collect::<Vec<_>>();
    match behind {
        -1 => stacking.insert(0, handle),
        -2 => stacking.push(handle),
        target if target > 0 => {
            let target = target as u32;
            let Some(index) = stacking.iter().position(|existing| *existing == target) else {
                return Err(program_error(
                    "Wimp_OpenWindow stack handle is unknown or closed",
                ));
            };
            stacking.insert(index + 1, handle);
        }
        _ => {
            return Err(program_error(
                "hosted Wimp_OpenWindow does not support this stack position",
            ));
        }
    }
    Ok(stacking)
}

fn allocate_handle(next: &mut u32) -> Result<u32, RuntimeError> {
    let handle = *next;
    if handle == 0 || handle > i32::MAX as u32 {
        return Err(program_error("Wimp handle space exhausted"));
    }
    *next = handle
        .checked_add(1)
        .ok_or_else(|| program_error("Wimp handle space exhausted"))?;
    Ok(handle)
}

fn read_control_string(task: &Task, address: u32, limit: usize) -> Result<String, RuntimeError> {
    let mut bytes = Vec::new();
    for offset in 0..limit {
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
    Err(program_error(
        "Wimp task description is not control-terminated",
    ))
}

fn read_word(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("word in block"))
}

fn read_halfword(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("halfword in block"),
    )
}

fn put_word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn control_terminated(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte <= 31)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn program_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::Program(message.into())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    const DESCRIPTION: u32 = 0x1100;
    const WINDOW_BLOCK: u32 = 0x1200;
    const OPEN_BLOCK: u32 = 0x1300;
    const POLL_BLOCK: u32 = 0x1400;
    const STATE_BLOCK: u32 = 0x1500;
    const TASK_MAGIC: u32 = 0x4B53_4154;
    const MODERN_WINDOW_FLAGS: u32 = 0xB600_0002;

    fn new_server() -> Arc<WimpServer> {
        let (updates, _receiver) = mpsc::channel();
        WimpServer::new(updates)
    }

    fn initialize(server: &WimpServer, task: &mut Task) -> u32 {
        task.memory
            .write_bytes(DESCRIPTION, b"test task\r")
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[0] = 310;
        context.registers[1] = TASK_MAGIC;
        context.registers[2] = DESCRIPTION;
        server
            .dispatch(WIMP_INITIALISE, task, &mut context)
            .unwrap();
        assert_eq!(context.registers[0], WIMP_VERSION);
        context.registers[1]
    }

    fn create_window(
        server: &WimpServer,
        task: &mut Task,
        title: &str,
        area: WorkArea,
        extent: WorkArea,
        button_type: u32,
    ) -> u32 {
        create_window_with_background(server, task, title, area, extent, button_type, 0)
    }

    fn create_window_with_background(
        server: &WimpServer,
        task: &mut Task,
        title: &str,
        area: WorkArea,
        extent: WorkArea,
        button_type: u32,
        background: u8,
    ) -> u32 {
        let mut block = [0u8; WINDOW_BLOCK_SIZE];
        put_word(&mut block, 0, area.min_x as u32);
        put_word(&mut block, 4, area.min_y as u32);
        put_word(&mut block, 8, area.max_x as u32);
        put_word(&mut block, 12, area.max_y as u32);
        put_word(&mut block, 16, 0);
        put_word(&mut block, 20, 0);
        put_word(&mut block, 28, MODERN_WINDOW_FLAGS);
        put_word(&mut block, 40, extent.min_x as u32);
        put_word(&mut block, 44, extent.min_y as u32);
        put_word(&mut block, 48, extent.max_x as u32);
        put_word(&mut block, 52, extent.max_y as u32);
        put_word(&mut block, 56, 1);
        put_word(&mut block, 60, button_type << 12);
        block[35] = background;
        block[68..70].copy_from_slice(&48u16.to_le_bytes());
        block[70..72].copy_from_slice(&48u16.to_le_bytes());
        let title_bytes = title.as_bytes();
        let title_length = title_bytes.len().min(11);
        block[72..72 + title_length].copy_from_slice(&title_bytes[..title_length]);
        put_word(&mut block, 84, 0);
        task.memory.write_bytes(WINDOW_BLOCK, &block).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = WINDOW_BLOCK;
        server
            .dispatch(WIMP_CREATE_WINDOW, task, &mut context)
            .unwrap();
        context.registers[0]
    }

    fn open_window(
        server: &WimpServer,
        task: &mut Task,
        handle: u32,
        area: WorkArea,
        scroll_x: i32,
        scroll_y: i32,
        behind: i32,
    ) -> Result<(), RuntimeError> {
        let mut block = [0u8; OPEN_BLOCK_SIZE];
        put_word(&mut block, 0, handle);
        put_word(&mut block, 4, area.min_x as u32);
        put_word(&mut block, 8, area.min_y as u32);
        put_word(&mut block, 12, area.max_x as u32);
        put_word(&mut block, 16, area.max_y as u32);
        put_word(&mut block, 20, scroll_x as u32);
        put_word(&mut block, 24, scroll_y as u32);
        put_word(&mut block, 28, behind as u32);
        task.memory.write_bytes(OPEN_BLOCK, &block).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = OPEN_BLOCK;
        server.dispatch(WIMP_OPEN_WINDOW, task, &mut context)
    }

    fn poll(
        server: &WimpServer,
        task: &mut Task,
        mask: u32,
        address: u32,
    ) -> Result<(u32, Vec<u8>), RuntimeError> {
        let mut context = SwiContext::default();
        context.registers[0] = mask;
        context.registers[1] = address;
        server.dispatch(WIMP_POLL, task, &mut context)?;
        Ok((
            context.registers[0],
            task.memory.read_bytes(address, POLL_BLOCK_SIZE)?,
        ))
    }

    fn complete_redraw(server: &WimpServer, task: &mut Task, address: u32) -> u32 {
        let (reason, event) = poll(server, task, 0, address).unwrap();
        assert_eq!(reason, 1);
        let handle = read_word(&event, 0);
        let mut context = SwiContext::default();
        context.registers[1] = address;
        server
            .dispatch(WIMP_REDRAW_WINDOW, task, &mut context)
            .unwrap();
        while context.registers[0] != 0 {
            context.registers[1] = address;
            server
                .dispatch(WIMP_GET_RECTANGLE, task, &mut context)
                .unwrap();
        }
        handle
    }

    fn read_task_word(task: &Task, address: u32, offset: usize) -> u32 {
        read_word(
            &task.memory.read_bytes(address + offset as u32, 4).unwrap(),
            0,
        )
    }

    #[test]
    fn console_geometry_matches_content_and_ignores_repeated_frames() {
        let server = new_server();
        server.task_started(901, "*Commands").unwrap();
        let window = server.desktop_windows()[0].clone();
        assert_eq!(window.work_area.max_x - window.work_area.min_x, 1280);
        assert_eq!(window.work_area.max_y - window.work_area.min_y, 1024);
        assert_eq!(window.work_extent.max_y, window.scroll_y);
        let mode = crate::graphics::GraphicsService::default().snapshot().mode;
        server.sync_console_mode(901, None, mode);
        assert_eq!(server.desktop_windows()[0].work_area, window.work_area);
        let grip = desktop_window_furniture(&window).adjust_size_icon.unwrap();
        let x = (grip.min_x + grip.max_x) / 2;
        let y = (grip.min_y + grip.max_y) / 2;
        let drag = server.mouse_down(x, y, 4).unwrap();
        server.drag_to(drag, x - 100, y + 100);
        server.finish_drag(drag);
        let resized = server.desktop_windows()[0].work_area;
        server.sync_console_mode(901, None, mode);
        assert_eq!(server.desktop_windows()[0].work_area, resized);
    }

    #[test]
    fn console_resize_commits_without_any_guest_poll_or_output() {
        let server = new_server();
        server.task_started(900, "Busy BASIC").unwrap();
        let original = server.desktop_windows()[0].work_area;
        for (dx, dy) in [(-200, 100), (200, -100)] {
            let window = server.desktop_windows()[0].clone();
            let grip = desktop_window_furniture(&window).adjust_size_icon.unwrap();
            let x = (grip.min_x + grip.max_x) / 2;
            let y = (grip.min_y + grip.max_y) / 2;
            let drag = server.mouse_down(x, y, 4).unwrap();
            server.drag_to(drag, x + dx, y + dy);
            let preview = server.desktop_windows()[0].preview_area.unwrap();
            server.finish_drag(drag);
            let resized = &server.desktop_windows()[0];
            assert_eq!(resized.work_area, preview);
            assert_eq!(resized.preview_area, None);
        }
        assert_eq!(server.desktop_windows()[0].work_area, original);
        let state = server.state.lock().unwrap();
        assert!(
            state
                .tasks
                .values()
                .all(|task| !task.events.iter().any(|event| event.reason == 2))
        );
    }

    #[test]
    fn window_indirected_title_uses_checked_guest_memory() {
        let server = new_server();
        let mut task = Task::new(42);
        initialize(&server, &mut task);
        create_window(&server, &mut task, "Seed", AREA, EXTENT, 0);
        let mut block = task
            .memory
            .read_bytes(WINDOW_BLOCK, WINDOW_BLOCK_SIZE)
            .unwrap();
        let title_address = 0x1800;
        task.memory
            .write_bytes(title_address, b"HostFS:$.Examples.Deep\0")
            .unwrap();
        put_word(&mut block, 56, 257);
        put_word(&mut block, 72, title_address);
        put_word(&mut block, 80, 64);
        task.memory.write_bytes(WINDOW_BLOCK, &block).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = WINDOW_BLOCK;
        server
            .dispatch(WIMP_CREATE_WINDOW, &mut task, &mut context)
            .unwrap();
        let handle = context.registers[0];
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();
        assert_eq!(server.desktop_windows()[0].title, "HostFS:$.Examples.Deep");
        put_word(&mut block, 72, u32::MAX);
        task.memory.write_bytes(WINDOW_BLOCK, &block).unwrap();
        context.registers[1] = WINDOW_BLOCK;
        assert!(
            server
                .dispatch(WIMP_CREATE_WINDOW, &mut task, &mut context)
                .is_err()
        );
    }

    #[test]
    fn redraw_update_and_force_redraw_follow_standard_rectangle_blocks() {
        let server = new_server();
        let mut task = Task::new(42);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Redraw", AREA, EXTENT, 0);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();

        let (reason, event) = poll(&server, &mut task, 0, POLL_BLOCK).unwrap();
        assert_eq!(reason, 1);
        assert_eq!(read_word(&event, 0), handle);
        let mut redraw = [0_u8; 44];
        put_word(&mut redraw, 0, handle);
        task.memory.write_bytes(STATE_BLOCK, &redraw).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK;
        server
            .dispatch(WIMP_REDRAW_WINDOW, &mut task, &mut context)
            .unwrap();
        assert_eq!(context.registers[0], 1);
        assert_eq!(server.current_redraw_background_colour(42), Some(7));
        let returned = task.memory.read_bytes(STATE_BLOCK, 44).unwrap();
        assert_eq!(read_word(&returned, 4) as i32, AREA.min_x);
        assert_eq!(read_word(&returned, 16) as i32, AREA.max_y);
        assert_eq!(read_word(&returned, 28) as i32, AREA.min_x);
        assert_eq!(read_word(&returned, 32) as i32, AREA.min_y);
        assert_eq!(read_word(&returned, 36) as i32, AREA.max_x);
        assert_eq!(read_word(&returned, 40) as i32, AREA.max_y);
        assert!(server.current_graphics_clip(42).is_some());
        assert!(server.current_redraw_clears_background(42));

        let mut next = [0_u8; 44];
        put_word(&mut next, 0, handle);
        task.memory.write_bytes(STATE_BLOCK, &next).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK;
        server
            .dispatch(WIMP_GET_RECTANGLE, &mut task, &mut context)
            .unwrap();
        assert_eq!(context.registers[0], 0);
        assert!(server.current_graphics_clip(42).is_none());

        let mut update = [0_u8; 44];
        put_word(&mut update, 0, handle);
        put_word(&mut update, 4, 40);
        put_word(&mut update, 8, (-80_i32) as u32);
        put_word(&mut update, 12, 100);
        put_word(&mut update, 16, (-20_i32) as u32);
        task.memory.write_bytes(STATE_BLOCK, &update).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK;
        server
            .dispatch(WIMP_UPDATE_WINDOW, &mut task, &mut context)
            .unwrap();
        assert_eq!(context.registers[0], 1);
        let returned = task.memory.read_bytes(STATE_BLOCK, 44).unwrap();
        assert_eq!(read_word(&returned, 28) as i32, AREA.min_x + 40);
        assert_eq!(read_word(&returned, 32) as i32, AREA.max_y - 80);
        assert_eq!(read_word(&returned, 36) as i32, AREA.min_x + 100);
        assert_eq!(read_word(&returned, 40) as i32, AREA.max_y - 20);
        assert!(!server.current_redraw_clears_background(42));
        let mut next = [0_u8; 44];
        put_word(&mut next, 0, handle);
        task.memory.write_bytes(STATE_BLOCK, &next).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK;
        server
            .dispatch(WIMP_GET_RECTANGLE, &mut task, &mut context)
            .unwrap();
        assert_eq!(context.registers[0], 0);

        let mut context = SwiContext::default();
        context.registers[0] = handle;
        context.registers[1] = 140;
        context.registers[2] = (-60_i32) as u32;
        context.registers[3] = 180;
        context.registers[4] = (-40_i32) as u32;
        server
            .dispatch(WIMP_FORCE_REDRAW, &mut task, &mut context)
            .unwrap();
        let (reason, event) = poll(&server, &mut task, 0, POLL_BLOCK).unwrap();
        assert_eq!(reason, 1);
        assert_eq!(read_word(&event, 0), handle);
        let mut redraw = [0_u8; 44];
        put_word(&mut redraw, 0, handle);
        task.memory.write_bytes(STATE_BLOCK, &redraw).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK;
        server
            .dispatch(WIMP_REDRAW_WINDOW, &mut task, &mut context)
            .unwrap();
        assert_eq!(context.registers[0], 1);
        let returned = task.memory.read_bytes(STATE_BLOCK, 44).unwrap();
        assert_eq!(read_word(&returned, 28) as i32, AREA.min_x + 140);
        assert_eq!(read_word(&returned, 32) as i32, AREA.max_y - 60);
        assert_eq!(read_word(&returned, 36) as i32, AREA.min_x + 180);
        assert_eq!(read_word(&returned, 40) as i32, AREA.max_y - 40);
    }

    #[test]
    fn uncovered_invalid_content_is_queued_and_reopening_keeps_valid_content() {
        let server = new_server();
        let mut task = Task::new(43);
        initialize(&server, &mut task);
        let rear = create_window(&server, &mut task, "Rear", AREA, EXTENT, 0);
        open_window(&server, &mut task, rear, AREA, 0, 0, -1).unwrap();
        assert_eq!(complete_redraw(&server, &mut task, POLL_BLOCK), rear);

        let front = create_window(&server, &mut task, "Front", AREA, EXTENT, 0);
        open_window(&server, &mut task, front, AREA, 0, 0, -1).unwrap();
        assert_eq!(complete_redraw(&server, &mut task, POLL_BLOCK), front);

        let mut context = SwiContext::default();
        context.registers[0] = rear;
        context.registers[1] = EXTENT.min_x as u32;
        context.registers[2] = EXTENT.min_y as u32;
        context.registers[3] = EXTENT.max_x as u32;
        context.registers[4] = EXTENT.max_y as u32;
        server
            .dispatch(WIMP_FORCE_REDRAW, &mut task, &mut context)
            .unwrap();
        assert_eq!(poll(&server, &mut task, 0, POLL_BLOCK).unwrap().0, 0);

        task.memory
            .write_bytes(STATE_BLOCK, &front.to_le_bytes())
            .unwrap();
        let mut close = SwiContext::default();
        close.registers[1] = STATE_BLOCK;
        server
            .dispatch(WIMP_CLOSE_WINDOW, &mut task, &mut close)
            .unwrap();
        let (reason, event) = poll(&server, &mut task, 0, POLL_BLOCK).unwrap();
        assert_eq!(reason, 1);
        assert_eq!(read_word(&event, 0), rear);
        let mut redraw = SwiContext::default();
        redraw.registers[1] = POLL_BLOCK;
        server
            .dispatch(WIMP_REDRAW_WINDOW, &mut task, &mut redraw)
            .unwrap();
        while redraw.registers[0] != 0 {
            server
                .dispatch(WIMP_GET_RECTANGLE, &mut task, &mut redraw)
                .unwrap();
        }

        task.memory
            .write_bytes(STATE_BLOCK, &rear.to_le_bytes())
            .unwrap();
        server
            .dispatch(WIMP_CLOSE_WINDOW, &mut task, &mut close)
            .unwrap();
        open_window(&server, &mut task, rear, AREA, 0, 0, -1).unwrap();
        assert_eq!(poll(&server, &mut task, 0, POLL_BLOCK).unwrap().0, 0);
    }

    #[test]
    fn transparent_redraw_preserves_contents_and_wimp_drawn_windows_skip_events() {
        let server = new_server();
        let mut task = Task::new(44);
        initialize(&server, &mut task);
        let transparent =
            create_window_with_background(&server, &mut task, "Transparent", AREA, EXTENT, 0, 0xFF);
        open_window(&server, &mut task, transparent, AREA, 0, 0, -1).unwrap();

        let (reason, event) = poll(&server, &mut task, 0, POLL_BLOCK).unwrap();
        assert_eq!(reason, 1);
        assert_eq!(read_word(&event, 0), transparent);
        let mut redraw = SwiContext::default();
        redraw.registers[1] = POLL_BLOCK;
        server
            .dispatch(WIMP_REDRAW_WINDOW, &mut task, &mut redraw)
            .unwrap();
        assert_eq!(redraw.registers[0], 1);
        assert!(!server.current_redraw_clears_background(44));
        while redraw.registers[0] != 0 {
            server
                .dispatch(WIMP_GET_RECTANGLE, &mut task, &mut redraw)
                .unwrap();
        }

        let wimp_drawn = create_window(&server, &mut task, "Wimp drawn", AREA, EXTENT, 0);
        server
            .state
            .lock()
            .unwrap()
            .windows
            .get_mut(&wimp_drawn)
            .unwrap()
            .flags |= 1 << 4;
        open_window(&server, &mut task, wimp_drawn, AREA, 0, 0, -1).unwrap();
        assert_eq!(poll(&server, &mut task, 0, POLL_BLOCK).unwrap().0, 0);
    }

    fn write_test_menu(task: &mut Task, address: u32, title: &str, rows: &[(u32, u32, u32, &str)]) {
        let mut block = vec![0u8; MENU_HEADER_SIZE + rows.len() * MENU_ITEM_SIZE];
        let title_bytes = title.as_bytes();
        let title_length = title_bytes.len().min(11);
        block[..title_length].copy_from_slice(&title_bytes[..title_length]);
        block[12] = 7;
        block[13] = 2;
        block[14] = 0;
        block[15] = 7;
        put_word(&mut block, 16, 180);
        put_word(&mut block, 20, 32);
        put_word(&mut block, 24, 0);
        for (index, (flags, submenu, icon_flags, label)) in rows.iter().enumerate() {
            let offset = MENU_HEADER_SIZE + index * MENU_ITEM_SIZE;
            put_word(&mut block, offset, *flags);
            put_word(&mut block, offset + 4, *submenu);
            put_word(&mut block, offset + 8, *icon_flags);
            let text = label.as_bytes();
            let length = text.len().min(11);
            block[offset + 12..offset + 12 + length].copy_from_slice(&text[..length]);
        }
        task.memory.write_bytes(address, &block).unwrap();
    }

    fn create_test_menu(server: &WimpServer, task: &mut Task, address: u32, x: i32, top: i32) {
        let mut context = SwiContext::default();
        context.registers[1] = address;
        context.registers[2] = x as u32;
        context.registers[3] = top as u32;
        server
            .dispatch(WIMP_CREATE_MENU, task, &mut context)
            .unwrap();
    }

    fn write_test_menu_tree(task: &mut Task) {
        const ROOT_MENU: u32 = 0x8000;
        const DISPLAY_MENU: u32 = 0x8400;
        let text_flags = 1 | (1 << 5);
        write_test_menu(
            task,
            ROOT_MENU,
            "Filer",
            &[
                (0, DISPLAY_MENU, text_flags, "Display"),
                (1 << 7, u32::MAX, text_flags | (1 << 22), "Clear selection"),
            ],
        );
        write_test_menu(
            task,
            DISPLAY_MENU,
            "Display",
            &[
                (0, u32::MAX, text_flags, "Large icons"),
                (1 << 7, u32::MAX, text_flags, "Small icons"),
            ],
        );
    }

    fn create_icon(server: &WimpServer, task: &mut Task, label: &str) -> u32 {
        create_icon_with_parent(server, task, -2, label)
    }

    fn create_icon_with_parent(
        server: &WimpServer,
        task: &mut Task,
        parent: i32,
        label: &str,
    ) -> u32 {
        let mut block = [0u8; 36];
        put_word(&mut block, 0, parent as u32);
        put_word(&mut block, 12, 240);
        put_word(&mut block, 16, 68);
        put_word(&mut block, 20, 0x3001);
        let bytes = label.as_bytes();
        let length = bytes.len().min(11);
        block[24..24 + length].copy_from_slice(&bytes[..length]);
        task.memory.write_bytes(WINDOW_BLOCK, &block).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = WINDOW_BLOCK;
        server
            .dispatch(WIMP_CREATE_ICON, task, &mut context)
            .unwrap();
        context.registers[0]
    }

    fn create_window_icon(
        server: &WimpServer,
        task: &mut Task,
        window: u32,
        bounds: DesktopRect,
        label: &str,
    ) -> u32 {
        let mut block = [0u8; 36];
        put_word(&mut block, 0, window);
        put_word(&mut block, 4, bounds.min_x as u32);
        put_word(&mut block, 8, bounds.min_y as u32);
        put_word(&mut block, 12, bounds.max_x as u32);
        put_word(&mut block, 16, bounds.max_y as u32);
        put_word(&mut block, 20, 1 | (3 << 12));
        let bytes = label.as_bytes();
        let length = bytes.len().min(11);
        block[24..24 + length].copy_from_slice(&bytes[..length]);
        task.memory.write_bytes(WINDOW_BLOCK, &block).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = WINDOW_BLOCK;
        server
            .dispatch(WIMP_CREATE_ICON, task, &mut context)
            .unwrap();
        context.registers[0]
    }

    const AREA: WorkArea = WorkArea {
        min_x: 30,
        min_y: DESKTOP_ICONBAR_HEIGHT + SIZE_ICON_HEIGHT + FRAME_BORDER,
        max_x: 390,
        max_y: 360,
    };
    const EXTENT: WorkArea = WorkArea {
        min_x: 0,
        min_y: -500,
        max_x: 500,
        max_y: 0,
    };

    #[test]
    fn menu_body_hover_delays_without_restarting_and_escape_dismisses() {
        let server = new_server();
        let mut task = Task::new(1);
        initialize(&server, &mut task);
        write_test_menu_tree(&mut task);
        create_test_menu(&server, &mut task, 0x8000, 80, 620);

        let row = server.desktop_menus()[0].rows[0].clone();
        let x = (row.bounds.min_x + row.bounds.max_x) / 2;
        let y = (row.bounds.min_y + row.bounds.max_y) / 2;
        assert!(server.mouse_move(x, y));
        assert_eq!(server.desktop_menus().len(), 1);
        let first_deadline = server.next_menu_hover_deadline().unwrap();
        server.mouse_move(x + 1, y);
        assert_eq!(server.next_menu_hover_deadline(), Some(first_deadline));
        assert!(
            !server.advance_menu_hover_at(first_deadline - std::time::Duration::from_millis(1))
        );
        assert!(server.advance_menu_hover_at(first_deadline));
        assert_eq!(server.desktop_menus().len(), 2);

        server.key_pressed(27);
        assert!(server.desktop_menus().is_empty());
    }

    #[test]
    fn menu_arrow_opens_immediately_and_submenu_flips_inside_desktop_edges() {
        let server = new_server();
        let mut task = Task::new(2);
        initialize(&server, &mut task);
        write_test_menu_tree(&mut task);
        create_test_menu(
            &server,
            &mut task,
            0x8000,
            DESKTOP_WIDTH - 20,
            DESKTOP_HEIGHT + 4,
        );

        let root = server.desktop_menus()[0].clone();
        assert_eq!(root.bounds.max_x, DESKTOP_WIDTH);
        assert_eq!(root.bounds.max_y, DESKTOP_HEIGHT);
        let row = root.rows[0].bounds;
        let x = row.max_x - 2;
        let y = (row.min_y + row.max_y) / 2;
        assert!(server.mouse_move(x, y));
        let panels = server.desktop_menus();
        assert_eq!(panels.len(), 2);
        assert!(panels[1].bounds.min_x < panels[0].bounds.min_x);
        assert!(panels[1].bounds.min_x >= 0);
        assert!(panels[1].bounds.max_x <= DESKTOP_WIDTH);
        assert!(panels[1].bounds.min_y >= 0);
        assert!(panels[1].bounds.max_y <= DESKTOP_HEIGHT);
    }

    #[test]
    fn menu_selection_reports_tree_path_adjust_state_and_preserves_cascade() {
        let server = new_server();
        let mut task = Task::new(3);
        initialize(&server, &mut task);
        write_test_menu_tree(&mut task);
        create_test_menu(&server, &mut task, 0x8000, 80, 620);

        let initial_panel = server.desktop_menus()[0].clone();
        let title = initial_panel.title_bounds.unwrap();
        let title_x = (title.min_x + title.max_x) / 2;
        let title_y = (title.min_y + title.max_y) / 2;
        server.mouse_down(title_x, title_y, 4);
        server.mouse_move(title_x + 60, title_y + 40);
        server.mouse_button_up(4);
        let dragged_panel = server.desktop_menus()[0].clone();
        assert_eq!(dragged_panel.bounds.min_x, initial_panel.bounds.min_x + 60);
        assert_eq!(dragged_panel.bounds.max_y, initial_panel.bounds.max_y + 40);

        let root_row = dragged_panel.rows[0].bounds;
        server.mouse_move(root_row.max_x - 2, (root_row.min_y + root_row.max_y) / 2);
        let child_row = server.desktop_menus()[1].rows[0].bounds;
        server.mouse_down(
            child_row.min_x + 36,
            (child_row.min_y + child_row.max_y) / 2,
            1,
        );
        server.mouse_button_up(1);

        let (reason, block) = poll(&server, &mut task, 1, POLL_BLOCK).unwrap();
        assert_eq!(reason, 9);
        assert_eq!(read_word(&block, 0), 0);
        assert_eq!(read_word(&block, 4), 0);
        assert_eq!(read_word(&block, 8), u32::MAX);

        let mut pointer = SwiContext::default();
        pointer.registers[1] = POLL_BLOCK;
        server
            .dispatch(WIMP_GET_POINTER_INFO, &mut task, &mut pointer)
            .unwrap();
        assert_eq!(read_task_word(&task, POLL_BLOCK, 8), 1);
        assert_eq!(server.desktop_menus().len(), 2);

        // A task updates its persistent menu block and reopens the same root.
        let tick_address = 0x8400 + MENU_HEADER_SIZE as u32;
        let mut item_flags = task.memory.read_bytes(tick_address, 4).unwrap();
        put_word(&mut item_flags, 0, 1);
        task.memory.write_bytes(tick_address, &item_flags).unwrap();
        create_test_menu(&server, &mut task, 0x8000, 80, 620);
        let panels = server.desktop_menus();
        assert_eq!(panels.len(), 2);
        assert_eq!(panels[0].bounds, dragged_panel.bounds);
        assert!(panels[1].rows[0].tick);
        assert!(panels[1].rows[0].selected);
    }

    #[test]
    fn menu_parser_accepts_indirect_text_and_rejects_cyclic_trees() {
        let server = new_server();
        let mut task = Task::new(4);
        initialize(&server, &mut task);
        const MENU: u32 = 0x8800;
        const TEXT: u32 = 0x9000;
        let mut block = vec![0u8; MENU_HEADER_SIZE + MENU_ITEM_SIZE];
        block[..5].copy_from_slice(b"Filer");
        put_word(&mut block, 16, 200);
        put_word(&mut block, 20, 32);
        put_word(&mut block, 24, 0);
        put_word(&mut block, MENU_HEADER_SIZE, 1 << 7);
        put_word(&mut block, MENU_HEADER_SIZE + 4, u32::MAX);
        put_word(&mut block, MENU_HEADER_SIZE + 8, 1 | (1 << 5) | (1 << 8));
        put_word(&mut block, MENU_HEADER_SIZE + 12, TEXT);
        put_word(&mut block, MENU_HEADER_SIZE + 16, 0);
        put_word(&mut block, MENU_HEADER_SIZE + 20, 48);
        task.memory
            .write_bytes(TEXT, b"Long indirect menu label\0")
            .unwrap();
        task.memory.write_bytes(MENU, &block).unwrap();
        create_test_menu(&server, &mut task, MENU, 12, 500);
        assert_eq!(
            server.desktop_menus()[0].rows[0].label,
            "Long indirect menu label"
        );
        server.dismiss_menu();

        put_word(&mut block, MENU_HEADER_SIZE + 4, MENU);
        task.memory.write_bytes(MENU, &block).unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = MENU;
        context.registers[2] = 12;
        context.registers[3] = 500;
        assert!(matches!(
            server.dispatch(WIMP_CREATE_MENU, &mut task, &mut context),
            Err(RuntimeError::Program(message)) if message.contains("cycle")
        ));
    }

    #[test]
    fn poll_returns_click_then_focused_key_with_riscos_button_bits() {
        let server = new_server();
        let mut task = Task::new(1);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Test", AREA, EXTENT, 3);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();

        assert!(server.mouse_down(100, 200, 4).is_none());
        server.key_pressed(65);

        let (reason, event) = poll(&server, &mut task, 1 | (1 << 1), POLL_BLOCK).unwrap();
        assert_eq!(reason, 6);
        assert_eq!(read_word(&event, 0), 100);
        assert_eq!(read_word(&event, 4), 200);
        assert_eq!(read_word(&event, 8), 4); // Select
        assert_eq!(read_word(&event, 12), handle);
        assert_eq!(read_word(&event, 16), u32::MAX); // work-area background

        let (reason, event) = poll(&server, &mut task, 1 | (1 << 1), POLL_BLOCK).unwrap();
        assert_eq!(reason, 8);
        assert_eq!(read_word(&event, 0), handle);
        assert_eq!(read_word(&event, 4), u32::MAX);
        assert_eq!(read_word(&event, 24), 65);
    }

    #[test]
    fn icon_bar_create_click_and_delete_use_standard_blocks_and_ownership() {
        let server = new_server();
        let mut first_task = Task::new(20);
        let mut second_task = Task::new(21);
        let first_handle = initialize(&server, &mut first_task);
        initialize(&server, &mut second_task);
        let icon = create_icon(&server, &mut first_task, "DemoDisk");
        let visible = server.desktop_icons();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].handle, icon);
        assert_eq!(visible[0].label, "DemoDisk");
        assert_eq!(visible[0].side, IconBarSide::Devices);
        assert!(visible[0].bounds.min_y >= 0);
        assert!(visible[0].bounds.max_y <= DESKTOP_ICONBAR_HEIGHT);

        let x = (visible[0].bounds.min_x + visible[0].bounds.max_x) / 2;
        let y = (visible[0].bounds.min_y + visible[0].bounds.max_y) / 2;
        assert!(server.mouse_down(x, y, 4).is_none());
        let (reason, event) = poll(&server, &mut first_task, 1, POLL_BLOCK).unwrap();
        assert_eq!(reason, 6);
        assert_eq!(read_word(&event, 8), 4);
        assert_eq!(read_word(&event, 12), (-2i32) as u32);
        assert_eq!(read_word(&event, 16), icon);
        assert!(server.desktop_windows().is_empty());

        let mut delete = [0u8; 8];
        put_word(&mut delete, 0, (-2i32) as u32);
        put_word(&mut delete, 4, icon);
        second_task
            .memory
            .write_bytes(STATE_BLOCK, &delete)
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK;
        assert!(
            server
                .dispatch(WIMP_DELETE_ICON, &mut second_task, &mut context)
                .is_err()
        );

        first_task.memory.write_bytes(STATE_BLOCK, &delete).unwrap();
        context.registers[1] = STATE_BLOCK;
        server
            .dispatch(WIMP_DELETE_ICON, &mut first_task, &mut context)
            .unwrap();
        assert!(server.desktop_icons().is_empty());

        let application_icon = create_icon_with_parent(&server, &mut first_task, -1, "Calculator");
        let visible = server.desktop_icons();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].side, IconBarSide::Applications);
        assert_eq!(visible[0].label, "Calculator");
        let x = (visible[0].bounds.min_x + visible[0].bounds.max_x) / 2;
        let y = (visible[0].bounds.min_y + visible[0].bounds.max_y) / 2;
        server.mouse_down(x, y, 4);
        let (reason, event) = poll(&server, &mut first_task, 1, POLL_BLOCK).unwrap();
        assert_eq!(reason, 6);
        assert_eq!(read_word(&event, 12), (-1i32) as u32);
        assert_eq!(read_word(&event, 16), application_icon);
        assert_ne!(first_handle, 0);
    }

    #[test]
    fn start_task_queues_isolated_guest_launch_and_console_input_then_cleans_up() {
        let server = new_server();
        let mut launcher = Task::new(22);
        initialize(&server, &mut launcher);
        launcher
            .memory
            .write_bytes(STATE_BLOCK, b"BASIC $.Examples.Echo\r")
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[0] = STATE_BLOCK;
        server
            .dispatch(WIMP_START_TASK, &mut launcher, &mut context)
            .unwrap();
        assert_ne!(context.registers[0], 0);

        let request = server.take_pending_launches().pop().unwrap();
        assert_eq!(request.guest_path, "$.Examples.Echo");
        assert_eq!(request.title, "Echo");
        assert!(server.desktop_icons().is_empty());
        assert_eq!(server.desktop_windows().len(), 1);

        let (input_sender, input_receiver) = mpsc::channel();
        server.set_task_input(request.task_id, input_sender);
        server.key_pressed(u32::from(b'A'));
        assert_eq!(input_receiver.try_recv().unwrap(), b'A');

        server.task_exited(request.task_id);
        assert!(
            server
                .desktop_icons()
                .iter()
                .all(|icon| icon.activate_task_id != Some(request.task_id))
        );
        assert!(server.desktop_windows().is_empty());
    }

    #[test]
    fn task_registration_does_not_create_an_iconbar_icon_or_hit_target() {
        let server = new_server();
        let task_id = 25;
        server.task_started(task_id, "NoWindow").unwrap();
        assert!(server.desktop_icons().is_empty());

        let mut app = Task::new(task_id);
        let task_handle = initialize(&server, &mut app);
        assert!(server.desktop_icons().is_empty());

        let x = DESKTOP_WIDTH - ICONBAR_SYSTEM_AREA_OS - 60;
        let y = DESKTOP_ICONBAR_HEIGHT / 2;
        server.mouse_down(x, y, 4);
        let state = server.state.lock().unwrap();
        assert!(state.tasks[&task_handle].events.is_empty());
    }

    #[test]
    fn window_icon_hits_are_clipped_to_the_visible_work_area() {
        let server = new_server();
        let mut task = Task::new(26);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Filer", AREA, EXTENT, 10);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();
        let icon = create_window_icon(
            &server,
            &mut task,
            handle,
            DesktopRect {
                min_x: 0,
                min_y: AREA.min_y - AREA.max_y - 20,
                max_x: 90,
                max_y: -140,
            },
            "Clipped",
        );
        let state = server.state.lock().unwrap();
        assert_eq!(
            pointer_window_and_icon(&state, 50, AREA.min_y + 10),
            (handle as i32, icon as i32)
        );
        assert_eq!(
            pointer_window_and_icon(&state, 50, AREA.min_y - 5),
            (-1, -1)
        );
    }

    #[test]
    fn title_controls_place_back_before_close() {
        let server = new_server();
        let mut task = Task::new(27);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Filer", AREA, EXTENT, 10);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();
        server
            .state
            .lock()
            .unwrap()
            .windows
            .get_mut(&handle)
            .unwrap()
            .has_back_icon = true;
        let layout = desktop_window_furniture(&server.desktop_windows()[0]);
        let back = layout.back_icon.unwrap();
        let close = layout.close_icon.unwrap();
        assert_eq!(back.min_x, AREA.min_x);
        assert_eq!(back.max_x, close.min_x);
    }

    #[test]
    fn pointer_info_keeps_adjust_state_for_close_request_after_release() {
        let server = new_server();
        let mut task = Task::new(28);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Filer", AREA, EXTENT, 10);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();
        let close = desktop_window_furniture(&server.desktop_windows()[0])
            .close_icon
            .unwrap();
        server.mouse_down(
            close.min_x + (close.max_x - close.min_x) / 2,
            close.min_y + (close.max_y - close.min_y) / 2,
            1,
        );
        server.mouse_button_up(1);

        let (reason, request) = poll(&server, &mut task, 1 << 1, POLL_BLOCK).unwrap();
        assert_eq!(reason, 3);
        assert_eq!(read_word(&request, 0), handle);
        let mut pointer = SwiContext::default();
        pointer.registers[1] = POLL_BLOCK;
        server
            .dispatch(WIMP_GET_POINTER_INFO, &mut task, &mut pointer)
            .unwrap();
        assert_eq!(read_task_word(&task, POLL_BLOCK, 8), 1);
    }

    #[test]
    fn wimp_started_task_remains_registered_after_close_down_without_an_implicit_icon() {
        let server = new_server();
        let mut launcher = Task::new(24);
        initialize(&server, &mut launcher);
        launcher
            .memory
            .write_bytes(STATE_BLOCK, b"BASIC $.Examples.Continued\r")
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[0] = STATE_BLOCK;
        server
            .dispatch(WIMP_START_TASK, &mut launcher, &mut context)
            .unwrap();
        let request = server.take_pending_launches().pop().unwrap();

        let mut child = Task::new(request.task_id);
        let child_handle = initialize(&server, &mut child);
        let mut close_down = SwiContext::default();
        close_down.registers[0] = child_handle;
        close_down.registers[1] = TASK_MAGIC;
        server
            .dispatch(WIMP_CLOSE_DOWN, &mut child, &mut close_down)
            .unwrap();

        assert!(server.desktop_icons().is_empty());
        assert!(
            server
                .desktop_windows()
                .iter()
                .any(|window| window.owner_task_id == request.task_id)
        );

        server.task_exited(request.task_id);
        assert!(server.desktop_icons().is_empty());
        assert!(
            server
                .desktop_windows()
                .iter()
                .all(|window| window.owner_task_id != request.task_id)
        );
    }

    #[test]
    fn double_click_window_flags_deliver_single_then_double_button_events() {
        let server = new_server();
        let mut task = Task::new(23);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Filer", AREA, EXTENT, 10);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();

        server.mouse_down(100, 200, 4);
        let (reason, first) = poll(&server, &mut task, 1 | (1 << 1), POLL_BLOCK).unwrap();
        assert_eq!(reason, 6);
        assert_eq!(read_word(&first, 8), 4 * 256);
        server.mouse_down(101, 201, 4);
        let (reason, second) = poll(&server, &mut task, 1 | (1 << 1), POLL_BLOCK).unwrap();
        assert_eq!(reason, 6);
        assert_eq!(read_word(&second, 8), 4);
    }

    #[test]
    fn menu_click_is_never_scaled_by_icon_button_type() {
        let server = new_server();
        let mut state = server.state.lock().unwrap();
        for button_type in 0..16 {
            for _ in 0..2 {
                assert_eq!(
                    event_button_state(&mut state, 1, 2, 3, 100, 200, 2, button_type,),
                    2
                );
            }
        }
        assert!(state.last_click.is_none());
    }

    #[test]
    fn poll_checks_mask_and_full_block_before_consuming_an_event() {
        let server = new_server();
        let mut task = Task::new(2);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Test", AREA, EXTENT, 3);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();
        server.mouse_down(100, 200, 4);

        assert!(
            server
                .dispatch(
                    WIMP_POLL,
                    &mut task,
                    &mut SwiContext {
                        registers: [1, u32::MAX - 32, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                        ..SwiContext::default()
                    },
                )
                .is_err()
        );
        assert_eq!(
            poll(&server, &mut task, (1 << 6) | (1 << 1), POLL_BLOCK)
                .unwrap()
                .0,
            0
        );
        assert_eq!(
            poll(&server, &mut task, 1 | (1 << 1), POLL_BLOCK)
                .unwrap()
                .0,
            6
        );

        assert!(matches!(
            poll(&server, &mut task, 1 << 2, POLL_BLOCK),
            Err(RuntimeError::Program(_))
        ));
        assert!(matches!(
            poll(&server, &mut task, 1 << 22, POLL_BLOCK),
            Err(RuntimeError::Program(_))
        ));
        // User-message mask bits 17-19 are valid; reserved bits are not.
        assert_eq!(
            poll(&server, &mut task, (1 << 17) | (1 << 1), POLL_BLOCK)
                .unwrap()
                .0,
            0
        );
    }

    #[test]
    fn open_validation_is_atomic_and_get_window_state_uses_standard_layout() {
        let server = new_server();
        let mut task = Task::new(3);
        initialize(&server, &mut task);
        let first = create_window(&server, &mut task, "First", AREA, EXTENT, 3);
        open_window(&server, &mut task, first, AREA, 0, 0, -1).unwrap();
        let second_area = WorkArea {
            min_x: 410,
            max_x: 770,
            ..AREA
        };
        let second = create_window(&server, &mut task, "Second", second_area, EXTENT, 3);
        open_window(&server, &mut task, second, second_area, 0, 0, -1).unwrap();
        let before = server.desktop_windows();

        let moved = WorkArea {
            min_x: 60,
            max_x: 420,
            ..AREA
        };
        assert!(open_window(&server, &mut task, first, moved, 0, 0, first as i32).is_err());
        assert_eq!(server.desktop_windows(), before);
        assert!(open_window(&server, &mut task, first, moved, 0, 0, 99_999).is_err());
        assert_eq!(server.desktop_windows(), before);
        assert!(
            open_window(
                &server,
                &mut task,
                first,
                WorkArea { max_x: 700, ..AREA },
                0,
                0,
                -1,
            )
            .is_err()
        );
        assert_eq!(server.desktop_windows(), before);

        task.memory
            .write_bytes(STATE_BLOCK, &first.to_le_bytes())
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK;
        server
            .dispatch(WIMP_GET_WINDOW_STATE, &mut task, &mut context)
            .unwrap();
        assert_eq!(read_task_word(&task, STATE_BLOCK, 0), first);
        assert_eq!(read_task_word(&task, STATE_BLOCK, 4), AREA.min_x as u32);
        assert_eq!(read_task_word(&task, STATE_BLOCK, 8), AREA.min_y as u32);
        assert_eq!(read_task_word(&task, STATE_BLOCK, 12), AREA.max_x as u32);
        assert_eq!(read_task_word(&task, STATE_BLOCK, 16), AREA.max_y as u32);
        assert_eq!(read_task_word(&task, STATE_BLOCK, 28), second);
        let flags = read_task_word(&task, STATE_BLOCK, 32);
        assert_ne!(flags & (1 << 16), 0);
        // The first window's right furniture is overlapped by the second
        // window's left frame, so Wimp_GetWindowState must clear fully-visible.
        assert_eq!(flags & (1 << 17), 0);
    }

    #[test]
    fn shared_handles_are_owner_scoped_and_close_down_cleans_windows() {
        let server = new_server();
        let mut first_task = Task::new(4);
        let mut second_task = Task::new(5);
        let first_task_handle = initialize(&server, &mut first_task);
        let second_task_handle = initialize(&server, &mut second_task);
        assert_ne!(first_task_handle, second_task_handle);
        let first_window = create_window(&server, &mut first_task, "First", AREA, EXTENT, 3);
        let second_window_area = WorkArea {
            min_x: 410,
            max_x: 770,
            ..AREA
        };
        let second_window = create_window(
            &server,
            &mut second_task,
            "Second",
            second_window_area,
            EXTENT,
            3,
        );
        assert_ne!(first_window, second_window);
        open_window(&server, &mut first_task, first_window, AREA, 0, 0, -1).unwrap();
        open_window(
            &server,
            &mut second_task,
            second_window,
            second_window_area,
            0,
            0,
            -1,
        )
        .unwrap();

        second_task
            .memory
            .write_bytes(STATE_BLOCK, &first_window.to_le_bytes())
            .unwrap();
        let mut context = SwiContext::default();
        context.registers[1] = STATE_BLOCK;
        assert!(
            server
                .dispatch(WIMP_GET_WINDOW_STATE, &mut second_task, &mut context)
                .is_err()
        );

        let mut close_down = SwiContext::default();
        close_down.registers[0] = first_task_handle;
        close_down.registers[1] = TASK_MAGIC;
        server
            .dispatch(WIMP_CLOSE_DOWN, &mut first_task, &mut close_down)
            .unwrap();
        let remaining = server.desktop_windows();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].owner_task_id, second_task.id);
    }

    #[test]
    fn resize_is_previewed_clamped_and_committed_only_by_open_window() {
        let server = new_server();
        let mut task = Task::new(6);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Test", AREA, EXTENT, 3);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();

        let size_icon = desktop_window_furniture(&server.desktop_windows()[0])
            .adjust_size_icon
            .unwrap();
        let grab_x = (size_icon.min_x + size_icon.max_x) / 2;
        let grab_y = (size_icon.min_y + size_icon.max_y) / 2;
        let drag = server
            .mouse_down(grab_x, grab_y, 4)
            .expect("size icon starts resize");
        assert_eq!(drag.kind, WindowDragKind::Resize);
        server.drag_to(drag, grab_x + 50, grab_y - 12);
        let preview = WorkArea {
            min_x: 30,
            min_y: AREA.min_y - 12,
            max_x: 440,
            max_y: 360,
        };
        let windows = server.desktop_windows();
        assert_eq!(windows[0].work_area, AREA);
        assert_eq!(windows[0].preview_area, Some(preview));

        server.drag_to(drag, 100_000, -100_000);
        let clamped = server.desktop_windows()[0].preview_area.unwrap();
        validate_visible_area(EXTENT, clamped, 0, 0).unwrap();
        validate_screen_area(&server.state.lock().unwrap().windows[&handle], clamped).unwrap();

        server.drag_to(drag, grab_x + 50, grab_y - 12);
        server.finish_drag(drag);
        let (reason, request) = poll(&server, &mut task, 1 | (1 << 1), POLL_BLOCK).unwrap();
        assert_eq!(reason, 2);
        assert_eq!(read_word(&request, 0), handle);
        assert_eq!(read_word(&request, 4), preview.min_x as u32);
        assert_eq!(read_word(&request, 8), preview.min_y as u32);
        assert_eq!(read_word(&request, 12), preview.max_x as u32);
        assert_eq!(read_word(&request, 16), preview.max_y as u32);

        let mut open_context = SwiContext::default();
        open_context.registers[1] = POLL_BLOCK;
        server
            .dispatch(WIMP_OPEN_WINDOW, &mut task, &mut open_context)
            .unwrap();
        let after = server.desktop_windows();
        assert_eq!(after[0].work_area, preview);
        assert_eq!(after[0].preview_area, None);
    }
}
