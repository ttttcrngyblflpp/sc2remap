#![deny(unused_results)]

use argh::FromArgs;
use evdev_rs::enums::{EventCode, EV_KEY, EV_REL};
use evdev_rs::{DeviceWrapper as _, InputEvent, UInputDevice};
use evdev_utils::AsyncDevice;
use evdev_utils::{DeviceWrapperExt as _, UInputExt as _};
use futures::{select, StreamExt as _, TryStreamExt as _};
use log::{debug, info, trace};

#[derive(FromArgs)]
/// SC2 input remapping arguments.
struct Args {
    /// log level
    #[argh(option, short = 'l', default = "log::LevelFilter::Info")]
    log_level: log::LevelFilter,
}

fn keymap(key: &EV_KEY) -> Option<EV_KEY> {
    match key {
        EV_KEY::KEY_LEFTCTRL
        | EV_KEY::KEY_RIGHTCTRL
        | EV_KEY::KEY_LEFTSHIFT
        | EV_KEY::KEY_RIGHTSHIFT
        | EV_KEY::KEY_LEFTALT
        | EV_KEY::KEY_RIGHTALT
        | EV_KEY::KEY_LEFTMETA
        | EV_KEY::KEY_RIGHTMETA => None,
        EV_KEY::KEY_1 => Some(EV_KEY::KEY_6),
        EV_KEY::KEY_2 => Some(EV_KEY::KEY_7),
        EV_KEY::KEY_3 => Some(EV_KEY::KEY_8),
        _ => Some(*key),
    }
}

struct State {
    current_key: Option<EV_KEY>,
    next_key: EV_KEY,
    held: bool,
}

fn log_event(event: &InputEvent) {
    match event.event_code {
        EventCode::EV_MSC(_) | EventCode::EV_SYN(_) | EventCode::EV_REL(_) => {
            trace!("event: {:?}", event)
        }
        _ => debug!("event: {:?}", event),
    }
}

const REPEAT_KEY: EV_KEY = EV_KEY::KEY_F24;

fn main() {
    let Args { log_level } = argh::from_env();

    simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Warn)
        .with_module_level(std::module_path!(), log_level)
        .init()
        .expect("failed to initialize logger");

    let (keeb_path, mouse_path) = futures::executor::block_on(evdev_utils::identify_mkb())
        .expect("failed to identify keyboard and mouse");
    info!("found mouse {:?} and keyboard {:?}", mouse_path, keeb_path);

    let uninit_device = evdev_rs::UninitDevice::new().expect("failed to create uninit device");
    uninit_device
        .enable_keys()
        .expect("failed to enable keyboard functionality");
    uninit_device
        .enable_mouse()
        .expect("failed to enable mouse functionality");
    uninit_device.set_name("sc2input");
    uninit_device.set_product_id(1);
    uninit_device.set_vendor_id(1);
    uninit_device.set_bustype(3);
    let l =
        UInputDevice::create_from_device(&uninit_device).expect("failed to create uinput device");

    let (mut keeb_device, mouse_device) = (
        AsyncDevice::new(keeb_path).expect("failed to create keyboard device"),
        AsyncDevice::new(mouse_path).expect("failed to create mouse device"),
    );
    keeb_device
        .grab(evdev_rs::GrabMode::Grab)
        .expect("failed to grab keyboard device");

    futures::executor::block_on(async {
        let mut mouse_device = mouse_device.fuse();
        let mut keeb_device = keeb_device.fuse();
        let mut state = State {
            current_key: None,
            next_key: REPEAT_KEY,
            held: false,
        };
        loop {
            select! {
                keeb_event = keeb_device.try_next() => {
                    let keeb_event = keeb_event.expect("failed to read keyboard event").expect("keyboard stream ended");
                    log_event(&keeb_event);
                    handle_keeb_event(keeb_event, &l, &mut state);
                }
                mouse_event = mouse_device.try_next() => {
                    let mouse_event = mouse_event.expect("failed to read mouse event").expect("mouse stream ended");
                    log_event(&mouse_event);
                    let InputEvent {
                        time: _,
                        event_code,
                        value,
                    } = mouse_event;
                    match event_code {
                        // scroll up
                        EventCode::EV_REL(EV_REL::REL_WHEEL) if value == 1 => {
                            l.inject_key_press(EV_KEY::KEY_PAGEUP)
                                .expect("failed to inject pageup on scrollup");
                        }
                        // scroll down
                        EventCode::EV_REL(EV_REL::REL_WHEEL) if value == -1 => {
                            l.inject_key_press(EV_KEY::KEY_PAGEDOWN)
                                .expect("failed to inject pagedown on scrolldown");
                        }
                        // forward side btn
                        EventCode::EV_KEY(EV_KEY::BTN_EXTRA) => {
                            l.inject_key_syn(EV_KEY::KEY_HOME, value)
                                .expect("failed to inject home on forward side button");
                        }
                        // back side btn
                        EventCode::EV_KEY(EV_KEY::BTN_SIDE) => {
                            l.inject_key_syn(EV_KEY::KEY_END, value)
                                .expect("failed to inject end on back side button");
                        }
                        _ => {}
                    }
                }
            }
        }
    });
}

fn handle_keeb_event(event: InputEvent, l: &UInputDevice, state: &mut State) {
    let InputEvent {
        time: _,
        event_code,
        value,
    } = event;
    match event_code {
        EventCode::EV_KEY(REPEAT_KEY) if value == 0 => {
            if let Some(current_key) = state.current_key.take() {
                l.inject_event(EventCode::EV_KEY(current_key), value)
                    .expect("failed to rewrite grave release event");
            } else {
                l.write_event(&event).expect("failed to forward event");
            }
        }
        EventCode::EV_KEY(REPEAT_KEY) => {
            state.current_key = Some(state.next_key);
            if value == 1 && state.held {
                l.inject_event(EventCode::EV_KEY(state.next_key), 0)
                    .expect("failed to inject artificial release before grave press event");
            }
            l.inject_event(EventCode::EV_KEY(state.next_key), value)
                .expect("failed to rewrite grave press/repeat event");
        }
        EventCode::EV_KEY(k) => {
            if let Some(mapped) = keymap(&k) {
                if value == 0 {
                    if mapped == state.next_key {
                        state.held = false;
                    }
                } else {
                    state.next_key = mapped;
                    state.held = true;
                    if state.current_key == Some(mapped) {
                        l.inject_event(event_code, 0)
                            .expect("failed to inject artificial release before key press event");
                    }
                }
            }
            l.write_event(&event).expect("failed to forward event");
        }
        _ => {
            l.write_event(&event).expect("failed to forward event");
        }
    }
}
