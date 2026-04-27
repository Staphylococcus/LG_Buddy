use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use evdev::Device;

use super::{is_controller_axis_code, is_controller_button_code, DeviceId};

const SYS_CLASS_INPUT_DIR: &str = "/sys/class/input";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GamepadDevice {
    pub id: DeviceId,
    pub path: PathBuf,
    pub vendor_id: u16,
    pub product_id: u16,
    pub hidraw_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceDiscovery {
    pub devices: Vec<GamepadDevice>,
    pub inspect_failures: Vec<DeviceInspectFailure>,
    pub input_dir_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceInspectFailure {
    path: PathBuf,
    error: String,
}

impl fmt::Display for DeviceInspectFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceCapabilities {
    pub keys: Vec<u16>,
    pub absolute_axes: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeviceInspection {
    capabilities: DeviceCapabilities,
    vendor_id: u16,
    product_id: u16,
}

pub(crate) fn discover_gamepad_devices(input_dir: &Path) -> DeviceDiscovery {
    discover_gamepad_devices_from_dirs(
        input_dir,
        Path::new(SYS_CLASS_INPUT_DIR),
        inspect_evdev_device,
    )
}

fn discover_gamepad_devices_from_dirs(
    input_dir: &Path,
    sys_class_input_dir: &Path,
    mut inspect_device: impl FnMut(&Path) -> io::Result<DeviceInspection>,
) -> DeviceDiscovery {
    let mut devices = Vec::new();
    let mut inspect_failures = Vec::new();

    let event_paths = match event_device_paths(input_dir) {
        Ok(paths) => paths,
        Err(err) => {
            return DeviceDiscovery {
                devices,
                inspect_failures,
                input_dir_error: Some(err.to_string()),
            }
        }
    };

    for path in event_paths {
        match inspect_device(&path) {
            Ok(inspection) => {
                if capabilities_are_gamepad_like(&inspection.capabilities) {
                    devices.push(GamepadDevice {
                        id: DeviceId::from_path(&path),
                        vendor_id: inspection.vendor_id,
                        product_id: inspection.product_id,
                        hidraw_paths: hidraw_paths_for_event_path(&path, sys_class_input_dir)
                            .unwrap_or_default(),
                        path,
                    });
                }
            }
            Err(err) => {
                inspect_failures.push(DeviceInspectFailure {
                    path,
                    error: inspect_error_message(&err),
                });
            }
        }
    }

    DeviceDiscovery {
        devices,
        inspect_failures,
        input_dir_error: None,
    }
}

fn inspect_evdev_device(path: &Path) -> io::Result<DeviceInspection> {
    let device = Device::open(path)?;
    let input_id = device.input_id();

    Ok(DeviceInspection {
        capabilities: capabilities_from_evdev(&device),
        vendor_id: input_id.vendor(),
        product_id: input_id.product(),
    })
}

pub(crate) fn capabilities_are_gamepad_like(capabilities: &DeviceCapabilities) -> bool {
    let controller_button_count = capabilities
        .keys
        .iter()
        .filter(|code| is_controller_button_code(**code))
        .count();
    let has_controller_axis = capabilities
        .absolute_axes
        .iter()
        .any(|code| is_controller_axis_code(*code));

    controller_button_count > 0 && (has_controller_axis || controller_button_count >= 2)
}

fn event_device_paths(input_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = fs::read_dir(input_dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("event"))
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn capabilities_from_evdev(device: &Device) -> DeviceCapabilities {
    DeviceCapabilities {
        keys: device
            .supported_keys()
            .map(|keys| keys.iter().map(|key| key.0).collect())
            .unwrap_or_default(),
        absolute_axes: device
            .supported_absolute_axes()
            .map(|axes| axes.iter().map(|axis| axis.0).collect())
            .unwrap_or_default(),
    }
}

fn hidraw_paths_for_event_path(
    event_path: &Path,
    sys_class_input_dir: &Path,
) -> io::Result<Vec<PathBuf>> {
    let event_name = event_path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "input event path `{}` has no file name",
                event_path.display()
            ),
        )
    })?;

    hidraw_paths_for_event_name(event_name, sys_class_input_dir)
}

fn hidraw_paths_for_event_name(
    event_name: &std::ffi::OsStr,
    sys_class_input_dir: &Path,
) -> io::Result<Vec<PathBuf>> {
    let input_device_dir = fs::canonicalize(sys_class_input_dir.join(event_name).join("device"))?;
    let Some(hid_device_dir) = input_device_dir
        .parent()
        .and_then(|input_dir| input_dir.parent())
    else {
        return Ok(Vec::new());
    };

    let hidraw_dir = hid_device_dir.join("hidraw");
    let mut paths = match fs::read_dir(&hidraw_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name();
                name.to_str()
                    .filter(|name| name.starts_with("hidraw"))
                    .map(|name| PathBuf::from("/dev").join(name))
            })
            .collect::<Vec<_>>(),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => return Err(err),
    };
    paths.sort();
    Ok(paths)
}

fn inspect_error_message(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        _ => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        capabilities_are_gamepad_like, discover_gamepad_devices_from_dirs,
        hidraw_paths_for_event_name, DeviceCapabilities, DeviceInspection,
    };
    use crate::session::gamepad::DeviceId;
    use std::ffi::OsStr;
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    fn temp_dir(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "lg-buddy-gamepad-devices-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn gamepad_inspection(vendor_id: u16, product_id: u16) -> DeviceInspection {
        DeviceInspection {
            capabilities: DeviceCapabilities {
                keys: vec![0x130],
                absolute_axes: vec![0x00],
            },
            vendor_id,
            product_id,
        }
    }

    fn keyboard_inspection() -> DeviceInspection {
        DeviceInspection {
            capabilities: DeviceCapabilities {
                keys: vec![30, 31, 32],
                absolute_axes: Vec::new(),
            },
            vendor_id: 0x0001,
            product_id: 0x0001,
        }
    }

    fn create_event_file(input_dir: &Path, name: &str) {
        fs::write(input_dir.join(name), []).expect("create input event file");
    }

    fn map_event_to_hidraw(root: &Path, event_name: &str, hidraw_names: &[&str]) {
        let sys_class_input = root.join("sys/class/input");
        let hid_device = root.join(format!("devices/usb/0003:046D:C267.{event_name}"));
        let input_device = hid_device.join("input/input56");
        let hidraw_dir = hid_device.join("hidraw");
        fs::create_dir_all(&sys_class_input).expect("create sys input dir");
        fs::create_dir_all(&input_device).expect("create input device dir");
        fs::create_dir_all(&hidraw_dir).expect("create hidraw dir");
        for hidraw_name in hidraw_names {
            fs::create_dir(hidraw_dir.join(hidraw_name)).expect("create hidraw entry");
        }

        let event_dir = sys_class_input.join(event_name);
        fs::create_dir(&event_dir).expect("create event sysfs dir");
        std::os::unix::fs::symlink(&input_device, event_dir.join("device"))
            .expect("symlink device");
    }

    #[test]
    fn gamepad_button_and_axis_capabilities_are_accepted() {
        let capabilities = DeviceCapabilities {
            keys: vec![0x130],
            absolute_axes: vec![0x00, 0x01],
        };

        assert!(capabilities_are_gamepad_like(&capabilities));
    }

    #[test]
    fn joystick_button_and_axis_capabilities_are_accepted() {
        let capabilities = DeviceCapabilities {
            keys: vec![0x120],
            absolute_axes: vec![0x06],
        };

        assert!(capabilities_are_gamepad_like(&capabilities));
    }

    #[test]
    fn digital_controller_buttons_without_axes_are_accepted() {
        let capabilities = DeviceCapabilities {
            keys: vec![0x130, 0x131],
            absolute_axes: vec![],
        };

        assert!(capabilities_are_gamepad_like(&capabilities));
    }

    #[test]
    fn keyboard_capabilities_are_rejected() {
        let capabilities = DeviceCapabilities {
            keys: vec![30, 31, 32],
            absolute_axes: vec![],
        };

        assert!(!capabilities_are_gamepad_like(&capabilities));
    }

    #[test]
    fn touchpad_capabilities_are_rejected() {
        let capabilities = DeviceCapabilities {
            keys: vec![0x14a],
            absolute_axes: vec![0x00, 0x01, 0x35, 0x36],
        };

        assert!(!capabilities_are_gamepad_like(&capabilities));
    }

    #[test]
    fn mouse_button_capabilities_are_rejected() {
        let capabilities = DeviceCapabilities {
            keys: vec![0x110, 0x111],
            absolute_axes: vec![],
        };

        assert!(!capabilities_are_gamepad_like(&capabilities));
    }

    #[test]
    fn hidraw_paths_are_mapped_from_event_sysfs_device() {
        let root = temp_dir("hidraw-map");
        let sys_class_input = root.join("sys/class/input");
        let hid_device = root.join("devices/usb/0003:046D:C267.0009");
        let input_device = hid_device.join("input/input56");
        let hidraw_dir = hid_device.join("hidraw");
        fs::create_dir_all(&sys_class_input).expect("create sys input dir");
        fs::create_dir_all(&input_device).expect("create input device dir");
        fs::create_dir_all(&hidraw_dir).expect("create hidraw dir");
        fs::create_dir(hidraw_dir.join("hidraw8")).expect("create hidraw entry");

        let event_dir = sys_class_input.join("event25");
        fs::create_dir(&event_dir).expect("create event dir");
        std::os::unix::fs::symlink(&input_device, event_dir.join("device"))
            .expect("symlink device");

        assert_eq!(
            hidraw_paths_for_event_name(OsStr::new("event25"), Path::new(&sys_class_input))
                .expect("hidraw paths"),
            vec![PathBuf::from("/dev/hidraw8")]
        );

        fs::remove_dir_all(root).expect("remove temp dir");
    }

    #[test]
    fn discovery_filters_event_devices_and_reports_gamepad_metadata() {
        let root = temp_dir("discover-success");
        let input_dir = root.join("dev/input");
        let sys_class_input = root.join("sys/class/input");
        fs::create_dir_all(&input_dir).expect("create input dir");
        create_event_file(&input_dir, "event2");
        create_event_file(&input_dir, "event10");
        create_event_file(&input_dir, "mouse0");
        map_event_to_hidraw(&root, "event2", &["hidraw8", "hidraw2"]);

        let mut inspected_paths = Vec::new();
        let discovery = discover_gamepad_devices_from_dirs(&input_dir, &sys_class_input, |path| {
            inspected_paths.push(path.file_name().expect("file name").to_owned());
            match path.file_name().and_then(|name| name.to_str()) {
                Some("event2") => Ok(gamepad_inspection(0x054c, 0x0df2)),
                Some("event10") => Ok(keyboard_inspection()),
                other => panic!("unexpected inspected path: {other:?}"),
            }
        });

        assert_eq!(discovery.input_dir_error, None);
        assert!(discovery.inspect_failures.is_empty());
        assert_eq!(
            inspected_paths,
            vec![
                OsStr::new("event10").to_owned(),
                OsStr::new("event2").to_owned()
            ]
        );
        assert_eq!(discovery.devices.len(), 1);
        let device = &discovery.devices[0];
        assert_eq!(
            device.id,
            DeviceId::new(input_dir.join("event2").display().to_string())
        );
        assert_eq!(device.path, input_dir.join("event2"));
        assert_eq!(device.vendor_id, 0x054c);
        assert_eq!(device.product_id, 0x0df2);
        assert_eq!(
            device.hidraw_paths,
            vec![PathBuf::from("/dev/hidraw2"), PathBuf::from("/dev/hidraw8")]
        );

        fs::remove_dir_all(root).expect("remove temp dir");
    }

    #[test]
    fn discovery_records_inspection_failures_without_stopping() {
        let root = temp_dir("discover-failure");
        let input_dir = root.join("dev/input");
        let sys_class_input = root.join("sys/class/input");
        fs::create_dir_all(&input_dir).expect("create input dir");
        create_event_file(&input_dir, "event0");
        create_event_file(&input_dir, "event1");

        let discovery = discover_gamepad_devices_from_dirs(&input_dir, &sys_class_input, |path| {
            match path.file_name().and_then(|name| name.to_str()) {
                Some("event0") => Err(io::Error::new(io::ErrorKind::PermissionDenied, "nope")),
                Some("event1") => Ok(gamepad_inspection(0x045e, 0x0b13)),
                other => panic!("unexpected inspected path: {other:?}"),
            }
        });

        assert_eq!(discovery.input_dir_error, None);
        assert_eq!(discovery.devices.len(), 1);
        assert_eq!(discovery.devices[0].path, input_dir.join("event1"));
        assert_eq!(discovery.inspect_failures.len(), 1);
        assert_eq!(discovery.inspect_failures[0].path, input_dir.join("event0"));
        assert_eq!(discovery.inspect_failures[0].error, "permission denied");

        fs::remove_dir_all(root).expect("remove temp dir");
    }

    #[test]
    fn discovery_reports_input_directory_errors() {
        let root = temp_dir("discover-input-error");
        let input_dir = root.join("missing-input");
        let sys_class_input = root.join("sys/class/input");

        let discovery = discover_gamepad_devices_from_dirs(&input_dir, &sys_class_input, |_| {
            panic!("input directory errors should stop before inspection")
        });

        assert!(discovery.input_dir_error.is_some());
        assert!(discovery.devices.is_empty());
        assert!(discovery.inspect_failures.is_empty());

        fs::remove_dir_all(root).expect("remove temp dir");
    }
}
