mod activity;
mod devices;
mod hidraw;
mod reader;
mod registry;

use std::fmt;
use std::path::Path;
use std::time::Instant;

pub(crate) use activity::ActivityPolicy;
use devices::discover_gamepad_devices;
use hidraw::{raw_hid_activity_is_supported, RawHidActivityReader};
use reader::GamepadDeviceReader;
use registry::ActivityRegistry;

const DEFAULT_INPUT_DIR: &str = "/dev/input";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DeviceId(String);

impl DeviceId {
    #[cfg(test)]
    pub(crate) fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn from_path(path: &Path) -> Self {
        Self(path.to_string_lossy().into_owned())
    }

    #[cfg(test)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AxisRange {
    pub minimum: i32,
    pub maximum: i32,
    pub flat: i32,
    pub fuzz: i32,
}

impl AxisRange {
    pub(crate) fn unknown() -> Self {
        Self {
            minimum: 0,
            maximum: 0,
            flat: 0,
            fuzz: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawGamepadEvent {
    pub device_id: DeviceId,
    pub kind: RawGamepadEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RawGamepadEventKind {
    Button {
        code: u16,
        pressed: bool,
    },
    Axis {
        code: u16,
        value: i32,
        range: AxisRange,
    },
}

#[derive(Debug)]
pub(crate) struct GamepadActivitySourceSetup {
    pub source: Option<SystemGamepadActivitySource>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct GamepadActivityPoll {
    pub activity: bool,
    #[cfg(test)]
    pub activity_devices: Vec<DeviceId>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct SystemGamepadActivitySource {
    readers: Vec<GamepadDeviceReader>,
    raw_hid_readers: Vec<RawHidActivityReader>,
    registry: ActivityRegistry,
}

impl SystemGamepadActivitySource {
    pub(crate) fn poll_once(&mut self, now: Instant) -> GamepadActivityPoll {
        let mut activity = false;
        #[cfg(test)]
        let mut activity_devices = Vec::new();
        let mut diagnostics = Vec::new();

        {
            let registry = &mut self.registry;
            self.readers.retain_mut(|reader| {
                let device_id = reader.device_id().clone();
                match reader.read_available() {
                    Ok(events) => {
                        for event in events {
                            #[cfg(test)]
                            let event_device_id = event.device_id.clone();
                            if registry.observe(event, now) {
                                activity = true;
                                #[cfg(test)]
                                if !activity_devices.contains(&event_device_id) {
                                    activity_devices.push(event_device_id);
                                }
                            }
                        }
                        true
                    }
                    Err(err) => {
                        registry.remove_device(&device_id);
                        diagnostics.push(format!(
                            "gamepad device `{device_id}` stopped producing activity events: {err}"
                        ));
                        false
                    }
                }
            });
        }

        self.raw_hid_readers.retain_mut(|reader| {
            let device_id = reader.device_id().clone();
            match reader.read_available() {
                Ok(report_seen) => {
                    if report_seen {
                        activity = true;
                        #[cfg(test)]
                        if !activity_devices.contains(&device_id) {
                            activity_devices.push(device_id.clone());
                        }
                    }
                    true
                }
                Err(err) => {
                    diagnostics.push(format!(
                        "gamepad raw HID device `{}` for `{device_id}` stopped producing activity events: {err}",
                        reader.path().display()
                    ));
                    false
                }
            }
        });

        GamepadActivityPoll {
            activity,
            #[cfg(test)]
            activity_devices,
            diagnostics,
        }
    }
}

pub(crate) fn open_system_gamepad_activity_source() -> GamepadActivitySourceSetup {
    open_gamepad_activity_source_from_dir(Path::new(DEFAULT_INPUT_DIR), ActivityPolicy::default())
}

fn open_gamepad_activity_source_from_dir(
    input_dir: &Path,
    policy: ActivityPolicy,
) -> GamepadActivitySourceSetup {
    let discovery = discover_gamepad_devices(input_dir);
    let mut diagnostics = Vec::new();

    let mut readers = Vec::new();
    let mut raw_hid_readers = Vec::new();
    let mut reader_failures = Vec::new();
    let mut raw_hid_reader_failures = Vec::new();
    for device in discovery.devices {
        if raw_hid_activity_is_supported(&device) {
            for path in &device.hidraw_paths {
                match RawHidActivityReader::open(device.id.clone(), path.clone()) {
                    Ok(reader) => raw_hid_readers.push(reader),
                    Err(err) => raw_hid_reader_failures.push(format!(
                        "{} for {}: {err}",
                        path.display(),
                        device.path.display()
                    )),
                }
            }
        }

        match GamepadDeviceReader::open(&device) {
            Ok(reader) => readers.push(reader),
            Err(err) => reader_failures.push(format!("{}: {err}", device.path.display())),
        }
    }

    let source_available = !readers.is_empty() || !raw_hid_readers.is_empty();
    if let Some(err) = discovery.input_dir_error {
        diagnostics.push(format!(
            "gamepad activity source unavailable: failed to read `{}`: {err}",
            input_dir.display()
        ));
    } else if !source_available && !reader_failures.is_empty() {
        diagnostics.push(format!(
            "gamepad activity source unavailable: failed to open {} detected gamepad device(s); first error: {}",
            reader_failures.len(),
            reader_failures[0]
        ));
    } else if !source_available && !discovery.inspect_failures.is_empty() {
        diagnostics.push(format!(
            "gamepad activity source unavailable: failed to inspect {} input device(s); first error: {}",
            discovery.inspect_failures.len(),
            discovery.inspect_failures[0]
        ));
    } else if readers.is_empty() && !reader_failures.is_empty() {
        diagnostics.push(format!(
            "gamepad evdev activity source unavailable: failed to open {} detected gamepad device(s); first error: {}; raw HID activity remains available",
            reader_failures.len(),
            reader_failures[0]
        ));
    } else if readers.is_empty() && !discovery.inspect_failures.is_empty() {
        diagnostics.push(format!(
            "gamepad evdev activity source unavailable: failed to inspect {} input device(s); first error: {}; raw HID activity remains available",
            discovery.inspect_failures.len(),
            discovery.inspect_failures[0]
        ));
    }
    if !raw_hid_reader_failures.is_empty() {
        let availability = if raw_hid_readers.is_empty() {
            "unavailable"
        } else {
            "partially unavailable"
        };
        diagnostics.push(format!(
            "gamepad raw HID activity source {availability} for {} detected device path(s); first error: {}",
            raw_hid_reader_failures.len(),
            raw_hid_reader_failures[0]
        ));
    }

    let mut registry = ActivityRegistry::new(policy);
    let baseline_seeded_at = Instant::now();
    for reader in &mut readers {
        for event in reader.take_initial_axis_events() {
            registry.observe(event, baseline_seeded_at);
        }
    }

    let source = if readers.is_empty() && raw_hid_readers.is_empty() {
        None
    } else {
        Some(SystemGamepadActivitySource {
            readers,
            raw_hid_readers,
            registry,
        })
    };

    GamepadActivitySourceSetup {
        source,
        diagnostics,
    }
}

pub(crate) fn is_controller_button_code(code: u16) -> bool {
    matches!(code, 0x120..=0x12f | 0x130..=0x13e | 0x2c0..=0x2ff)
}

pub(crate) fn is_controller_axis_code(code: u16) -> bool {
    matches!(code, 0x00..=0x0a | 0x10..=0x17)
}

#[cfg(test)]
mod tests {
    use super::{
        is_controller_axis_code, is_controller_button_code, open_system_gamepad_activity_source,
        DeviceId,
    };
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn controller_button_ranges_are_identified() {
        assert!(is_controller_button_code(0x120));
        assert!(is_controller_button_code(0x130));
        assert!(is_controller_button_code(0x2c0));
        assert!(!is_controller_button_code(0x110));
        assert!(!is_controller_button_code(30));
    }

    #[test]
    fn controller_axis_ranges_are_identified() {
        assert!(is_controller_axis_code(0x00));
        assert!(is_controller_axis_code(0x05));
        assert!(is_controller_axis_code(0x10));
        assert!(!is_controller_axis_code(0x18));
        assert!(!is_controller_axis_code(0x35));
    }

    #[test]
    fn device_id_is_rendered_as_its_inner_value() {
        let id = DeviceId::new("event0");

        assert_eq!(id.as_str(), "event0");
        assert_eq!(id.to_string(), "event0");
    }

    #[test]
    #[ignore = "requires local readable gamepad input devices and manual input"]
    fn hardware_smoke_reports_real_gamepad_activity() {
        let duration = std::env::var("LG_BUDDY_GAMEPAD_SMOKE_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(15));
        let setup = open_system_gamepad_activity_source();
        for diagnostic in setup.diagnostics {
            println!("{diagnostic}");
        }

        let Some(mut source) = setup.source else {
            panic!("expected at least one readable gamepad activity source");
        };

        println!(
            "Move or press each controller for {} seconds.",
            duration.as_secs()
        );

        let started = Instant::now();
        let mut observed_devices = Vec::new();
        while started.elapsed() < duration {
            let poll = source.poll_once(Instant::now());
            for diagnostic in poll.diagnostics {
                println!("{diagnostic}");
            }
            for device_id in poll.activity_devices {
                println!("activity: {device_id}");
                if !observed_devices.contains(&device_id) {
                    observed_devices.push(device_id);
                }
            }
            thread::sleep(Duration::from_millis(50));
        }

        assert!(
            !observed_devices.is_empty(),
            "expected activity from at least one real gamepad"
        );
    }
}
