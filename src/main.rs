use std::{env, process::ExitCode};

use acorn_2026::{runtime::Runtime, snapshot, window};

fn main() -> ExitCode {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let result = if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--task-menu-snapshot")
    {
        match arguments.get(index + 1) {
            Some(path) => {
                snapshot::write_task_menu_snapshot(path).map_err(|error| error.to_string())
            }
            None => Err("--task-menu-snapshot requires an output path".into()),
        }
    } else if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--filer-interaction-snapshots")
    {
        match arguments.get(index + 1) {
            Some(directory) => snapshot::write_filer_interaction_snapshots(directory)
                .map_err(|error| error.to_string()),
            None => Err("--filer-interaction-snapshots requires an output directory".into()),
        }
    } else if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--filer-menu-snapshot")
    {
        match arguments.get(index + 1) {
            Some(path) => {
                snapshot::write_filer_menu_snapshot(path).map_err(|error| error.to_string())
            }
            None => Err("--filer-menu-snapshot requires an output path".into()),
        }
    } else if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--filer-large-snapshot")
    {
        match arguments.get(index + 1) {
            Some(path) => {
                snapshot::write_filer_large_snapshot(path).map_err(|error| error.to_string())
            }
            None => Err("--filer-large-snapshot requires an output path".into()),
        }
    } else if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--filer-snapshot")
    {
        match arguments.get(index + 1) {
            Some(path) => snapshot::write_filer_snapshot(path).map_err(|error| error.to_string()),
            None => Err("--filer-snapshot requires an output path".into()),
        }
    } else if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--riscos-font-specimen")
    {
        match arguments.get(index + 1) {
            Some(path) => {
                snapshot::write_riscos_font_specimen(path).map_err(|error| error.to_string())
            }
            None => Err("--riscos-font-specimen requires an output path".into()),
        }
    } else if let Some(index) = arguments
        .iter()
        .position(|argument| argument == "--desktop-demo-snapshot")
    {
        match arguments.get(index + 1) {
            Some(path) => {
                snapshot::write_desktop_demo_snapshot(path).map_err(|error| error.to_string())
            }
            None => Err("--desktop-demo-snapshot requires an output path".into()),
        }
    } else if arguments.iter().any(|argument| argument == "--stdio") {
        let mut runtime = Runtime::stdio();
        runtime.run().map_err(|error| {
            let _ = runtime.report_error(&error);
            error.to_string()
        })
    } else if arguments
        .iter()
        .any(|argument| argument == "--desktop-demo")
    {
        window::run_desktop_demo().map_err(|error| error.to_string())
    } else {
        window::run().map_err(|error| error.to_string())
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("Acorn-2026: {message}");
            ExitCode::FAILURE
        }
    }
}
