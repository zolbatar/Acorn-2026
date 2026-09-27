//! Headless snapshots of the real two-task Wimp demonstration.

use std::{
    collections::HashMap,
    error::Error,
    fs, io,
    path::Path,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

use crate::{
    graphics::{GraphicsService, GraphicsSnapshot},
    renderer, riscos_font,
    runtime::Runtime,
    swi::DisplayEvent,
    wimp::{DESKTOP_PIXEL_HEIGHT, DESKTOP_PIXEL_WIDTH, DesktopWindow, WimpServer},
};

const ALPHA_TASK: u64 = 101;
const BETA_TASK: u64 = 102;
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);
const EVENT_WAIT_SLICE: Duration = Duration::from_millis(50);
const ALPHA_READY_TEXT: &[u8] = b"Menu in the work area.";
const BETA_READY_TEXT: &[u8] = b"Drag the title or size icon.";

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
    renderer::render_desktop(&windows, &scenes, &mut rgba);
    write_ppm(path.as_ref(), &rgba)?;
    Ok(())
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
) -> Result<(Vec<DesktopWindow>, HashMap<u64, GraphicsSnapshot>), Box<dyn Error>> {
    let deadline = Instant::now() + SNAPSHOT_TIMEOUT;
    let mut graphics = HashMap::<u64, GraphicsService>::new();

    loop {
        let windows = wimp.desktop_windows();
        let has_alpha = windows
            .iter()
            .any(|window| window.owner_task_id == ALPHA_TASK && window.title == "Alpha App");
        let has_beta = windows
            .iter()
            .any(|window| window.owner_task_id == BETA_TASK && window.title == "Beta App");
        let alpha_painted = graphics
            .get(&ALPHA_TASK)
            .is_some_and(|scene| has_text(scene.snapshot(), ALPHA_READY_TEXT));
        let beta_painted = graphics
            .get(&BETA_TASK)
            .is_some_and(|scene| has_text(scene.snapshot(), BETA_READY_TEXT));
        if has_alpha && has_beta && alpha_painted && beta_painted {
            let scenes = graphics
                .into_iter()
                .map(|(task_id, graphics)| (task_id, graphics.snapshot().clone()))
                .collect();
            return Ok((windows, scenes));
        }

        if let Ok(error) = task_errors.try_recv() {
            return Err(Box::new(io::Error::other(error)));
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(Box::new(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("desktop demo did not publish both initial scenes; windows: {windows:?}"),
            )));
        }
        let wait = EVENT_WAIT_SLICE.min(deadline.saturating_duration_since(now));
        match display_events.recv_timeout(wait) {
            Ok(DisplayEvent::WriteByte { task_id, byte }) => {
                graphics.entry(task_id).or_default().write_byte(byte)?;
            }
            Ok(DisplayEvent::Plot {
                task_id,
                code,
                x,
                y,
            }) => {
                graphics.entry(task_id).or_default().plot(code, x, y)?;
            }
            Ok(DisplayEvent::GraphicsSnapshot { task_id, snapshot }) => {
                graphics.insert(task_id, GraphicsService::from_snapshot(snapshot));
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
    let pixel_count = DESKTOP_PIXEL_WIDTH as usize * DESKTOP_PIXEL_HEIGHT as usize;
    let expected_rgba_bytes = pixel_count * 4;
    if rgba.len() < expected_rgba_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rendered desktop buffer is shorter than its pixel dimensions",
        ));
    }
    let mut ppm = format!(
        "P6\n{} {}\n255\n",
        DESKTOP_PIXEL_WIDTH, DESKTOP_PIXEL_HEIGHT
    )
    .into_bytes();
    ppm.reserve(pixel_count * 3);
    for pixel in rgba[..expected_rgba_bytes].chunks_exact(4) {
        ppm.extend_from_slice(&pixel[..3]);
    }
    fs::write(path, ppm)
}
