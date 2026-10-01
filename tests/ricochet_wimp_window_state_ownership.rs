use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    host::HostConsole,
    memory::Task,
    runtime::Runtime,
    swi::{DisplayEvent, SwiContext, SwiDispatchRoute, SwiDispatcher},
    wimp::{
        WIMP_CLOSE_WINDOW, WIMP_GET_WINDOW_STATE, WIMP_OPEN_WINDOW, WIMP_SET_EXTENT, WimpServer,
    },
};

const X_BIT: u32 = 1 << 17;
const TASK_ID: u64 = 0xC501;
const OTHER_TASK_ID: u64 = 0xC502;

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
            "ricochet-wimp-window-state-{}-{nonce}",
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

fn output_text(events: &mpsc::Receiver<DisplayEvent>, task_id: u64) -> String {
    events
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::WriteByte {
                task_id: actual,
                byte,
                ..
            } if actual == task_id => Some(char::from(byte)),
            _ => None,
        })
        .collect()
}

fn call_headless(dispatcher: &mut SwiDispatcher, number: u32, address: u32) -> SwiContext {
    let mut task = Task::new(OTHER_TASK_ID);
    let mut context = SwiContext::default();
    context.registers[0] = 0xAABB_CCDD;
    context.registers[1] = address;
    assert!(
        dispatcher
            .dispatch(number, &mut task, &mut context)
            .is_err()
    );
    context
}

fn assert_module_route(dispatcher: &SwiDispatcher, number: u32, definition: &str) {
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned {
                number: actual,
                module,
                definition: actual_definition,
                ..
            }) if *actual == number
                && module.eq_ignore_ascii_case("Wimp")
                && actual_definition.eq_ignore_ascii_case(definition)
        ),
        "SWI &{number:X} did not route to Wimp::{definition}: {:?}",
        dispatcher.last_dispatch_route()
    );
    assert_eq!(dispatcher.transitional_dispatch_count(), 0);
}

#[test]
fn wimp_window_state_services_are_owned_checked_atomic_and_task_scoped() {
    let _environment = IsolatedEnvironment::new();

    // The public numbers remain module-owned even when the host has no Wimp
    // desktop. This also proves there is no hidden Rust numeric fallback.
    let mut headless = SwiDispatcher::new(HostConsole::stdio());
    for (number, definition) in [
        (WIMP_OPEN_WINDOW, "OPENWINDOWSERVICE"),
        (WIMP_CLOSE_WINDOW, "CLOSEWINDOWSERVICE"),
        (WIMP_GET_WINDOW_STATE, "GETWINDOWSTATESERVICE"),
        (WIMP_SET_EXTENT, "SETEXTENTSERVICE"),
    ] {
        let descriptor = headless
            .module_registry()
            .current_swi_definition(number)
            .expect("window-state service is published");
        assert_eq!(descriptor.name.to_ascii_uppercase(), definition);
        assert!(descriptor.source_path.ends_with("Wimp.bas64"));
        let context = call_headless(&mut headless, number, u32::MAX);
        assert_module_route(&headless, number, definition);
        assert_eq!(context.registers[1], u32::MAX, "R1 is preserved on errors");
    }
    let mut headless_task = Task::new(OTHER_TASK_ID + 1);
    let mut x_context = SwiContext::default();
    x_context.registers[1] = u32::MAX;
    assert!(
        headless
            .dispatch(
                WIMP_GET_WINDOW_STATE | X_BIT,
                &mut headless_task,
                &mut x_context
            )
            .is_ok()
    );
    assert!(x_context.overflow, "X form reports the owner error via V");
    assert_module_route(&headless, WIMP_GET_WINDOW_STATE, "GETWINDOWSTATESERVICE");

    let mut module_manager_task = Task::trusted_mos_session(OTHER_TASK_ID + 2);
    headless
        .basic64_module_manager()
        .quiesce("Wimp", &mut module_manager_task)
        .expect("trusted module manager can quiesce Wimp");
    for (number, definition) in [
        (WIMP_OPEN_WINDOW, "OPENWINDOWSERVICE"),
        (WIMP_CLOSE_WINDOW, "CLOSEWINDOWSERVICE"),
        (WIMP_GET_WINDOW_STATE, "GETWINDOWSTATESERVICE"),
        (WIMP_SET_EXTENT, "SETEXTENTSERVICE"),
    ] {
        let mut task = Task::new(OTHER_TASK_ID + 3);
        let mut context = SwiContext::default();
        context.registers[1] = u32::MAX;
        assert!(headless.dispatch(number, &mut task, &mut context).is_err());
        assert_eq!(headless.transitional_dispatch_count(), 0);
        assert!(
            !matches!(
                headless.last_dispatch_route(),
                Some(SwiDispatchRoute::TransitionalRust { number: actual }) if *actual == number
            ),
            "quiesced Wimp must not fall back through the old Rust handler for {definition}"
        );
    }

    let (updates, _update_events) = mpsc::channel();
    let wimp = WimpServer::new(updates);
    let (display_tx, display_rx) = mpsc::channel();
    let mut runtime = Runtime::desktop_task(TASK_ID, mpsc::channel().1, display_tx, wimp.clone());

    // Create one ordinary Wimp window through the established API, then run
    // every migrated operation through its public named SYS route.
    let source = String::from(
        "10 MODE 20\n\
         20 DIM description% 24\n\
         30 $description%=\"Window state service test\"\n\
         40 SYS \"Wimp_Initialise\",310,&4B534154,description%,0 TO version%,task%\n\
         50 DIM definition% 87\n\
         60 !definition%+28=&3F000002-2147483648\n\
         70 !definition%+56=1\n\
         80 title%=definition%+72\n\
         90 $title%=\"State test\"\n\
         100 !definition%=72:!definition%+4=280:!definition%+8=872:!definition%+12=840\n\
         110 !definition%+40=0:!definition%+44=-1024:!definition%+48=840:!definition%+52=0\n\
         120 !definition%+60=&3000:!definition%+68=0:!definition%+84=0\n\
         130 SYS \"Wimp_CreateWindow\",,definition% TO window%\n\
         140 DIM open% 31\n\
         150 !open%=window%:!open%+4=72:!open%+8=280:!open%+12=872:!open%+16=840\n\
         160 !open%+20=0:!open%+24=0:!open%+28=-1\n\
         170 SYS \"Wimp_OpenWindow\",,open%\n\
         180 DIM state% 35:!state%=window%:SYS \"Wimp_GetWindowState\",,state% TO r0%,r1%\n\
         190 PRINT \"OPEN=\";!state%;\",\";!(state%+4);\",\";!(state%+8);\",\";!(state%+12);\",\";!(state%+16);\",\";!(state%+20);\",\";!(state%+24);\",\";!(state%+28);\",\";(!(state%+32) AND 196608)\n\
         200 END",
    );
    runtime
        .run_application(&source)
        .expect("create/open/query via public Wimp services");
    let window = wimp
        .desktop_windows()
        .into_iter()
        .find(|window| window.owner_task_id == TASK_ID)
        .expect("window remains open after initial source");
    assert_eq!(window.work_area.min_x, 72);
    assert_eq!(window.work_area.min_y, 280);
    assert_eq!(window.work_area.max_x, 872);
    assert_eq!(window.work_area.max_y, 840);
    let first_output = output_text(&display_rx, TASK_ID);
    let open_state = first_output
        .split("OPEN=")
        .nth(1)
        .unwrap_or_else(|| panic!("state dump is emitted; output was {first_output:?}"));
    let open_state = open_state.lines().next().unwrap_or_default();
    let words = open_state
        .split(',')
        .map(|word| word.parse::<i32>().expect("state word is signed decimal"))
        .collect::<Vec<_>>();
    assert_eq!(words.len(), 9, "the full 36-byte state block is observable");
    assert_eq!(words[0] as u32, window.handle);
    assert_eq!(&words[1..7], &[72, 280, 872, 840, 0, 0]);
    assert_eq!(words[7], -1, "sole open window has no window in front");
    assert_eq!(
        words[8] & ((1 << 16) | (1 << 17)),
        (1 << 16) | (1 << 17),
        "state reports open and unobscured"
    );

    // A second task may initialise Wimp, but cannot open/query/close or change
    // the first caller's window by reusing its handle.
    let (other_display_tx, _other_display_rx) = mpsc::channel();
    let mut other = Runtime::desktop_task(
        OTHER_TASK_ID,
        mpsc::channel().1,
        other_display_tx,
        wimp.clone(),
    );
    let cross_task = format!(
        "10 MODE 20\n20 DIM d% 24\n30 $d%=\"Other task\"\n40 SYS \"Wimp_Initialise\",310,&4B534154,d%,0 TO v%,t%\n\
             50 DIM b% 35\n60 !b%={}\n70 SYS \"Wimp_GetWindowState\",,b%\n80 END",
        window.handle
    );
    let before = wimp.desktop_windows();
    assert!(other.run_application(&cross_task).is_err());
    assert_eq!(
        wimp.desktop_windows(),
        before,
        "failed cross-task query is inert"
    );
    for cross_call in [
        format!(
            "10 DIM b% 31\n20 !b%={}\n30 !b%+4=72:!b%+8=280:!b%+12=872:!b%+16=840\n\
             40 !b%+28=-1\n50 SYS \"Wimp_OpenWindow\",,b%\n60 END",
            window.handle
        ),
        format!(
            "10 DIM b% 3:!b%={}\n20 SYS \"Wimp_CloseWindow\",,b%\n30 END",
            window.handle
        ),
        format!(
            "10 DIM b% 15\n20 !b%=-100:!b%+4=-2000:!b%+8=900:!b%+12=0\n\
             30 SYS \"Wimp_SetExtent\",{},b%\n40 END",
            window.handle
        ),
    ] {
        other
            .run_application(&cross_call)
            .expect_err("cross-task window-state access is denied");
        assert_eq!(
            wimp.desktop_windows(),
            before,
            "cross-task Wimp call is inert"
        );
    }

    // Invalid output span and invalid geometry/stacking are rejected before
    // state changes. A later healthy close/reopen proves the route recovers.
    let invalid_query = format!("10 SYS \"XWimp_GetWindowState\",,{}\n20 END", u32::MAX - 8);
    runtime
        .run_application(&invalid_query)
        .expect("X form reports invalid block address without stopping BASIC");
    assert_eq!(wimp.desktop_windows(), before);

    let invalid_handle_query = "10 DIM b% 35\n\
         20 !b%=999999:!b%+4=11:!b%+8=22:!b%+12=33:!b%+16=44\n\
         30 !b%+20=55:!b%+24=66:!b%+28=77:!b%+32=88\n\
         40 SYS \"XWimp_GetWindowState\",,b%\n\
         50 PRINT \"BAD=\";!b%;\",\";!(b%+4);\",\";!(b%+8);\",\";!(b%+12);\",\";!(b%+16);\",\";!(b%+20);\",\";!(b%+24);\",\";!(b%+28);\",\";!(b%+32)\n60 END";
    runtime
        .run_application(invalid_handle_query)
        .expect("X form returns stale-handle error without mutating caller block");
    let bad_output = output_text(&display_rx, TASK_ID);
    let bad_state = bad_output
        .split("BAD=")
        .nth(1)
        .and_then(|tail| tail.lines().next())
        .unwrap_or_else(|| panic!("invalid-handle block dump missing: {bad_output:?}"));
    let bad_words = bad_state
        .split(',')
        .map(|word| word.trim().parse::<i32>().expect("sentinel word is signed"))
        .collect::<Vec<_>>();
    assert_eq!(bad_words, [999999, 11, 22, 33, 44, 55, 66, 77, 88]);

    let invalid_open = format!(
        "10 DIM b% 31\n20 !b%={}\n30 !b%+4=20:!b%+8=100:!b%+12=10:!b%+16=380\n\
         40 !b%+28=-1\n50 SYS \"XWimp_OpenWindow\",,b%\n60 END",
        window.handle
    );
    runtime
        .run_application(&invalid_open)
        .expect("X form returns invalid geometry through V");
    assert_eq!(wimp.desktop_windows(), before);

    let extent = format!(
        "10 DIM e% 15\n20 !e%=0:!e%+4=-2000:!e%+8=900:!e%+12=0\n\
         30 SYS \"Wimp_SetExtent\",{},e%\n40 DIM o% 31:!o%={}\n\
         50 !o%+4=72:!o%+8=280:!o%+12=872:!o%+16=840\n\
         60 !o%+20=0:!o%+24=-1400:!o%+28=-1:SYS \"Wimp_OpenWindow\",,o%\n\
         70 DIM s% 35:!s%={}\n80 SYS \"Wimp_GetWindowState\",,s%\n\
         90 PRINT \"EXTENT=\";!s%;\",\";!(s%+4);\",\";!(s%+8);\",\";!(s%+12);\",\";!(s%+16);\",\";!(s%+20);\",\";!(s%+24);\",\";!(s%+28);\",\";(!(s%+32) AND 196608)\n100 END",
        window.handle, window.handle, window.handle
    );
    runtime
        .run_application(&extent)
        .expect("valid SetExtent and subsequent state query succeed");
    let changed = wimp
        .desktop_windows()
        .into_iter()
        .find(|item| item.handle == window.handle)
        .unwrap();
    assert_eq!(changed.work_extent.min_y, -2000);
    assert_eq!(changed.work_extent.max_x, 900);
    let extent_output = output_text(&display_rx, TASK_ID);
    let extent_state = extent_output
        .split("EXTENT=")
        .nth(1)
        .and_then(|tail| tail.lines().next())
        .unwrap_or_else(|| panic!("post-extent state dump missing: {extent_output:?}"));
    let extent_words = extent_state
        .split(',')
        .map(|word| {
            word.trim()
                .parse::<i32>()
                .expect("extent state word is signed")
        })
        .collect::<Vec<_>>();
    assert_eq!(extent_words.len(), 9);
    assert_eq!(&extent_words[1..5], &[72, 280, 872, 840]);
    assert_eq!(
        extent_words[6], -1400,
        "new extent permits this scroll position"
    );

    let invalid_extent = format!(
        "10 DIM e% 15\n20 !e%=0:!e%+4=-100:!e%+8=10:!e%+12=0\n\
         30 SYS \"XWimp_SetExtent\",{},e%\n40 END",
        window.handle
    );
    let before_bad_extent = wimp.desktop_windows();
    runtime
        .run_application(&invalid_extent)
        .expect("X form returns invalid extent through V");
    assert_eq!(wimp.desktop_windows(), before_bad_extent);

    let close_and_query = format!(
        "10 DIM c% 3:!c%={}\n20 SYS \"Wimp_CloseWindow\",,c%\n\
         30 DIM s% 35:!s%={}\n40 SYS \"Wimp_GetWindowState\",,s%\n\
         50 PRINT \"CLOSED=\";!s%;\",\";!(s%+28);\",\";(!(s%+32) AND 196608)\n60 END",
        window.handle, window.handle
    );
    runtime
        .run_application(&close_and_query)
        .expect("close and closed-state query both succeed");
    let closed_output = output_text(&display_rx, TASK_ID);
    let closed_state = closed_output
        .split("CLOSED=")
        .nth(1)
        .and_then(|tail| tail.lines().next())
        .unwrap_or_else(|| panic!("closed-state block dump missing: {closed_output:?}"));
    let closed_words = closed_state
        .split(',')
        .map(|word| {
            word.trim()
                .parse::<i32>()
                .expect("closed state word is signed")
        })
        .collect::<Vec<_>>();
    assert_eq!(closed_words.len(), 3);
    assert_eq!(closed_words[0] as u32, window.handle);
    assert_eq!(closed_words[1], -1);
    assert_eq!(
        closed_words[2] & (1 << 16),
        0,
        "closed window clears the open flag"
    );

    let reopen = format!(
        "10 DIM o% 31:!o%={}\n20 !o%+4=72:!o%+8=280:!o%+12=872:!o%+16=840\n\
         30 !o%+20=0:!o%+24=0:!o%+28=-1\n40 SYS \"Wimp_OpenWindow\",,o%\n50 END",
        window.handle
    );
    runtime
        .run_application(&reopen)
        .expect("previously closed window reopens cleanly");
    assert!(
        wimp.desktop_windows()
            .iter()
            .any(|item| item.handle == window.handle && item.owner_task_id == TASK_ID)
    );
}
