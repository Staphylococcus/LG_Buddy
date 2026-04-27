use std::io;
use std::mem;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const UEVENT_GROUP_KERNEL: u32 = 1;
const UEVENT_BUFFER_SIZE: usize = 16 * 1024;

#[derive(Debug)]
pub(crate) struct SystemGamepadDeviceEventMonitor {
    socket: OwnedFd,
}

impl SystemGamepadDeviceEventMonitor {
    pub(crate) fn open() -> io::Result<Self> {
        let raw_fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::NETLINK_KOBJECT_UEVENT,
            )
        };
        if raw_fd < 0 {
            return Err(io::Error::last_os_error());
        }

        let socket = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        let mut address = unsafe { mem::zeroed::<libc::sockaddr_nl>() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address.nl_pid = 0;
        address.nl_groups = UEVENT_GROUP_KERNEL;
        let bind_result = unsafe {
            libc::bind(
                socket.as_raw_fd(),
                &address as *const libc::sockaddr_nl as *const libc::sockaddr,
                mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if bind_result < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self { socket })
    }

    pub(crate) fn has_relevant_event(&mut self) -> io::Result<bool> {
        has_relevant_gamepad_device_event(|buffer| {
            let bytes_read = unsafe {
                libc::recv(
                    self.socket.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                    0,
                )
            };

            if bytes_read >= 0 {
                Ok(usize::try_from(bytes_read).unwrap_or(buffer.len()))
            } else {
                Err(io::Error::last_os_error())
            }
        })
    }
}

fn has_relevant_gamepad_device_event(
    mut recv_next: impl FnMut(&mut [u8]) -> io::Result<usize>,
) -> io::Result<bool> {
    let mut saw_relevant_event = false;
    let mut buffer = [0_u8; UEVENT_BUFFER_SIZE];

    loop {
        match recv_next(&mut buffer) {
            Ok(0) => continue,
            Ok(bytes_read) => {
                if is_relevant_gamepad_device_event(&buffer[..bytes_read]) {
                    saw_relevant_event = true;
                }
            }
            Err(err) => match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(saw_relevant_event),
                _ => return Err(err),
            },
        }
    }
}

pub(crate) fn open_system_gamepad_device_event_monitor(
) -> io::Result<SystemGamepadDeviceEventMonitor> {
    SystemGamepadDeviceEventMonitor::open()
}

fn is_relevant_gamepad_device_event(message: &[u8]) -> bool {
    let fields = uevent_fields(message).collect::<Vec<_>>();
    let action = field_value(&fields, "ACTION");

    if !matches!(action, Some("add" | "remove" | "change")) {
        return false;
    }

    let subsystem = field_value(&fields, "SUBSYSTEM");
    match subsystem {
        Some("input") => event_path_is_input_event_device(&fields),
        Some("hidraw") => event_path_is_hidraw_device(&fields),
        _ => false,
    }
}

fn event_path_is_input_event_device(fields: &[&str]) -> bool {
    field_value(fields, "DEVNAME")
        .map(|devname| {
            devname.starts_with("/dev/input/event") || devname.starts_with("input/event")
        })
        .unwrap_or(false)
        || field_value(fields, "DEVPATH")
            .map(|devpath| devpath_has_path_component_with_prefix(devpath, "event"))
            .unwrap_or(false)
        || fields
            .first()
            .map(|header| devpath_has_path_component_with_prefix(header, "event"))
            .unwrap_or(false)
}

fn event_path_is_hidraw_device(fields: &[&str]) -> bool {
    field_value(fields, "DEVNAME")
        .map(|devname| devname.starts_with("/dev/hidraw") || devname.starts_with("hidraw"))
        .unwrap_or(false)
        || field_value(fields, "DEVPATH")
            .map(|devpath| devpath_has_path_component_with_prefix(devpath, "hidraw"))
            .unwrap_or(false)
        || fields
            .first()
            .map(|header| devpath_has_path_component_with_prefix(header, "hidraw"))
            .unwrap_or(false)
}

fn devpath_has_path_component_with_prefix(devpath: &str, prefix: &str) -> bool {
    devpath
        .split('/')
        .any(|component| component.starts_with(prefix))
}

fn field_value<'a>(fields: &'a [&'a str], name: &str) -> Option<&'a str> {
    let prefix = format!("{name}=");
    fields
        .iter()
        .find_map(|field| field.strip_prefix(prefix.as_str()))
}

fn uevent_fields(message: &[u8]) -> impl Iterator<Item = &str> {
    message.split(|byte| *byte == 0).filter_map(|field| {
        if field.is_empty() {
            return None;
        }

        std::str::from_utf8(field).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::{has_relevant_gamepad_device_event, is_relevant_gamepad_device_event};
    use std::io;

    fn message(fields: &[&str]) -> Vec<u8> {
        fields.join("\0").into_bytes()
    }

    #[test]
    fn input_event_device_add_is_relevant() {
        let message = message(&[
            "add@/devices/pci0000:00/input/input8/event21",
            "ACTION=add",
            "DEVPATH=/devices/pci0000:00/input/input8/event21",
            "SUBSYSTEM=input",
            "DEVNAME=/dev/input/event21",
        ]);

        assert!(is_relevant_gamepad_device_event(&message));
    }

    #[test]
    fn hidraw_device_remove_is_relevant() {
        let message = message(&[
            "remove@/devices/pci0000:00/0003:046D:C267.0009/hidraw/hidraw8",
            "ACTION=remove",
            "DEVPATH=/devices/pci0000:00/0003:046D:C267.0009/hidraw/hidraw8",
            "SUBSYSTEM=hidraw",
            "DEVNAME=/dev/hidraw8",
        ]);

        assert!(is_relevant_gamepad_device_event(&message));
    }

    #[test]
    fn input_parent_device_is_not_relevant() {
        let message = message(&[
            "change@/devices/pci0000:00/input/input8",
            "ACTION=change",
            "DEVPATH=/devices/pci0000:00/input/input8",
            "SUBSYSTEM=input",
        ]);

        assert!(!is_relevant_gamepad_device_event(&message));
    }

    #[test]
    fn unrelated_subsystem_is_not_relevant() {
        let message = message(&[
            "add@/devices/pci0000:00/block/sda",
            "ACTION=add",
            "DEVPATH=/devices/pci0000:00/block/sda",
            "SUBSYSTEM=block",
            "DEVNAME=/dev/sda",
        ]);

        assert!(!is_relevant_gamepad_device_event(&message));
    }

    #[test]
    fn bind_events_are_ignored() {
        let message = message(&[
            "bind@/devices/pci0000:00/input/input8/event21",
            "ACTION=bind",
            "DEVPATH=/devices/pci0000:00/input/input8/event21",
            "SUBSYSTEM=input",
            "DEVNAME=/dev/input/event21",
        ]);

        assert!(!is_relevant_gamepad_device_event(&message));
    }

    #[test]
    fn malformed_utf8_is_ignored() {
        assert!(!is_relevant_gamepad_device_event(&[0xff, 0xfe, 0]));
    }

    #[test]
    fn zero_length_datagram_does_not_stop_event_drain() {
        let relevant_message = message(&[
            "add@/devices/pci0000:00/input/input8/event21",
            "ACTION=add",
            "DEVPATH=/devices/pci0000:00/input/input8/event21",
            "SUBSYSTEM=input",
            "DEVNAME=/dev/input/event21",
        ]);
        let mut calls = 0;

        let result = has_relevant_gamepad_device_event(|buffer| {
            calls += 1;
            match calls {
                1 => Ok(0),
                2 => {
                    buffer[..relevant_message.len()].copy_from_slice(&relevant_message);
                    Ok(relevant_message.len())
                }
                _ => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            }
        });

        assert!(result.expect("event drain"));
        assert_eq!(calls, 3);
    }
}
