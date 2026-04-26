use std::collections::HashMap;
use std::time::Instant;

use super::activity::{
    evaluate_axis_event, evaluate_button_event, ActivityPolicy, AxisActivityState,
};
use super::{DeviceId, RawGamepadEvent, RawGamepadEventKind};

#[derive(Debug)]
pub(crate) struct ActivityRegistry {
    policy: ActivityPolicy,
    devices: HashMap<DeviceId, DeviceActivityState>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct DeviceActivityState {
    axes: HashMap<u16, AxisActivityState>,
}

impl ActivityRegistry {
    pub(crate) fn new(policy: ActivityPolicy) -> Self {
        Self {
            policy,
            devices: HashMap::new(),
        }
    }

    pub(crate) fn observe(&mut self, event: RawGamepadEvent, now: Instant) -> bool {
        match event.kind {
            RawGamepadEventKind::Button { .. } => {
                self.devices.entry(event.device_id).or_default();
                evaluate_button_event().is_activity()
            }
            RawGamepadEventKind::Axis { code, value, range } => {
                let device = self.devices.entry(event.device_id).or_default();
                let evaluation =
                    evaluate_axis_event(&self.policy, device.axes.get(&code), value, range, now);
                device.axes.insert(code, evaluation.next_state);
                evaluation.decision.is_activity()
            }
        }
    }

    pub(crate) fn remove_device(&mut self, device_id: &DeviceId) {
        self.devices.remove(device_id);
    }
}

#[cfg(test)]
impl ActivityRegistry {
    fn has_device(&self, device_id: &DeviceId) -> bool {
        self.devices.contains_key(device_id)
    }

    fn axis_state(&self, device_id: &DeviceId, axis: u16) -> Option<AxisActivityState> {
        self.devices
            .get(device_id)
            .and_then(|device| device.axes.get(&axis).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::ActivityRegistry;
    use crate::session::gamepad::activity::ActivityPolicy;
    use crate::session::gamepad::{AxisRange, DeviceId, RawGamepadEvent, RawGamepadEventKind};
    use std::time::{Duration, Instant};

    fn test_policy() -> ActivityPolicy {
        ActivityPolicy {
            axis_movement_threshold_ppm: 100_000,
            minimum_axis_movement: 2,
            quiet_baseline_after: Duration::from_secs(2),
            activity_cooldown: Duration::from_millis(500),
        }
    }

    fn test_range() -> AxisRange {
        AxisRange {
            minimum: 0,
            maximum: 100,
            flat: 0,
            fuzz: 0,
        }
    }

    fn axis_event(device_id: DeviceId, code: u16, value: i32) -> RawGamepadEvent {
        RawGamepadEvent {
            device_id,
            kind: RawGamepadEventKind::Axis {
                code,
                value,
                range: test_range(),
            },
        }
    }

    fn button_event(device_id: DeviceId) -> RawGamepadEvent {
        RawGamepadEvent {
            device_id,
            kind: RawGamepadEventKind::Button {
                code: 0x130,
                pressed: true,
            },
        }
    }

    #[test]
    fn registry_creates_device_state_on_first_button_event() {
        let mut registry = ActivityRegistry::new(test_policy());
        let device_id = DeviceId::new("controller");

        assert!(registry.observe(button_event(device_id.clone()), Instant::now()));

        assert!(registry.has_device(&device_id));
    }

    #[test]
    fn registry_stores_separate_axis_state_per_device() {
        let mut registry = ActivityRegistry::new(test_policy());
        let now = Instant::now();
        let first = DeviceId::new("controller-a");
        let second = DeviceId::new("controller-b");

        assert!(!registry.observe(axis_event(first.clone(), 0, 10), now));
        assert!(!registry.observe(axis_event(second.clone(), 0, 80), now));

        assert_eq!(
            registry.axis_state(&first, 0).expect("first axis").baseline,
            10
        );
        assert_eq!(
            registry
                .axis_state(&second, 0)
                .expect("second axis")
                .baseline,
            80
        );
    }

    #[test]
    fn registry_stores_separate_axis_state_per_axis() {
        let mut registry = ActivityRegistry::new(test_policy());
        let now = Instant::now();
        let device_id = DeviceId::new("controller");

        assert!(!registry.observe(axis_event(device_id.clone(), 0, 10), now));
        assert!(!registry.observe(axis_event(device_id.clone(), 1, 80), now));

        assert_eq!(
            registry.axis_state(&device_id, 0).expect("x axis").baseline,
            10
        );
        assert_eq!(
            registry.axis_state(&device_id, 1).expect("y axis").baseline,
            80
        );
    }

    #[test]
    fn registry_returns_activity_when_policy_reports_activity() {
        let mut registry = ActivityRegistry::new(test_policy());
        let now = Instant::now();
        let device_id = DeviceId::new("controller");

        assert!(!registry.observe(axis_event(device_id.clone(), 0, 10), now));
        assert!(registry.observe(
            axis_event(device_id, 0, 30),
            now + Duration::from_millis(100),
        ));
    }

    #[test]
    fn registry_removes_device_state() {
        let mut registry = ActivityRegistry::new(test_policy());
        let device_id = DeviceId::new("controller");

        assert!(registry.observe(button_event(device_id.clone()), Instant::now()));
        registry.remove_device(&device_id);

        assert!(!registry.has_device(&device_id));
    }
}
