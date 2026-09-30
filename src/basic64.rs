//! BASIC source and saved-program entry points.
//!
//! Both UTF-8 source and decoded tokenized programs are parsed into the
//! compatibility engine's `ParsedProgram` and executed by its interpreter or
//! optional JIT.

use std::{fs, path::Path};

use crate::{
    basic_compat::{self, JitExecutionReport, StrictJitOptions},
    configure::{BasicConfiguration, BasicEngine},
    error::RuntimeError,
    filesystem::{FILETYPE_BASIC, FileMetadata},
    memory::Task,
    swi::SwiDispatcher,
    tokenized_basic::TokenizedBasicProgram,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProgramFormat {
    Basic64Utf8Source,
    ClassicUtf8Source,
    TokenizedBbcProgram,
}

pub fn detect_program_format(path: &Path) -> Result<ProgramFormat, RuntimeError> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) if extension.eq_ignore_ascii_case("bas64") => {
            Ok(ProgramFormat::Basic64Utf8Source)
        }
        Some(extension)
            if extension.eq_ignore_ascii_case("bas")
                || extension.eq_ignore_ascii_case("txt")
                || extension.eq_ignore_ascii_case("asc") =>
        {
            Ok(ProgramFormat::ClassicUtf8Source)
        }
        Some(extension) if extension.eq_ignore_ascii_case("bbc") => {
            Ok(ProgramFormat::TokenizedBbcProgram)
        }
        _ => Err(RuntimeError::Program(
            "unsupported file format; use .bas64, .bas, .txt, or .asc for UTF-8 source, or .bbc for a tokenized BASIC program".into(),
        )),
    }
}

pub fn run_file(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    match detect_program_format(Path::new(path))? {
        ProgramFormat::Basic64Utf8Source | ProgramFormat::ClassicUtf8Source => {
            let source = fs::read_to_string(path)?;
            run_source(&source, task, dispatcher)
        }
        ProgramFormat::TokenizedBbcProgram => {
            let program = TokenizedBasicProgram::load_file(path)?;
            basic_compat::run_program(&program, task, dispatcher)
        }
    }
}

/// Runs a file resolved by the task's selected filing system. RISC OS file
/// type metadata selects tokenized BASIC; other types are treated as text
/// source when their contents are valid UTF-8.
pub fn run_guest_file(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let (bytes, metadata) = dispatcher.read_guest_file(task, path)?;
    run_guest_bytes(&bytes, &metadata, task, dispatcher)
}

pub(crate) fn run_guest_file_configured(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    run_guest_file_with_engine_options(
        path,
        task,
        dispatcher,
        configuration,
        None,
        StrictJitOptions::default(),
    )
}

pub(crate) fn run_guest_file_with_launch_options(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
    launch: basic_compat::BasicLaunchOptions,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    let (bytes, metadata) = dispatcher.read_guest_file(task, path)?;
    if metadata.file_type & 0xFFF == FILETYPE_BASIC {
        let program = TokenizedBasicProgram::decode(&bytes)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        basic_compat::run_program_with_launch_options(
            &program,
            task,
            dispatcher,
            configuration,
            launch,
        )
    } else {
        let source = std::str::from_utf8(&bytes).map_err(|error| {
            RuntimeError::Program(format!(
                "file type &{:03X} is not tokenized BASIC and the file is not UTF-8 text: {error}",
                metadata.file_type & 0xFFF
            ))
        })?;
        basic_compat::run_source_with_launch_options(
            source,
            task,
            dispatcher,
            configuration,
            launch,
        )
    }
}

pub(crate) fn run_guest_file_with_engine_options(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
    engine_override: Option<BasicEngine>,
    strict_options: StrictJitOptions,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    let (bytes, metadata) = dispatcher.read_guest_file(task, path)?;
    if metadata.file_type & 0xFFF == FILETYPE_BASIC {
        let program = TokenizedBasicProgram::decode(&bytes)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        basic_compat::run_program_with_engine_options(
            &program,
            task,
            dispatcher,
            configuration,
            engine_override,
            strict_options,
        )
    } else {
        let source = std::str::from_utf8(&bytes).map_err(|error| {
            RuntimeError::Program(format!(
                "file type &{:03X} is not tokenized BASIC and the file is not UTF-8 text: {error}",
                metadata.file_type & 0xFFF
            ))
        })?;
        basic_compat::run_source_with_engine_options(
            source,
            task,
            dispatcher,
            configuration,
            engine_override,
            strict_options,
        )
    }
}

pub fn run_guest_bytes(
    bytes: &[u8],
    metadata: &FileMetadata,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    if metadata.file_type & 0xFFF == FILETYPE_BASIC {
        let program = TokenizedBasicProgram::decode(bytes)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        basic_compat::run_program(&program, task, dispatcher)
    } else {
        let source = std::str::from_utf8(bytes).map_err(|error| {
            RuntimeError::Program(format!(
                "file type &{:03X} is not tokenized BASIC and the file is not UTF-8 text: {error}",
                metadata.file_type & 0xFFF
            ))
        })?;
        run_source(source, task, dispatcher)
    }
}

pub fn run_file_jit(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    match detect_program_format(Path::new(path))? {
        ProgramFormat::Basic64Utf8Source | ProgramFormat::ClassicUtf8Source => {
            let source = fs::read_to_string(path)?;
            run_source_jit(&source, task, dispatcher)
        }
        ProgramFormat::TokenizedBbcProgram => {
            let program = TokenizedBasicProgram::load_file(path)?;
            basic_compat::run_program_jit(&program, task, dispatcher)
        }
    }
}

pub fn run_guest_file_jit(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    let (bytes, metadata) = dispatcher.read_guest_file(task, path)?;
    if metadata.file_type & 0xFFF == FILETYPE_BASIC {
        let program = TokenizedBasicProgram::decode(&bytes)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        basic_compat::run_program_jit(&program, task, dispatcher)
    } else {
        let source = std::str::from_utf8(&bytes).map_err(|error| {
            RuntimeError::Program(format!(
                "file type &{:03X} is not tokenized BASIC and the file is not UTF-8 text: {error}",
                metadata.file_type & 0xFFF
            ))
        })?;
        run_source_jit(source, task, dispatcher)
    }
}

pub fn run_guest_file_jit_strict(
    path: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    options: StrictJitOptions,
) -> Result<JitExecutionReport, RuntimeError> {
    let (bytes, metadata) = dispatcher.read_guest_file(task, path)?;
    if metadata.file_type & 0xFFF == FILETYPE_BASIC {
        let program = TokenizedBasicProgram::decode(&bytes)
            .map_err(|error| RuntimeError::Program(error.to_string()))?;
        basic_compat::run_program_jit_strict_with_options(&program, task, dispatcher, options)
    } else {
        let source = std::str::from_utf8(&bytes).map_err(|error| {
            RuntimeError::Program(format!(
                "file type &{:03X} is not tokenized BASIC and the file is not UTF-8 text: {error}",
                metadata.file_type & 0xFFF
            ))
        })?;
        basic_compat::run_source_jit_strict_with_options(source, task, dispatcher, options)
    }
}

pub fn run_source(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    basic_compat::run_source(source, task, dispatcher)
}

pub fn run_source_jit(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    basic_compat::run_source_jit(source, task, dispatcher)
}
