use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, OS_CLI, SwiContext, SwiDispatcher},
};

const CLI_ADDRESS: u32 = 0x2100;

struct IsolatedEnvironment {
    root: PathBuf,
    old_volume: Option<std::ffi::OsString>,
    old_config: Option<std::ffi::OsString>,
    old_capsule: Option<std::ffi::OsString>,
}

impl IsolatedEnvironment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("ricochet-basic-surface-{nonce}"));
        fs::create_dir_all(&root).unwrap();
        let old_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        let old_config = std::env::var_os("RICOCHET_CONFIG_PATH");
        let old_capsule = std::env::var_os("RICOCHET_BOOT_CAPSULE");
        unsafe {
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
            std::env::set_var("RICOCHET_CONFIG_PATH", root.join("configure"));
            std::env::remove_var("RICOCHET_BOOT_CAPSULE");
        }
        Self {
            root,
            old_volume,
            old_config,
            old_capsule,
        }
    }

    fn guest_path(&self, name: &str) -> String {
        format!("HostFS::DemoDisk.$.{name}")
    }

    fn write_basic(&self, host_name: &str, guest_name: &str, file_type: u32, bytes: &[u8]) {
        fs::write(self.root.join(host_name), bytes).unwrap();
        fs::write(
            self.root.join(format!("{host_name}.ricochetmeta")),
            format!(
                "Ricochet file metadata v1\nformat-version=1\nguest-name={guest_name}\nfile-type=0x{file_type:08X}\nload-address=0x00000000\nexecution-address=0x00000000\nattributes=0x00000000\n"
            ),
        )
        .unwrap();
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = &self.old_volume {
                std::env::set_var("RICOCHET_DEMO_VOLUME", value);
            } else {
                std::env::remove_var("RICOCHET_DEMO_VOLUME");
            }
            if let Some(value) = &self.old_config {
                std::env::set_var("RICOCHET_CONFIG_PATH", value);
            } else {
                std::env::remove_var("RICOCHET_CONFIG_PATH");
            }
            if let Some(value) = &self.old_capsule {
                std::env::set_var("RICOCHET_BOOT_CAPSULE", value);
            } else {
                std::env::remove_var("RICOCHET_BOOT_CAPSULE");
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn new_dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender),
        display_receiver,
    )
}

fn cli(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    display: &mpsc::Receiver<DisplayEvent>,
    command: &str,
) -> (Result<(), RuntimeError>, String) {
    let _ = display.try_iter().count();
    task.memory
        .write_bytes(CLI_ADDRESS, command.as_bytes())
        .unwrap();
    task.memory
        .write_byte(CLI_ADDRESS + command.len() as u32, 0)
        .unwrap();
    let mut context = SwiContext::default();
    context.registers[0] = CLI_ADDRESS;
    let result = dispatcher.dispatch(OS_CLI, task, &mut context);
    assert_eq!(context.registers[0], CLI_ADDRESS, "OS_CLI preserves R0");
    let output = display
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    (result, String::from_utf8_lossy(&output).into_owned())
}

fn help_names(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let name = line
                .trim_start()
                .strip_prefix('*')?
                .split_whitespace()
                .next()?;
            Some(name.to_ascii_uppercase())
        })
        .collect()
}

#[test]
fn retired_cli_family_is_removed_and_durable_basic_routes_remain() {
    let environment = IsolatedEnvironment::new();
    environment.write_basic(
        "surface-source.bas",
        "surface-source",
        0xFFF,
        b"10 PRINT \"BASIC-SOURCE-OK\"\n20 END\n",
    );
    environment.write_basic(
        "surface-tokenized.bbc",
        "surface-tokenized",
        0xFFB,
        include_bytes!("../examples/tokenized-compat/classic-core-smoke.bbc"),
    );
    environment.write_basic(
        "strict-unsupported.bas",
        "strict-unsupported",
        0xFFF,
        b"10 DIM A\n20 END\n",
    );

    let (mut dispatcher, display) = new_dispatcher();
    let mut task = Task::trusted_mos_session(0xB451);

    let (help_result, help) = cli(&mut dispatcher, &mut task, &display, "*HELP");
    help_result.expect("Help remains available");
    let names = help_names(&help);
    for retired in ["BASICLOAD", "BASICRUN", "BASICJIT"] {
        assert!(
            !names.iter().any(|name| name == retired),
            "retired command {retired} remains in the live Help registry: {help:?}"
        );
    }
    for retained in ["BASIC", "BASIC64", "RUN"] {
        assert!(
            names.iter().any(|name| name == retained),
            "canonical command {retained} disappeared from Help: {help:?}"
        );
    }

    for retired in ["BASICLOAD", "BASICRUN", "BASICJIT"] {
        let (result, output) = cli(&mut dispatcher, &mut task, &display, &format!("*{retired}"));
        assert!(
            matches!(
                result,
                Err(RuntimeError::Structured {
                    ref type_name,
                    ref message,
                    ..
                }) if type_name == "CLIERROR"
                    && message.contains(&format!("Bad command: {retired}"))
            ),
            "retired command {retired} must fail through the standard unknown-command diagnostic: {result:?}, {output:?}"
        );
    }

    let (source_result, source_output) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*BASIC {}", environment.guest_path("surface-source")),
    );
    source_result.expect("canonical BASIC source launch succeeds");
    assert!(
        source_output.contains("BASIC-SOURCE-OK"),
        "{source_output:?}"
    );

    let (tokenized_result, tokenized_output) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*RUN {}", environment.guest_path("surface-tokenized")),
    );
    tokenized_result.expect("RUN directly accepts a tokenized .bbc program");
    assert!(tokenized_output.contains("LEGACY"), "{tokenized_output:?}");

    for engine in ["INTERPRETER", "HYBRID", "STRICT"] {
        let (set_result, set_output) = cli(
            &mut dispatcher,
            &mut task,
            &display,
            &format!("*CONFIGURE BASICEngine {engine}"),
        );
        set_result.expect("BASICEngine remains a public configuration setting");
        assert!(set_output.contains("BASICEngine set to"), "{set_output:?}");
        let (show_result, show_output) =
            cli(&mut dispatcher, &mut task, &display, "*STATUS BASICEngine");
        show_result.expect("BASICEngine status remains readable");
        assert!(show_output.contains(engine), "{show_output:?}");
    }

    let (strict_result, strict_output) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*CONFIGURE BASICEngine STRICT"),
    );
    strict_result.expect("Strict remains selectable for subsequent BASIC launches");
    let (launch_result, launch_output) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        &format!("*BASIC {}", environment.guest_path("strict-unsupported")),
    );
    launch_result.expect("Strict compilation errors are presented by the BASIC command route");
    assert!(
        launch_output.to_ascii_lowercase().contains("strict")
            || launch_output.to_ascii_lowercase().contains("unsupported"),
        "BASICEngine STRICT failure must not be silently swallowed: {launch_output:?}; {strict_output:?}"
    );

    let (syntax_result, syntax_output) = cli(
        &mut dispatcher,
        &mut task,
        &display,
        "*BASIC64 --not-an-option x.bas64",
    );
    syntax_result.expect("invalid BASIC64 options retain user-facing diagnostics");
    assert!(
        syntax_output.contains("unknown *BASIC64 option"),
        "{syntax_output:?}"
    );

    assert!(!dispatcher.quit_requested());
}
