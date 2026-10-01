use std::{
    fmt::Write as _,
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    host::HostConsole,
    memory::{GUEST_MEMORY_BASE, Task},
    runtime::Runtime,
    swi::{SwiContext, SwiDispatchRoute, SwiDispatcher},
    wimp::{DesktopTaskKind, WIMP_CLOSE_DOWN, WIMP_INITIALISE, WIMP_START_TASK, WimpServer},
};

const TASK_MAGIC: u32 = u32::from_le_bytes(*b"TASK");
const DESCRIPTION: u32 = GUEST_MEMORY_BASE + 0x1800;
const PARENT_TASK: u64 = 0xD35A;

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
            "ricochet-wimp-lifecycle-{}-{nonce}",
            std::process::id()
        ));
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

fn assert_owned_route(dispatcher: &SwiDispatcher, number: u32, definition: &str) {
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned {
                number: actual_number,
                name,
                module,
                definition: actual_definition,
                ..
            }) if *actual_number == number
                && name.eq_ignore_ascii_case(match number {
                    WIMP_INITIALISE => "Wimp_Initialise",
                    WIMP_CLOSE_DOWN => "Wimp_CloseDown",
                    WIMP_START_TASK => "Wimp_StartTask",
                    _ => unreachable!(),
                })
                && module.eq_ignore_ascii_case("Wimp")
                && actual_definition.eq_ignore_ascii_case(definition)
        ),
        "wrong Wimp lifecycle dispatch route: {:?}",
        dispatcher.last_dispatch_route()
    );
    assert_eq!(dispatcher.transitional_dispatch_count(), 0);
}

fn desktop_runtime(task_id: u64, wimp: Arc<WimpServer>) -> Runtime {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, _display_receiver) = mpsc::channel();
    Runtime::desktop_task(task_id, input_receiver, display_sender, wimp)
}

fn initialise_source(version: u32, magic: u32, list: u32) -> String {
    format!(
        "10 DIM description% 32\n\
         20 $description%=\"Wimp lifecycle test\"\n\
         30 SYS \"Wimp_Initialise\",{version},&{magic:X},description%,&{list:X} TO version%,handle%\n\
         40 END"
    )
}

fn start_task_source(command: &[u8]) -> String {
    let mut source = String::from("10 DIM command% 256\n");
    for (index, byte) in command.iter().copied().chain([0]).enumerate() {
        writeln!(
            &mut source,
            "{} ?command%+{}={}",
            (index + 2) * 10,
            index,
            byte
        )
        .unwrap();
    }
    let next_line = (command.len() + 3) * 10;
    writeln!(&mut source, "{next_line} SYS \"Wimp_StartTask\",command%").unwrap();
    write!(&mut source, "{} END", next_line + 10).unwrap();
    source
}

#[test]
fn wimp_lifecycle_swis_are_module_owned_checked_and_task_scoped() {
    let _environment = IsolatedEnvironment::new();

    // A dispatcher without a desktop must still route these numeric SWIs to
    // the BASIC64 owner and fail closed at the absent host mechanism.
    let mut headless = SwiDispatcher::new(HostConsole::stdio());
    let mut headless_task = Task::new(0xD359);
    headless_task
        .memory
        .write_bytes(DESCRIPTION, b"headless task\r")
        .unwrap();
    let modules = headless
        .module_registry()
        .active_modules_sorted()
        .into_iter()
        .map(|module| module.manifest.name.clone())
        .collect::<Vec<_>>();
    assert!(
        modules.iter().any(|name| name.eq_ignore_ascii_case("Wimp")),
        "Wimp module is not published: {modules:?}"
    );
    for (number, definition) in [
        (WIMP_INITIALISE, "INITIALISESERVICE"),
        (WIMP_CLOSE_DOWN, "CLOSEDOWNSERVICE"),
        (WIMP_START_TASK, "STARTTASKSERVICE"),
    ] {
        let descriptor = headless
            .module_registry()
            .current_swi_definition(number)
            .expect("Wimp service definition is published");
        assert!(descriptor.name.eq_ignore_ascii_case(definition));
        assert!(descriptor.source_path.ends_with("Wimp.bas64"));
    }
    let mut init = SwiContext::default();
    init.registers[0] = 310;
    init.registers[1] = TASK_MAGIC;
    init.registers[2] = DESCRIPTION;
    assert!(
        headless
            .dispatch(WIMP_INITIALISE, &mut headless_task, &mut init)
            .is_err()
    );
    assert_owned_route(&headless, WIMP_INITIALISE, "INITIALISESERVICE");
    let mut x_init = SwiContext::default();
    x_init.registers[0] = 310;
    x_init.registers[1] = TASK_MAGIC;
    x_init.registers[2] = DESCRIPTION;
    assert!(
        headless
            .dispatch(WIMP_INITIALISE | (1 << 17), &mut headless_task, &mut x_init)
            .is_ok()
    );
    assert!(x_init.overflow, "X form reports absent-backend error via V");
    assert_owned_route(&headless, WIMP_INITIALISE, "INITIALISESERVICE");
    let mut close_without_wimp = SwiContext::default();
    close_without_wimp.registers[1] = TASK_MAGIC;
    assert!(
        headless
            .dispatch(WIMP_CLOSE_DOWN, &mut headless_task, &mut close_without_wimp)
            .is_err()
    );
    assert_owned_route(&headless, WIMP_CLOSE_DOWN, "CLOSEDOWNSERVICE");
    let mut start_without_wimp = SwiContext::default();
    start_without_wimp.registers[0] = GUEST_MEMORY_BASE;
    assert!(
        headless
            .dispatch(
                WIMP_START_TASK | (1 << 17),
                &mut headless_task,
                &mut start_without_wimp,
            )
            .is_ok()
    );
    assert!(start_without_wimp.overflow);
    assert_owned_route(&headless, WIMP_START_TASK, "STARTTASKSERVICE");

    let (desktop_updates, _desktop_update_events) = mpsc::channel();
    let wimp = WimpServer::new(desktop_updates);
    let mut parent = desktop_runtime(PARENT_TASK, wimp.clone());

    // Rejected inputs are atomic: bad magic and an inaccessible message list
    // cannot leave a half-registered task behind. Version 200 ignores R3 per
    // PRM; version 300+ validates a non-null list before registration.
    assert!(
        parent
            .run_application(&initialise_source(310, 0, 0))
            .is_err()
    );
    assert!(!wimp.is_guest_task_active(PARENT_TASK));
    assert!(
        parent
            .run_application(&initialise_source(300, TASK_MAGIC, u32::MAX))
            .is_err()
    );
    assert!(!wimp.is_guest_task_active(PARENT_TASK));
    parent
        .run_application(&initialise_source(200, TASK_MAGIC, u32::MAX))
        .expect("version 200 does not consume R3");
    assert!(wimp.is_guest_task_active(PARENT_TASK));

    // Re-initialisation is rejected without unregistering the live task.
    assert!(
        parent
            .run_application(&initialise_source(310, TASK_MAGIC, 0))
            .is_err()
    );
    assert!(wimp.is_guest_task_active(PARENT_TASK));

    // The hosted StartTask implementation accepts only its documented command
    // subset and queues requests; it never invokes an arbitrary host command.
    parent
        .run_application(
            "10 DIM command% 256\n\
             20 $command%=\"*Commands\"\n\
             30 SYS \"Wimp_StartTask\",command% TO child%\n\
             40 END",
        )
        .expect("supported Commands launch is queued");
    let commands_request = wimp.take_pending_launches();
    assert_eq!(commands_request.len(), 1);
    assert_eq!(commands_request[0].kind, DesktopTaskKind::Commands);

    parent
        .run_application(
            "10 DIM command% 256\n\
             20 $command%=\"*BASIC $.Apps.Sample\"\n\
             30 SYS \"Wimp_StartTask\",command% TO child%\n\
             40 END",
        )
        .expect("supported BASIC file launch is queued");
    let requests = wimp.take_pending_launches();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, DesktopTaskKind::File);
    assert_eq!(requests[0].guest_path, "$.Apps.Sample");
    let child_id = requests[0].task_id;
    let mut children_before = wimp.active_guest_task_ids();
    children_before.sort_unstable();
    assert!(children_before.contains(&child_id));

    // A tab is a supported command separator; the same launch contract still
    // applies after normalization.
    parent
        .run_application(
            "10 DIM command% 256\n\
             20 $command%=\"*BASIC\t$.Apps.Tabbed\"\n\
             30 SYS \"Wimp_StartTask\",command% TO child%\n\
             40 END",
        )
        .expect("tab-separated BASIC launch is accepted");
    let tabbed = wimp.take_pending_launches();
    assert_eq!(tabbed.len(), 1);
    assert_eq!(tabbed[0].kind, DesktopTaskKind::File);
    assert_eq!(tabbed[0].guest_path, "$.Apps.Tabbed");
    let tabbed_task_id = tabbed[0].task_id;
    assert!(wimp.is_guest_task_active(tabbed_task_id));
    children_before = wimp.active_guest_task_ids();
    children_before.sort_unstable();

    // StartTask accepts both quote styles around guest paths with spaces and
    // returns the exact guest path bytes in the queued launch request.
    fs::create_dir_all(_environment.root.join("Quoted Dir")).unwrap();
    for leaf in ["Double", "Single", "Trailing", "Carriage", "Café"] {
        fs::write(_environment.root.join("Quoted Dir").join(leaf), b"10 END\n").unwrap();
    }
    let valid_commands = [
        (
            b"*BASIC \"$.Quoted Dir.Double\"   \t".to_vec(),
            "$.Quoted Dir.Double".to_string(),
        ),
        (
            b"*BASIC '$.Quoted Dir.Single'\t  ".to_vec(),
            "$.Quoted Dir.Single".to_string(),
        ),
        (
            b"*BASIC $.Quoted Dir.Trailing \t".to_vec(),
            "$.Quoted Dir.Trailing".to_string(),
        ),
        (
            b"*BASIC $.Quoted Dir.Carriage\r".to_vec(),
            "$.Quoted Dir.Carriage".to_string(),
        ),
        (
            "*BASIC '$.Quoted Dir.Café'".as_bytes().to_vec(),
            "$.Quoted Dir.Café".to_string(),
        ),
    ];
    let mut quoted_task_ids = Vec::new();
    for (command, expected_path) in valid_commands {
        parent
            .run_application(&start_task_source(&command))
            .unwrap_or_else(|error| panic!("valid StartTask command {command:?}: {error}"));
        let launches = wimp.take_pending_launches();
        assert_eq!(launches.len(), 1, "exactly one launch should be queued");
        assert_eq!(launches[0].kind, DesktopTaskKind::File);
        assert_eq!(launches[0].guest_path.as_bytes(), expected_path.as_bytes());
        quoted_task_ids.push(launches[0].task_id);
    }

    // The byte-count limit is enforced before task/queue side effects. A
    // near-boundary UTF-8 path remains byte-preserving; an overlong command
    // is rejected and a following ordinary launch still works.
    let near_limit_path = format!("$.{}", "é".repeat(116));
    let near_limit_command = format!("*BASIC '{near_limit_path}'").into_bytes();
    assert!(near_limit_command.len() <= 255);
    parent
        .run_application(&start_task_source(&near_limit_command))
        .expect("bounded multibyte path is accepted");
    let near_limit = wimp.take_pending_launches();
    assert_eq!(near_limit.len(), 1);
    assert_eq!(
        near_limit[0].guest_path.as_bytes(),
        near_limit_path.as_bytes()
    );
    quoted_task_ids.push(near_limit[0].task_id);

    let boundary_path = format!("$.{}", "A".repeat(244));
    let boundary_command = format!("*BASIC '{boundary_path}'").into_bytes();
    assert_eq!(boundary_command.len(), 255);
    parent
        .run_application(&start_task_source(&boundary_command))
        .expect("exactly 255 command bytes are accepted");
    let boundary_launch = wimp.take_pending_launches();
    assert_eq!(boundary_launch.len(), 1);
    assert_eq!(boundary_launch[0].guest_path, boundary_path);
    quoted_task_ids.push(boundary_launch[0].task_id);

    let overlong_command = format!("*BASIC '{}B'", "A".repeat(246)).into_bytes();
    assert_eq!(overlong_command.len(), 256);
    assert!(
        parent
            .run_application(&start_task_source(&overlong_command))
            .is_err()
    );
    assert!(wimp.take_pending_launches().is_empty());
    let mut after_overlong = wimp.active_guest_task_ids();
    after_overlong.sort_unstable();
    let mut expected_active = children_before.clone();
    expected_active.extend(quoted_task_ids.iter().copied());
    expected_active.sort_unstable();
    assert_eq!(after_overlong, expected_active);

    for malformed in [
        b"*BASIC \"$.Quoted Dir.Unclosed".as_slice(),
        b"*BASIC '$.Quoted Dir.Unclosed".as_slice(),
        b"*BASIC \"\"".as_slice(),
        b"*BASIC '$.Quoted Dir.Single'junk".as_slice(),
        b"*BASIC \"$.Quoted Dir.Double\"junk".as_slice(),
        b"*BASIC \xff".as_slice(),
    ] {
        assert!(
            parent
                .run_application(&start_task_source(malformed))
                .is_err(),
            "malformed command should fail: {malformed:?}"
        );
        assert!(wimp.take_pending_launches().is_empty());
        let mut active = wimp.active_guest_task_ids();
        active.sort_unstable();
        assert_eq!(active, expected_active, "rejected command allocated task");
    }

    // A healthy request after rejected commands proves parser/dispatcher
    // cleanup did not poison the caller's next StartTask call.
    parent
        .run_application(&start_task_source(b"*BASIC $.After.Errors"))
        .expect("healthy StartTask recovers after malformed requests");
    let recovered = wimp.take_pending_launches();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].guest_path, "$.After.Errors");
    quoted_task_ids.push(recovered[0].task_id);
    expected_active.push(recovered[0].task_id);
    expected_active.sort_unstable();

    assert!(
        parent
            .run_application(
                "10 DIM command% 256\n\
             20 $command%=\"*RUN $.Apps.NotSupported\"\n\
             30 SYS \"Wimp_StartTask\",command%\n\
             40 END",
            )
            .is_err()
    );
    assert!(wimp.take_pending_launches().is_empty());
    let mut children_after = wimp.active_guest_task_ids();
    children_after.sort_unstable();
    assert_eq!(
        children_after, expected_active,
        "bad StartTask allocated a task"
    );

    // Embedded control bytes must be rejected as malformed input rather than
    // truncating the command and launching its valid prefix.
    let malformed_source = "10 DIM command% 256\n20 $command%=\"*BASIC\"+CHR$(7)+\"suffix\"\n30 SYS \"Wimp_StartTask\",command%\n40 END";
    assert!(parent.run_application(&malformed_source).is_err());
    assert!(wimp.take_pending_launches().is_empty());
    let mut children_after_control = wimp.active_guest_task_ids();
    children_after_control.sort_unstable();
    assert_eq!(children_after_control, expected_active);

    // Give the parent caller-owned desktop objects so CloseDown cleanup is
    // observable independently of the child launch queue.
    parent
        .run_application(
            "10 DIM definition% 87\n\
             20 !definition%+28=128:!definition%+40=0:!definition%+44=-2000\n\
             30 !definition%+48=900:!definition%+52=0\n\
             40 !definition%+56=1:!definition%+60=0\n\
             45 !definition%+68=48:!definition%+70=48:!definition%+84=0\n\
             50 SYS \"Wimp_CreateWindow\",,definition% TO window%\n\
             60 DIM open% 31:DIM icon% 35\n\
             70 !open%=window%:!open%+4=0:!open%+8=0\n\
             75 !open%+4=30:!open%+8=140:!open%+12=390:!open%+16=440\n\
             76 !open%+20=0:!open%+24=-800:!open%+28=-1\n\
             80 SYS \"Wimp_OpenWindow\",,open%\n\
             90 DIM icon% 35\n\
             100 ?icon%=255:?icon%+1=255:?icon%+2=255:?icon%+3=255\n\
             110 !icon%+20=&3001:?icon%+24=80:?icon%+25=0\n\
             120 SYS \"Wimp_CreateIcon\",,icon% TO icon_handle%\n\
             130 END",
        )
        .expect("parent can create caller-owned window and icon");
    assert!(
        wimp.desktop_windows()
            .iter()
            .any(|window| window.owner_task_id == PARENT_TASK),
        "parent window not present: {:?}",
        wimp.desktop_windows()
    );
    assert!(
        wimp.desktop_icons()
            .iter()
            .any(|icon| icon.owner_task_id == PARENT_TASK)
    );

    // The child is created by StartTask, initializes against its preallocated
    // Wimp identity, rejects the parent's handle, and on CloseDown keeps its
    // host console while its Wimp registration is marked inactive.
    let mut child = desktop_runtime(child_id, wimp.clone());
    child
        .run_application(&initialise_source(300, TASK_MAGIC, 0))
        .expect("queued child can initialize");
    assert!(
        child
            .run_application("10 SYS \"Wimp_CloseDown\",1,&4B534154\n20 END",)
            .is_err(),
        "a child cannot close its parent task handle"
    );
    assert!(wimp.is_guest_task_active(child_id));
    child
        .run_application("10 SYS \"Wimp_CloseDown\",3,&4B534154\n20 END")
        .expect("child can close its own Wimp handle");
    assert!(wimp.is_guest_task_active(child_id));
    assert!(
        wimp.desktop_windows()
            .iter()
            .any(|window| window.owner_task_id == child_id && window.is_console_output)
    );

    // Ordinary task CloseDown removes its Wimp registration and owned windows.
    parent
        .run_application("10 SYS \"Wimp_CloseDown\",1,&4B534154\n20 END")
        .expect("parent closes its own Wimp registration");
    assert!(!wimp.is_guest_task_active(PARENT_TASK));
    assert!(
        wimp.desktop_windows()
            .iter()
            .all(|window| window.owner_task_id != PARENT_TASK)
    );
    assert!(
        wimp.desktop_icons()
            .iter()
            .all(|icon| icon.owner_task_id != PARENT_TASK)
    );
    assert!(
        parent
            .run_application("10 SYS \"Wimp_CloseDown\",1,&4B534154\n20 END")
            .is_err(),
        "repeated CloseDown is rejected rather than affecting another task"
    );

    // Normal task exit is still the final cleanup boundary for started guests.
    wimp.task_exited(child_id);
    assert!(!wimp.is_guest_task_active(child_id));
    assert!(
        wimp.desktop_windows()
            .iter()
            .all(|window| window.owner_task_id != child_id)
    );
    wimp.task_exited(tabbed_task_id);

    // R3=0 is accepted for version 310 in this hosted profile as well.
    let (other_updates, _other_update_events) = mpsc::channel();
    let other_wimp = WimpServer::new(other_updates);
    let mut version310 = desktop_runtime(PARENT_TASK + 1, other_wimp.clone());
    version310
        .run_application(&initialise_source(310, TASK_MAGIC, 0))
        .expect("version 310 accepts the hosted null message-list form");
    assert!(other_wimp.is_guest_task_active(PARENT_TASK + 1));
    version310
        .run_application("10 SYS \"Wimp_CloseDown\",1,&4B534154\n20 END")
        .expect("version 310 task closes cleanly");
    assert!(!other_wimp.is_guest_task_active(PARENT_TASK + 1));
}
