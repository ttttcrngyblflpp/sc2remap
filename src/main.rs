use anyhow::Context as _;
use argh::FromArgs;
use async_io::Async;
use evdev_rs::enums::{EventCode, EventType, EV_KEY, EV_REL, EV_SYN};
use evdev_rs::{DeviceWrapper as _, InputEvent, UInputDevice};
use futures::{ready, select, StreamExt as _, TryStreamExt as _};
use log::{debug, info, trace, warn};
use std::fs::File;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

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
        _ => Some(key.clone()),
    }
}

struct Device(evdev_rs::Device);

impl AsRawFd for Device {
    fn as_raw_fd(&self) -> RawFd {
        self.0.file().as_raw_fd()
    }
}

struct AsyncDevice(Async<Device>);

impl futures::Stream for AsyncDevice {
    type Item = Result<InputEvent, anyhow::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // XXX This logic is hideous because libevdev's `next_event` function will read all
        // available events from the fd and buffer them internally, so when the fd becomes readable
        // it's necessary to continue from libevdev until the buffer is exhausted before the fd
        // will signal readable again.
        Poll::Ready(Some(if self.has_event_pending() {
            self.next_event(evdev_rs::ReadFlag::NORMAL)
                .map(|(_, event)| event)
                .context("has event pending")
        } else {
            match ready!(self.0.poll_readable(cx)) {
                Ok(()) => {
                    match self
                        .next_event(evdev_rs::ReadFlag::NORMAL)
                        .map(|(_, event)| event)
                    {
                        Ok(event) => Ok(event),
                        Err(e) => {
                            if e.kind() == std::io::ErrorKind::WouldBlock {
                                return self.poll_next(cx);
                            } else {
                                Err(e).context("next_event after poll")
                            }
                        }
                    }
                }
                Err(e) => Err(e).context("poll error"),
            }
        }))
    }
}

impl AsyncDevice {
    fn new<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        File::open(path)
            .and_then(|file| evdev_rs::Device::new_from_file(file))
            .and_then(|device| Async::new(Device(device)))
            .map(AsyncDevice)
    }

    fn grab(&mut self, grab: evdev_rs::GrabMode) -> std::io::Result<()> {
        self.0.get_mut().0.grab(grab)
    }

    fn next_event(
        &self,
        flags: evdev_rs::ReadFlag,
    ) -> std::io::Result<(evdev_rs::ReadStatus, InputEvent)> {
        self.0.get_ref().0.next_event(flags)
    }

    fn has_event_pending(&self) -> bool {
        self.0.get_ref().0.has_event_pending()
    }
}

struct State {
    current_key: Option<EV_KEY>,
    next_key: EV_KEY,
    held: bool,
}

fn inject_event(l: &UInputDevice, event_code: EventCode, value: i32) -> std::io::Result<()> {
    let event = InputEvent {
        event_code,
        value,
        time: evdev_rs::TimeVal {
            tv_sec: 0,
            tv_usec: 0,
        },
    };
    info!("injecting event: {:?} {:?}", event_code, value);
    l.write_event(&event)
}

fn inject_btn(l: &UInputDevice, btn: EV_KEY) -> std::io::Result<()> {
    let () = inject_event(&l, EventCode::EV_KEY(btn), 1)?;
    let () = inject_event(&l, EventCode::EV_SYN(EV_SYN::SYN_REPORT), 0)?;
    let () = inject_event(&l, EventCode::EV_KEY(btn), 0)?;
    let () = inject_event(&l, EventCode::EV_SYN(EV_SYN::SYN_REPORT), 0)?;
    Ok(())
}

fn log_event(event: &InputEvent) {
    match event.event_code {
        EventCode::EV_MSC(_) | EventCode::EV_SYN(_) | EventCode::EV_REL(_) => {
            trace!("event: {:?}", event)
        }
        _ => debug!("event: {:?}", event),
    }
}

fn main() {
    let Args { log_level } = argh::from_env();

    let () = simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Warn)
        .with_module_level(std::module_path!(), log_level)
        .init()
        .expect("failed to initialize logger");

    info!("scanning for mouse and keyboard devices");
    let (keeb_path, mouse_path) = async_io::block_on(async {
        let mut streams = futures::stream::select_all(
            glob::glob("/dev/input/event*")
                .expect("glob for /dev/input/event* failed")
                .into_iter()
                .filter_map(|r| match r {
                    Ok(path) => Some(path),
                    Err(e) => {
                        warn!("glob iterator failed: {:?}", e);
                        None
                    }
                })
                .map(|path| {
                    AsyncDevice::new(&path)
                        .expect("failed to initialize async device")
                        .map(move |event| event.map(|event| (path.clone(), event)))
                }),
        );
        let (mut keeb_path, mut mouse_path) = (None, None);
        loop {
            let (
                path,
                InputEvent {
                    time: _,
                    event_code,
                    value,
                },
            ) = streams
                .try_next()
                .await
                .expect("device stream empty")
                .expect("failed to read event");
            match event_code {
                EventCode::EV_KEY(EV_KEY::BTN_LEFT)
                | EventCode::EV_KEY(EV_KEY::BTN_RIGHT)
                | EventCode::EV_KEY(EV_KEY::BTN_MIDDLE)
                | EventCode::EV_KEY(EV_KEY::BTN_EXTRA)
                | EventCode::EV_KEY(EV_KEY::BTN_SIDE)
                | EventCode::EV_REL(_) => {
                    if mouse_path.is_none() {
                        info!("mouse device {:?}", path);
                        mouse_path = Some(path);
                    }
                }
                EventCode::EV_KEY(_) => {
                    if value == 0 && keeb_path.is_none() {
                        info!("keeb device {:?}", path);
                        keeb_path = Some(path);
                    }
                }
                _ => {}
            }
            if let (Some(keeb_path), Some(mouse_path)) = (&keeb_path, &mouse_path) {
                return (keeb_path.clone(), mouse_path.clone());
            }
        }
    });

    let uninit_device = evdev_rs::UninitDevice::new().expect("failed to create uninit device");
    macro_rules! enable_codes {
        ($etype:ident, $first:ident) => {
            let () = uninit_device
                .enable(&EventType::$etype)
                .expect("failed to enable events");
            for code in EventCode::$etype($etype::$first).iter().take_while(|e| {
                if let EventCode::$etype(_) = e {
                    true
                } else {
                    false
                }
            }) {
                debug!("adding code: {:?}", code);
                let () = uninit_device.enable(&code).unwrap();
            }
        };
    }
    enable_codes!(EV_KEY, KEY_RESERVED);
    enable_codes!(EV_REL, REL_X);
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
    let () = keeb_device
        .grab(evdev_rs::GrabMode::Grab)
        .expect("failed to grab keyboard device");

    async_io::block_on(async {
        let mut mouse_device = mouse_device.fuse();
        let mut keeb_device = keeb_device.fuse();
        let mut state = State {
            current_key: None,
            next_key: EV_KEY::KEY_GRAVE,
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
                            inject_btn(&l, EV_KEY::KEY_PAGEUP)
                                .expect("failed to inject pageup on scrollup");
                        }
                        // scroll down
                        EventCode::EV_REL(EV_REL::REL_WHEEL) if value == -1 => {
                            inject_btn(&l, EV_KEY::KEY_PAGEDOWN)
                                .expect("failed to inject pagedown on scrolldown");
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
        EventCode::EV_KEY(EV_KEY::KEY_GRAVE) if value == 0 => {
            if let Some(current_key) = state.current_key.take() {
                inject_event(&l, EventCode::EV_KEY(current_key), value)
                    .expect("failed to rewrite grave release event");
            } else {
                let () = l.write_event(&event).expect("failed to forward event");
            }
        }
        EventCode::EV_KEY(EV_KEY::KEY_GRAVE) => {
            state.current_key = Some(state.next_key);
            if value == 1 && state.held {
                inject_event(&l, EventCode::EV_KEY(state.next_key), 0)
                    .expect("failed to inject artificial release before grave press event");
            }
            inject_event(&l, EventCode::EV_KEY(state.next_key), value)
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
                        inject_event(&l, event_code, 0)
                            .expect("failed to inject artificial release before key press event");
                    }
                }
            }
            let () = l.write_event(&event).expect("failed to forward event");
        }
        _ => {
            let () = l.write_event(&event).expect("failed to forward event");
        }
    }
}
