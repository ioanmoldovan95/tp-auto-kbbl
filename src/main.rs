extern crate dbus;
extern crate evdev_rs as evdev;
extern crate getopts;
extern crate nix;

#[macro_use]
extern crate log;

use dbus::blocking::Connection;
use evdev::*;
use getopts::Options;
use std::io;
use std::fs::File;
use std::process::exit;
use std::sync::mpsc;
use std::{env, thread, time};

mod upower_kbd_backlight;
use upower_kbd_backlight::OrgFreedesktopUPowerKbdBacklight;

const VERSION: &'static str = env!("CARGO_PKG_VERSION");

// Because the event loop *waits* for keyboard events, we use a thread
fn spawn_input_handle(device_file: String, tx: mpsc::Sender<bool>) {
    // Sleep on the key up event from launch
    thread::sleep(time::Duration::from_millis(100));
    let _ = thread::spawn(move || {
        // Open the device file (e.g. /dev/input/event1)
        let device_file = File::open(&device_file).unwrap_or_else(|e| panic!("{}", e));
        // Setup evdev
        let mut device = Device::new().unwrap();
        device.set_fd(device_file).unwrap();
        println!(
            "Input device ID: bus 0x{:x} vendor 0x{:x} product 0x{:x}",
            device.bustype(),
            device.vendor_id(),
            device.product_id()
        );
        println!("Evdev version: {:x}", device.driver_version());
        println!("Input device name: \"{}\"", device.name().unwrap_or(""));
        println!("Phys location: {}", device.phys().unwrap_or(""));
        // Events (key presses) will be stored here
        let mut event: io::Result<(ReadStatus, InputEvent)>;
        loop {
            // Blocks until a new event is received (waits for key press)
            event = device.next_event(ReadFlag::NORMAL | ReadFlag::BLOCKING);
            if event.is_err() {
                debug!("Device event error: {:?}", event.err());
                continue;
            }
            tx.send(true).unwrap();
        }
    });
}

fn main() {
    let config = parse_args();
    debug!("Config: {:?}", config);
    // Setup messaging channel and spawn input thread
    let (tx, rx) = mpsc::channel();

    let devices: [String; 3] = ["/dev/input/event3".to_string(), "/dev/input/event8".to_string(), "/dev/input/event9".to_string()];

    for i in 0..devices.len() {
        let tx1 = tx.clone();
        let device = devices[i].clone();
        spawn_input_handle(device, tx1);
    }

    // Setup dbus
    let conn = Connection::new_system().unwrap();
    let proxy = conn.with_proxy(
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower/KbdBacklight",
        time::Duration::from_millis(5000),
    );

    // Was there an key event?
    let mut key_event = false;
    // Desired brightness
    let mut brightness = 0;
    // Current brightness (internal state)
    let mut current_brightness = -1;
    // timestamp of the last keyboard event
    let mut last_event_ts = time::SystemTime::now();
    // tick counter for non-lazy hw state reading
    let mut ticks = 0;

    loop {
        // Wait 100ms in each loop to limit CPU usage
        thread::sleep(time::Duration::from_millis(100));
        // We only care for the LAST keyboard event, if there is any.
        for msg in rx.try_iter() {
            key_event = msg;
        }
        debug!(
            "e: {:?}, b: {:?}, ts: {:?}",
            key_event, brightness, last_event_ts
        );
        if key_event {
            brightness = 1;
            if current_brightness > 0 {
                brightness = config.brightness;
            } else {
                thread::sleep(time::Duration::from_millis(250));
            }
            last_event_ts = time::SystemTime::now();
            key_event = false;
        } else {
            // Elapsed seconds since the last keyboard event
            let es = last_event_ts.elapsed().unwrap().as_secs();
            if es >= config.timeout {
                // Larger than timeout: Lights off
                brightness  = 0
            } else if config.dim
                && config.brightness > 1
                && current_brightness > 1
                && es >= config.timeout / 2
            {
                // Larger than half of timeout: Dim lights
                brightness = 1
            }
        }
        // Check the actual hardware state, if not lazy
        if !config.lazy {
            // Do this only every second
            if ticks >= 10 {
                // The actual brightness might differ, e.g. after standby
                let actual_brightness = proxy.get_brightness().unwrap();
                if actual_brightness != current_brightness {
                    println!(
                        "Actual brightness differs: {} != {}",
                        actual_brightness, current_brightness
                    );
                    current_brightness = actual_brightness;
                }
                // Reset ticks
                ticks = 0;
            } else {
                // Increase ticks
                ticks += 1;
            }
        }
        // Set backlight brightness
        if brightness != current_brightness {
            println!("Setting brightness to {}", brightness);
            proxy.set_brightness(brightness).unwrap();
            current_brightness = brightness;
        }
    }
}

#[derive(Debug)]
struct Config {
    device_file: String,
    brightness: i32,
    timeout: u64,
    dim: bool,
    lazy: bool,
}

impl Config {
    fn new(device_file: String, brightness: i32, timeout: u64, dim: bool, lazy: bool) -> Self {
        Config {
            device_file: device_file,
            brightness: brightness,
            timeout: timeout,
            dim: dim,
            lazy: lazy,
        }
    }
}

// This function unpacks cli arguments and puts them into Config
fn parse_args() -> Config {
    fn print_usage(program: &str, opts: Options) {
        let brief = format!("Usage: {} [options]", program);
        println!("{}", opts.usage(&brief));
    }

    let args: Vec<_> = env::args().collect();

    let mut opts = Options::new();
    opts.optflag("h", "help", "prints this help message");
    opts.optflag("v", "version", "prints the version");
    opts.optflag("n", "no-dim", "don't dim before bg turns off");
    opts.optflag("l", "lazy", "don't check actual hw brightness state");
    opts.optopt("d", "device", "specify the device file", "DEVICE");
    opts.optopt(
        "b",
        "brightness",
        "target keyboard brightness (1-2)",
        "BRIGHTNESS",
    );
    opts.optopt(
        "t",
        "timeout",
        "time before the bg light turns off",
        "TIMEOUT",
    );

    let matches = opts.parse(&args[1..]).unwrap_or_else(|e| panic!("{}", e));
    if matches.opt_present("h") {
        print_usage(&args[0], opts);
        exit(0);
    }

    if matches.opt_present("v") {
        println!("{}", VERSION);
        exit(0);
    }

    let dim = !matches.opt_present("n");

    let lazy = matches.opt_present("l");

    let device_file = matches
        .opt_str("d")
        .unwrap_or("/dev/input/event3".to_string());

    let brightness: i32 = matches
        .opt_str("b")
        .map(|s| s.parse().unwrap())
        .unwrap_or(2);

    let timeout: u64 = matches
        .opt_str("t")
        .map(|s| s.parse().unwrap())
        .unwrap_or(15);

    Config::new(device_file, brightness, timeout, dim, lazy)
}
