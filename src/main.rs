#![deny(unused_results)]

use argh::FromArgs;
use evdev_rs::enums::{EventCode, EV_KEY, EV_REL};
use evdev_rs::{DeviceWrapper as _, InputEvent, UInputDevice};
use evdev_utils::AsyncDevice;
use evdev_utils::{DeviceWrapperExt as _, UInputExt as _};
use futures::TryStreamExt as _;
use log::{debug, info, trace};

#[derive(FromArgs)]
/// SC2 input remapping arguments.
struct Args {
    /// log level
    #[argh(option, short = 'l', default = "log::LevelFilter::Info")]
    log_level: log::LevelFilter,
}

fn log_event(event: &InputEvent) {
    match event.event_code {
        EventCode::EV_MSC(_) | EventCode::EV_SYN(_) | EventCode::EV_REL(EV_REL::REL_X) | EventCode::EV_REL(EV_REL::REL_Y) => {
            trace!("event: {:?}", event)
        }
        _ => debug!("event: {:?}", event),
    }
}

fn main() {
    let Args { log_level } = argh::from_env();

    simple_logger::SimpleLogger::new()
        .with_utc_timestamps()
        .with_level(log::LevelFilter::Warn)
        .with_module_level(std::module_path!(), log_level)
        .init()
        .expect("failed to initialize logger");

    let mut pidlock = pidlock::Pidlock::new(&format!("/var/run/user/{}/sc2remap.pid", unsafe {
        libc::geteuid()
    }));
    pidlock.acquire().unwrap();

    loop {
        let mouse_path = loop {
            log::info!("waiting");
            match futures::executor::block_on(evdev_utils::identify_mouse()) {
                Ok(mouse_path) => break mouse_path,
                Err(e) => log::warn!("failed to identify mouse: {}", e),
            }
        };
        info!("found mouse {:?}", mouse_path);

        let uninit_device = evdev_rs::UninitDevice::new().expect("failed to create uninit device");
        uninit_device
            .enable_keys()
            .expect("failed to enable keyboard functionality");
        uninit_device.set_name("sc2input");
        uninit_device.set_product_id(1);
        uninit_device.set_vendor_id(1);
        uninit_device.set_bustype(3);
        let l =
            UInputDevice::create_from_device(&uninit_device).expect("failed to create uinput device");

        let mouse_device = AsyncDevice::new(mouse_path).expect("failed to create mouse device");

        let mut drag_scroll_held = false;
        let r = futures::executor::block_on(mouse_device.try_for_each(|mouse_event| {
            log_event(&mouse_event);
            let InputEvent {
                time: _,
                event_code,
                value,
            } = mouse_event;
            match event_code {
                // middle click
                EventCode::EV_KEY(EV_KEY::BTN_MIDDLE) => {
                    drag_scroll_held = value == 1;
                }
                // scroll up
                EventCode::EV_REL(EV_REL::REL_WHEEL) if value == 1 => {
                    if !drag_scroll_held {
                        debug!("injecting UP");
                        l.inject_key_press(EV_KEY::KEY_UP)
                            .expect("failed to inject up on scrollup");
                    }
                }
                // scroll down
                EventCode::EV_REL(EV_REL::REL_WHEEL) if value == -1 => {
                    if !drag_scroll_held {
                        debug!("injecting DOWN");
                        l.inject_key_press(EV_KEY::KEY_DOWN)
                            .expect("failed to inject down on scrolldown");
                    }
                }
                EventCode::EV_KEY(EV_KEY::BTN_SIDE) if value == 1 => {
                    log::info!("status: {:?}", std::process::Command::new("/home/tone/.local/bin/side_btn.sh").status());
                }
                _ => {}
            }
            futures::future::ok(())
        }));
        log::warn!("mouse event loop ended with: {:?}", r);
    }
}
