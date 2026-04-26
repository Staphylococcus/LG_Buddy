use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::devices::GamepadDevice;
use super::DeviceId;

const LOGITECH_VENDOR_ID: u16 = 0x046d;
const LOGITECH_G923_PRODUCT_ID: u16 = 0xc267;
const HID_REPORT_BUFFER_SIZE: usize = 64;

#[derive(Debug)]
pub(crate) struct RawHidActivityReader {
    device_id: DeviceId,
    path: PathBuf,
    file: File,
}

impl RawHidActivityReader {
    pub(crate) fn open(device_id: DeviceId, path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)?;

        Ok(Self {
            device_id,
            path,
            file,
        })
    }

    pub(crate) fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn read_available(&mut self) -> io::Result<bool> {
        let mut saw_report = false;
        let mut buffer = [0_u8; HID_REPORT_BUFFER_SIZE];

        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => return Ok(saw_report),
                Ok(_) => saw_report = true,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(saw_report),
                Err(err) => return Err(err),
            }
        }
    }
}

pub(crate) fn raw_hid_activity_is_supported(device: &GamepadDevice) -> bool {
    device.vendor_id == LOGITECH_VENDOR_ID && device.product_id == LOGITECH_G923_PRODUCT_ID
}

#[cfg(test)]
mod tests {
    use super::raw_hid_activity_is_supported;
    use crate::session::gamepad::devices::GamepadDevice;
    use crate::session::gamepad::DeviceId;
    use std::path::PathBuf;

    fn device(vendor_id: u16, product_id: u16) -> GamepadDevice {
        GamepadDevice {
            id: DeviceId::new("event-controller"),
            path: PathBuf::from("/dev/input/event0"),
            vendor_id,
            product_id,
            hidraw_paths: Vec::new(),
        }
    }

    #[test]
    fn logitech_g923_enables_raw_hid_activity() {
        assert!(raw_hid_activity_is_supported(&device(0x046d, 0xc267)));
    }

    #[test]
    fn other_devices_do_not_enable_raw_hid_activity() {
        assert!(!raw_hid_activity_is_supported(&device(0x054c, 0x0df2)));
        assert!(!raw_hid_activity_is_supported(&device(0x046d, 0xc299)));
    }
}
