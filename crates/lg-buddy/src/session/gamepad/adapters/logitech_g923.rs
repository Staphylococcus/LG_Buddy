use super::{ActivityReaderSpec, GamepadActivityAdapter};
use crate::session::gamepad::devices::GamepadDevice;
use crate::session::gamepad::hidraw::RawHidActivityReaderSpec;

const LOGITECH_VENDOR_ID: u16 = 0x046d;
const LOGITECH_G923_PRODUCT_ID: u16 = 0xc267;

#[derive(Debug)]
pub(super) struct LogitechG923Adapter;

pub(super) static ADAPTER: LogitechG923Adapter = LogitechG923Adapter;

impl GamepadActivityAdapter for LogitechG923Adapter {
    fn name(&self) -> &'static str {
        "logitech-g923"
    }

    fn supports(&self, device: &GamepadDevice) -> bool {
        device.vendor_id == LOGITECH_VENDOR_ID && device.product_id == LOGITECH_G923_PRODUCT_ID
    }

    fn reader_specs(&self, device: &GamepadDevice) -> Vec<Box<dyn ActivityReaderSpec>> {
        device
            .hidraw_paths
            .iter()
            .map(|path| {
                Box::new(RawHidActivityReaderSpec::new(
                    self.name(),
                    device.id.clone(),
                    path.clone(),
                )) as Box<dyn ActivityReaderSpec>
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{GamepadActivityAdapter, LogitechG923Adapter};
    use crate::session::gamepad::devices::GamepadDevice;
    use crate::session::gamepad::DeviceId;
    use std::path::PathBuf;

    fn device(vendor_id: u16, product_id: u16, hidraw_paths: Vec<PathBuf>) -> GamepadDevice {
        GamepadDevice {
            id: DeviceId::new("event-controller"),
            path: PathBuf::from("/dev/input/event0"),
            vendor_id,
            product_id,
            hidraw_paths,
        }
    }

    #[test]
    fn supports_only_logitech_g923() {
        let adapter = LogitechG923Adapter;

        assert!(adapter.supports(&device(0x046d, 0xc267, Vec::new())));
        assert!(!adapter.supports(&device(0x054c, 0x0df2, Vec::new())));
        assert!(!adapter.supports(&device(0x046d, 0xc299, Vec::new())));
    }

    #[test]
    fn reader_specs_follow_hidraw_paths() {
        let adapter = LogitechG923Adapter;
        let specs = adapter.reader_specs(&device(
            0x046d,
            0xc267,
            vec![PathBuf::from("/dev/hidraw2"), PathBuf::from("/dev/hidraw8")],
        ));

        assert_eq!(specs.len(), 2);
        assert_eq!(
            specs[0].key().to_string(),
            "logitech-g923 hidraw /dev/hidraw2 for event-controller"
        );
        assert_eq!(
            specs[1].key().to_string(),
            "logitech-g923 hidraw /dev/hidraw8 for event-controller"
        );
    }
}
