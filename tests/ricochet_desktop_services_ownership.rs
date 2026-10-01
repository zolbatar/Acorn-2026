use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    basic_compat,
    display::{DesktopResolution, DisplayColour, DisplaySettings},
    filesystem::{FILETYPE_TEXT, FileMetadata, metadata_path, write_metadata},
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, GUEST_MEMORY_SIZE, Task},
    runtime::Runtime,
    swi::{DisplayEvent, SwiDispatchRoute, SwiDispatcher},
    wimp::{WIMP_POLL, WimpServer},
};

const RESULT: u32 = 0x6000;
const DIRECTORY: u32 = 0x1800;
const NAME_BUFFER: u32 = 0x1900;
const VOLUME_BUFFER: u32 = 0x1A00;

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
            .expect("clock is after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ricochet-desktop-services-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("Catalog")).unwrap();
        let old_volume = std::env::var_os("RICOCHET_DEMO_VOLUME");
        let old_config = std::env::var_os("RICOCHET_CONFIG_PATH");
        let old_capsule = std::env::var_os("RICOCHET_BOOT_CAPSULE");
        unsafe {
            std::env::set_var("RICOCHET_DEMO_VOLUME", &root);
            std::env::set_var("RICOCHET_CONFIG_PATH", root.join("configure"));
            std::env::remove_var("RICOCHET_BOOT_CAPSULE");
        }

        let file = root.join("Catalog").join("Alpha");
        fs::write(&file, b"fixture").unwrap();
        write_metadata(
            &metadata_path(&file),
            &FileMetadata {
                guest_name: "Alpha".into(),
                file_type: FILETYPE_TEXT,
                load_address: 0x1234,
                execution_address: 0x5678,
                attributes: 0x21,
            },
        )
        .unwrap();

        Self {
            root,
            old_volume,
            old_config,
            old_capsule,
        }
    }
}

impl Drop for IsolatedEnvironment {
    fn drop(&mut self) {
        unsafe {
            for (key, value) in [
                ("RICOCHET_DEMO_VOLUME", self.old_volume.as_ref()),
                ("RICOCHET_CONFIG_PATH", self.old_config.as_ref()),
                ("RICOCHET_BOOT_CAPSULE", self.old_capsule.as_ref()),
            ] {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender),
        display_receiver,
    )
}

fn word(task: &Task, address: u32) -> u32 {
    u32::from_le_bytes(
        task.memory
            .read_bytes(address, 4)
            .unwrap()
            .try_into()
            .unwrap(),
    )
}

fn display_text(receiver: &mpsc::Receiver<DisplayEvent>) -> String {
    let bytes = receiver
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte { byte, .. } => Some(byte),
            _ => None,
        })
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn assert_named_route(dispatcher: &SwiDispatcher, name: &str, module: &str, definition: &str) {
    let Some(SwiDispatchRoute::ModuleOwnedNamed {
        name: routed_name,
        module: routed_module,
        definition: routed_definition,
        definition_id,
        source_hash,
    }) = dispatcher.last_dispatch_route()
    else {
        panic!(
            "{name} did not use module-owned named dispatch: {:?}",
            dispatcher.last_dispatch_route()
        );
    };
    assert!(routed_name.eq_ignore_ascii_case(name));
    assert!(routed_module.eq_ignore_ascii_case(module));
    assert!(routed_definition.eq_ignore_ascii_case(definition));
    let record = dispatcher
        .module_registry()
        .module_named(module)
        .expect("routed service owner is still published");
    let descriptor = record
        .definitions
        .get(&definition.to_ascii_uppercase())
        .expect("module retains the routed BASIC64 definition");
    assert_eq!(*definition_id, descriptor.id.diagnostic_value());
    assert_eq!(source_hash, &descriptor.source_hash);
    assert_eq!(source_hash, &record.manifest.source_hash);
    assert!(descriptor.source_path.ends_with(&format!("{module}.bas64")));
    assert_eq!(dispatcher.transitional_dispatch_count(), 0);
}

#[test]
fn desktop_catalogue_and_display_services_keep_their_named_task_scoped_contracts() {
    let _environment = IsolatedEnvironment::new();
    let (mut dispatcher, _display_events) = dispatcher();
    let mut task = Task::new(0xD357);

    let modules = dispatcher
        .module_registry()
        .active_modules_sorted()
        .into_iter()
        .map(|module| module.manifest.name.clone())
        .collect::<Vec<_>>();
    assert!(
        modules
            .iter()
            .any(|name| name.eq_ignore_ascii_case("DesktopServices")),
        "DesktopServices module is not published: {modules:?}"
    );
    assert!(
        modules
            .iter()
            .any(|name| name.eq_ignore_ascii_case("DisplayManager")),
        "DisplayManager module is not published: {modules:?}"
    );
    for name in ["Ricochet_Desktop", "Ricochet_Display"] {
        assert_eq!(
            dispatcher.module_registry().swi_number(name),
            None,
            "these project services are named-only and must not publish invented numeric IDs"
        );
    }

    task.memory.write_bytes(DIRECTORY, b"$.Catalog\0").unwrap();
    task.memory.write_bytes(NAME_BUFFER, &[0xA5; 32]).unwrap();
    basic_compat::run_source(
        "10 SYS \"Ricochet_Desktop\",1,&1800,0,&1900,32 TO KIND%,TYPE%,SIZE%\n\
         20 !&6000=KIND%:!&6004=TYPE%:!&6008=SIZE%\n\
         30 END",
        &mut task,
        &mut dispatcher,
    )
    .expect("Desktop catalogue query executes through the published named service");
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    assert_eq!(
        [RESULT, RESULT + 4, RESULT + 8].map(|address| word(&task, address)),
        [3, FILETYPE_TEXT, 7]
    );
    assert_eq!(
        task.memory.read_c_string(NAME_BUFFER, 32).unwrap(),
        b"Alpha"
    );

    basic_compat::run_source(
        "10 SYS \"Ricochet_Desktop\",1,&1800,1,&1900,32 TO KIND%,TYPE%,SIZE%\n20 !&600C=KIND%:END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    assert_eq!(word(&task, RESULT + 0x0C), 0, "catalogue end uses kind 0");
    assert_eq!(task.memory.read_byte(NAME_BUFFER).unwrap(), 0);

    // X-form success preserves the named route and leaves V clear. A short
    // output buffer must fail before any prefix of the string is written.
    task.memory.write_bytes(NAME_BUFFER, &[0x5A; 32]).unwrap();
    basic_compat::run_source(
        "10 SYS \"XRicochet_Desktop\",1,&1800,0,&1900,3 TO KIND%,TYPE%,SIZE% ; FLAGS%\n\
         20 !&6010=FLAGS%:END",
        &mut task,
        &mut dispatcher,
    )
    .expect("X-form converts the buffer-fit error into normal V status");
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    assert_ne!(word(&task, RESULT + 0x10) & 1, 0);
    assert_eq!(task.memory.read_bytes(NAME_BUFFER, 8).unwrap(), &[0x5A; 8]);

    let edge = GUEST_MEMORY_BASE + GUEST_MEMORY_SIZE as u32 - 2;
    task.memory.write_bytes(edge, &[0x2D; 2]).unwrap();
    basic_compat::run_source(
        &format!(
            "10 SYS \"XRicochet_Desktop\",1,&1800,0,{},32 TO KIND%,TYPE%,SIZE% ; FLAGS%\n20 !&6018=FLAGS%:END",
            edge
        ),
        &mut task,
        &mut dispatcher,
    )
    .expect("invalid complete output range becomes an X error");
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    assert_ne!(word(&task, RESULT + 0x18) & 1, 0);
    assert_eq!(task.memory.read_bytes(edge, 2).unwrap(), &[0x2D; 2]);

    task.memory.write_bytes(VOLUME_BUFFER, &[0x6B; 16]).unwrap();
    basic_compat::run_source(
        "10 SYS \"XRicochet_Desktop\",2,&1A00,3 TO VLEN%,A%,B%,C% ; FLAGS%\n\
         20 !&6014=FLAGS%:END",
        &mut task,
        &mut dispatcher,
    )
    .expect("X volume-name buffer error is returned through V");
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    assert_ne!(word(&task, RESULT + 0x14) & 1, 0);
    assert_eq!(
        task.memory.read_bytes(VOLUME_BUFFER, 9).unwrap(),
        &[0x6B; 9]
    );

    task.memory.write_bytes(edge, &[0x4C; 2]).unwrap();
    basic_compat::run_source(
        &format!(
            "10 SYS \"XRicochet_Desktop\",2,{},16 TO VLEN%,A%,B%,C% ; FLAGS%\n20 !&601C=FLAGS%:END",
            edge
        ),
        &mut task,
        &mut dispatcher,
    )
    .expect("invalid volume output span is an X error");
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    assert_ne!(word(&task, RESULT + 0x1C) & 1, 0);
    assert_eq!(task.memory.read_bytes(edge, 2).unwrap(), &[0x4C; 2]);

    task.memory.write_bytes(VOLUME_BUFFER, &[0xC3; 16]).unwrap();
    basic_compat::run_source(
        "10 SYS \"Ricochet_Desktop\",2,&1A00,16 TO VLEN%\n\
         20 !&6020=VLEN%:END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    assert_eq!(word(&task, RESULT + 0x20), 8);
    assert_eq!(
        task.memory.read_c_string(VOLUME_BUFFER, 16).unwrap(),
        b"DemoDisk"
    );

    basic_compat::run_source(
        "10 SYS \"Ricochet_Desktop\",3,&1800,0 TO PRESENT%,MODIFIED%\n\
         20 !&6028=PRESENT%:!&602C=MODIFIED%:END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    assert_eq!(word(&task, RESULT + 0x28), 1);
    assert!(word(&task, RESULT + 0x2C) > 0);
    basic_compat::run_source(
        "10 SYS \"Ricochet_Desktop\",3,&1800,&FFFFFFFF TO PRESENT%,MODIFIED%\n20 !&6038=PRESENT%:!&603C=MODIFIED%:END",
        &mut task,
        &mut dispatcher,
    )
    .unwrap();
    assert_eq!(
        [word(&task, RESULT + 0x38), word(&task, RESULT + 0x3C)],
        [0, 0],
        "unavailable modification times remain an explicit false/zero result"
    );

    task.memory
        .write_bytes(DIRECTORY, b"../../outside\0")
        .unwrap();
    task.memory.write_bytes(NAME_BUFFER, &[0x71; 32]).unwrap();
    let invalid_path = basic_compat::run_source(
        "10 SYS \"Ricochet_Desktop\",1,&1800,0,&1900,32\n20 END",
        &mut task,
        &mut dispatcher,
    )
    .expect_err("HostFS traversal is rejected");
    assert!(
        !invalid_path
            .to_string()
            .contains(&_environment.root.display().to_string())
    );
    assert_eq!(task.memory.read_bytes(NAME_BUFFER, 8).unwrap(), &[0x71; 8]);
    task.memory.write_bytes(DIRECTORY, b"$.Catalog\0").unwrap();

    task.memory.write_bytes(edge, b"xx").unwrap();
    task.memory.write_bytes(NAME_BUFFER, &[0x39; 16]).unwrap();
    let unterminated_directory = basic_compat::run_source(
        &format!("10 SYS \"Ricochet_Desktop\",1,{},0,&1900,32\n20 END", edge),
        &mut task,
        &mut dispatcher,
    )
    .expect_err("directory pointer must be a terminated guest string");
    assert!(
        !unterminated_directory
            .to_string()
            .contains(&_environment.root.display().to_string())
    );
    assert_eq!(task.memory.read_bytes(NAME_BUFFER, 8).unwrap(), &[0x39; 8]);

    // Action 4's host mechanism must register this caller's existing Wimp
    // task, not the module's execution task. A subsequent OS-icon Menu click
    // is observed by that same task's Wimp_Poll event queue.
    assert!(
        basic_compat::run_source(
            "10 SYS \"Ricochet_Desktop\",4\n20 END",
            &mut task,
            &mut dispatcher,
        )
        .is_err(),
        "action 4 requires an existing caller Wimp registration"
    );
    assert_named_route(
        &dispatcher,
        "RICOCHET_DESKTOP",
        "DesktopServices",
        "DESKTOPSERVICE",
    );
    let menu_wimp = WimpServer::new(mpsc::channel().0);
    const MENU_TASK_ID: u64 = 0xD359;
    menu_wimp
        .task_started(MENU_TASK_ID, "desktop-service caller")
        .unwrap();
    let mut menu_runtime = Runtime::desktop_task(
        MENU_TASK_ID,
        mpsc::channel().1,
        mpsc::channel().0,
        menu_wimp.clone(),
    );
    menu_runtime
        .run_application(
            "10 DIM description% 32\n\
             20 $description%=\"desktop service test\"\n\
             30 SYS \"Wimp_Initialise\",310,&4B534154,description% TO version%,handle%\n\
             40 END",
        )
        .expect("public module-owned Wimp_Initialise registers the menu caller");
    let mut menu_task = Task::new(MENU_TASK_ID);
    menu_runtime
        .run_application("10 SYS \"Ricochet_Desktop\",4\n20 END")
        .expect("action 4 registers the original desktop caller");
    let metrics = menu_wimp.desktop_metrics();
    menu_wimp.mouse_down(metrics.os_width() - 2, 2, 2);
    let mut poll = ricochet::swi::SwiContext::default();
    poll.registers[1] = 0x1C00;
    menu_wimp
        .dispatch(WIMP_POLL, &mut menu_task, &mut poll)
        .unwrap();
    assert_eq!(
        poll.registers[0], 6,
        "system Menu click is delivered as Mouse_Click"
    );
    assert_eq!(word(&menu_task, 0x1C00 + 12), u32::MAX);
    assert_eq!(word(&menu_task, 0x1C00 + 16), u32::MAX);

    // Display query is read-only and available to an ordinary caller. Check
    // the active module route even on this dispatcher, which lacks a Wimp
    // instance and therefore cannot complete a real display query.
    let query_route = basic_compat::run_source(
        "10 SYS \"Ricochet_Display\",2,0\n20 END",
        &mut task,
        &mut dispatcher,
    );
    assert!(query_route.is_err());
    assert_named_route(
        &dispatcher,
        "RICOCHET_DISPLAY",
        "DisplayManager",
        "DISPLAYSERVICE",
    );
    basic_compat::run_source(
        "10 SYS \"XRicochet_Display\",2,0 TO VERSION%,ACTION% ; FLAGS%\n20 !&6030=FLAGS%:END",
        &mut task,
        &mut dispatcher,
    )
    .expect("X display ABI errors are delivered as V status");
    assert_named_route(
        &dispatcher,
        "RICOCHET_DISPLAY",
        "DisplayManager",
        "DISPLAYSERVICE",
    );
    assert_ne!(word(&task, RESULT + 0x30) & 1, 0);

    // Unpublishing either fixed owner must not leave a Rust fallback behind.
    let mut manager = Task::trusted_mos_session(0xD35A);
    for owner in ["DesktopServices", "DisplayManager"] {
        dispatcher
            .basic64_module_manager()
            .quiesce(owner, &mut manager)
            .unwrap();
        assert!(
            basic_compat::run_source(
                if owner == "DesktopServices" {
                    "10 SYS \"Ricochet_Desktop\",2,&1A00,16\n20 END"
                } else {
                    "10 SYS \"Ricochet_Display\",1,0\n20 END"
                },
                &mut task,
                &mut dispatcher,
            )
            .is_err()
        );
        dispatcher
            .basic64_module_manager()
            .retire(owner, &mut manager)
            .unwrap();
        assert!(
            basic_compat::run_source(
                if owner == "DesktopServices" {
                    "10 SYS \"Ricochet_Desktop\",2,&1A00,16\n20 END"
                } else {
                    "10 SYS \"Ricochet_Display\",1,0\n20 END"
                },
                &mut task,
                &mut dispatcher,
            )
            .is_err()
        );
    }

    // A Wimp-bound ordinary caller can query the register-only ABI, but its
    // apply is denied before persistence or active settings change.
    let wimp = WimpServer::new(mpsc::channel().0);
    let (query_display_tx, query_display_rx) = mpsc::channel();
    let mut desktop =
        Runtime::desktop_task(0xD357, mpsc::channel().1, query_display_tx, wimp.clone());
    let query = desktop.run_application(
        "10 SYS \"Ricochet_Display\",1,0 TO ABI%,ACTION%,RES%,COLOUR%,WIDTH%,HEIGHT%,HOSTW%,HOSTH%,STATUS%\n\
         20 PRINT ABI%;\",\";ACTION%;\",\";RES%;\",\";COLOUR%;\",\";WIDTH%;\",\";HEIGHT%;\",\";HOSTW%;\",\";HOSTH%;\",\";STATUS%\n\
         30 END",
    );
    assert!(
        query.is_ok(),
        "ordinary display query remains permitted: {query:?}"
    );
    assert_eq!(wimp.display_settings(), DisplaySettings::default());
    let query_output = display_text(&query_display_rx);
    let query_fields = query_output
        .trim()
        .split(',')
        .map(|field| {
            field
                .parse::<u32>()
                .expect("display query field is numeric")
        })
        .collect::<Vec<_>>();
    assert_eq!(&query_fields[..4], &[1, 0, 0, 7]);
    assert_eq!(query_fields.len(), 9);
    assert!(query_fields[4..8].iter().all(|dimension| *dimension > 0));
    assert_eq!(query_fields[8], 0, "successful query returns R8=0");

    let before_config = fs::read(_environment.root.join("configure")).unwrap_or_default();
    let mut ordinary =
        Runtime::desktop_task(0xD358, mpsc::channel().1, mpsc::channel().0, wimp.clone());
    let denied = ordinary.run_application("10 SYS \"Ricochet_Display\",1,1,1,3\n20 END");
    assert!(
        denied.is_err(),
        "ordinary caller must not gain ConfigurationWrite"
    );
    assert_eq!(wimp.display_settings(), DisplaySettings::default());
    assert_eq!(
        fs::read(_environment.root.join("configure")).unwrap_or_default(),
        before_config
    );

    let (apply_display_tx, apply_display_rx) = mpsc::channel();
    let mut trusted =
        Runtime::windowed_with_desktop(mpsc::channel().1, apply_display_tx, wimp.clone());
    trusted
        .run_application(&format!(
            "10 SYS \"Ricochet_Display\",1,1,{},{} TO ABI%,ACTION%,RES%,COLOUR%,WIDTH%,HEIGHT%,HOSTW%,HOSTH%,STATUS%\n20 PRINT STATUS%;\",\";RES%;\",\";COLOUR%\n30 END",
            DesktopResolution::R640x480.id(),
            DisplayColour::Rgb555.id()
        ))
        .expect("trusted caller can persist then apply a valid combined request");
    let apply_output = display_text(&apply_display_rx);
    assert!(
        apply_output.contains("0,1,6"),
        "successful display apply should return status and selected enum IDs: {apply_output:?}"
    );
    assert_eq!(
        wimp.display_settings(),
        DisplaySettings {
            resolution: DesktopResolution::R640x480,
            colour: DisplayColour::Rgb555,
        }
    );
    let persisted = fs::read_to_string(_environment.root.join("configure")).unwrap();
    assert!(persisted.contains("WimpMode=X640 Y480 C32K"));

    let before_invalid = wimp.display_settings();
    let mut invalid_ids =
        Runtime::windowed_with_desktop(mpsc::channel().1, mpsc::channel().0, wimp.clone());
    assert!(
        invalid_ids
            .run_application("10 SYS \"Ricochet_Display\",1,1,99,99\n20 END")
            .is_err()
    );
    assert_eq!(wimp.display_settings(), before_invalid);
    assert_eq!(
        fs::read_to_string(_environment.root.join("configure")).unwrap(),
        persisted,
        "unsupported enum IDs fail before persistence"
    );

    #[cfg(target_os = "linux")]
    {
        let failed_wimp = WimpServer::new(mpsc::channel().0);
        let prior = failed_wimp.display_settings();
        let failure_path = PathBuf::from("/proc/self/ricochet-desktop/configure");
        unsafe { std::env::set_var("RICOCHET_CONFIG_PATH", &failure_path) };
        let (_input_tx, input_rx) = mpsc::channel();
        let (failed_display_tx, failed_display_rx) = mpsc::channel();
        let mut failed =
            Runtime::windowed_with_desktop(input_rx, failed_display_tx, failed_wimp.clone());
        failed
            .run_application("10 SYS \"Ricochet_Display\",1,1,1,6 TO A%,B%,C%,D%,E%,F%,G%,H%,STATUS%\n20 PRINT STATUS%;\",\";C%;\",\";D%\n30 END")
            .expect("save failures return through R8 rather than applying partial state");
        let output = display_text(&failed_display_rx);
        assert!(
            output.ends_with("1,0,7\n\r"),
            "failed save reports R8=1 and the unchanged Window/Rgb888 IDs: {output:?}"
        );
        assert_eq!(failed_wimp.display_settings(), prior);
    }
}
