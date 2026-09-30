#[doc(hidden)]
pub mod compiler_api;

pub mod system_profile;

#[cfg(feature = "experimental-jit")]
mod jit;
#[cfg(feature = "experimental-jit")]
mod native_runtime;
mod parser;
mod runtime;
#[cfg(feature = "experimental-jit")]
mod strict_jit;
mod system_ir;

#[derive(Clone, Debug, Default)]
pub struct JitExecutionReport {
    pub compiled_units: Vec<String>,
    pub compiled_calls: u64,
    pub rendered_pixels: u64,
    /// Executable BASIC statements entered through the reference interpreter.
    pub interpreted_statement_count: u64,
    /// Recursive expression nodes evaluated by the reference interpreter.
    pub interpreted_expression_count: u64,
    /// Checked runtime service/helper calls made by generated native code.
    pub runtime_helper_calls: u64,
    /// True only for a complete strict-native compilation and execution.
    pub strict_native: bool,
    pub compiled_time: std::time::Duration,
    pub compile_time: std::time::Duration,
    pub fallback_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct StrictJitOptions {
    /// Disable speed optimizations to retain loops and procedure calls used
    /// solely for validation measurements.
    pub benchmark_validation: bool,
}

/// One-shot command defaults and explicit profile selections. Existing
/// *BASIC and RUN paths pass no launch options, preserving their behavior.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BasicLaunchOptions {
    pub default_mode: Option<BasicLanguageMode>,
    pub default_text_profile: Option<TextRenderingProfile>,
    pub selected_mode: Option<BasicLanguageMode>,
    pub selected_text_profile: Option<TextRenderingProfile>,
    pub override_declarations: bool,
}

use crate::{
    configure::{BasicConfiguration, BasicEngine, BasicLanguageMode},
    error::RuntimeError,
    graphics::{TextEncoding, TextRenderingProfile},
    memory::Task,
    swi::SwiDispatcher,
    tokenized_basic::{TokenizedBasicProgram, TokenizedBasicRecordLayout},
};

/// Run a tokenised earlier-version BASIC program in the hosted compatibility
/// personality.
pub fn run_program(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let parsed = parse_tokenized_program(program)?;
    run_parsed_program(parsed, task, dispatcher, false)
}

/// Run plain-text BASIC source through the same parser output and interpreter
/// used for decoded tokenized programs.
pub fn run_source(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<(), RuntimeError> {
    let parsed = parser::parse_source(source)?;
    run_parsed_program(parsed, task, dispatcher, false)
}

/// Run a program using the persisted MOS configuration, while allowing any
/// per-file `REM @BASIC64` fields to remain authoritative.
pub(crate) fn run_program_configured(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    run_program_with_engine_options(
        program,
        task,
        dispatcher,
        configuration,
        None,
        StrictJitOptions::default(),
    )
}

/// Run a program with an explicit one-shot engine choice, retaining configured
/// language, target, and profile defaults not specified by the source.
pub(crate) fn run_program_with_engine_options(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
    engine_override: Option<BasicEngine>,
    strict_options: StrictJitOptions,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    let mut parsed = parse_tokenized_program(program)?;
    apply_configuration(&mut parsed, configuration);
    run_parsed_configured(
        parsed,
        task,
        dispatcher,
        configuration,
        engine_override,
        strict_options,
        false,
    )
}

pub(crate) fn run_program_with_launch_options(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
    launch: BasicLaunchOptions,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    let mut parsed = parse_tokenized_program(program)?;
    apply_configuration(&mut parsed, configuration);
    apply_launch_options(&mut parsed, launch)?;
    run_parsed_configured(
        parsed,
        task,
        dispatcher,
        configuration,
        None,
        StrictJitOptions::default(),
        false,
    )
}

pub(crate) fn run_source_configured(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    run_source_with_engine_options(
        source,
        task,
        dispatcher,
        configuration,
        None,
        StrictJitOptions::default(),
    )
}

pub(crate) fn run_source_with_engine_options(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
    engine_override: Option<BasicEngine>,
    strict_options: StrictJitOptions,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    let mut parsed = parser::parse_source(source)?;
    apply_configuration(&mut parsed, configuration);
    run_parsed_configured(
        parsed,
        task,
        dispatcher,
        configuration,
        engine_override,
        strict_options,
        false,
    )
}

pub(crate) fn run_source_with_launch_options(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
    launch: BasicLaunchOptions,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    let mut parsed = parser::parse_source(source)?;
    apply_configuration(&mut parsed, configuration);
    apply_launch_options(&mut parsed, launch)?;
    run_parsed_configured(
        parsed,
        task,
        dispatcher,
        configuration,
        None,
        StrictJitOptions::default(),
        false,
    )
}

/// Run an immediate BASIC console line with a modern shell default. A source
/// that explicitly requests Classic uses a temporary guest view; Modern output
/// in the compatible mode remains visible on the shell's native text canvas.
pub(crate) fn run_source_from_basic_console(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    let mut parsed = parser::parse_source(source)?;
    apply_configuration(&mut parsed, configuration);
    apply_launch_options(
        &mut parsed,
        BasicLaunchOptions {
            default_text_profile: Some(TextRenderingProfile::Modern),
            ..BasicLaunchOptions::default()
        },
    )?;
    let preserve_shell = dispatcher.can_keep_modern_shell_for_basic_console(
        parsed.options.target,
        parsed.options.text_profile,
        execution_text_encoding(&parsed),
    );
    let run = |dispatcher: &mut SwiDispatcher| {
        run_parsed_configured(
            parsed,
            task,
            dispatcher,
            configuration,
            None,
            StrictJitOptions::default(),
            preserve_shell,
        )
    };
    if preserve_shell {
        run(dispatcher)
    } else {
        dispatcher.with_mos_shell_suspended(run)
    }
}

fn apply_configuration(parsed: &mut parser::ParsedProgram, configuration: &BasicConfiguration) {
    if !parsed.options.mode_declared {
        if let Some(mode) = configuration.language {
            parsed.options.mode = mode;
        }
    }
    if !parsed.options.target_declared {
        if let Some(target) = configuration.target {
            parsed.options.target = target;
        }
    }
    if !parsed.options.profile_declared && parsed.options.mode == BasicLanguageMode::Classic {
        if let Some(profile) = &configuration.profile {
            parsed.options.profile = Some(profile.clone());
        }
    }
}

fn apply_launch_options(
    parsed: &mut parser::ParsedProgram,
    launch: BasicLaunchOptions,
) -> Result<(), RuntimeError> {
    if let Some(selected) = launch.selected_mode {
        if parsed.options.mode_declared
            && parsed.options.mode != selected
            && !launch.override_declarations
        {
            return Err(RuntimeError::Program(format!(
                "launch MODE={} conflicts with source MODE={}; pass --override to select the launch mode",
                basic_mode_name(selected),
                basic_mode_name(parsed.options.mode),
            )));
        }
        parsed.options.mode = selected;
    } else if !parsed.options.mode_declared {
        if let Some(default) = launch.default_mode {
            parsed.options.mode = default;
        }
    }

    if let Some(selected) = launch.selected_text_profile {
        if parsed.options.text_profile_declared
            && parsed.options.text_profile != selected
            && !launch.override_declarations
        {
            return Err(RuntimeError::Program(format!(
                "launch TEXT={} conflicts with source TEXT={}; pass --override to select the launch profile",
                text_profile_name(selected),
                text_profile_name(parsed.options.text_profile),
            )));
        }
        parsed.options.text_profile = selected;
    } else if !parsed.options.text_profile_declared {
        if let Some(default) = launch.default_text_profile {
            parsed.options.text_profile = default;
        }
    }
    Ok(())
}

fn basic_mode_name(mode: BasicLanguageMode) -> &'static str {
    match mode {
        BasicLanguageMode::Classic => "CLASSIC",
        BasicLanguageMode::Basic64 => "BASIC64",
        BasicLanguageMode::Hybrid => "HYBRID",
    }
}

fn text_profile_name(profile: TextRenderingProfile) -> &'static str {
    match profile {
        TextRenderingProfile::Classic => "CLASSIC",
        TextRenderingProfile::Modern => "MODERN",
    }
}

fn run_parsed_configured(
    parsed: parser::ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    configuration: &BasicConfiguration,
    engine_override: Option<BasicEngine>,
    strict_options: StrictJitOptions,
    preserve_shell: bool,
) -> Result<Option<JitExecutionReport>, RuntimeError> {
    match engine_override.unwrap_or(configuration.engine) {
        BasicEngine::Interpreter => {
            run_parsed_program(parsed, task, dispatcher, preserve_shell)?;
            Ok(None)
        }
        BasicEngine::HybridJit => {
            run_parsed_program_jit(parsed, task, dispatcher, preserve_shell).map(Some)
        }
        BasicEngine::StrictJit => {
            run_parsed_program_jit_strict(parsed, task, dispatcher, strict_options, preserve_shell)
                .map(Some)
        }
    }
}

fn parse_tokenized_program(
    program: &TokenizedBasicProgram,
) -> Result<parser::ParsedProgram, RuntimeError> {
    let profile = match program.record_layout {
        Some(TokenizedBasicRecordLayout::SharedBoundaryCarriageReturn) => {
            parser::TokenProfile::SharedBoundaryCore
        }
        Some(TokenizedBasicRecordLayout::SeparateLineCarriageReturn) | None => {
            parser::TokenProfile::ArmBasicV
        }
    };
    parser::parse_program(program, profile)
}

fn run_parsed_program(
    parsed: parser::ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    preserve_shell: bool,
) -> Result<(), RuntimeError> {
    validate_program_options(&parsed)?;
    set_execution_display_profile(&parsed, dispatcher, preserve_shell)?;
    runtime::Interpreter::new(parsed).run(task, dispatcher)
}

/// Run a tokenised BASIC program with the experimental hybrid Cranelift path.
/// Eligible demo kernels compile and run natively; every other statement
/// continues through the compatibility interpreter.
pub fn run_program_jit(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parse_tokenized_program(program)?;
    run_parsed_program_jit(parsed, task, dispatcher, false)
}

pub fn run_source_jit(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parser::parse_source(source)?;
    run_parsed_program_jit(parsed, task, dispatcher, false)
}

/// Compile the complete parsed program before execution. Unsupported code is
/// rejected with a BASIC source location; strict execution never falls back to
/// the interpreter.
pub fn run_program_jit_strict(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    run_program_jit_strict_with_options(program, task, dispatcher, StrictJitOptions::default())
}

pub fn run_program_jit_strict_with_options(
    program: &TokenizedBasicProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    options: StrictJitOptions,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parse_tokenized_program(program)?;
    run_parsed_program_jit_strict(parsed, task, dispatcher, options, false)
}

pub fn run_source_jit_strict(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
) -> Result<JitExecutionReport, RuntimeError> {
    run_source_jit_strict_with_options(source, task, dispatcher, StrictJitOptions::default())
}

pub fn run_source_jit_strict_with_options(
    source: &str,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    options: StrictJitOptions,
) -> Result<JitExecutionReport, RuntimeError> {
    let parsed = parser::parse_source(source)?;
    run_parsed_program_jit_strict(parsed, task, dispatcher, options, false)
}

fn run_parsed_program_jit_strict(
    parsed: parser::ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    options: StrictJitOptions,
    preserve_shell: bool,
) -> Result<JitExecutionReport, RuntimeError> {
    validate_program_options(&parsed)?;
    #[cfg(feature = "experimental-jit")]
    {
        set_execution_display_profile(&parsed, dispatcher, preserve_shell)?;
        return strict_jit::run_parsed_program_with_options(parsed, task, dispatcher, options);
    }

    #[cfg(not(feature = "experimental-jit"))]
    {
        let _ = (parsed, task, dispatcher, options, preserve_shell);
        Err(RuntimeError::Program(
            "strict BASICJIT is experimental; use the JIT-enabled executable (`cargo run-jit`)"
                .into(),
        ))
    }
}

fn run_parsed_program_jit(
    parsed: parser::ParsedProgram,
    task: &mut Task,
    dispatcher: &mut SwiDispatcher,
    preserve_shell: bool,
) -> Result<JitExecutionReport, RuntimeError> {
    validate_program_options(&parsed)?;
    #[cfg(feature = "experimental-jit")]
    {
        set_execution_display_profile(&parsed, dispatcher, preserve_shell)?;
        return jit::run_parsed_program_jit(parsed, task, dispatcher);
    }

    #[cfg(not(feature = "experimental-jit"))]
    {
        let _ = (parsed, task, dispatcher, preserve_shell);
        Err(RuntimeError::Program(
            "BASICJIT is experimental; use the JIT-enabled executable (`cargo run-jit`)".into(),
        ))
    }
}

fn validate_program_options(parsed: &parser::ParsedProgram) -> Result<(), RuntimeError> {
    if parsed.options.profile.is_some() && parsed.options.mode != BasicLanguageMode::Classic {
        return Err(RuntimeError::Program(
            "PROFILE is only valid with MODE=CLASSIC".into(),
        ));
    }
    Ok(())
}

fn set_execution_display_profile(
    parsed: &parser::ParsedProgram,
    dispatcher: &mut SwiDispatcher,
    preserve_shell: bool,
) -> Result<(), RuntimeError> {
    let encoding = execution_text_encoding(parsed);
    if preserve_shell {
        dispatcher.set_basic_console_display_profiles(
            parsed.options.target,
            parsed.options.text_profile,
            encoding,
        )
    } else {
        dispatcher.set_display_profiles(
            parsed.options.target,
            parsed.options.text_profile,
            encoding,
        )
    }
}

fn execution_text_encoding(parsed: &parser::ParsedProgram) -> TextEncoding {
    match parsed.options.text_profile {
        TextRenderingProfile::Classic => match parsed.options.mode {
            BasicLanguageMode::Basic64 => TextEncoding::Utf8,
            BasicLanguageMode::Classic | BasicLanguageMode::Hybrid => TextEncoding::ClassicBytes,
        },
        TextRenderingProfile::Modern if parsed.options.mode == BasicLanguageMode::Classic => {
            TextEncoding::Latin1
        }
        TextRenderingProfile::Modern => TextEncoding::Utf8,
    }
}

#[cfg(test)]
mod profile_tests {
    use super::{
        BasicLanguageMode, BasicLaunchOptions, TextEncoding, apply_launch_options, parser,
        run_source, run_source_from_basic_console, run_source_with_launch_options,
    };
    use crate::graphics::TextRenderingProfile;
    use crate::swi::DisplayEvent;

    fn windowed_dispatcher() -> (
        crate::swi::SwiDispatcher,
        std::sync::mpsc::Receiver<DisplayEvent>,
    ) {
        let (_input_sender, input_receiver) = std::sync::mpsc::channel();
        let (display_sender, display_receiver) = std::sync::mpsc::channel();
        let console = crate::host::HostConsole::windowed(input_receiver);
        (
            crate::swi::SwiDispatcher::windowed(console, display_sender),
            display_receiver,
        )
    }

    fn replay_display_events(
        events: std::sync::mpsc::Receiver<DisplayEvent>,
    ) -> crate::graphics::GraphicsSnapshot {
        let mut graphics = crate::graphics::GraphicsService::default();
        for event in events.try_iter() {
            match event {
                DisplayEvent::GraphicsSnapshot { snapshot, .. } => {
                    graphics.replace_snapshot(snapshot)
                }
                DisplayEvent::WriteByte { byte, .. } => {
                    graphics.write_byte(byte).unwrap();
                }
                _ => {}
            }
        }
        graphics.snapshot().clone()
    }

    #[test]
    fn declared_profiles_win_by_default_and_explicit_overrides_are_scoped() {
        let source = "REM @BASIC64 MODE=CLASSIC TEXT=CLASSIC\nPRINT \"hello\"\n";
        let mut parsed = parser::parse_source(source).unwrap();
        apply_launch_options(
            &mut parsed,
            BasicLaunchOptions {
                default_mode: Some(BasicLanguageMode::Basic64),
                default_text_profile: Some(TextRenderingProfile::Modern),
                ..BasicLaunchOptions::default()
            },
        )
        .unwrap();
        assert_eq!(parsed.options.mode, BasicLanguageMode::Classic);
        assert_eq!(parsed.options.text_profile, TextRenderingProfile::Classic);

        let mut parsed = parser::parse_source(source).unwrap();
        let conflict = apply_launch_options(
            &mut parsed,
            BasicLaunchOptions {
                selected_mode: Some(BasicLanguageMode::Basic64),
                ..BasicLaunchOptions::default()
            },
        )
        .expect_err("conflicting explicit selection needs --override");
        assert!(conflict.to_string().contains("pass --override"));

        let mut parsed = parser::parse_source(source).unwrap();
        apply_launch_options(
            &mut parsed,
            BasicLaunchOptions {
                default_mode: Some(BasicLanguageMode::Basic64),
                default_text_profile: Some(TextRenderingProfile::Modern),
                selected_mode: Some(BasicLanguageMode::Basic64),
                override_declarations: true,
                ..BasicLaunchOptions::default()
            },
        )
        .unwrap();
        assert_eq!(parsed.options.mode, BasicLanguageMode::Basic64);
        assert_eq!(
            parsed.options.text_profile,
            TextRenderingProfile::Classic,
            "--override changes only explicitly selected fields"
        );
    }

    #[test]
    fn basic64_launch_decodes_utf8_while_existing_source_run_stays_classic() {
        let configuration = crate::configure::BasicConfiguration::default();
        let (mut dispatcher, display_events) = windowed_dispatcher();
        let mut task = crate::memory::Task::new(1);
        run_source_with_launch_options(
            "PRINT \"é🙂\"\n",
            &mut task,
            &mut dispatcher,
            &configuration,
            BasicLaunchOptions {
                default_mode: Some(BasicLanguageMode::Basic64),
                default_text_profile: Some(TextRenderingProfile::Modern),
                ..BasicLaunchOptions::default()
            },
        )
        .unwrap();
        let modern = replay_display_events(display_events);
        assert_eq!(modern.text_profile, TextRenderingProfile::Modern);
        assert_eq!(modern.text_encoding, TextEncoding::Utf8);
        assert_eq!(&modern.modern_text_cells[..2], &['é', '🙂']);

        let (mut dispatcher, display_events) = windowed_dispatcher();
        let mut task = crate::memory::Task::new(2);
        run_source("PRINT \"é\"\n", &mut task, &mut dispatcher).unwrap();
        let classic = replay_display_events(display_events);
        assert_eq!(classic.text_profile, TextRenderingProfile::Classic);
        assert_eq!(classic.text_encoding, TextEncoding::ClassicBytes);
        assert_eq!(&classic.text_cells[..2], "é".as_bytes());
    }

    #[test]
    fn native_basic64_classic_text_decodes_utf8_to_latin1_and_replaces_unrepresentable_glyphs() {
        let configuration = crate::configure::BasicConfiguration::default();
        let (mut dispatcher, display_events) = windowed_dispatcher();
        let mut task = crate::memory::Task::new(3);
        run_source_with_launch_options(
            "REM @BASIC64 MODE=BASIC64 TEXT=CLASSIC\nPRINT \"é🙂\"\n",
            &mut task,
            &mut dispatcher,
            &configuration,
            BasicLaunchOptions::default(),
        )
        .unwrap();
        let snapshot = replay_display_events(display_events);
        assert_eq!(snapshot.text_profile, TextRenderingProfile::Classic);
        assert_eq!(snapshot.text_encoding, TextEncoding::Utf8);
        assert_eq!(&snapshot.text_cells[..2], &[0xE9, b'?']);
        assert_eq!(&snapshot.modern_text_cells[..2], &['é', '🙂']);
    }

    #[test]
    fn modern_read_point_through_basic_sys_has_the_same_profile_guard_as_the_swi() {
        let (mut dispatcher, _) = windowed_dispatcher();
        let mut task = crate::memory::Task::new(4);
        let error = run_source_with_launch_options(
            "REM @BASIC64 MODE=BASIC64 TEXT=MODERN\nSYS \"OS_READPOINT\"\n",
            &mut task,
            &mut dispatcher,
            &crate::configure::BasicConfiguration::default(),
            BasicLaunchOptions {
                default_mode: Some(BasicLanguageMode::Basic64),
                default_text_profile: Some(TextRenderingProfile::Modern),
                ..BasicLaunchOptions::default()
            },
        )
        .expect_err("BASIC SYS must not read through the Modern text overlay");
        assert!(
            error
                .to_string()
                .contains("OS_ReadPoint requires TEXT=CLASSIC")
        );
    }

    #[test]
    fn consecutive_launches_switch_from_modern_to_classic_teletext_safely() {
        let configuration = crate::configure::BasicConfiguration::default();
        let (mut dispatcher, _) = windowed_dispatcher();
        let mut task = crate::memory::Task::new(5);
        run_source_with_launch_options(
            "REM @BASIC64 MODE=BASIC64 TEXT=MODERN\nPRINT \"modern\"\n",
            &mut task,
            &mut dispatcher,
            &configuration,
            BasicLaunchOptions::default(),
        )
        .unwrap();
        assert_eq!(
            dispatcher.graphics().snapshot().text_profile,
            TextRenderingProfile::Modern
        );
        run_source_with_launch_options(
            "REM @BASIC64 MODE=CLASSIC TEXT=CLASSIC\nMODE 7\nPRINT \"classic\"\n",
            &mut task,
            &mut dispatcher,
            &configuration,
            BasicLaunchOptions::default(),
        )
        .unwrap();
        let snapshot = dispatcher.graphics().snapshot();
        assert_eq!(snapshot.text_profile, TextRenderingProfile::Classic);
        assert_eq!(snapshot.mode.number, 7);
    }

    #[test]
    fn immediate_basic_console_defaults_modern_and_restores_shell_after_classic() {
        let configuration = crate::configure::BasicConfiguration::default();
        let (mut dispatcher, _) = windowed_dispatcher();
        dispatcher.initialize_mos_shell_console();
        let mut task = crate::memory::Task::new(6);
        run_source_from_basic_console(
            "PRINT \"shell\"\n",
            &mut task,
            &mut dispatcher,
            &configuration,
        )
        .unwrap();
        let shell_before_classic = dispatcher.graphics().snapshot().clone();
        assert_eq!(
            shell_before_classic.text_profile,
            TextRenderingProfile::Modern
        );
        assert!(shell_before_classic.modern_shell_console);
        assert_eq!(
            &shell_before_classic.modern_text_cells[..5],
            &['s', 'h', 'e', 'l', 'l']
        );

        run_source_from_basic_console(
            "REM @BASIC64 MODE=CLASSIC TEXT=CLASSIC\nPRINT \"legacy\"\n",
            &mut task,
            &mut dispatcher,
            &configuration,
        )
        .unwrap();
        let restored = dispatcher.graphics().snapshot();
        assert_eq!(restored.text_profile, TextRenderingProfile::Modern);
        assert!(restored.modern_shell_console);
        assert_eq!(&restored.modern_text_cells[..5], &['s', 'h', 'e', 'l', 'l']);
        assert_eq!(restored.text_cells, shell_before_classic.text_cells);
        assert_eq!(restored.text_cursor, shell_before_classic.text_cursor);
    }

    #[test]
    fn compatible_modern_guest_output_is_kept_in_the_mos_shell() {
        let configuration = crate::configure::BasicConfiguration::default();
        let (mut dispatcher, _) = windowed_dispatcher();
        dispatcher.initialize_mos_shell_console();
        let mut task = crate::memory::Task::new(8);
        dispatcher
            .with_mos_shell_suspended(|dispatcher| {
                run_source_with_launch_options(
                    "REM @BASIC64 MODE=BASIC64 TEXT=MODERN\nPRINT \"modern file\"\n",
                    &mut task,
                    dispatcher,
                    &configuration,
                    BasicLaunchOptions::default(),
                )
            })
            .unwrap();

        let snapshot = dispatcher.graphics().snapshot();
        assert_eq!(snapshot.text_profile, TextRenderingProfile::Modern);
        assert_eq!(snapshot.text_encoding, TextEncoding::Utf8);
        assert!(snapshot.modern_shell_console);
        assert_eq!(
            &snapshot.modern_text_cells[..11],
            &['m', 'o', 'd', 'e', 'r', 'n', ' ', 'f', 'i', 'l', 'e']
        );
    }

    #[test]
    fn utf8_decoder_state_survives_a_snapshot_between_multibyte_bytes() {
        let mut producer = crate::graphics::GraphicsService::default();
        producer
            .set_text_profile(TextRenderingProfile::Modern, TextEncoding::Utf8)
            .unwrap();
        producer.write_byte(0xC3).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        sender
            .send(DisplayEvent::GraphicsSnapshot {
                task_id: 1,
                window_handle: None,
                snapshot: producer.snapshot().clone(),
            })
            .unwrap();
        sender
            .send(DisplayEvent::WriteByte {
                task_id: 1,
                window_handle: None,
                byte: 0xA9,
            })
            .unwrap();
        drop(sender);
        let snapshot = replay_display_events(receiver);
        assert_eq!(snapshot.modern_text_cells[0], 'é');
        assert_eq!(snapshot.text_cursor.x, 1);
    }
}
