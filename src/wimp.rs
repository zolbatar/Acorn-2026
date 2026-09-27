//! Shared hosted Wimp service for the first two-task desktop slice.
//!
//! The SWI numbers and parameter blocks implemented here follow the RISC OS
//! Programmer's Reference Manual. The hosted service deliberately implements
//! only window registration, opening/closing, polling, basic mouse/key events,
//! and desktop stacking. It does not claim the complete Wimp API.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Condvar, Mutex, mpsc::Sender},
};

use crate::{configure::ConfigureStore, error::RuntimeError, memory::Task, swi::SwiContext};

pub const WIMP_INITIALISE: u32 = 0x400C0;
pub const WIMP_CREATE_WINDOW: u32 = 0x400C1;
pub const WIMP_OPEN_WINDOW: u32 = 0x400C5;
pub const WIMP_CLOSE_WINDOW: u32 = 0x400C6;
pub const WIMP_POLL: u32 = 0x400C7;
pub const WIMP_GET_WINDOW_STATE: u32 = 0x400CB;
pub const WIMP_CLOSE_DOWN: u32 = 0x400DD;

const TASK_MAGIC: u32 = 0x4B53_4154;
const WIMP_VERSION: u32 = 310;
const WINDOW_BLOCK_SIZE: usize = 88;
const OPEN_BLOCK_SIZE: usize = 32;
const WINDOW_STATE_BLOCK_SIZE: usize = 36;
const POLL_BLOCK_SIZE: usize = 256;
const MAX_EVENT_QUEUE: usize = 256;
/// Hosted desktop scene size in pixels. Host window scaling is a separate step.
pub const DESKTOP_PIXEL_WIDTH: u32 = 800;
pub const DESKTOP_PIXEL_HEIGHT: u32 = 600;
/// RISC OS Wimp coordinates are OS graphics units, not framebuffer pixels.
/// This hosted desktop uses the classic 2 OS units per square display pixel.
pub const DESKTOP_OS_UNITS_PER_PIXEL_X: i32 = 2;
pub const DESKTOP_OS_UNITS_PER_PIXEL_Y: i32 = 2;
pub const DESKTOP_WIDTH: i32 = DESKTOP_PIXEL_WIDTH as i32 * DESKTOP_OS_UNITS_PER_PIXEL_X;
pub const DESKTOP_HEIGHT: i32 = DESKTOP_PIXEL_HEIGHT as i32 * DESKTOP_OS_UNITS_PER_PIXEL_Y;
pub const SYSTEM_FONT_WIDTH: i32 = 16;
pub const SYSTEM_FONT_HEIGHT: i32 = 32;
const FRAME_BORDER: i32 = 2;
const TITLE_HEIGHT: i32 = 44;
const VERTICAL_SCROLLBAR_WIDTH: i32 = 44;
const SCROLL_ARROW_SIZE: i32 = 44;
const MIN_SLIDER_SIZE: i32 = 44;
const SIZE_ICON_HEIGHT: i32 = 44;
const MAX_DESKTOP_COORDINATE: i32 = 16_384;
const SCROLL_ARROW_STEP: i32 = 32;

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
            min_y: work.min_y - if resizable { SIZE_ICON_HEIGHT } else { 0 } - FRAME_BORDER,
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
                min_y: work.min_y,
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
            min_y: work.min_y - SIZE_ICON_HEIGHT,
            max_x: outer.max_x - FRAME_BORDER,
            max_y: work.min_y,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopWindow {
    pub handle: u32,
    pub owner_task_id: u64,
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
}

#[derive(Debug)]
struct WimpTask {
    events: VecDeque<QueuedEvent>,
}

#[derive(Clone, Debug)]
struct WimpWindow {
    handle: u32,
    owner_task_handle: u32,
    owner_task_id: u64,
    title: String,
    flags: u32,
    work_area_flags: u32,
    work_area: WorkArea,
    work_extent: WorkArea,
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
    preview_area: Option<WorkArea>,
    preview_scroll: Option<(i32, i32)>,
    last_user_area: WorkArea,
    last_user_scroll: (i32, i32),
    maximized: bool,
    toggle_request_pending: bool,
    restore_behind: i32,
}

#[derive(Debug, Default)]
struct WimpState {
    next_task_handle: u32,
    next_window_handle: u32,
    guest_to_task: HashMap<u64, u32>,
    tasks: HashMap<u32, WimpTask>,
    windows: HashMap<u32, WimpWindow>,
    /// Front to back, as in the Wimp's active-window list.
    stacking: Vec<u32>,
    keyboard_focus: Option<u32>,
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
        match swi {
            WIMP_INITIALISE => self.initialise(task, context),
            WIMP_CREATE_WINDOW => self.create_window(task, context),
            WIMP_OPEN_WINDOW => self.open_window(task, context),
            WIMP_CLOSE_WINDOW => self.close_window(task, context),
            WIMP_POLL => self.poll(task, context),
            WIMP_GET_WINDOW_STATE => self.get_window_state(task, context),
            WIMP_CLOSE_DOWN => self.close_down(task, context),
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
                    let _ = enqueue_for_owner(
                        &mut state,
                        window.owner_task_handle,
                        event_with_word(3, 0, window.handle),
                    );
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
            if button_type == 3 {
                let _ = enqueue_for_owner(
                    &mut state,
                    window.owner_task_handle,
                    mouse_event_with_icon(x, y, buttons, window.handle, -1),
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
            .min(drag.original.max_y - window_outer_bottom_extra_y(window))
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
    }

    pub fn key_pressed(&self, key_code: u32) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let Some(handle) = state.keyboard_focus else {
            return;
        };
        let Some(window) = state.windows.get(&handle) else {
            return;
        };
        let (window_handle, owner_task_handle) = (window.handle, window.owner_task_handle);
        let mut event = QueuedEvent {
            reason: 8,
            block: [0; POLL_BLOCK_SIZE],
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
        if state.guest_to_task.contains_key(&task.id) {
            return Err(program_error("task has already called Wimp_Initialise"));
        }
        let handle = allocate_handle(&mut state.next_task_handle)?;
        state.guest_to_task.insert(task.id, handle);
        state.tasks.insert(
            handle,
            WimpTask {
                events: VecDeque::new(),
            },
        );
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
        if has_title && (title_flags & (1 << 8) != 0 || title_flags & (1 << 1) != 0) {
            return Err(program_error(
                "hosted Wimp_CreateWindow supports direct text titles only",
            ));
        }
        if title_flags & !(1 << 0) != 0 {
            return Err(program_error(
                "hosted Wimp_CreateWindow supports plain, left-aligned title text only",
            ));
        }
        let title = if has_title && title_flags & 1 != 0 {
            control_terminated(&block[72..84])
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
        if !matches!(button_type, 0 | 3) {
            return Err(program_error(
                "hosted Wimp_CreateWindow supports ignored or once-only click work areas (types 0 and 3)",
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
                work_area: initial_area,
                work_extent,
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
                preview_area: None,
                preview_scroll: None,
                last_user_area: initial_area,
                last_user_scroll: (initial_scroll_x, initial_scroll_y),
                maximized: false,
                toggle_request_pending: false,
                restore_behind: -1,
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
        if !window.maximized && !window.toggle_request_pending {
            window.last_user_area = area;
            window.last_user_scroll = (scroll_x, scroll_y);
        }
        window.work_area = area;
        window.scroll_x = scroll_x;
        window.scroll_y = scroll_y;
        window.open = true;
        window.preview_area = None;
        window.preview_scroll = None;
        window.toggle_request_pending = false;
        state.stacking = next_stacking;
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
        window.open = false;
        state.stacking.retain(|item| *item != handle);
        if state.keyboard_focus == Some(handle) {
            state.keyboard_focus = state.stacking.first().copied();
        }
        drop(state);
        let _ = self.desktop_updates.send(());
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
        remove_task(&mut state, task.id);
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
    if window.resizable {
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
        .checked_sub(if resizable {
            SIZE_ICON_HEIGHT + FRAME_BORDER
        } else {
            FRAME_BORDER
        })
        .ok_or_else(|| program_error("window system area coordinate overflowed"))?;
    if left < 0 || bottom < 0 || right > DESKTOP_WIDTH || top > DESKTOP_HEIGHT {
        return Err(program_error(
            "window lies outside the hosted screen (off-screen windows are unsupported)",
        ));
    }
    Ok(())
}

fn rects_overlap(a: DesktopRect, b: DesktopRect) -> bool {
    a.min_x < b.max_x && a.max_x > b.min_x && a.min_y < b.max_y && a.max_y > b.min_y
}

fn enqueue_for_owner(state: &mut WimpState, owner: u32, event: QueuedEvent) -> bool {
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
    };
    put_word(&mut event.block, offset, value);
    event
}

fn mouse_event_with_icon(x: i32, y: i32, buttons: u32, window: u32, icon: i32) -> QueuedEvent {
    let mut event = QueuedEvent {
        reason: 6,
        block: [0; POLL_BLOCK_SIZE],
    };
    put_word(&mut event.block, 0, x as u32);
    put_word(&mut event.block, 4, y as u32);
    put_word(&mut event.block, 8, buttons);
    put_word(&mut event.block, 12, window);
    put_word(&mut event.block, 16, icon as u32);
    event
}

fn null_event() -> QueuedEvent {
    QueuedEvent {
        reason: 0,
        block: [0; POLL_BLOCK_SIZE],
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
}

fn maximum_window_area(window: &WimpWindow) -> WorkArea {
    let min_x = FRAME_BORDER;
    let min_y = if window.resizable {
        SIZE_ICON_HEIGHT + FRAME_BORDER
    } else {
        FRAME_BORDER
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
        visible_width
    };
    let y_step = if y_direction.abs() == 1 {
        SCROLL_ARROW_STEP
    } else {
        visible_height
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

fn remove_task(state: &mut WimpState, guest_task_id: u64) {
    let Some(handle) = state.guest_to_task.remove(&guest_task_id) else {
        return;
    };
    state.tasks.remove(&handle);
    state
        .windows
        .retain(|_, window| window.owner_task_handle != handle);
    state
        .stacking
        .retain(|window| state.windows.contains_key(window));
    if state
        .keyboard_focus
        .is_some_and(|focused| !state.windows.contains_key(&focused))
    {
        state.keyboard_focus = state.stacking.first().copied();
    }
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

    fn read_task_word(task: &Task, address: u32, offset: usize) -> u32 {
        read_word(
            &task.memory.read_bytes(address + offset as u32, 4).unwrap(),
            0,
        )
    }

    const AREA: WorkArea = WorkArea {
        min_x: 30,
        min_y: 80,
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
    fn poll_returns_click_then_focused_key_with_riscos_button_bits() {
        let server = new_server();
        let mut task = Task::new(1);
        initialize(&server, &mut task);
        let handle = create_window(&server, &mut task, "Test", AREA, EXTENT, 3);
        open_window(&server, &mut task, handle, AREA, 0, 0, -1).unwrap();

        assert!(server.mouse_down(100, 200, 4).is_none());
        server.key_pressed(65);

        let (reason, event) = poll(&server, &mut task, 1, POLL_BLOCK).unwrap();
        assert_eq!(reason, 6);
        assert_eq!(read_word(&event, 0), 100);
        assert_eq!(read_word(&event, 4), 200);
        assert_eq!(read_word(&event, 8), 4); // Select
        assert_eq!(read_word(&event, 12), handle);
        assert_eq!(read_word(&event, 16), u32::MAX); // work-area background

        let (reason, event) = poll(&server, &mut task, 1, POLL_BLOCK).unwrap();
        assert_eq!(reason, 8);
        assert_eq!(read_word(&event, 0), handle);
        assert_eq!(read_word(&event, 4), u32::MAX);
        assert_eq!(read_word(&event, 24), 65);
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
        assert_eq!(poll(&server, &mut task, 1 << 6, POLL_BLOCK).unwrap().0, 0);
        assert_eq!(poll(&server, &mut task, 1, POLL_BLOCK).unwrap().0, 6);

        assert!(matches!(
            poll(&server, &mut task, 1 << 2, POLL_BLOCK),
            Err(RuntimeError::Program(_))
        ));
        assert!(matches!(
            poll(&server, &mut task, 1 << 22, POLL_BLOCK),
            Err(RuntimeError::Program(_))
        ));
        // User-message mask bits 17-19 are valid; reserved bits are not.
        assert_eq!(poll(&server, &mut task, 1 << 17, POLL_BLOCK).unwrap().0, 0);
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

        let drag = server
            .mouse_down(400, 72, 4)
            .expect("size icon starts resize");
        assert_eq!(drag.kind, WindowDragKind::Resize);
        server.drag_to(drag, 450, 30);
        let preview = WorkArea {
            min_x: 30,
            min_y: 46,
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

        server.drag_to(drag, 450, 30);
        server.finish_drag(drag);
        let (reason, request) = poll(&server, &mut task, 1, POLL_BLOCK).unwrap();
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
