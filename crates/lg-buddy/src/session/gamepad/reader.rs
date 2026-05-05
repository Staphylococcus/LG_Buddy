use std::collections::HashMap;
use std::io;

use evdev::{Device, EventSummary, InputEvent};

use super::devices::GamepadDevice;
use super::{
    is_controller_axis_code, is_controller_button_code, AxisRange, DeviceId, RawGamepadEvent,
    RawGamepadEventKind,
};

#[derive(Debug)]
pub(crate) struct GamepadDeviceReader {
    device_id: DeviceId,
    device: Device,
    axis_ranges: HashMap<u16, AxisRange>,
    initial_axis_events: Vec<RawGamepadEvent>,
}

impl GamepadDeviceReader {
    pub(crate) fn open(device: &GamepadDevice) -> io::Result<Self> {
        let evdev_device = Device::open(&device.path)?;
        let device_id = device.id.clone();
        let (axis_ranges, initial_axis_events) =
            axis_ranges_and_initial_events(&device_id, &evdev_device)?;
        evdev_device.set_nonblocking(true)?;

        Ok(Self {
            device_id,
            device: evdev_device,
            axis_ranges,
            initial_axis_events,
        })
    }

    pub(crate) fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    pub(crate) fn take_initial_axis_events(&mut self) -> Vec<RawGamepadEvent> {
        std::mem::take(&mut self.initial_axis_events)
    }

    pub(crate) fn read_available(&mut self) -> io::Result<Vec<RawGamepadEvent>> {
        match self.device.fetch_events() {
            Ok(events) => Ok(events
                .filter_map(|event| map_input_event(&self.device_id, &self.axis_ranges, event))
                .collect()),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => Ok(Vec::new()),
            Err(err) => Err(err),
        }
    }
}

fn axis_ranges_and_initial_events(
    device_id: &DeviceId,
    device: &Device,
) -> io::Result<(HashMap<u16, AxisRange>, Vec<RawGamepadEvent>)> {
    let mut axis_ranges = HashMap::new();
    let mut initial_axis_events = Vec::new();

    for (axis, info) in device
        .get_absinfo()?
        .filter(|(axis, _)| is_controller_axis_code(axis.0))
    {
        let range = AxisRange {
            minimum: info.minimum(),
            maximum: info.maximum(),
            flat: info.flat(),
            fuzz: info.fuzz(),
        };
        axis_ranges.insert(axis.0, range);
        initial_axis_events.push(RawGamepadEvent {
            device_id: device_id.clone(),
            kind: RawGamepadEventKind::Axis {
                code: axis.0,
                value: info.value(),
                range,
            },
        });
    }

    Ok((axis_ranges, initial_axis_events))
}

pub(crate) fn map_input_event(
    device_id: &DeviceId,
    axis_ranges: &HashMap<u16, AxisRange>,
    event: InputEvent,
) -> Option<RawGamepadEvent> {
    match event.destructure() {
        EventSummary::Key(_, key, value) if is_controller_button_code(key.0) => {
            Some(RawGamepadEvent {
                device_id: device_id.clone(),
                kind: RawGamepadEventKind::Button {
                    code: key.0,
                    pressed: value != 0,
                },
            })
        }
        EventSummary::AbsoluteAxis(_, axis, value) if is_controller_axis_code(axis.0) => {
            Some(RawGamepadEvent {
                device_id: device_id.clone(),
                kind: RawGamepadEventKind::Axis {
                    code: axis.0,
                    value,
                    range: axis_ranges
                        .get(&axis.0)
                        .copied()
                        .unwrap_or_else(AxisRange::unknown),
                },
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::map_input_event;
    use crate::session::gamepad::{AxisRange, DeviceId, RawGamepadEvent, RawGamepadEventKind};
    use evdev::{AbsoluteAxisCode, EventType, InputEvent, KeyCode};
    use std::collections::HashMap;

    #[test]
    fn button_press_maps_to_raw_gamepad_event() {
        let device_id = DeviceId::new("event-controller");
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_SOUTH.0, 1);

        assert_eq!(
            map_input_event(&device_id, &HashMap::new(), event),
            Some(RawGamepadEvent {
                device_id,
                kind: RawGamepadEventKind::Button {
                    code: KeyCode::BTN_SOUTH.0,
                    pressed: true,
                },
            })
        );
    }

    #[test]
    fn button_release_maps_to_raw_gamepad_event() {
        let device_id = DeviceId::new("event-controller");
        let event = InputEvent::new(EventType::KEY.0, KeyCode::BTN_SOUTH.0, 0);

        assert_eq!(
            map_input_event(&device_id, &HashMap::new(), event),
            Some(RawGamepadEvent {
                device_id,
                kind: RawGamepadEventKind::Button {
                    code: KeyCode::BTN_SOUTH.0,
                    pressed: false,
                },
            })
        );
    }

    #[test]
    fn axis_event_maps_to_raw_gamepad_event_with_range() {
        let device_id = DeviceId::new("event-controller");
        let range = AxisRange {
            minimum: -32768,
            maximum: 32767,
            flat: 128,
            fuzz: 16,
        };
        let axis_ranges = HashMap::from([(AbsoluteAxisCode::ABS_X.0, range)]);
        let event = InputEvent::new(EventType::ABSOLUTE.0, AbsoluteAxisCode::ABS_X.0, 1200);

        assert_eq!(
            map_input_event(&device_id, &axis_ranges, event),
            Some(RawGamepadEvent {
                device_id,
                kind: RawGamepadEventKind::Axis {
                    code: AbsoluteAxisCode::ABS_X.0,
                    value: 1200,
                    range,
                },
            })
        );
    }

    #[test]
    fn unsupported_events_are_ignored() {
        let device_id = DeviceId::new("event-controller");
        let keyboard_event = InputEvent::new(EventType::KEY.0, KeyCode::KEY_A.0, 1);
        let touch_event = InputEvent::new(
            EventType::ABSOLUTE.0,
            AbsoluteAxisCode::ABS_MT_POSITION_X.0,
            7,
        );

        assert_eq!(
            map_input_event(&device_id, &HashMap::new(), keyboard_event),
            None
        );
        assert_eq!(
            map_input_event(&device_id, &HashMap::new(), touch_event),
            None
        );
    }
}
