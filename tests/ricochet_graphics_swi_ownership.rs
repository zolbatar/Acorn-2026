use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    basic_compat,
    error::RuntimeError,
    host::HostConsole,
    memory::Task,
    swi::{
        DisplayEvent, OS_PLOT, OS_READ_POINT, OS_WRITE_C, SwiContext, SwiDispatchRoute,
        SwiDispatcher,
    },
};

const X_BIT: u32 = 1 << 17;
const RESULT_ADDRESS: u32 = 0x6000;

struct Environment {
    root: PathBuf,
    old_volume: Option<std::ffi::OsString>,
    old_config: Option<std::ffi::OsString>,
    old_capsule: Option<std::ffi::OsString>,
}

impl Environment {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is valid")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "ricochet-graphics-swi-ownership-{}-{nonce}",
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

impl Drop for Environment {
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

fn dispatcher() -> (SwiDispatcher, mpsc::Receiver<DisplayEvent>) {
    let (_input_sender, input_receiver) = mpsc::channel();
    let (display_sender, display_receiver) = mpsc::channel();
    (
        SwiDispatcher::windowed(HostConsole::windowed(input_receiver), display_sender),
        display_receiver,
    )
}

fn call(
    dispatcher: &mut SwiDispatcher,
    task: &mut Task,
    number: u32,
    registers: &[u32],
) -> (Result<(), RuntimeError>, SwiContext) {
    let mut context = SwiContext::default();
    context.registers[..registers.len()].copy_from_slice(registers);
    let result = dispatcher.dispatch(number, task, &mut context);
    (result, context)
}

fn assert_owner(dispatcher: &SwiDispatcher, number: u32, definition: &str) {
    let swi_name = if number == OS_PLOT {
        "OS_Plot"
    } else {
        "OS_ReadPoint"
    };
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned {
                number: routed_number,
                name,
                module,
                definition: routed_definition,
                ..
            }) if *routed_number == number
                && name.eq_ignore_ascii_case(swi_name)
                && module.eq_ignore_ascii_case("Graphics")
                && routed_definition.eq_ignore_ascii_case(definition)
        ),
        "{swi_name} did not use Graphics::{definition}: {:?}",
        dispatcher.last_dispatch_route()
    );
    assert_eq!(dispatcher.transitional_dispatch_count(), 0);
}

fn write_byte(dispatcher: &mut SwiDispatcher, task: &mut Task, byte: u8) {
    let (result, _) = call(dispatcher, task, OS_WRITE_C, &[u32::from(byte)]);
    result.expect("VDU byte is accepted");
}

fn read_u32(task: &Task, address: u32) -> u32 {
    u32::from_le_bytes(
        task.memory
            .read_bytes(address, 4)
            .unwrap()
            .try_into()
            .unwrap(),
    )
}

#[test]
fn graphics_plot_and_readpoint_are_module_owned_and_preserve_the_public_contract() {
    let _environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut caller = Task::new(0x4501);
    let modules = dispatcher
        .module_registry()
        .active_modules_sorted()
        .into_iter()
        .map(|module| module.manifest.name.clone())
        .collect::<Vec<_>>();
    assert!(
        modules
            .iter()
            .any(|name| name.eq_ignore_ascii_case("Graphics")),
        "Graphics module is not published: {modules:?}"
    );

    // Establish a deterministic mode and ink through the existing VDU path.
    for byte in [22, 0, 18, 0, 1] {
        write_byte(&mut dispatcher, &mut caller, byte);
    }

    let plot_before = dispatcher.module_dispatch_count();
    let (plot_result, plotted) = call(
        &mut dispatcher,
        &mut caller,
        OS_PLOT,
        &[0xAB00_0045, 640, 512],
    );
    plot_result.expect("OS_Plot point operation succeeds for an ordinary Task");
    assert_owner(&dispatcher, OS_PLOT, "PlotService");
    assert_eq!(dispatcher.module_dispatch_count(), plot_before + 1);
    assert_eq!(
        &plotted.registers[..3],
        &[0xAB00_0045, 640, 512],
        "hosted OS_Plot deterministically preserves R0-R2"
    );
    let plot_events = display
        .try_iter()
        .filter(|event| {
            matches!(
                event,
                DisplayEvent::Plot {
                    code: 0x45,
                    x: 640,
                    y: 512,
                    ..
                }
            )
        })
        .count();
    assert_eq!(plot_events, 1, "one public plot must publish exactly once");

    let (read_result, read) = call(
        &mut dispatcher,
        &mut caller,
        OS_READ_POINT,
        &[640, 512, 0xAAAA_AAAA, 0xBBBB_BBBB, 0xCCCC_CCCC],
    );
    read_result.expect("OS_ReadPoint reads the caller's active raster");
    assert_owner(&dispatcher, OS_READ_POINT, "ReadPointService");
    assert_eq!(&read.registers[..2], &[640, 512]);
    assert_eq!(&read.registers[2..5], &[1, 0, 0]);
    let (outside_result, outside) = call(
        &mut dispatcher,
        &mut caller,
        OS_READ_POINT,
        &[u32::MAX, 512, 1, 2, 3],
    );
    outside_result.expect("outside-raster reads return the documented sentinel");
    assert_eq!(&outside.registers[..2], &[u32::MAX, 512]);
    assert_eq!(&outside.registers[2..5], &[u32::MAX, 0, u32::MAX]);

    // Relative movement/line operations and low-byte plot-code handling are
    // exercised independently of the BASIC statement path.
    let (move_result, move_to) = call(
        &mut dispatcher,
        &mut caller,
        OS_PLOT,
        &[0x1000_0040, 700, 500],
    );
    move_result.expect("absolute cursor move operation is supported");
    assert_eq!(&move_to.registers[..3], &[0x1000_0040, 700, 500]);
    let (line_result, line) = call(
        &mut dispatcher,
        &mut caller,
        OS_PLOT,
        &[0x2000_0041, 20, 12],
    );
    line_result.expect("relative line operation is supported");
    assert_owner(&dispatcher, OS_PLOT, "PlotService");
    assert_eq!(&line.registers[..3], &[0x2000_0041, 20, 12]);
    let (negative_result, negative) = call(
        &mut dispatcher,
        &mut caller,
        OS_PLOT,
        &[0x45, (-4_i32) as u32, 512],
    );
    negative_result.expect("signed coordinate bit patterns are accepted");
    assert_eq!(negative.registers[1] as i32, -4);

    let (unsupported_result, unsupported) =
        call(&mut dispatcher, &mut caller, OS_PLOT, &[0x78, 900, 700]);
    assert!(
        unsupported_result.is_err(),
        "unsupported plot groups stay explicit"
    );
    assert_owner(&dispatcher, OS_PLOT, "PlotService");
    assert_eq!(&unsupported.registers[..3], &[0x78, 900, 700]);

    // Public BASIC PLOT and named SYS OS_ReadPoint use the same raster state.
    basic_compat::run_source(
        "10 MODE 0:GCOL 0,1:PLOT 69,320,256:END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("BASIC PLOT executes");
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwned { number, module, definition, .. })
                if *number == OS_PLOT
                    && module.eq_ignore_ascii_case("Graphics")
                    && definition.eq_ignore_ascii_case("PlotService")
        ),
        "BASIC PLOT must enter Graphics::PlotService: {:?}",
        dispatcher.last_dispatch_route()
    );
    let before_named_read = dispatcher.module_dispatch_count();
    basic_compat::run_source(
        "10 SYS \"OS_ReadPoint\",320,256 TO X%,Y%,C%,T%,F%\n\
         20 !&6000=C%:!&6004=T%:!&6008=F%:END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("named BASIC SYS OS_ReadPoint executes");
    assert_eq!(dispatcher.module_dispatch_count(), before_named_read + 1);
    assert_owner(&dispatcher, OS_READ_POINT, "ReadPointService");
    assert_eq!(read_u32(&caller, RESULT_ADDRESS), 1);
    assert_eq!(read_u32(&caller, RESULT_ADDRESS + 4), 0);
    assert_eq!(read_u32(&caller, RESULT_ADDRESS + 8), 0);
    let basic_plot_events = display
        .try_iter()
        .filter(|event| {
            matches!(
                event,
                DisplayEvent::Plot {
                    code: 69,
                    x: 320,
                    y: 256,
                    ..
                }
            )
        })
        .count();
    assert_eq!(basic_plot_events, 1, "BASIC PLOT publishes one plot event");

    let (x_success, x_context) = call(
        &mut dispatcher,
        &mut caller,
        OS_READ_POINT | X_BIT,
        &[320, 256, 0xDEAD_BEEF],
    );
    x_success.expect("X-form successful Graphics SWI returns normally");
    assert_owner(&dispatcher, OS_READ_POINT, "ReadPointService");
    assert_eq!(&x_context.registers[..5], &[320, 256, 1, 0, 0]);

    // Modern text is not stored in the classic guest raster; the error must
    // remain explicit for both named BASIC SYS and the X-form public SWI.
    let modern_error = basic_compat::run_source(
        "REM @BASIC64 MODE=BASIC64 TEXT=MODERN\nSYS \"OS_ReadPoint\"\n",
        &mut caller,
        &mut dispatcher,
    )
    .expect_err("Modern text profile must reject raster readback");
    assert!(
        modern_error
            .to_string()
            .contains("OS_ReadPoint requires TEXT=CLASSIC")
    );
    let (x_error, x_error_context) = call(
        &mut dispatcher,
        &mut caller,
        OS_READ_POINT | X_BIT,
        &[1, 1, 0xCAFE_BABE],
    );
    x_error.expect("X-form Modern-profile failure is delivered through V/error block");
    assert!(x_error_context.overflow);

    // A quiesced owner must fail closed: there is no residual Rust public
    // implementation underneath the module route.
    let mut manager_task = Task::trusted_mos_session(0x4502);
    {
        let mut manager = dispatcher.basic64_module_manager();
        manager
            .quiesce("Graphics", &mut manager_task)
            .expect("trusted manager quiesces Graphics");
    }
    let (inactive, _) = call(&mut dispatcher, &mut caller, OS_PLOT, &[0x45, 1, 1]);
    let inactive = inactive.expect_err("inactive Graphics owner must not fall back to Rust");
    assert!(inactive.to_string().contains("owning module is not active"));

    dispatcher
        .basic64_module_manager()
        .retire("Graphics", &mut manager_task)
        .expect("trusted manager retires the quiesced Graphics owner");
    let (retired_plot, _) = call(&mut dispatcher, &mut caller, OS_PLOT, &[0x45, 1, 1]);
    assert!(
        matches!(&retired_plot, Err(RuntimeError::Structured { type_name, message, .. })
            if type_name == "UnknownSwi" && message.contains("no such SWI &45")),
        "retired OS_Plot must not fall through to a Rust implementation: {retired_plot:?}"
    );
    let (retired_read, _) = call(&mut dispatcher, &mut caller, OS_READ_POINT, &[1, 1]);
    assert!(
        matches!(&retired_read, Err(RuntimeError::Structured { type_name, message, .. })
            if type_name == "UnknownSwi" && message.contains("no such SWI &32")),
        "retired OS_ReadPoint must not fall through to a Rust implementation: {retired_read:?}"
    );
}
