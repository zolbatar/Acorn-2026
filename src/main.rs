use std::{env, process::ExitCode};

use acorn_2026::{runtime::Runtime, window};

fn main() -> ExitCode {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let result = if arguments.iter().any(|argument| argument == "--stdio") {
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
