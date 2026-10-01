use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet::{
    basic_compat,
    host::HostConsole,
    memory::Task,
    swi::{DisplayEvent, SwiDispatchRoute, SwiDispatcher},
};

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
            "ricochet-colourtrans-ownership-{}-{nonce}",
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

fn read_u32(task: &Task, address: u32) -> u32 {
    u32::from_le_bytes(
        task.memory
            .read_bytes(address, 4)
            .unwrap()
            .try_into()
            .unwrap(),
    )
}

fn assert_named_owner(dispatcher: &SwiDispatcher, name: &str, definition: &str) {
    assert!(
        matches!(
            dispatcher.last_dispatch_route(),
            Some(SwiDispatchRoute::ModuleOwnedNamed {
                name: routed_name,
                module,
                definition: routed_definition,
                ..
            }) if routed_name.eq_ignore_ascii_case(name)
                && module.eq_ignore_ascii_case("ColourTrans")
                && routed_definition.eq_ignore_ascii_case(definition)
        ),
        "{name} did not route through ColourTrans::{definition}: {:?}",
        dispatcher.last_dispatch_route()
    );
    assert_eq!(dispatcher.transitional_dispatch_count(), 0);
}

#[test]
fn named_colourtrans_services_are_module_owned_and_keep_the_hosted_subset_explicit() {
    let _environment = Environment::new();
    let (mut dispatcher, display) = dispatcher();
    let mut caller = Task::new(0x4601);

    let modules = dispatcher
        .module_registry()
        .active_modules_sorted()
        .into_iter()
        .map(|module| module.manifest.name.clone())
        .collect::<Vec<_>>();
    assert!(
        modules
            .iter()
            .any(|name| name.eq_ignore_ascii_case("ColourTrans")),
        "ColourTrans module is not published: {modules:?}"
    );
    for name in [
        "ColourTrans_ConvertHSVToRGB",
        "ColourTrans_SetGCOL",
        "ColourTrans_WritePalette",
    ] {
        assert_eq!(
            dispatcher.module_registry().swi_number(name),
            None,
            "the transitional name-only contract must not invent or publish a numeric SWI ID"
        );
    }

    // Hosted conversion intentionally differs from native ColourTrans
    // ConvertHSVToRGB: it accepts a signed 16.16 hue, saturation scaled by
    // 65280, and value in R2's low byte. Native PRM instead uses 16.16 for
    // all three values and reports an error when both hue and saturation are
    // zero. These cases pin the existing hosted subset, not native ABI.
    basic_compat::run_source(
        "10 SYS \"ColourTrans_ConvertHSVToRGB\",0,65280,255 TO R%,G%,B%\n\
         20 !&6000=R%:!&6004=G%:!&6008=B%\n\
         30 SYS \"ColourTrans_ConvertHSVToRGB\",7864320,65280,255 TO R%,G%,B%\n\
         40 !&6010=R%:!&6014=G%:!&6018=B%\n\
         50 SYS \"ColourTrans_ConvertHSVToRGB\",15728640,65280,255 TO R%,G%,B%\n\
         60 !&6020=R%:!&6024=G%:!&6028=B%\n\
         70 SYS \"ColourTrans_ConvertHSVToRGB\",0,0,128 TO R%,G%,B%\n\
         80 !&6030=R%:!&6034=G%:!&6038=B%\n\
         90 SYS \"ColourTrans_ConvertHSVToRGB\",0,65280,&123456FF TO R%,G%,B%\n\
         100 !&6040=R%:!&6044=G%:!&6048=B%\n\
         110 SYS \"XColourTrans_ConvertHSVToRGB\",-3932160,65280,255 TO R%,G%,B% ; FLAGS%\n\
         120 !&6050=R%:!&6054=G%:!&6058=B%:!&6060=FLAGS%:END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("published ColourTrans named and X-named services execute");
    assert_named_owner(
        &dispatcher,
        "COLOURTRANS_CONVERTHSVTORGB",
        "COLOURTRANS_CONVERTHSVTORGB",
    );
    assert_eq!(
        [
            0, 4, 8, 16, 20, 24, 32, 36, 40, 48, 52, 56, 64, 68, 72, 80, 84, 88, 96
        ]
        .map(|offset| read_u32(&caller, RESULT_ADDRESS + offset)),
        [
            255, 0, 0, // hue 0 -> red
            0, 255, 0, // hue 120 -> green
            0, 0, 255, // hue 240 -> blue
            128, 128, 128, // zero saturation -> grey
            255, 0, 0, // only R2's low byte participates
            255, 0, 255, // negative hue wraps to 300 degrees
            0,   // successful X form leaves V clear
        ],
        "hosted HSV edges and successful X-form flags are stable"
    );

    // SetGCOL stores the requested packed BBGGRR00 word on this caller's
    // graphics context. The existing indexed default mode reduces rendered
    // pixels; the graphics snapshot is the stable assertion for the packed
    // hosted state, while the subsequent public PLOT/ReadPoint proves the
    // shared service path without claiming exact indexed palette behavior.
    const PACKED_RGB: u32 = 0x1234_5600;
    basic_compat::run_source(
        "10 SYS \"ColourTrans_SetGCOL\",305419776,11,22,33,44 TO A%,B%,C%,D%,E%\n\
         20 !&60B0=A%:!&60B4=B%:!&60B8=C%:!&60BC=D%:!&60C0=E%:END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("SetGCOL accepts the hosted packed-RGB input");
    assert_named_owner(&dispatcher, "COLOURTRANS_SETGCOL", "COLOURTRANS_SETGCOL");
    assert_eq!(
        [0x60B0, 0x60B4, 0x60B8, 0x60BC, 0x60C0].map(|address| read_u32(&caller, address)),
        [PACKED_RGB, 11, 22, 33, 44],
        "SetGCOL changes graphics state without corrupting supplied registers"
    );
    basic_compat::run_source(
        "10 PLOT 69,100,100\n\
         20 SYS \"OS_ReadPoint\",100,100 TO X%,Y%,C%,T%,F%\n\
         30 !&6070=C%:!&6074=T%:!&6078=F%:END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("PLOT and ReadPoint continue to use the same caller graphics context");
    assert_eq!(read_u32(&caller, RESULT_ADDRESS + 0x70), 0);
    assert_eq!(read_u32(&caller, RESULT_ADDRESS + 0x74), 0);
    assert_eq!(read_u32(&caller, RESULT_ADDRESS + 0x78), 0);
    let snapshots = display
        .try_iter()
        .filter_map(|event| match event {
            DisplayEvent::GraphicsSnapshot { snapshot, .. } => Some(snapshot),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        snapshots
            .iter()
            .any(|snapshot| snapshot.graphics_colour == PACKED_RGB),
        "SetGCOL's packed colour should be visible in the snapshot consumed by Plot"
    );

    // The hosted WritePalette shim deliberately does not read guest pointers
    // or mutate any palette state. Invalid pointer-shaped inputs are safe and
    // every supplied register is preserved by this no-op.
    basic_compat::run_source(
        "10 SYS \"ColourTrans_WritePalette\",2146435072,2145386496,2144337920,2143289344,2142240768,2141192192,2140143616,2139095040,2138046464,2136997888 TO A%,B%,C%,D%,E%,F%,G%,H%,I%,J%\n\
         20 !&6080=A%:!&6084=B%:!&6088=C%:!&608C=D%:!&6090=E%\n\
         30 !&6094=F%:!&6098=G%:!&609C=H%:!&60A4=I%:!&60A8=J%:END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("hosted WritePalette is accepted without dereferencing its native pointer arguments");
    assert_named_owner(
        &dispatcher,
        "COLOURTRANS_WRITEPALETTE",
        "COLOURTRANS_WRITEPALETTE",
    );
    assert_eq!(
        [
            0x6080, 0x6084, 0x6088, 0x608C, 0x6090, 0x6094, 0x6098, 0x609C, 0x60A4, 0x60A8,
        ]
        .map(|address| read_u32(&caller, address)),
        [
            2146435072, 2145386496, 2144337920, 2143289344, 2142240768, 2141192192, 2140143616,
            2139095040, 2138046464, 2136997888,
        ],
        "WritePalette remains an accepted register-preserving hosted no-op across R0-R9"
    );
    basic_compat::run_source(
        "10 SYS \"OS_ReadPoint\",100,100 TO X%,Y%,C%,T%,F%\n\
         20 !&60D0=C%:!&60D4=T%:!&60D8=F%:END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("point read after the hosted palette shim remains available");
    assert_eq!(
        [0x6070, 0x6074, 0x6078].map(|address| read_u32(&caller, address)),
        [0x60D0, 0x60D4, 0x60D8].map(|address| read_u32(&caller, address)),
        "WritePalette does not change the palette/readback result"
    );

    // Quiescing and retiring the name-only owner must not expose the removed
    // Rust inline conversion or graphics mutation as a fallback.
    let mut manager_task = Task::trusted_mos_session(0x4602);
    dispatcher
        .basic64_module_manager()
        .quiesce("ColourTrans", &mut manager_task)
        .expect("trusted manager quiesces ColourTrans");
    let inactive = basic_compat::run_source(
        "10 SYS \"ColourTrans_ConvertHSVToRGB\",0,65280,255 TO R%,G%,B%\n\
         20 END",
        &mut caller,
        &mut dispatcher,
    )
    .expect_err("quiesced ColourTrans must fail closed");
    assert!(inactive.to_string().contains("owning module is not active"));
    basic_compat::run_source(
        "10 SYS \"XColourTrans_ConvertHSVToRGB\",0,65280,255 TO R%,G%,B% ; FLAGS%\n\
         20 !&60A0=FLAGS%:END",
        &mut caller,
        &mut dispatcher,
    )
    .expect("X-form quiesced failure is returned through the BASIC V/error-block convention");
    assert_ne!(read_u32(&caller, 0x60A0) & 1, 0);

    dispatcher
        .basic64_module_manager()
        .retire("ColourTrans", &mut manager_task)
        .expect("trusted manager retires the quiesced owner");
    let retired = basic_compat::run_source(
        "10 SYS \"ColourTrans_WritePalette\"\n\
         20 END",
        &mut caller,
        &mut dispatcher,
    )
    .expect_err("retired name-only service must not reach a legacy handler");
    assert_eq!(dispatcher.transitional_dispatch_count(), 0);
    assert!(
        !retired.to_string().contains("ColourTrans::"),
        "retired service must not dispatch a retained Rust or module body: {retired:?}"
    );

    // The existing experimental frame JIT calls the same named ColourTrans
    // services from its checked pixel callback. This tiny source matches the
    // recognized frame shape and renders only a 2x2 frame.
    #[cfg(feature = "experimental-jit")]
    {
        let (_key_sender, key_receiver) = mpsc::channel();
        let (display_sender, _display_receiver) = mpsc::channel();
        let mut jit_dispatcher =
            SwiDispatcher::windowed(HostConsole::windowed(key_receiver), display_sender);
        let mut jit_task = Task::new(0x4603);
        let source = "10 xsize%=2:ysize%=2:aspect=ysize%/xsize%\n\
            20 xcentre=-1:ycentre=0:scale=2\n\
            30 xmin=xcentre-(scale/2):xmax=xcentre+(scale/2)\n\
            40 xwidth=xmax-xmin:ymin=ycentre+(scale*aspect/2):ymax=ycentre-(scale*aspect/2)\n\
            50 ywidth=ymax-ymin:max%=8\n\
            60 FOR X%=0 TO (xsize%-1) STEP 1\n\
            70 FOR Y%=0 TO (ysize%-1) STEP 1\n\
            80 a=(xwidth*X%/xsize%)+xmin\n\
            90 b=(ywidth*Y%/ysize%)+ymin\n\
            100 PROCit(a,b,max%)\n\
            110 h%=360-360*LOG(IT%)/LOG(max%)\n\
            120 IF (ABS(e)+ABS(f))>4 PROCsethsv(h%,255,255) ELSE SYS\"ColourTrans_SetGCOL\",0,,,&100,0\n\
            130 MOVE X%*2,Y%*2:DRAW X%*2,Y%*2\n\
            140 NEXT Y%\n\
            150 NEXT X%\n\
            160 END\n\
            170 DEF PROC IT(A,B,ITER%)\n\
            180 IT%=0\n\
            190 e=0:f=0\n\
            200 REPEAT\n\
            210 u=(e*e)-(f*f)\n\
            220 v=2*e*f\n\
            230 e=u+a\n\
            240 f=v+b\n\
            250 IT%=IT%+1\n\
            260 UNTIL IT%=ITER% OR (ABS(e)+ABS(f))>4\n\
            270 ENDPROC\n\
            280 DEF PROC sethsv(H%,S%,V%)\n\
            290 SYS\"ColourTrans_ConvertHSVToRGB\",H%*&10000,S%*&100,V% TO R%,G%,B%\n\
            300 SYS\"ColourTrans_SetGCOL\",(B%<<24)+(G%<<16)+(R%<<8),,,&100,0\n\
            310 ENDPROC";
        let report = basic_compat::run_source_jit(&source, &mut jit_task, &mut jit_dispatcher)
            .expect("bounded Mandelbrot frame uses the existing ColourTrans callback route");
        assert!(
            report
                .compiled_units
                .iter()
                .any(|unit| unit.starts_with("Mandelbrot full frame loop")),
            "the test must actually enter the full-frame JIT path: {:?}",
            report.compiled_units
        );
    }
}
