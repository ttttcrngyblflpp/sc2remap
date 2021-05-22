use anyhow::Context as _;
use argh::FromArgs;
use evdev_rs::enums::{EventCode, EventType, EV_KEY, EV_REL, EV_SYN};
use evdev_rs::{DeviceWrapper as _, InputEvent};
use log::{debug, info, trace};
use std::collections::HashMap;
use std::fs::File;
use std::os::unix::io::{AsRawFd as _, RawFd};

#[derive(FromArgs)]
/// SC2 input remapping arguments.
struct Args {
    /// log level
    #[argh(option, short = 'l', default = "simplelog::LevelFilter::Info")]
    log_level: simplelog::LevelFilter,
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
        _ => Some(key.clone()),
    }
}

struct State {
    current_key: Option<EV_KEY>,
    next_key: EV_KEY,
    held: bool,
}

fn process_event<F: FnMut(InputEvent)>(device: &evdev_rs::Device, mut f: F) {
    loop {
        match device.next_event(evdev_rs::ReadFlag::NORMAL) {
            Err(e) => {
                if e.kind() == std::io::ErrorKind::WouldBlock {
                    break;
                } else {
                    panic!("failed to read event from keyboard device: {:?}", e);
                }
            }
            Ok((_read_status, event)) => {
                match event.event_code {
                    EventCode::EV_SYN(_) | EventCode::EV_MSC(_) => trace!("event: {:?}", event),
                    _ => debug!("event: {:?}", event),
                }
                f(event);
            }
        }
    }
}

fn init_device(
    device_id: usize,
    epoll_fd: RawFd,
) -> anyhow::Result<Option<(usize, evdev_rs::Device)>> {
    let path = format!("/dev/input/event{}", device_id);
    let file = match File::open(path.clone()) {
        Ok(file) => file,
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            } else {
                return Err(e).with_context(|| format!("failed to open {}", path));
            }
        }
    };
    let fd = file.as_raw_fd();

    let _ = nix::fcntl::fcntl(
        fd,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .context("failed to put device into non-blocking mode")?;
    let () = epoll::ctl(
        epoll_fd,
        epoll::ControlOptions::EPOLL_CTL_ADD,
        fd,
        epoll::Event::new(epoll::Events::EPOLLIN, device_id as u64),
    )
    .context("failed to add fd to epoll")?;

    Ok(Some((
        device_id,
        evdev_rs::Device::new_from_file(file).expect("failed to init keyboard device"),
    )))
}

fn inject_event(l: &evdev_rs::UInputDevice, event: InputEvent) -> std::io::Result<()> {
    info!("injecting event: {:?}", event);
    l.write_event(&event)
}

fn inject_btn(
    l: &evdev_rs::UInputDevice,
    time: evdev_rs::TimeVal,
    btn: EV_KEY,
) -> std::io::Result<()> {
    let () = inject_event(
        &l,
        InputEvent {
            time,
            event_code: EventCode::EV_KEY(btn),
            value: 1,
        },
    )?;
    let () = inject_event(
        &l,
        InputEvent {
            time,
            event_code: EventCode::EV_SYN(EV_SYN::SYN_REPORT),
            value: 0,
        },
    )?;
    let () = inject_event(
        &l,
        InputEvent {
            time,
            event_code: EventCode::EV_KEY(btn),
            value: 0,
        },
    )?;
    inject_event(
        &l,
        InputEvent {
            time,
            event_code: EventCode::EV_SYN(EV_SYN::SYN_REPORT),
            value: 0,
        },
    )
}

fn main() {
    let Args { log_level } = argh::from_env();

    let () = simplelog::SimpleLogger::init(log_level, simplelog::Config::default())
        .expect("failed to initialize logger");

    info!("scanning for mouse and keyboard devices");
    let epoll_fd = epoll::create(false).expect("failed to create epoll fd");
    let devices = (0..100)
        .into_iter()
        .map(|id| init_device(id, epoll_fd).unwrap())
        .filter_map(|opt| opt)
        .collect::<HashMap<_, _>>();
    let mut epoll_buf = epoll::Event::new(epoll::Events::empty(), 0);
    let (keeb_id, mouse_id) = (|| {
        let (mut keeb_id, mut mouse_id) = (None, None);
        loop {
            let _must_be_one: usize =
                epoll::wait(epoll_fd, -1, std::slice::from_mut(&mut epoll_buf))
                    .expect("failed to wait on epoll");
            let id = epoll_buf.data as usize;
            process_event(
                &devices.get(&id).expect("unknown fd returned by epoll"),
                |InputEvent {
                     time: _,
                     event_code,
                     value,
                 }| {
                    match event_code {
                        EventCode::EV_KEY(EV_KEY::BTN_LEFT)
                        | EventCode::EV_KEY(EV_KEY::BTN_RIGHT)
                        | EventCode::EV_KEY(EV_KEY::BTN_MIDDLE)
                        | EventCode::EV_KEY(EV_KEY::BTN_EXTRA)
                        | EventCode::EV_KEY(EV_KEY::BTN_SIDE)
                        | EventCode::EV_REL(_) => {
                            if mouse_id.is_none() {
                                mouse_id = Some(id);
                                info!("mouse id {}", id);
                            }
                        }
                        EventCode::EV_KEY(_) => {
                            if value == 0 && keeb_id.is_none() {
                                keeb_id = Some(id);
                                info!("keeb id {}", id);
                            }
                        }
                        _ => {}
                    }
                },
            );
            if let (Some(keeb_id), Some(mouse_id)) = (keeb_id, mouse_id) {
                return (keeb_id, mouse_id);
            }
        }
    })();

    let epoll_fd = epoll::create(false).expect("failed to create epoll fd");
    let (_, mut keeb_device) = init_device(keeb_id, epoll_fd).unwrap().unwrap();
    let (_, mouse_device) = init_device(mouse_id, epoll_fd).unwrap().unwrap();

    let uninit_device = evdev_rs::UninitDevice::new().expect("failed to create uninit device");
    let () = uninit_device
        .enable(&EventType::EV_KEY)
        .expect("failed to enable key events");
    let () = uninit_device
        .enable(&EventType::EV_REL)
        .expect("failed to enable rel events");
    // TODO this should be a macro.
    for code in EventCode::EV_KEY(EV_KEY::KEY_RESERVED)
        .iter()
        .take_while(|e| {
            if let EventCode::EV_KEY(_) = e {
                true
            } else {
                false
            }
        })
    {
        debug!("adding code: {:?}", code);
        let () = uninit_device.enable(&code).unwrap();
    }
    for code in EventCode::EV_REL(EV_REL::REL_X).iter().take_while(|e| {
        if let EventCode::EV_REL(_) = e {
            true
        } else {
            false
        }
    }) {
        debug!("adding code: {:?}", code);
        let () = uninit_device.enable(&code).unwrap();
    }
    uninit_device.set_name("sc2input");
    uninit_device.set_product_id(1);
    uninit_device.set_vendor_id(1);
    uninit_device.set_bustype(3);
    let l = evdev_rs::UInputDevice::create_from_device(&uninit_device)
        .expect("failed to create uinput device");

    let () = keeb_device
        .grab(evdev_rs::GrabMode::Grab)
        .expect("failed to grab keyboard device");

    let mut state = State {
        current_key: None,
        next_key: EV_KEY::KEY_GRAVE,
        held: false,
    };
    let mut epoll_buf = epoll::Event::new(epoll::Events::empty(), 0);
    loop {
        let _must_be_one: usize = epoll::wait(epoll_fd, -1, std::slice::from_mut(&mut epoll_buf))
            .expect("failed to wait on epoll");
        if epoll_buf.data as usize == keeb_id {
            process_event(&keeb_device, |event| {
                let InputEvent {
                    time: _,
                    event_code,
                    value,
                } = event;
                match event_code {
                    EventCode::EV_KEY(EV_KEY::KEY_GRAVE) if value == 0 => {
                        if let Some(current_key) = state.current_key.take() {
                            inject_event(
                                &l,
                                InputEvent {
                                    event_code: EventCode::EV_KEY(current_key),
                                    ..event.clone()
                                },
                            )
                            .expect("failed to rewrite grave release event");
                            return;
                        }
                    }
                    EventCode::EV_KEY(EV_KEY::KEY_GRAVE) => {
                        state.current_key = Some(state.next_key);
                        if value == 1 && state.held {
                            inject_event(
                                &l,
                                InputEvent {
                                    value: 0,
                                    event_code: EventCode::EV_KEY(state.next_key),
                                    ..event.clone()
                                },
                            )
                            .expect("failed to inject artificial release before grave press event");
                        }
                        inject_event(
                            &l,
                            InputEvent {
                                event_code: EventCode::EV_KEY(state.next_key),
                                ..event.clone()
                            },
                        )
                        .expect("failed to rewrite grave press/repeat event");
                        return;
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
                                    inject_event(&l, InputEvent {
                                        value: 0,
                                        ..event.clone()
                                    })
                                    .expect("failed to inject artificial release before key press event");
                                }
                            }
                        }
                    }
                    _ => {}
                }
                let () = l.write_event(&event).expect("failed to forward event");
            });
        } else if epoll_buf.data as usize == mouse_id {
            process_event(&mouse_device, |event| {
                let InputEvent {
                    time,
                    event_code,
                    value,
                } = event;
                match event_code {
                    // scroll up
                    EventCode::EV_REL(EV_REL::REL_WHEEL) if value == 1 => {
                        inject_btn(&l, time, EV_KEY::KEY_PAGEUP)
                            .expect("failed to inject pageup on scrollup");
                    }
                    // scroll down
                    EventCode::EV_REL(EV_REL::REL_WHEEL) if value == -1 => {
                        inject_btn(&l, time, EV_KEY::KEY_PAGEDOWN)
                            .expect("failed to inject pagedown on scrolldown");
                    }
                    _ => {}
                }
            });
        }
    }
}
