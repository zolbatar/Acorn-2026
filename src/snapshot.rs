//! Headless snapshots of the real two-task Wimp demonstration.

use std::{
    collections::{BTreeSet, HashMap},
    error::Error,
    fs, io,
    path::Path,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::{
    configure::ConfigureStore,
    display::{DesktopResolution, DisplayColour, DisplaySettings},
    graphics::{GraphicsService, GraphicsSnapshot},
    renderer, riscos_font,
    runtime::Runtime,
    swi::DisplayEvent,
    wimp::{
        DESKTOP_PIXEL_HEIGHT, DESKTOP_PIXEL_WIDTH, DesktopIcon, DesktopMenu, DesktopMenuItem,
        DesktopRect, DesktopWindow, DesktopWindowIcon, IconBarSide, WimpServer,
    },
};

const ALPHA_TASK: u64 = 101;
const BETA_TASK: u64 = 102;
const DESKTOP_TASK: u64 = 110;
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);
const EVENT_WAIT_SLICE: Duration = Duration::from_millis(50);
const ALPHA_READY_TEXT: &[u8] = b"Menu in the work area.";
// Alpha's window occludes the left side of Beta during the initial capture.
const BETA_READY_TEXT: &[u8] = b"ze icon.";

#[derive(Clone, Copy, Eq, PartialEq)]
enum FilerSnapshotMode {
    Default,
    Menu,
    Large,
    Interactions,
    TaskMenu,
}

#[derive(Clone)]
struct FilerDesktopScene {
    windows: Vec<DesktopWindow>,
    desktop_icons: Vec<DesktopIcon>,
    window_icons: Vec<DesktopWindowIcon>,
    menus: Vec<DesktopMenu>,
}

#[derive(Clone)]
struct DisplayManagerScene {
    windows: Vec<DesktopWindow>,
    desktop_icons: Vec<DesktopIcon>,
    window_icons: Vec<DesktopWindowIcon>,
    menus: Vec<DesktopMenu>,
    metrics: crate::display::DesktopMetrics,
    colour: DisplayColour,
}

/// Execute both BASIC Wimp examples and save the rendered desktop as a P6 PPM.
pub fn write_desktop_demo_snapshot(path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    let (display_sender, display_receiver) = mpsc::channel();
    let (task_error_sender, task_error_receiver) = mpsc::channel();
    let (desktop_updates, _desktop_update_receiver) = mpsc::channel();
    let wimp = WimpServer::new(desktop_updates);

    let alpha_source = fs::read_to_string(format!(
        "{}/examples/wimp/two-windows/alpha.bas64",
        env!("CARGO_MANIFEST_DIR")
    ))?;
    let beta_source = fs::read_to_string(format!(
        "{}/examples/wimp/two-windows/beta.bas64",
        env!("CARGO_MANIFEST_DIR")
    ))?;

    let mut guests = Vec::with_capacity(2);
    for (task_id, source, name) in [
        (ALPHA_TASK, alpha_source, "snapshot-alpha"),
        (BETA_TASK, beta_source, "snapshot-beta"),
    ] {
        let (input_sender, input_receiver) = mpsc::channel();
        let task_wimp = Arc::clone(&wimp);
        let task_display = display_sender.clone();
        let task_errors = task_error_sender.clone();
        let guest = thread::Builder::new().name(name.into()).spawn(move || {
            let _keep_input_open = input_sender;
            let mut runtime =
                Runtime::desktop_task(task_id, input_receiver, task_display, task_wimp);
            match runtime.run_application(&source) {
                Ok(()) => {
                    let _ =
                        task_errors.send(format!("task {task_id} exited before snapshot capture"));
                }
                Err(crate::error::RuntimeError::EndOfInput) => {}
                Err(error) => {
                    let _ = task_errors.send(format!("task {task_id} failed: {error}"));
                }
            }
        });
        match guest {
            Ok(guest) => guests.push(guest),
            Err(error) => {
                wimp.stop();
                for guest in guests {
                    let _ = guest.join();
                }
                return Err(Box::new(error));
            }
        }
    }
    drop(display_sender);
    drop(task_error_sender);

    let capture = collect_demo_scene(&wimp, &display_receiver, &task_error_receiver);
    wimp.stop();
    let mut join_error = None;
    for guest in guests {
        if guest.join().is_err() {
            join_error = Some(io::Error::other("desktop demo task panicked"));
        }
    }
    let (windows, scenes) = capture?;
    if let Some(error) = join_error {
        return Err(Box::new(error));
    }

    let mut rgba = vec![0; DESKTOP_PIXEL_WIDTH as usize * DESKTOP_PIXEL_HEIGHT as usize * 4];
    renderer::render_desktop(
        &windows,
        &scenes,
        &wimp.desktop_icons(),
        &wimp.desktop_window_icons(),
        &wimp.desktop_menus(),
        wimp.desktop_notice().as_deref(),
        &mut rgba,
    );
    write_ppm(path.as_ref(), &rgba)?;
    Ok(())
}

/// Execute the BASIC64 desktop and Filer, then save their initial high-resolution icon layout.
pub fn write_filer_snapshot(path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    write_filer_snapshot_mode(path, FilerSnapshotMode::Default)
}

pub fn write_task_menu_snapshot(path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    write_filer_snapshot_mode(path, FilerSnapshotMode::TaskMenu)
}

/// Execute the BASIC64 desktop and save the Filer with its transient menu open.
pub fn write_filer_menu_snapshot(path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    write_filer_snapshot_mode(path, FilerSnapshotMode::Menu)
}

/// Execute the BASIC64 desktop and save the Filer after a real resize gesture.
pub fn write_filer_large_snapshot(path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    write_filer_snapshot_mode(path, FilerSnapshotMode::Large)
}

/// Execute the BASIC64 Filer's selection and cascading menu interactions, then
/// save the visible states as PPM desktop captures in the supplied directory.
pub fn write_filer_interaction_snapshots(
    directory: impl AsRef<Path>,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(directory.as_ref())?;
    write_filer_snapshot_mode(directory, FilerSnapshotMode::Interactions)
}

/// Exercise the BASIC64 Display Manager and save dynamic-size, colour-profile
/// captures for its popup, pending, Cancel, and Change states.
pub fn write_display_manager_snapshots(directory: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    let directory = directory.as_ref().to_path_buf();
    fs::create_dir_all(&directory)?;
    let configure_path = std::env::temp_dir().join(format!(
        "acorn-2026-display-snapshot-{}-{}.configure",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let _ = fs::remove_file(&configure_path);
    let blocked_parent_path = configure_path.with_extension("blocker-file");
    let configure = ConfigureStore::with_path(&configure_path);
    let initial_settings = configure.load().map_err(io::Error::other)?.display;

    let (display_sender, display_receiver) = mpsc::channel();
    let (task_error_sender, task_error_receiver) = mpsc::channel();
    let (desktop_updates, _desktop_update_receiver) = mpsc::channel();
    let wimp = WimpServer::new(desktop_updates);
    wimp.set_configure_store(configure.clone(), initial_settings);
    wimp.task_started(DESKTOP_TASK, "Acorn Desktop")?;

    let desktop_source = fs::read_to_string(format!(
        "{}/demo-volume/System/Desktop.bas64",
        env!("CARGO_MANIFEST_DIR")
    ))?;
    let (desktop_input_sender, desktop_input_receiver) = mpsc::channel();
    let task_wimp = Arc::clone(&wimp);
    let task_display = display_sender.clone();
    let task_errors = task_error_sender.clone();
    let desktop_worker = thread::Builder::new()
        .name("snapshot-display-manager-desktop".into())
        .spawn(move || {
            let _keep_input_open = desktop_input_sender;
            let mut runtime = Runtime::desktop_task(
                DESKTOP_TASK,
                desktop_input_receiver,
                task_display,
                Arc::clone(&task_wimp),
            );
            if let Err(error) = runtime.run_application(&desktop_source) {
                if !matches!(error, crate::error::RuntimeError::EndOfInput) {
                    let _ = task_errors.send(format!("Desktop task failed: {error}"));
                }
            }
            task_wimp.task_exited(DESKTOP_TASK);
        })?;
    drop(display_sender);
    drop(task_error_sender);

    let capture = (|| -> Result<(), Box<dyn Error>> {
        wait_for_display_icon(&wimp, &task_error_receiver)?;
        write_display_manager_scene(
            directory.join("display-manager-iconbar.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;

        click_display_icon(&wimp)?;
        let manager = wait_for_display_manager_window(&wimp, &task_error_receiver)?;
        let singleton_handle = manager.handle;
        write_display_manager_scene(
            directory.join("display-manager-open.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;

        click_display_window_control(&wimp, "16 million")?;
        let colour_menu = wait_for_display_menu("Colours", &wimp, &task_error_receiver)?;
        let colour_labels = colour_menu
            .rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();
        if colour_labels
            != [
                "Black/white",
                "4 greys",
                "16 greys",
                "16 colours",
                "256 greys",
                "256 colours",
                "32 thousand",
                "16 million",
            ]
        {
            return Err(io::Error::other(format!(
                "Display Manager colour menu is incomplete: {colour_labels:?}"
            ))
            .into());
        }
        write_display_manager_scene(
            directory.join("display-manager-colours-menu.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;
        click_display_menu_row(&wimp, &colour_menu, "Black/white")?;
        wait_for_display_window_icon(&wimp, &task_error_receiver, "Black/white")?;

        let resolution_label = wimp
            .desktop_window_icons()
            .into_iter()
            .find(|icon| {
                icon.window_handle == singleton_handle && icon.label.starts_with("Window (")
            })
            .map(|icon| icon.label)
            .ok_or_else(|| io::Error::other("Display Manager lost its Window resolution value"))?;
        click_display_window_control(&wimp, &resolution_label)?;
        let resolution_menu = wait_for_display_menu("Resolution", &wimp, &task_error_receiver)?;
        let resolution_labels = resolution_menu
            .rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>();
        if resolution_labels
            != [
                "Window (800 x 600)",
                "640 x 480",
                "800 x 600",
                "1024 x 768",
                "1152 x 864",
                "1280 x 1024",
                "1600 x 1200",
            ]
        {
            return Err(io::Error::other(format!(
                "Display Manager resolution menu is incomplete: {resolution_labels:?}"
            ))
            .into());
        }
        write_display_manager_scene(
            directory.join("display-manager-resolution-menu.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;
        click_display_menu_row(&wimp, &resolution_menu, "1024 x 768")?;
        wait_for_display_window_icon(&wimp, &task_error_receiver, "1024 x 768")?;
        write_display_manager_scene(
            directory.join("display-manager-cancel-pending.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;
        click_display_window_control(&wimp, "Cancel")?;
        wait_for_display_manager_closed(&wimp, &task_error_receiver)?;
        if wimp.display_settings() != initial_settings {
            return Err(io::Error::other("Cancel applied pending Display Manager settings").into());
        }
        write_display_manager_scene(
            directory.join("display-manager-cancelled.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;

        click_display_icon(&wimp)?;
        let reopened = wait_for_display_manager_window(&wimp, &task_error_receiver)?;
        if reopened.handle != singleton_handle {
            return Err(
                io::Error::other("Display Manager opened a second window after Cancel").into(),
            );
        }
        let resolution_label = wimp
            .desktop_window_icons()
            .into_iter()
            .find(|icon| {
                icon.window_handle == singleton_handle && icon.label.starts_with("Window (")
            })
            .map(|icon| icon.label)
            .ok_or_else(|| {
                io::Error::other("Display Manager did not refresh its resolution value")
            })?;
        click_display_window_control(&wimp, &resolution_label)?;
        let resolution_menu = wait_for_display_menu("Resolution", &wimp, &task_error_receiver)?;
        click_display_menu_row(&wimp, &resolution_menu, "1024 x 768")?;
        wait_for_display_window_icon(&wimp, &task_error_receiver, "1024 x 768")?;
        click_display_window_control(&wimp, "16 million")?;
        let colour_menu = wait_for_display_menu("Colours", &wimp, &task_error_receiver)?;
        click_display_menu_row(&wimp, &colour_menu, "256 greys")?;
        wait_for_display_window_icon(&wimp, &task_error_receiver, "256 greys")?;
        write_display_manager_scene(
            directory.join("display-manager-change-pending.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;
        click_display_window_control(&wimp, "Change")?;
        wait_for_display_manager_closed(&wimp, &task_error_receiver)?;
        let expected = DisplaySettings {
            resolution: DesktopResolution::R1024x768,
            colour: DisplayColour::Grey256,
        };
        if wimp.display_settings() != expected {
            return Err(io::Error::other(format!(
                "Change did not apply both Display Manager settings: {:?}",
                wimp.display_settings()
            ))
            .into());
        }
        if configure.load().map_err(io::Error::other)?.display != expected {
            return Err(
                io::Error::other("Change did not persist both Display Manager settings").into(),
            );
        }
        if wimp.desktop_metrics().pixel_size() != (1024, 768) {
            return Err(io::Error::other(format!(
                "Change left unexpected desktop metrics: {:?}",
                wimp.desktop_metrics().pixel_size()
            ))
            .into());
        }
        write_display_manager_scene(
            directory.join("display-manager-changed.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;

        wimp.set_host_window_size(900, 700);
        click_display_icon(&wimp)?;
        let windowed_manager = wait_for_display_manager_window(&wimp, &task_error_receiver)?;
        if windowed_manager.handle != singleton_handle {
            return Err(
                io::Error::other("Display Manager opened a second window after Change").into(),
            );
        }
        let active_resolution_label = wimp
            .desktop_window_icons()
            .into_iter()
            .find(|icon| icon.window_handle == singleton_handle && icon.label == "1024 x 768")
            .map(|icon| icon.label)
            .ok_or_else(|| {
                io::Error::other("Display Manager did not refresh its fixed resolution")
            })?;
        click_display_window_control(&wimp, &active_resolution_label)?;
        let resolution_menu = wait_for_display_menu("Resolution", &wimp, &task_error_receiver)?;
        let host_resolution_label = "Window (900 x 700)";
        if !resolution_menu
            .rows
            .iter()
            .any(|row| row.label == host_resolution_label)
        {
            return Err(io::Error::other(format!(
                "Window resolution did not refresh to the current host size: {:?}",
                resolution_menu
                    .rows
                    .iter()
                    .map(|row| &row.label)
                    .collect::<Vec<_>>()
            ))
            .into());
        }
        click_display_menu_row(&wimp, &resolution_menu, host_resolution_label)?;
        wait_for_display_window_icon(&wimp, &task_error_receiver, host_resolution_label)?;
        click_display_window_control(&wimp, "Change")?;
        wait_for_display_manager_closed(&wimp, &task_error_receiver)?;
        let expected_windowed = DisplaySettings {
            resolution: DesktopResolution::Window,
            colour: DisplayColour::Grey256,
        };
        if wimp.display_settings() != expected_windowed
            || wimp.desktop_metrics().pixel_size() != (900, 700)
        {
            return Err(io::Error::other(format!(
                "Window mode did not use the current host size: {:?} {:?}",
                wimp.display_settings(),
                wimp.desktop_metrics().pixel_size()
            ))
            .into());
        }
        if configure.load().map_err(io::Error::other)?.display != expected_windowed {
            return Err(io::Error::other("Window mode change was not persisted").into());
        }
        write_display_manager_scene(
            directory.join("display-manager-windowed.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;

        fs::write(
            &blocked_parent_path,
            b"file blocking ConfigureStore parent creation",
        )?;
        let failing_configure = ConfigureStore::with_path(blocked_parent_path.join("configure"));
        wimp.set_configure_store(failing_configure, expected_windowed);
        click_display_icon(&wimp)?;
        let failure_manager = wait_for_display_manager_window(&wimp, &task_error_receiver)?;
        if failure_manager.handle != singleton_handle {
            return Err(
                io::Error::other("Display Manager lost its singleton window after resize").into(),
            );
        }
        click_display_window_control(&wimp, "256 greys")?;
        let colour_menu = wait_for_display_menu("Colours", &wimp, &task_error_receiver)?;
        click_display_menu_row(&wimp, &colour_menu, "Black/white")?;
        wait_for_display_window_icon(&wimp, &task_error_receiver, "Black/white")?;
        click_display_window_control(&wimp, "Change")?;
        wait_for_display_window_icon(
            &wimp,
            &task_error_receiver,
            "Unable to save display settings.",
        )?;
        if !wimp.desktop_windows().iter().any(|window| {
            window.owner_task_id == DESKTOP_TASK
                && window.title == "Display Manager"
                && window.handle == singleton_handle
        }) {
            return Err(io::Error::other("Display Manager closed after a save failure").into());
        }
        if wimp.display_settings() != expected_windowed
            || configure.load().map_err(io::Error::other)?.display != expected_windowed
        {
            return Err(io::Error::other(
                "Save failure changed the active or persisted Display Manager settings",
            )
            .into());
        }
        write_display_manager_scene(
            directory.join("display-manager-save-error.ppm"),
            &capture_display_manager_scene(&wimp),
        )?;
        click_display_window_control(&wimp, "Cancel")?;
        wait_for_display_manager_closed(&wimp, &task_error_receiver)?;
        if wimp.display_settings() != expected_windowed
            || configure.load().map_err(io::Error::other)?.display != expected_windowed
        {
            return Err(io::Error::other("Cancel did not recover from a save failure").into());
        }
        Ok(())
    })();

    wimp.stop();
    let join = desktop_worker
        .join()
        .map_err(|_| io::Error::other("Display Manager desktop task panicked"));
    let _ = fs::remove_file(&configure_path);
    let _ = fs::remove_file(&blocked_parent_path);
    capture?;
    join?;
    let _ = display_receiver.try_iter().count();
    Ok(())
}

fn capture_display_manager_scene(wimp: &WimpServer) -> DisplayManagerScene {
    DisplayManagerScene {
        windows: wimp.desktop_windows(),
        desktop_icons: wimp.desktop_icons(),
        window_icons: wimp.desktop_window_icons(),
        menus: wimp.desktop_menus(),
        metrics: wimp.desktop_metrics(),
        colour: wimp.display_settings().colour,
    }
}

fn write_display_manager_scene(
    path: impl AsRef<Path>,
    scene: &DisplayManagerScene,
) -> io::Result<()> {
    let metrics = scene.metrics;
    let (logical_width, logical_height) = metrics.pixel_size();
    let width = logical_width.saturating_mul(2);
    let height = logical_height.saturating_mul(2);
    let mut builder = crate::desktop_scene::DesktopSceneBuilder::new();
    let gpu_scene = builder.build(
        &scene.windows,
        &HashMap::new(),
        &scene.desktop_icons,
        &scene.window_icons,
        &scene.menus,
        None,
        crate::desktop_scene::Viewport::for_desktop(
            width,
            height,
            metrics.os_width(),
            metrics.os_height(),
        ),
    );
    let rgba =
        crate::vello_backend::snapshot_scene_with_colour(&gpu_scene, width, height, scene.colour)
            .map_err(io::Error::other)?;
    write_ppm_with_size(path.as_ref(), width, height, &rgba)
}

fn wait_for_display_icon(
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<DesktopIcon, Box<dyn Error>> {
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    loop {
        if let Some(icon) = wimp
            .desktop_icons()
            .into_iter()
            .find(|icon| icon.owner_task_id == DESKTOP_TASK && icon.label == "Display")
        {
            if icon.side != IconBarSide::Applications
                || icon.sprite_name.as_deref() != Some("display")
            {
                return Err(io::Error::other("Display icon has the wrong side or artwork").into());
            }
            return Ok(icon);
        }
        if let Ok(error) = task_errors.try_recv() {
            return Err(io::Error::other(error).into());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "BASIC64 Desktop did not create the Display icon",
            )
            .into());
        }
        thread::sleep(EVENT_WAIT_SLICE);
    }
}

fn click_display_icon(wimp: &WimpServer) -> Result<(), Box<dyn Error>> {
    let icon = wimp
        .desktop_icons()
        .into_iter()
        .find(|icon| icon.owner_task_id == DESKTOP_TASK && icon.label == "Display")
        .ok_or_else(|| io::Error::other("Display icon disappeared"))?;
    click_display_point(wimp, icon.bounds)
}

fn wait_for_display_manager_window(
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<DesktopWindow, Box<dyn Error>> {
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    loop {
        if let Some(window) = wimp.desktop_windows().into_iter().find(|window| {
            window.owner_task_id == DESKTOP_TASK && window.title == "Display Manager"
        }) {
            return Ok(window);
        }
        if let Ok(error) = task_errors.try_recv() {
            return Err(io::Error::other(error).into());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Display icon did not open the Display Manager",
            )
            .into());
        }
        thread::sleep(EVENT_WAIT_SLICE);
    }
}

fn wait_for_display_manager_closed(
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    loop {
        if !wimp
            .desktop_windows()
            .iter()
            .any(|window| window.owner_task_id == DESKTOP_TASK && window.title == "Display Manager")
        {
            return Ok(());
        }
        if let Ok(error) = task_errors.try_recv() {
            return Err(io::Error::other(error).into());
        }
        if Instant::now() >= deadline {
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "Display Manager did not close").into(),
            );
        }
        thread::sleep(EVENT_WAIT_SLICE);
    }
}

fn wait_for_display_menu(
    title: &str,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<DesktopMenu, Box<dyn Error>> {
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    loop {
        if let Some(menu) = wimp
            .desktop_menus()
            .into_iter()
            .find(|menu| menu.title == title)
        {
            return Ok(menu);
        }
        if let Ok(error) = task_errors.try_recv() {
            return Err(io::Error::other(error).into());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Display Manager {title:?} popup did not open"),
            )
            .into());
        }
        thread::sleep(EVENT_WAIT_SLICE);
    }
}

fn click_display_window_control(wimp: &WimpServer, label: &str) -> Result<(), Box<dyn Error>> {
    let window = wimp
        .desktop_windows()
        .into_iter()
        .find(|window| window.owner_task_id == DESKTOP_TASK && window.title == "Display Manager")
        .ok_or_else(|| io::Error::other("Display Manager window is not open"))?;
    let icon = wimp
        .desktop_window_icons()
        .into_iter()
        .find(|icon| icon.window_handle == window.handle && icon.label == label)
        .ok_or_else(|| io::Error::other(format!("Display Manager has no {label:?} control")))?;
    click_display_point(wimp, icon.bounds)
}

fn wait_for_display_window_icon(
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    label: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    loop {
        if let Some(window) = wimp.desktop_windows().into_iter().find(|window| {
            window.owner_task_id == DESKTOP_TASK && window.title == "Display Manager"
        }) {
            if wimp
                .desktop_window_icons()
                .iter()
                .any(|icon| icon.window_handle == window.handle && icon.label == label)
            {
                return Ok(());
            }
        }
        if let Ok(error) = task_errors.try_recv() {
            return Err(io::Error::other(error).into());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("Display Manager did not show {label:?}"),
            )
            .into());
        }
        thread::sleep(EVENT_WAIT_SLICE);
    }
}

fn click_display_menu_row(
    wimp: &WimpServer,
    menu: &DesktopMenu,
    label: &str,
) -> Result<(), Box<dyn Error>> {
    let row = menu
        .rows
        .iter()
        .find(|row| row.label == label)
        .ok_or_else(|| io::Error::other(format!("Display Manager menu has no {label:?} row")))?;
    click_display_point(wimp, row.bounds)
}

fn click_display_point(wimp: &WimpServer, bounds: DesktopRect) -> Result<(), Box<dyn Error>> {
    let x = (bounds.min_x + bounds.max_x) / 2;
    let y = (bounds.min_y + bounds.max_y) / 2;
    if wimp.mouse_down(x, y, 4).is_some() {
        return Err(io::Error::other("Display Manager click started a drag").into());
    }
    wimp.mouse_button_up(4);
    Ok(())
}

fn write_filer_snapshot_mode(
    path: impl AsRef<Path>,
    mode: FilerSnapshotMode,
) -> Result<(), Box<dyn Error>> {
    let output_path = path.as_ref().to_path_buf();
    let (display_sender, display_receiver) = mpsc::channel();
    let (task_error_sender, task_error_receiver) = mpsc::channel();
    let (desktop_updates, _desktop_update_receiver) = mpsc::channel();
    let wimp = WimpServer::new(desktop_updates);
    wimp.task_started(DESKTOP_TASK, "Acorn Desktop")?;

    let mut guests = Vec::new();
    let desktop_source = fs::read_to_string(format!(
        "{}/demo-volume/System/Desktop.bas64",
        env!("CARGO_MANIFEST_DIR")
    ))?;
    let (desktop_input_sender, desktop_input_receiver) = mpsc::channel();
    let task_wimp = Arc::clone(&wimp);
    let task_display = display_sender.clone();
    let task_errors = task_error_sender.clone();
    guests.push(
        thread::Builder::new()
            .name("snapshot-desktop".into())
            .spawn(move || {
                let _keep_input_open = desktop_input_sender;
                let mut runtime = Runtime::desktop_task(
                    DESKTOP_TASK,
                    desktop_input_receiver,
                    task_display,
                    task_wimp.clone(),
                );
                if let Err(error) = runtime.run_application(&desktop_source) {
                    if !matches!(error, crate::error::RuntimeError::EndOfInput) {
                        let _ = task_errors.send(format!("Desktop task failed: {error}"));
                    }
                }
                task_wimp.task_exited(DESKTOP_TASK);
            })?,
    );
    let capture = (|| -> Result<Vec<(std::path::PathBuf, FilerDesktopScene)>, Box<dyn Error>> {
        let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
        let volume_icon = loop {
            if let Some(icon) = wimp.desktop_icons().into_iter().find(|icon| {
                icon.owner_task_id == DESKTOP_TASK && icon.side == crate::wimp::IconBarSide::Devices
            }) {
                break icon;
            }
            if let Ok(error) = task_error_receiver.try_recv() {
                return Err(Box::new(io::Error::other(error)));
            }
            if Instant::now() >= deadline {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "BASIC64 Desktop did not create its HostFS volume icon",
                )));
            }
            let wait = EVENT_WAIT_SLICE.min(deadline.saturating_duration_since(Instant::now()));
            match display_receiver.recv_timeout(wait) {
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Box::new(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "desktop snapshot display channel closed before the volume icon appeared",
                    )));
                }
            }
        };

        // Exercise the real BASIC desktop's OS menu and both launch requests.
        for (row, expected) in [
            (0, crate::wimp::DesktopTaskKind::Commands),
            (1, crate::wimp::DesktopTaskKind::BasicWindow),
        ] {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(error) = task_error_receiver.try_recv() {
                    return Err(io::Error::other(error).into());
                }
                if !wimp.desktop_menus().is_empty() {
                    break;
                }
                wimp.mouse_down(1580, 40, 2);
                if Instant::now() >= deadline {
                    return Err(io::Error::other("OS Task menu did not open").into());
                }
                thread::sleep(Duration::from_millis(10));
            }
            let menu = wimp.desktop_menus().remove(0);
            if menu.title != "Task" || menu.rows.len() != 2 {
                return Err(io::Error::other("Unexpected OS menu").into());
            }
            click_menu_item(&wimp, &menu.rows[row], 4)?;
            loop {
                if let Some(request) = wimp.take_pending_launches().into_iter().next() {
                    if request.kind != expected {
                        return Err(io::Error::other("OS menu launched wrong console").into());
                    }
                    wimp.task_exited(request.task_id);
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::other("OS menu failed to launch console").into());
                }
                thread::sleep(Duration::from_millis(5));
            }
        }

        wimp.mouse_down(
            (volume_icon.bounds.min_x + volume_icon.bounds.max_x) / 2,
            (volume_icon.bounds.min_y + volume_icon.bounds.max_y) / 2,
            4,
        );
        let request = loop {
            if let Some(request) = wimp.take_pending_launches().into_iter().next() {
                if request.guest_path != "$.System.Filer" {
                    return Err(Box::new(io::Error::other(format!(
                        "volume icon launched unexpected task {}",
                        request.guest_path
                    ))));
                }
                break request;
            }
            if let Ok(error) = task_error_receiver.try_recv() {
                return Err(Box::new(io::Error::other(error)));
            }
            if Instant::now() >= deadline {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "selecting the volume did not launch the BASIC64 Filer",
                )));
            }
            let wait = EVENT_WAIT_SLICE.min(deadline.saturating_duration_since(Instant::now()));
            match display_receiver.recv_timeout(wait) {
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Box::new(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "desktop snapshot display channel closed before Filer launch",
                    )));
                }
            }
        };

        let filer_task_id = request.task_id;
        let filer_source = fs::read_to_string(format!(
            "{}/demo-volume/System/Filer.bas64",
            env!("CARGO_MANIFEST_DIR")
        ))?;
        let (filer_input_sender, filer_input_receiver) = mpsc::channel();
        wimp.set_task_input(filer_task_id, filer_input_sender);
        let task_wimp = Arc::clone(&wimp);
        let task_display = display_sender.clone();
        let task_errors = task_error_sender.clone();
        guests.push(
            thread::Builder::new()
                .name("snapshot-filer".into())
                .spawn(move || {
                    let mut runtime = Runtime::desktop_task(
                        filer_task_id,
                        filer_input_receiver,
                        task_display,
                        task_wimp.clone(),
                    );
                    if let Err(error) = runtime.run_application(&filer_source) {
                        if !matches!(error, crate::error::RuntimeError::EndOfInput) {
                            let _ = task_errors.send(format!("Filer task failed: {error}"));
                        }
                    }
                    task_wimp.task_exited(filer_task_id);
                })?,
        );

        let _initial_scene = collect_filer_scene(
            filer_task_id,
            &wimp,
            &display_receiver,
            &task_error_receiver,
        )?;
        exercise_filer_navigation(filer_task_id, &wimp, &task_error_receiver)?;
        exercise_filer_open_request(filer_task_id, &wimp, &task_error_receiver)?;
        if mode == FilerSnapshotMode::Large {
            exercise_filer_resize_request(filer_task_id, &wimp, &task_error_receiver)?;
        }
        if mode == FilerSnapshotMode::Interactions {
            let named_scenes =
                exercise_filer_interactions(filer_task_id, &wimp, &task_error_receiver)?;
            return Ok(named_scenes
                .into_iter()
                .map(|(name, scene)| (output_path.join(name), scene))
                .collect());
        }
        let scene = if mode == FilerSnapshotMode::TaskMenu {
            wimp.mouse_down(1580, 40, 2);
            let deadline = Instant::now() + Duration::from_secs(2);
            while wimp.desktop_menus().is_empty() {
                if Instant::now() >= deadline {
                    return Err(io::Error::other("Task menu capture timed out").into());
                }
                thread::sleep(Duration::from_millis(5));
            }
            capture_filer_desktop_scene(&wimp)
        } else if mode == FilerSnapshotMode::Menu {
            let scene = open_filer_menu_snapshot(filer_task_id, &wimp, &task_error_receiver)?;
            wimp.key_pressed(27);
            wait_for_no_filer_menu(
                &wimp,
                &task_error_receiver,
                "Escape did not close the Filer menu",
            )?;
            scene
        } else {
            capture_filer_desktop_scene(&wimp)
        };
        Ok(vec![(output_path.clone(), scene)])
    })();
    wimp.stop();
    let mut join_error = None;
    for guest in guests {
        if guest.join().is_err() {
            join_error = Some(io::Error::other("desktop snapshot task panicked"));
        }
    }
    let captures = capture?;
    if let Some(error) = join_error {
        return Err(Box::new(error));
    }

    for (capture_path, scene) in captures {
        write_filer_desktop_scene(&capture_path, &scene)?;
    }
    Ok(())
}

fn capture_filer_desktop_scene(wimp: &WimpServer) -> FilerDesktopScene {
    FilerDesktopScene {
        windows: wimp.desktop_windows(),
        desktop_icons: wimp.desktop_icons(),
        window_icons: wimp.desktop_window_icons(),
        menus: wimp.desktop_menus(),
    }
}

fn write_filer_desktop_scene(path: &Path, scene: &FilerDesktopScene) -> io::Result<()> {
    if std::env::var_os("ACORN_VELLO_SNAPSHOT").is_some() {
        let mut builder = crate::desktop_scene::DesktopSceneBuilder::new();
        let gpu_scene = builder.build(
            &scene.windows,
            &HashMap::new(),
            &scene.desktop_icons,
            &scene.window_icons,
            &scene.menus,
            None,
            crate::desktop_scene::Viewport::new(DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT),
        );
        let rgba = crate::vello_backend::snapshot_scene(
            &gpu_scene,
            DESKTOP_PIXEL_WIDTH,
            DESKTOP_PIXEL_HEIGHT,
        )
        .map_err(io::Error::other)?;
        return write_ppm(path, &rgba);
    }
    let mut rgba = vec![0; DESKTOP_PIXEL_WIDTH as usize * DESKTOP_PIXEL_HEIGHT as usize * 4];
    renderer::render_desktop(
        &scene.windows,
        &HashMap::new(),
        &scene.desktop_icons,
        &scene.window_icons,
        &scene.menus,
        None,
        &mut rgba,
    );
    write_ppm(path, &rgba)
}

fn exercise_filer_open_request(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<(), Box<dyn Error>> {
    let window = wimp
        .desktop_windows()
        .into_iter()
        .find(|window| window.owner_task_id == filer_task_id)
        .ok_or_else(|| io::Error::other("Filer window disappeared before move request"))?;
    let title_bar = crate::wimp::desktop_window_furniture(&window)
        .title_bar
        .ok_or_else(|| io::Error::other("Filer window has no title bar"))?;
    let x = (title_bar.min_x + title_bar.max_x) / 2;
    let y = (title_bar.min_y + title_bar.max_y) / 2;
    let drag = wimp
        .mouse_down(x, y, 4)
        .ok_or_else(|| io::Error::other("Filer title bar did not start a move"))?;
    wimp.drag_to(drag, x + 32, y + 8);
    wimp.finish_drag(drag);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while handling Wimp_OpenWindow_Request: {error}"
            ))));
        }
        let moved = wimp.desktop_windows().iter().any(|current| {
            current.owner_task_id == filer_task_id
                && current.handle == window.handle
                && current.work_area.min_x != window.work_area.min_x
        });
        if moved {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                "BASIC64 Filer did not accept Wimp_OpenWindow_Request after a title-bar move",
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn exercise_filer_resize_request(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<(), Box<dyn Error>> {
    for (dx, dy) in [(-240, 80), (480, -240)] {
        exercise_filer_resize_step(filer_task_id, wimp, task_errors, dx, dy)?;
    }
    // A previously unseen child must accept the enlarged parent's width too.
    let directory = filer_icons(filer_task_id, wimp)
        .into_iter()
        .find(|icon| icon.label == "Empty")
        .ok_or_else(|| io::Error::other("Missing Empty directory for resize navigation check"))?;
    click_filer_icon(wimp, &directory, 4)?;
    click_filer_icon(wimp, &directory, 4)?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(error) = task_errors.try_iter().next() {
            return Err(io::Error::other(format!(
                "Opening child after enlarging Filer failed: {error}"
            ))
            .into());
        }
        if wimp
            .desktop_windows()
            .iter()
            .any(|window| window.owner_task_id == filer_task_id && window.title == "HostFS:$.Empty")
        {
            break;
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other("Enlarged Filer did not open its child directory").into());
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

fn exercise_filer_resize_step(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    dx: i32,
    dy: i32,
) -> Result<(), Box<dyn Error>> {
    let window = wimp
        .desktop_windows()
        .into_iter()
        .find(|window| window.owner_task_id == filer_task_id)
        .ok_or_else(|| io::Error::other("Filer window disappeared before resize request"))?;
    let furniture = crate::wimp::desktop_window_furniture(&window);
    let adjust = furniture
        .adjust_size_icon
        .ok_or_else(|| io::Error::other("Filer window has no Adjust Size icon"))?;
    let x = (adjust.min_x + adjust.max_x) / 2;
    let y = (adjust.min_y + adjust.max_y) / 2;
    let drag = wimp
        .mouse_down(x, y, 4)
        .ok_or_else(|| io::Error::other("Filer Adjust Size icon did not start a resize"))?;
    wimp.drag_to(drag, x + dx, y + dy);
    let preview = wimp
        .desktop_windows()
        .into_iter()
        .find(|current| current.handle == window.handle)
        .and_then(|current| current.preview_area)
        .ok_or_else(|| io::Error::other("Filer resize did not produce a preview area"))?;
    let original_width = window.work_area.max_x - window.work_area.min_x;
    let original_height = window.work_area.max_y - window.work_area.min_y;
    if preview.max_x - preview.min_x != original_width + dx
        || preview.max_y - preview.min_y != original_height - dy
    {
        return Err(Box::new(io::Error::other(
            "Filer resize preview was constrained before reaching the requested size",
        )));
    }
    wimp.finish_drag(drag);

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while handling Wimp_OpenWindow_Request after resize: {error}"
            ))));
        }
        let resized = wimp.desktop_windows().iter().any(|current| {
            current.owner_task_id == filer_task_id
                && current.handle == window.handle
                && (current.work_area.max_x - current.work_area.min_x == original_width + dx
                    && current.work_area.max_y - current.work_area.min_y == original_height - dy)
        });
        if resized {
            wait_for_filer_directory_state(
                filer_task_id,
                wimp,
                task_errors,
                false,
                "Filer did not restore its icons after resize",
            )?;
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                "BASIC64 Filer did not accept Wimp_OpenWindow_Request after resize",
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn open_filer_menu_snapshot(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<FilerDesktopScene, Box<dyn Error>> {
    let selection_before = selected_filer_handles(filer_task_id, wimp);
    let context_icon = filer_icons(filer_task_id, wimp)
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::other("Filer has no entry to open its Menu over"))?;
    let x = (context_icon.bounds.min_x + context_icon.bounds.max_x) / 2;
    let y = (context_icon.bounds.min_y + context_icon.bounds.max_y) / 2;
    if wimp.mouse_down(x, y, 2).is_some() {
        return Err(Box::new(io::Error::other(
            "Menu click in the Filer work area unexpectedly started a drag",
        )));
    }

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let menus = wimp.desktop_menus();
        if let Some(menu) = menus.first()
            && menu.title == "Filer"
            && menu.rows.iter().any(|row| row.label == "Display")
            && menu.rows.iter().any(|row| row.label == "Select all")
            && menu.rows.iter().any(|row| row.label == "Clear selection")
        {
            let selection_after = selected_filer_handles(filer_task_id, wimp);
            if selection_after != selection_before {
                return Err(Box::new(io::Error::other(format!(
                    "opening the Filer Menu changed selection from {selection_before:?} to {selection_after:?}"
                ))));
            }
            return Ok(capture_filer_desktop_scene(wimp));
        }
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while opening its Menu: {error}"
            ))));
        }
        if Instant::now() >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "Filer Wimp menu did not appear with Display, Select all and Clear selection; menus: {menus:?}"
                ),
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn exercise_filer_navigation(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<(), Box<dyn Error>> {
    open_system_directory(filer_task_id, wimp, task_errors)?;
    let viewer_count = || {
        wimp.desktop_windows()
            .iter()
            .filter(|w| w.owner_task_id == filer_task_id)
            .count()
    };
    if viewer_count() != 2 {
        return Err(
            io::Error::other("Select must leave both parent and child viewers open").into(),
        );
    }
    let child = wimp
        .desktop_windows()
        .into_iter()
        .find(|w| w.owner_task_id == filer_task_id)
        .unwrap();
    wimp.mouse_down(child.work_area.min_x + 100, child.work_area.max_y + 16, 4);
    wimp.key_pressed(8);
    wait_for_filer_directory_state(
        filer_task_id,
        wimp,
        task_errors,
        false,
        "Backspace did not open the parent directory",
    )?;

    open_system_directory(filer_task_id, wimp, task_errors)?;
    if viewer_count() != 2 {
        return Err(io::Error::other("Reopening a directory created a duplicate viewer").into());
    }
    let menu_scene = open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    let parent_item = menu_scene
        .menus
        .first()
        .and_then(|menu| find_menu_item(menu, "Open parent"))
        .ok_or_else(|| io::Error::other("Filer Menu did not expose Open parent"))?;
    click_menu_item(wimp, parent_item, 4)?;
    wait_for_no_filer_menu(
        wimp,
        task_errors,
        "Select on Open parent did not dismiss the menu",
    )?;
    wait_for_filer_directory_state(
        filer_task_id,
        wimp,
        task_errors,
        false,
        "Open parent menu selection did not open the parent directory",
    )?;

    let system_icon = wimp
        .desktop_window_icons()
        .into_iter()
        .find(|icon| icon.owner_task_id == filer_task_id && icon.label == "System")
        .ok_or_else(|| io::Error::other("Filer root view did not restore the System folder"))?;
    let root = wimp
        .desktop_windows()
        .into_iter()
        .find(|w| w.owner_task_id == filer_task_id)
        .unwrap();
    wimp.mouse_down(root.work_area.min_x + 100, root.work_area.max_y + 16, 4);
    wimp.key_pressed(91);
    wait_for_filer_icon_recreated(filer_task_id, wimp, task_errors, system_icon.handle)?;
    wimp.key_pressed(93);
    // Adjust double-click replaces the source viewer; Adjust-Close reverses it.
    thread::sleep(Duration::from_millis(50));
    let system = filer_icons(filer_task_id, wimp)
        .into_iter()
        .find(|i| i.label == "System")
        .unwrap();
    click_filer_icon(wimp, &system, 1)?;
    click_filer_icon(wimp, &system, 1)?;
    wait_for_filer_directory_state(
        filer_task_id,
        wimp,
        task_errors,
        true,
        "Adjust did not open child",
    )?;
    if viewer_count() != 1 {
        return Err(io::Error::other("Adjust opening must close the parent viewer").into());
    }
    let (child, furniture) = wimp
        .furniture_layout()
        .into_iter()
        .find(|(w, _)| w.owner_task_id == filer_task_id)
        .unwrap();
    let close = furniture.close_icon.unwrap();
    wimp.mouse_down(
        (close.min_x + close.max_x) / 2,
        (close.min_y + close.max_y) / 2,
        1,
    );
    wait_for_filer_directory_state(
        filer_task_id,
        wimp,
        task_errors,
        false,
        "Adjust-Close did not return to parent",
    )?;
    let parent = wimp
        .desktop_windows()
        .into_iter()
        .find(|w| w.owner_task_id == filer_task_id)
        .unwrap();
    if viewer_count() != 1
        || parent.work_area.min_x != child.work_area.min_x
        || parent.work_area.max_y != child.work_area.max_y
    {
        return Err(io::Error::other(
            "Adjust-Close must replace child at the same close-button position",
        )
        .into());
    }
    if let Some(error) = task_errors.try_iter().next() {
        return Err(Box::new(io::Error::other(format!(
            "BASIC64 Filer failed while handling the Next page key: {error}"
        ))));
    }
    Ok(())
}

fn open_system_directory(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<(), Box<dyn Error>> {
    let system_icon = wimp
        .desktop_window_icons()
        .into_iter()
        .find(|icon| icon.owner_task_id == filer_task_id && icon.label == "System")
        .ok_or_else(|| io::Error::other("Filer root view has no System folder icon"))?;
    let x = (system_icon.bounds.min_x + system_icon.bounds.max_x) / 2;
    let y = (system_icon.bounds.min_y + system_icon.bounds.max_y) / 2;
    if wimp.mouse_down(x, y, 4).is_some() {
        return Err(Box::new(io::Error::other(
            "Selecting the System folder unexpectedly started a drag",
        )));
    }
    if wimp.mouse_down(x, y, 4).is_some() {
        return Err(Box::new(io::Error::other(
            "Opening the System folder unexpectedly started a drag",
        )));
    }
    wait_for_filer_directory_state(
        filer_task_id,
        wimp,
        task_errors,
        true,
        "Return did not open the selected System directory",
    )
}

fn wait_for_filer_directory_state(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    in_system: bool,
    timeout_message: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut stable_samples = 0;
    loop {
        let front = wimp
            .desktop_windows()
            .into_iter()
            .find(|w| w.owner_task_id == filer_task_id)
            .map(|w| w.handle);
        let icons = wimp
            .desktop_window_icons()
            .into_iter()
            .filter(|icon| icon.owner_task_id == filer_task_id && Some(icon.window_handle) == front)
            .collect::<Vec<_>>();
        let has_system = icons.iter().any(|icon| icon.label == "System");
        let has_system_contents = icons
            .iter()
            .any(|icon| icon.label.starts_with("Desktop") || icon.label.starts_with("Filer"));
        let state_reached = if in_system {
            !has_system && has_system_contents
        } else {
            has_system && !has_system_contents
        };
        if state_reached {
            stable_samples += 1;
            if stable_samples >= 5 {
                return Ok(());
            }
        } else {
            stable_samples = 0;
        }
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while navigating directories: {error}"
            ))));
        }
        if Instant::now() >= deadline {
            let labels = icons
                .iter()
                .map(|icon| icon.label.as_str())
                .collect::<Vec<_>>();
            let focused = wimp
                .desktop_windows()
                .into_iter()
                .find(|window| window.owner_task_id == filer_task_id)
                .map(|window| window.focused);
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{timeout_message}; labels: {labels:?}; focused: {focused:?}"),
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_filer_icon_recreated(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    previous_handle: u32,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let recreated = wimp.desktop_window_icons().into_iter().any(|icon| {
            icon.owner_task_id == filer_task_id
                && icon.label == "System"
                && icon.handle != previous_handle
        });
        if recreated {
            return Ok(());
        }
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while handling Previous page: {error}"
            ))));
        }
        if Instant::now() >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                "Previous page key did not refresh the Filer's first page",
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_no_filer_menu(
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    timeout_message: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if wimp.desktop_menus().is_empty() {
            return Ok(());
        }
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while closing its Wimp menu: {error}"
            ))));
        }
        if Instant::now() >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{timeout_message}; menus: {:?}", wimp.desktop_menus()),
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn find_menu_item<'a>(menu: &'a DesktopMenu, label: &str) -> Option<&'a DesktopMenuItem> {
    menu.rows.iter().find(|row| row.label == label)
}

fn click_menu_item(
    wimp: &WimpServer,
    item: &DesktopMenuItem,
    buttons: u32,
) -> Result<(), Box<dyn Error>> {
    if item.shaded {
        return Err(Box::new(io::Error::other(format!(
            "Filer menu item {:?} is shaded",
            item.label
        ))));
    }
    let x = (item.bounds.min_x + item.bounds.max_x) / 2;
    let y = (item.bounds.min_y + item.bounds.max_y) / 2;
    if wimp.mouse_down(x, y, buttons).is_some() {
        return Err(Box::new(io::Error::other(format!(
            "menu selection on {:?} unexpectedly started a drag",
            item.label
        ))));
    }
    Ok(())
}

fn exercise_filer_interactions(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
) -> Result<Vec<(String, FilerDesktopScene)>, Box<dyn Error>> {
    let initial_icons = filer_icons(filer_task_id, wimp);
    if initial_icons.len() < 2 {
        return Err(Box::new(io::Error::other(format!(
            "Filer has fewer than two selectable entries: {initial_icons:?}"
        ))));
    }
    let first = initial_icons[0].clone();
    let second = initial_icons[1].clone();
    let selected_prefixes = vec![first.label.clone(), second.label.clone()];

    click_filer_icon(wimp, &first, 4)?;
    wait_for_filer_selection(
        filer_task_id,
        wimp,
        task_errors,
        [first.handle].into_iter().collect(),
        "Select did not replace the Filer selection with the first entry",
    )?;
    click_filer_icon(wimp, &second, 4)?;
    wait_for_filer_selection(
        filer_task_id,
        wimp,
        task_errors,
        [second.handle].into_iter().collect(),
        "Select did not replace the Filer selection with the second entry",
    )?;
    click_filer_icon(wimp, &first, 1)?;
    wait_for_filer_selection(
        filer_task_id,
        wimp,
        task_errors,
        [first.handle, second.handle].into_iter().collect(),
        "Adjust did not add the first entry to the selection",
    )?;
    let mut captures = vec![(
        "selection-adjust.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    )];

    let main_menu = open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    captures.push(("filer-menu.ppm".to_owned(), main_menu));
    open_menu_submenu(wimp, task_errors, "Display")?;
    let display_menu = menu_with_row(wimp, "Large icons")
        .ok_or_else(|| io::Error::other("Display submenu did not contain the Large icons row"))?;
    for required in [
        "Large icons",
        "Small icons",
        "Full info",
        "Sort by name",
        "Sort by type",
        "Sort by size",
        "Sort by date",
    ] {
        if find_menu_item(&display_menu, required).is_none() {
            return Err(Box::new(io::Error::other(format!(
                "Display submenu is missing {required:?}; rows: {:?}",
                display_menu.rows
            ))));
        }
    }
    if !find_menu_item(&display_menu, "Large icons").is_some_and(|row| row.tick) {
        return Err(Box::new(io::Error::other(
            "Filer's default Large icons layout is not ticked in Display",
        )));
    }
    captures.push((
        "display-submenu.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));
    click_live_menu_item(wimp, "Small icons", 4)?;
    wait_for_no_filer_menu(
        wimp,
        task_errors,
        "Select on Small icons did not dismiss the menu",
    )?;
    wait_for_filer_selected_labels(
        filer_task_id,
        wimp,
        task_errors,
        &selected_prefixes,
        "Small icons did not preserve the explicit selection",
    )?;
    captures.push((
        "display-small-selection.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));

    open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    open_menu_submenu(wimp, task_errors, "Display")?;
    let display_menu = menu_with_row(wimp, "Small icons").ok_or_else(|| {
        io::Error::other("Display submenu disappeared after choosing Small icons")
    })?;
    if !find_menu_item(&display_menu, "Small icons").is_some_and(|row| row.tick) {
        return Err(Box::new(io::Error::other(
            "Small icons layout was not ticked in the Display submenu",
        )));
    }
    captures.push((
        "display-small-menu.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));
    click_live_menu_item(wimp, "Full info", 4)?;
    wait_for_no_filer_menu(
        wimp,
        task_errors,
        "Select on Full info did not dismiss the menu",
    )?;
    wait_for_filer_selected_labels(
        filer_task_id,
        wimp,
        task_errors,
        &selected_prefixes,
        "Full info did not preserve the explicit selection",
    )?;
    let full_info_icons = filer_icons(filer_task_id, wimp);
    if !full_info_icons.iter().any(|icon| {
        icon.label.starts_with(&selected_prefixes[0])
            && icon.label.contains("BASIC")
            && icon.label.contains("bytes")
    }) {
        return Err(Box::new(io::Error::other(format!(
            "Full info did not show the selected file's type and size: {full_info_icons:?}"
        ))));
    }
    captures.push((
        "display-full-info-selection.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));

    open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    open_menu_submenu(wimp, task_errors, "Display")?;
    let display_menu = menu_with_row(wimp, "Full info")
        .ok_or_else(|| io::Error::other("Display submenu disappeared after choosing Full info"))?;
    if !find_menu_item(&display_menu, "Full info").is_some_and(|row| row.tick) {
        return Err(Box::new(io::Error::other(
            "Full info layout was not ticked in the Display submenu",
        )));
    }
    click_live_menu_item(wimp, "Large icons", 4)?;
    wait_for_no_filer_menu(
        wimp,
        task_errors,
        "Select on Large icons did not dismiss the menu",
    )?;
    wait_for_filer_selected_labels(
        filer_task_id,
        wimp,
        task_errors,
        &selected_prefixes,
        "Large icons did not preserve the explicit selection",
    )?;

    open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    open_menu_submenu(wimp, task_errors, "Display")?;
    let display_menu = menu_with_row(wimp, "Large icons").ok_or_else(|| {
        io::Error::other("Display submenu disappeared after choosing Large icons")
    })?;
    if !find_menu_item(&display_menu, "Large icons").is_some_and(|row| row.tick) {
        return Err(Box::new(io::Error::other(
            "Large icons layout was not ticked after returning from Full info",
        )));
    }
    click_live_menu_item(wimp, "Sort by type", 4)?;
    wait_for_no_filer_menu(
        wimp,
        task_errors,
        "Select on Sort by type did not dismiss the menu",
    )?;
    wait_for_filer_selected_labels(
        filer_task_id,
        wimp,
        task_errors,
        &selected_prefixes,
        "sorting by type did not preserve the explicit selection",
    )?;
    open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    open_menu_submenu(wimp, task_errors, "Display")?;
    let display_menu = menu_with_row(wimp, "Sort by type")
        .ok_or_else(|| io::Error::other("Display submenu disappeared after sorting by type"))?;
    if !find_menu_item(&display_menu, "Sort by type").is_some_and(|row| row.tick)
        || find_menu_item(&display_menu, "Sort by name").is_some_and(|row| row.tick)
    {
        return Err(Box::new(io::Error::other(
            "Sort by type did not become the sole active sort tick",
        )));
    }
    captures.push((
        "display-sort-type-menu.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));
    wimp.key_pressed(27);
    wait_for_no_filer_menu(wimp, task_errors, "Escape did not close the Display menu")?;

    let second = filer_icon_with_prefix(filer_task_id, wimp, &selected_prefixes[1])?;
    click_filer_icon(wimp, &second, 1)?;
    wait_for_filer_selected_labels(
        filer_task_id,
        wimp,
        task_errors,
        &selected_prefixes[..1],
        "Adjust did not remove the second entry after changing Display modes",
    )?;
    captures.push((
        "selection-adjust-toggle-off.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));

    let background = filer_background_point(filer_task_id, wimp)?;
    if wimp.mouse_down(background.0, background.1, 4).is_some() {
        return Err(Box::new(io::Error::other(
            "background Select unexpectedly started a drag",
        )));
    }
    wait_for_filer_selection(
        filer_task_id,
        wimp,
        task_errors,
        BTreeSet::new(),
        "background Select did not clear the Filer selection",
    )?;

    let main_menu = open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    captures.push(("filer-menu-cleared.ppm".to_owned(), main_menu));
    open_menu_submenu(wimp, task_errors, "Display")?;
    let display_menu = menu_with_row(wimp, "Large icons")
        .ok_or_else(|| io::Error::other("Display submenu did not contain the Large icons row"))?;
    if !find_menu_item(&display_menu, "Large icons").is_some_and(|row| row.tick)
        || !find_menu_item(&display_menu, "Sort by type").is_some_and(|row| row.tick)
    {
        return Err(Box::new(io::Error::other(
            "Large icons and Sort by type state was not retained in Display",
        )));
    }
    captures.push((
        "display-cleared-menu.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));
    wimp.key_pressed(27);
    wait_for_no_filer_menu(
        wimp,
        task_errors,
        "Escape did not close the Display submenu",
    )?;

    let all_handles = filer_icons(filer_task_id, wimp)
        .into_iter()
        .map(|icon| icon.handle)
        .collect::<BTreeSet<_>>();
    open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    click_live_menu_item(wimp, "Select all", 4)?;
    wait_for_no_filer_menu(wimp, task_errors, "Select all did not dismiss its menu")?;
    wait_for_filer_selection(
        filer_task_id,
        wimp,
        task_errors,
        all_handles,
        "Select all did not select every visible Filer entry",
    )?;
    captures.push((
        "selection-all.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));

    open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    click_live_menu_item(wimp, "Clear selection", 1)?;
    wait_for_filer_selection(
        filer_task_id,
        wimp,
        task_errors,
        BTreeSet::new(),
        "Adjust on Clear selection did not clear selected entries",
    )?;
    wait_for_menu_levels(
        wimp,
        task_errors,
        1,
        "Adjust on Clear selection did not retain the menu tree",
    )?;
    captures.push((
        "menu-adjust-retained.ppm".to_owned(),
        capture_filer_desktop_scene(wimp),
    ));

    wimp.key_pressed(27);
    wait_for_no_filer_menu(wimp, task_errors, "Escape did not close the Filer menu")?;
    open_filer_menu_snapshot(filer_task_id, wimp, task_errors)?;
    let outside = (
        DESKTOP_PIXEL_WIDTH as i32 - 16,
        DESKTOP_PIXEL_HEIGHT as i32 - 16,
    );
    wimp.mouse_down(outside.0, outside.1, 4);
    wait_for_no_filer_menu(
        wimp,
        task_errors,
        "an outside Select did not close the Filer menu",
    )?;
    Ok(captures)
}

fn filer_icons(filer_task_id: u64, wimp: &WimpServer) -> Vec<DesktopWindowIcon> {
    let front = wimp
        .desktop_windows()
        .into_iter()
        .find(|w| w.owner_task_id == filer_task_id)
        .map(|w| w.handle);
    wimp.desktop_window_icons()
        .into_iter()
        .filter(|icon| {
            icon.owner_task_id == filer_task_id
                && Some(icon.window_handle) == front
                && icon.flags & (1 << 22) == 0
                && icon.label != ""
        })
        .collect()
}

fn click_filer_icon(
    wimp: &WimpServer,
    icon: &DesktopWindowIcon,
    buttons: u32,
) -> Result<(), Box<dyn Error>> {
    let x = (icon.bounds.min_x + icon.bounds.max_x) / 2;
    let y = (icon.bounds.min_y + icon.bounds.max_y) / 2;
    if wimp.mouse_down(x, y, buttons).is_some() {
        return Err(Box::new(io::Error::other(format!(
            "click on Filer entry {:?} unexpectedly started a drag",
            icon.label
        ))));
    }
    Ok(())
}

fn selected_filer_handles(filer_task_id: u64, wimp: &WimpServer) -> BTreeSet<u32> {
    filer_icons(filer_task_id, wimp)
        .into_iter()
        .filter(|icon| icon.flags & (1 << 21) != 0)
        .map(|icon| icon.handle)
        .collect()
}

fn selected_filer_labels(filer_task_id: u64, wimp: &WimpServer) -> Vec<String> {
    let mut labels = filer_icons(filer_task_id, wimp)
        .into_iter()
        .filter(|icon| icon.flags & (1 << 21) != 0)
        .map(|icon| icon.label)
        .collect::<Vec<_>>();
    labels.sort();
    labels
}

fn wait_for_filer_selected_labels(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    expected_prefixes: &[String],
    timeout_message: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut stable_samples = 0;
    loop {
        let labels = selected_filer_labels(filer_task_id, wimp);
        let expected = labels.len() == expected_prefixes.len()
            && expected_prefixes
                .iter()
                .all(|prefix| labels.iter().any(|label| label.starts_with(prefix)));
        if expected {
            stable_samples += 1;
            if stable_samples >= 3 {
                return Ok(());
            }
        } else {
            stable_samples = 0;
        }
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while preserving selection across Display changes: {error}"
            ))));
        }
        if Instant::now() >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{timeout_message}; selected labels: {labels:?}"),
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn filer_icon_with_prefix(
    filer_task_id: u64,
    wimp: &WimpServer,
    prefix: &str,
) -> Result<DesktopWindowIcon, Box<dyn Error>> {
    filer_icons(filer_task_id, wimp)
        .into_iter()
        .find(|icon| icon.label.starts_with(prefix))
        .ok_or_else(|| {
            io::Error::other(format!("Filer has no entry beginning with {prefix:?}")).into()
        })
}

fn wait_for_filer_selection(
    filer_task_id: u64,
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    expected: BTreeSet<u32>,
    timeout_message: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut stable_samples = 0;
    loop {
        if selected_filer_handles(filer_task_id, wimp) == expected {
            stable_samples += 1;
            if stable_samples >= 3 {
                return Ok(());
            }
        } else {
            stable_samples = 0;
        }
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while updating selection: {error}"
            ))));
        }
        if Instant::now() >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "{timeout_message}; selected: {:?}",
                    selected_filer_handles(filer_task_id, wimp)
                ),
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn filer_background_point(
    filer_task_id: u64,
    wimp: &WimpServer,
) -> Result<(i32, i32), Box<dyn Error>> {
    let window = wimp
        .desktop_windows()
        .into_iter()
        .find(|window| window.owner_task_id == filer_task_id)
        .ok_or_else(|| io::Error::other("Filer window disappeared before background click"))?;
    let icons = filer_icons(filer_task_id, wimp);
    let preferred = (window.work_area.max_x - 16, window.work_area.max_y - 16);
    if icons
        .iter()
        .all(|icon| !icon.bounds.contains(preferred.0, preferred.1))
    {
        return Ok(preferred);
    }
    for y_step in 1..20 {
        for x_step in 1..20 {
            let point = (
                window.work_area.max_x - x_step * 16,
                window.work_area.max_y - y_step * 16,
            );
            if icons
                .iter()
                .all(|icon| !icon.bounds.contains(point.0, point.1))
            {
                return Ok(point);
            }
        }
    }
    Err(Box::new(io::Error::other(
        "could not find an empty point in the Filer work area",
    )))
}

fn open_menu_submenu(
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    row_label: &str,
) -> Result<(), Box<dyn Error>> {
    let menu = wimp
        .desktop_menus()
        .into_iter()
        .find(|menu| find_menu_item(menu, row_label).is_some())
        .ok_or_else(|| io::Error::other(format!("Filer menu has no {row_label:?} row")))?;
    let row = find_menu_item(&menu, row_label)
        .cloned()
        .expect("row was checked");
    if !row.has_submenu || row.shaded {
        return Err(Box::new(io::Error::other(format!(
            "Filer menu row {row_label:?} does not expose an enabled submenu"
        ))));
    }
    let x = (row.bounds.min_x + row.bounds.max_x) / 2;
    let y = (row.bounds.min_y + row.bounds.max_y) / 2;
    wimp.mouse_move(x, y);
    let deadline = wimp.next_menu_hover_deadline().ok_or_else(|| {
        io::Error::other(format!(
            "hovering {row_label:?} did not arm a submenu delay"
        ))
    })?;
    if !wimp.advance_menu_hover_at(deadline) {
        return Err(Box::new(io::Error::other(format!(
            "hovering {row_label:?} did not open its submenu at the recorded deadline"
        ))));
    }
    wait_for_menu_levels(
        wimp,
        task_errors,
        2,
        &format!("hovering {row_label:?} did not open a cascading menu"),
    )
}

fn menu_with_row(wimp: &WimpServer, label: &str) -> Option<DesktopMenu> {
    wimp.desktop_menus()
        .into_iter()
        .find(|menu| find_menu_item(menu, label).is_some())
}

fn click_live_menu_item(
    wimp: &WimpServer,
    label: &str,
    buttons: u32,
) -> Result<(), Box<dyn Error>> {
    let menu = menu_with_row(wimp, label)
        .ok_or_else(|| io::Error::other(format!("visible Filer menu has no {label:?} row")))?;
    let item = find_menu_item(&menu, label)
        .cloned()
        .expect("row was checked");
    click_menu_item(wimp, &item, buttons)
}

fn wait_for_menu_levels(
    wimp: &WimpServer,
    task_errors: &mpsc::Receiver<String>,
    minimum_levels: usize,
    timeout_message: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let menus = wimp.desktop_menus();
        if menus.len() >= minimum_levels {
            return Ok(());
        }
        if let Some(error) = task_errors.try_iter().next() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed while traversing its menu: {error}"
            ))));
        }
        if Instant::now() >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{timeout_message}; visible menus: {menus:?}"),
            )));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn collect_filer_scene(
    filer_task_id: u64,
    wimp: &WimpServer,
    display_events: &mpsc::Receiver<DisplayEvent>,
    task_errors: &mpsc::Receiver<String>,
) -> Result<
    (
        Vec<DesktopWindow>,
        Vec<crate::wimp::DesktopIcon>,
        Vec<crate::wimp::DesktopWindowIcon>,
    ),
    Box<dyn Error>,
> {
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    let mut previous_icon_count = 0;
    let mut stable_icon_count = 0;

    loop {
        let windows = wimp.desktop_windows();
        let filer_open = windows
            .iter()
            .any(|window| window.owner_task_id == filer_task_id && window.title == "HostFS:$");
        let window_icons = wimp.desktop_window_icons();
        let filer_icon_count = window_icons
            .iter()
            .filter(|icon| icon.owner_task_id == filer_task_id)
            .count();
        // The BASIC Filer publishes only catalogue entries as work-area
        // icons; Wimp menu panels are transient renderer-owned overlays.
        if filer_open && filer_icon_count >= 4 {
            if window_icons.len() == previous_icon_count {
                stable_icon_count += 1;
            } else {
                previous_icon_count = window_icons.len();
                stable_icon_count = 0;
            }
            if stable_icon_count >= 3 {
                return Ok((windows, wimp.desktop_icons(), window_icons));
            }
        }

        if let Ok(error) = task_errors.try_recv() {
            return Err(Box::new(io::Error::other(format!(
                "BASIC64 Filer failed during startup: {error}"
            ))));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "BASIC64 Filer did not publish its first window and icons; windows: {windows:?}"
                ),
            )));
        }
        let wait = EVENT_WAIT_SLICE.min(deadline.saturating_duration_since(now));
        match display_events.recv_timeout(wait) {
            Ok(
                DisplayEvent::WriteByte { .. }
                | DisplayEvent::Plot { .. }
                | DisplayEvent::GraphicsSnapshot { .. }
                | DisplayEvent::DesktopStarted
                | DisplayEvent::DesktopChanged
                | DisplayEvent::RuntimeExited,
            ) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Ok(error) = task_errors.try_recv() {
                    return Err(Box::new(io::Error::other(format!(
                        "BASIC64 Filer failed during startup: {error}"
                    ))));
                }
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Filer snapshot display event channel closed before capture",
                )));
            }
        }
    }
}

/// Save visible specimens from the original RISC OS 3.71 outline families.
pub fn write_riscos_font_specimen(path: impl AsRef<Path>) -> Result<(), Box<dyn Error>> {
    let mut rgba = vec![0; DESKTOP_PIXEL_WIDTH as usize * DESKTOP_PIXEL_HEIGHT as usize * 4];
    riscos_font::render_font_specimen(&mut rgba, DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT)?;
    write_ppm(path.as_ref(), &rgba)?;
    Ok(())
}

fn collect_demo_scene(
    wimp: &WimpServer,
    display_events: &mpsc::Receiver<DisplayEvent>,
    task_errors: &mpsc::Receiver<String>,
) -> Result<
    (
        Vec<DesktopWindow>,
        HashMap<(u64, Option<u32>), GraphicsSnapshot>,
    ),
    Box<dyn Error>,
> {
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    let mut graphics = HashMap::<(u64, Option<u32>), GraphicsService>::new();

    loop {
        let windows = wimp.desktop_windows();
        let has_alpha = windows
            .iter()
            .any(|window| window.owner_task_id == ALPHA_TASK && window.title == "Alpha App");
        let has_beta = windows
            .iter()
            .any(|window| window.owner_task_id == BETA_TASK && window.title == "Beta App");
        let alpha_handle = windows
            .iter()
            .find(|window| window.owner_task_id == ALPHA_TASK && window.title == "Alpha App")
            .map(|window| window.handle);
        let beta_handle = windows
            .iter()
            .find(|window| window.owner_task_id == BETA_TASK && window.title == "Beta App")
            .map(|window| window.handle);
        let alpha_painted = alpha_handle
            .and_then(|handle| graphics.get(&(ALPHA_TASK, Some(handle))))
            .is_some_and(|scene| has_text(scene.snapshot(), ALPHA_READY_TEXT));
        let beta_painted = beta_handle
            .and_then(|handle| graphics.get(&(BETA_TASK, Some(handle))))
            .is_some_and(|scene| has_text(scene.snapshot(), BETA_READY_TEXT));
        if has_alpha && has_beta && alpha_painted && beta_painted {
            let scenes = graphics
                .into_iter()
                .map(|(surface, graphics)| (surface, graphics.snapshot().clone()))
                .collect();
            return Ok((windows, scenes));
        }

        if let Ok(error) = task_errors.try_recv() {
            return Err(Box::new(io::Error::other(error)));
        }
        let now = Instant::now();
        if now >= deadline {
            let scene_status = graphics
                .iter()
                .map(|(surface, scene)| {
                    let snapshot = scene.snapshot();
                    (
                        surface,
                        snapshot.mode.number,
                        snapshot.revision,
                        snapshot.wimp_clip,
                        has_text(snapshot, ALPHA_READY_TEXT),
                        has_text(snapshot, BETA_READY_TEXT),
                        snapshot
                            .text_cells
                            .chunks(usize::from(snapshot.mode.text_columns.max(1)))
                            .take(7)
                            .map(|row| String::from_utf8_lossy(row).trim_end().to_owned())
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>();
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "desktop demo did not publish both initial scenes; windows: {windows:?}; graphics: {scene_status:?}"
                ),
            )));
        }
        let wait = EVENT_WAIT_SLICE.min(deadline.saturating_duration_since(now));
        match display_events.recv_timeout(wait) {
            Ok(DisplayEvent::WriteByte {
                task_id,
                window_handle,
                byte,
            }) => {
                graphics
                    .entry((task_id, window_handle))
                    .or_default()
                    .write_byte(byte)?;
            }
            Ok(DisplayEvent::Plot {
                task_id,
                window_handle,
                code,
                x,
                y,
            }) => {
                graphics
                    .entry((task_id, window_handle))
                    .or_default()
                    .plot(code, x, y)?;
            }
            Ok(DisplayEvent::GraphicsSnapshot {
                task_id,
                window_handle,
                snapshot,
            }) => {
                graphics.insert(
                    (task_id, window_handle),
                    GraphicsService::from_snapshot(snapshot),
                );
            }
            Ok(
                DisplayEvent::DesktopStarted
                | DisplayEvent::DesktopChanged
                | DisplayEvent::RuntimeExited,
            ) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Box::new(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "desktop demo display event channel closed before capture",
                )));
            }
        }
    }
}

fn has_text(snapshot: &GraphicsSnapshot, needle: &[u8]) -> bool {
    snapshot
        .text_cells
        .windows(needle.len())
        .any(|cells| cells == needle)
}

fn write_ppm(path: &Path, rgba: &[u8]) -> io::Result<()> {
    write_ppm_with_size(path, DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT, rgba)
}

fn write_ppm_with_size(path: &Path, width: u32, height: u32, rgba: &[u8]) -> io::Result<()> {
    let pixel_count = width as usize * height as usize;
    let expected_rgba_bytes = pixel_count * 4;
    if rgba.len() < expected_rgba_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rendered desktop buffer is shorter than its pixel dimensions",
        ));
    }
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    ppm.reserve(pixel_count * 3);
    for pixel in rgba[..expected_rgba_bytes].chunks_exact(4) {
        ppm.extend_from_slice(&pixel[..3]);
    }
    fs::write(path, ppm)
}

#[cfg(test)]
mod display_manager_name_tests {
    use super::*;

    #[test]
    fn desktop_display_name_functions_return_every_menu_label() {
        let desktop_source = include_str!("../demo-volume/System/Desktop.bas64");
        let definitions_start = desktop_source
            .find("DEF FNcolour_name$(")
            .expect("Desktop should define display name functions");
        let definitions = &desktop_source[definitions_start..];
        let colour_names = [
            "Black/white",
            "4 greys",
            "16 greys",
            "16 colours",
            "256 greys",
            "256 colours",
            "32 thousand",
            "16 million",
        ];
        let resolution_names = [
            "Window (900 x 700)",
            "640 x 480",
            "800 x 600",
            "1024 x 768",
            "1152 x 864",
            "1280 x 1024",
            "1600 x 1200",
        ];
        let mut calls = String::from("DIM dmcolours$(7)\nDIM dmresolutions$(6)\n");
        for (id, name) in colour_names.iter().enumerate() {
            calls.push_str(&format!("dmcolours$({id})=\"{name}\"\n"));
        }
        for (id, name) in resolution_names.iter().enumerate() {
            calls.push_str(&format!("dmresolutions$({id})=\"{name}\"\n"));
        }
        for id in 0..colour_names.len() {
            calls.push_str(&format!("PRINT FNcolour_name$({id})\n"));
        }
        for id in 0..resolution_names.len() {
            calls.push_str(&format!("PRINT FNresolution_menu_name$({id})\n"));
            calls.push_str(&format!("PRINT FNresolution_name$({id})\n"));
        }
        calls.push_str("END\n");
        let source = format!("{calls}{definitions}");

        let (_input_sender, input_receiver) = mpsc::channel();
        let (display_sender, display_receiver) = mpsc::channel();
        let (updates, _update_receiver) = mpsc::channel();
        let wimp = WimpServer::new(updates);
        let configure_path = std::env::temp_dir().join(format!(
            "acorn-2026-display-name-test-{}.configure",
            std::process::id()
        ));
        let _ = fs::remove_file(&configure_path);
        wimp.set_configure_store(
            ConfigureStore::with_path(&configure_path),
            DisplaySettings::default(),
        );
        wimp.task_started(991, "display-name-test")
            .expect("test task should register with the hosted Wimp");
        let mut runtime = Runtime::desktop_task(991, input_receiver, display_sender, wimp.clone());
        runtime
            .run_application(&source)
            .expect("Desktop display-name functions should execute");

        let output = display_receiver
            .try_iter()
            .filter_map(|event| match event {
                DisplayEvent::WriteByte { byte, .. } => Some(char::from(byte)),
                _ => None,
            })
            .collect::<String>();
        for label in colour_names.into_iter().chain(resolution_names) {
            assert!(
                output.contains(label),
                "output omitted {label:?}: {output:?}"
            );
        }
        wimp.stop();
        let _ = fs::remove_file(configure_path);
    }
}
