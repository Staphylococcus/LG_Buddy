use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use super::adapters::{
    ActivityObservation, ActivityReader, ActivityReaderKey, ActivityReaderSpec,
    ActivityReaderSurface,
};
use super::DeviceId;

const HID_REPORT_BUFFER_SIZE: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct RawHidActivityReaderSpec {
    key: ActivityReaderKey,
    device_id: DeviceId,
    path: PathBuf,
}

impl RawHidActivityReaderSpec {
    pub(crate) fn new(adapter: &'static str, device_id: DeviceId, path: PathBuf) -> Self {
        let key = ActivityReaderKey::Adapter {
            adapter,
            device_id: device_id.clone(),
            surface: ActivityReaderSurface::Hidraw(path.clone()),
        };

        Self {
            key,
            device_id,
            path,
        }
    }
}

impl ActivityReaderSpec for RawHidActivityReaderSpec {
    fn key(&self) -> ActivityReaderKey {
        self.key.clone()
    }

    fn open(&self) -> io::Result<Box<dyn ActivityReader>> {
        Ok(Box::new(RawHidActivityReader::open(
            self.key.clone(),
            self.device_id.clone(),
            self.path.clone(),
        )?))
    }
}

#[derive(Debug)]
pub(crate) struct RawHidActivityReader {
    key: ActivityReaderKey,
    device_id: DeviceId,
    file: File,
}

impl RawHidActivityReader {
    fn open(key: ActivityReaderKey, device_id: DeviceId, path: PathBuf) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)?;

        Ok(Self {
            key,
            device_id,
            file,
        })
    }

    fn read_report_available(&mut self) -> io::Result<bool> {
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

impl ActivityReader for RawHidActivityReader {
    fn key(&self) -> &ActivityReaderKey {
        &self.key
    }

    fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    fn read_available(&mut self) -> io::Result<Vec<ActivityObservation>> {
        if self.read_report_available()? {
            Ok(vec![ActivityObservation::ActivityPulse {
                device_id: self.device_id.clone(),
            }])
        } else {
            Ok(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RawHidActivityReaderSpec;
    use crate::session::gamepad::adapters::ActivityReaderSpec;
    use crate::session::gamepad::DeviceId;
    use std::path::PathBuf;

    #[test]
    fn raw_hid_reader_spec_uses_adapter_device_and_path_as_key() {
        let spec = RawHidActivityReaderSpec::new(
            "test-adapter",
            DeviceId::new("event-controller"),
            PathBuf::from("/dev/hidraw8"),
        );

        assert_eq!(
            spec.key().to_string(),
            "test-adapter hidraw /dev/hidraw8 for event-controller"
        );
    }
}
