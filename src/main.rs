use std::process::ExitCode;

use acorn_2026::runtime::Runtime;

fn main() -> ExitCode {
    let mut runtime = Runtime::stdio();
    match runtime.run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = runtime.report_error(&error);
            ExitCode::FAILURE
        }
    }
}
