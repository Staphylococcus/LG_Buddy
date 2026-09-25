//! Linux DRM DPMS (Display Power Management Signaling) source.
//!
//! The OS (Xorg / a Wayland compositor) owns the physical display and blanks
//! it on its own schedule. This source polls the DRM sysfs tree
//! (`/sys/class/drm/card*-*/{status,dpms}`) and reports the *semantic* event
//! "the system display was blanked" — a known On -> Off transition on a
//! connector that is still connected. The runner feeds that observation into
//! the shared inactivity engine and reuses its existing blanking, completion,
//! ownership, delayed power-off, and suspend coordination. This source never
//! issues TV commands and never owns a timer of its own.
//!
//! Transition semantics (kept entirely in this source):
//! * a *known* On -> Off transition on a `connected` connector is a system
//!   blank and is reported;
//! * an initial reading of `Off`, an `unknown`/unreadable reading, a
//!   `disconnected` connector, Off -> On, and repeated On/Off are NOT
//!   transitions and must not be reported as a system blank or as user
//!   activity.
//!
//! The sysfs tree only exists on Linux; on other platforms the observer is a
//! no-op and starts no thread.

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DPMS_POLL_INTERVAL: Duration = Duration::from_millis(500);
const DPMS_POLL_INTERVAL_SECS_ENV: &str = "LG_BUDDY_DPMS_POLL_INTERVAL_SECS";

/// How the last successful read of a connector's DPMS state should be treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DpmsReading {
    /// Connector is connected and DPMS read as `On`.
    On,
    /// Connector is connected and DPMS read as `Off`.
    Off,
    /// Connector is disconnected, or the reading was unknown/unreadable.
    Inactive,
}

/// One connector's current state as read from sysfs.
#[derive(Debug, Clone)]
pub(crate) struct DpmsSnapshot {
    connected: bool,
    /// Raw `dpms` file value: `On`, `Off`, or anything else (treated as unknown).
    dpms: String,
}

/// How a connector snapshot should be treated by the transition detector.
fn reading_from_snapshot(snap: &DpmsSnapshot) -> DpmsReading {
    if !snap.connected {
        return DpmsReading::Inactive;
    }
    match snap.dpms.as_str() {
        "On" => DpmsReading::On,
        "Off" => DpmsReading::Off,
        _ => DpmsReading::Inactive,
    }
}

/// Fold the current connector readings `now` into `prev` and report whether a
/// known On -> Off transition occurred on this step.
///
/// A transition is only reported when a connector that was *last read as On*
/// (and connected) is now read as Off (and connected). Aggregation across
/// connectors is a logical OR: if any connected connector made a known On ->
/// Off transition, the step reports a system blank. An initial Off, an
/// unknown/unreadable reading, a disconnected connector, Off -> On, and
/// repeated On/Off never report a transition.
///
/// Pure over `prev`/`now` so it can be unit-tested without touching sysfs.
pub(crate) fn step_dpms_observations(
    prev: &mut HashMap<String, DpmsReading>,
    now: &HashMap<String, DpmsSnapshot>,
) -> bool {
    let transitioned = now.iter().any(|(name, snap)| {
        let current = reading_from_snapshot(snap);
        let previous = prev.get(name).copied().unwrap_or(DpmsReading::Inactive);
        previous == DpmsReading::On && current == DpmsReading::Off
    });
    for (name, snap) in now {
        prev.insert(name.clone(), reading_from_snapshot(snap));
    }
    // A connector missing from the new snapshot (disconnected, hot-unplugged,
    // or absent) must lose its stored baseline. Otherwise a vanished connector
    // that later reappears connected Off would be misread as a false On -> Off
    // transition (a fake system blank).
    let present: std::collections::HashSet<&str> = now.keys().map(|name| name.as_str()).collect();
    prev.retain(|name, _| present.contains(name.as_str()));
    transitioned
}

/// Read the current state of every DRM connector from sysfs.
///
/// Returns `None` when the DRM tree cannot be read (non-Linux, or a
/// restricted environment); a missing tree must not manufacture a transition.
#[cfg(target_os = "linux")]
fn read_sysfs_connector_states() -> Option<HashMap<String, DpmsSnapshot>> {
    let entries = std::fs::read_dir("/sys/class/drm").ok()?;
    let mut states = HashMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Connectors are named like `card1-HDMI-A-1` and expose status/dpms.
        if !name.starts_with("card") || !name.contains('-') {
            continue;
        }
        let dir = entry.path();
        let (connected, dpms) = match (
            std::fs::read_to_string(dir.join("status")),
            std::fs::read_to_string(dir.join("dpms")),
        ) {
            (Ok(status), Ok(dpms)) => (status.trim() == "connected", dpms.trim().to_string()),
            // Unreadable state is inactive, never a transition.
            _ => (false, String::from("unknown")),
        };
        states.insert(name, DpmsSnapshot { connected, dpms });
    }
    Some(states)
}

fn dpms_poll_interval() -> Duration {
    std::env::var(DPMS_POLL_INTERVAL_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .and_then(|value| Duration::try_from_secs_f64(value).ok())
        .unwrap_or(DPMS_POLL_INTERVAL)
}

/// Handle to a running DPMS blank observer. Stopping it joins the poll thread.
#[derive(Debug)]
pub struct DpmsBlankObserver {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for DpmsBlankObserver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Start a DPMS blank observer.
///
/// `on_system_blank` is invoked with the observation instant each time a known
/// On -> Off transition is seen; returning `false` stops the observer. On
/// non-Linux platforms this is a no-op that starts no thread.
pub fn spawn_dpms_blank_observer<F>(on_system_blank: F) -> DpmsBlankObserver
where
    F: FnMut(Instant) -> bool + Send + 'static,
{
    let stop = Arc::new(AtomicBool::new(false));
    let handle = {
        #[cfg(target_os = "linux")]
        {
            let thread_stop = Arc::clone(&stop);
            Some(thread::spawn(move || {
                run_dpms_observer(on_system_blank, &thread_stop)
            }))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = on_system_blank;
            None
        }
    };
    DpmsBlankObserver { stop, handle }
}

#[cfg(target_os = "linux")]
fn run_dpms_observer<F>(mut on_system_blank: F, stop: &AtomicBool)
where
    F: FnMut(Instant) -> bool,
{
    let mut prev: HashMap<String, DpmsReading> = HashMap::new();
    while !stop.load(Ordering::SeqCst) {
        thread::sleep(dpms_poll_interval());
        // A failed whole-tree read leaves the interval's state unknown, so the
        // stored baselines can no longer be trusted: drop them. Keeping a stale
        // `On` baseline across an unknown gap would let a connector that later
        // reappears connected Off manufacture a transition that never happened.
        let Some(now) = read_sysfs_connector_states() else {
            prev.clear();
            continue;
        };
        if step_dpms_observations(&mut prev, &now) && !on_system_blank(Instant::now()) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{step_dpms_observations, DpmsSnapshot};
    use std::collections::HashMap;

    fn snap(connected: bool, dpms: &str) -> DpmsSnapshot {
        DpmsSnapshot {
            connected,
            dpms: dpms.to_string(),
        }
    }

    fn reading(name: &str, connected: bool, dpms: &str) -> (String, DpmsSnapshot) {
        (name.to_string(), snap(connected, dpms))
    }

    fn map(states: Vec<(String, DpmsSnapshot)>) -> HashMap<String, DpmsSnapshot> {
        states.into_iter().collect()
    }

    #[test]
    fn known_on_to_off_on_connected_connector_fires() {
        let mut prev = HashMap::new();
        // Initial read: On (establishes the "was On" baseline; no transition).
        let on = map(vec![reading("card1-HDMI-A-1", true, "On")]);
        assert!(!step_dpms_observations(&mut prev, &on));
        // Second read: Off. On -> Off transition fires.
        let off = map(vec![reading("card1-HDMI-A-1", true, "Off")]);
        assert!(step_dpms_observations(&mut prev, &off));
    }

    #[test]
    fn initial_off_unknown_and_disconnected_do_not_fire() {
        let mut prev = HashMap::new();
        let initial_off = map(vec![
            reading("card1-HDMI-A-1", true, "Off"),
            reading("card2-VGA-1", true, "unknown"),
            reading("card3-DP-1", false, "On"),
        ]);
        assert!(!step_dpms_observations(&mut prev, &initial_off));
        // Repeating those same readings still must not fire.
        assert!(!step_dpms_observations(&mut prev, &initial_off));
    }

    #[test]
    fn off_to_on_and_repeated_on_do_not_fire() {
        let mut prev = HashMap::new();
        // Baseline: Off.
        let off = map(vec![reading("card1-HDMI-A-1", true, "Off")]);
        assert!(!step_dpms_observations(&mut prev, &off));
        // Off -> On is a restore, not a system blank: no fire.
        let on = map(vec![reading("card1-HDMI-A-1", true, "On")]);
        assert!(!step_dpms_observations(&mut prev, &on));
        // Repeated On: no fire.
        assert!(!step_dpms_observations(&mut prev, &on));
    }

    #[test]
    fn repeated_off_does_not_fire_again() {
        let mut prev = HashMap::new();
        let on = map(vec![reading("card1-HDMI-A-1", true, "On")]);
        assert!(!step_dpms_observations(&mut prev, &on));
        let off = map(vec![reading("card1-HDMI-A-1", true, "Off")]);
        assert!(step_dpms_observations(&mut prev, &off));
        // The display is already off; a repeated Off is not a new blank.
        assert!(!step_dpms_observations(&mut prev, &off));
    }

    #[test]
    fn hotplug_disconnect_does_not_fire() {
        let mut prev = HashMap::new();
        let on = map(vec![reading("card1-HDMI-A-1", true, "On")]);
        assert!(!step_dpms_observations(&mut prev, &on));
        // Connector disappears (disconnected): inactive, not a blank.
        let gone = map(vec![reading("card1-HDMI-A-1", false, "Off")]);
        assert!(!step_dpms_observations(&mut prev, &gone));
    }

    #[test]
    fn any_connected_connector_transition_fires_aggregated() {
        let mut prev = HashMap::new();
        let on = map(vec![
            reading("card1-HDMI-A-1", true, "On"),
            reading("card2-HDMI-A-2", true, "On"),
        ]);
        assert!(!step_dpms_observations(&mut prev, &on));
        // Only one connector blanks; the other stays On. Aggregation is OR.
        let one_off = map(vec![
            reading("card1-HDMI-A-1", true, "Off"),
            reading("card2-HDMI-A-2", true, "On"),
        ]);
        assert!(step_dpms_observations(&mut prev, &one_off));
    }

    #[test]
    fn vanished_connector_losing_its_on_baseline_does_not_fire_on_reappearance() {
        let mut prev = HashMap::new();
        let state = |value: &str| {
            HashMap::from([(
                "card1-HDMI-A-1".to_owned(),
                DpmsSnapshot {
                    connected: true,
                    dpms: value.to_owned(),
                },
            )])
        };
        // Connected On establishes the baseline; the next read omits the
        // connector entirely (it vanished).
        assert!(!step_dpms_observations(&mut prev, &state("On")));
        assert!(!step_dpms_observations(&mut prev, &HashMap::new()));
        // The same connector reappears connected Off: the vanished connector
        // must not carry a stale On baseline, so this is NOT a blank.
        assert!(
            !step_dpms_observations(&mut prev, &state("Off")),
            "a vanished connector must lose its On baseline"
        );
    }

    #[test]
    fn failed_source_read_resets_baselines_so_reappearance_is_not_a_blank() {
        // Models a failed whole-tree read: `prev` is cleared, then the connector
        // reappears connected Off. With a stale On baseline it would falsely fire.
        let mut prev = HashMap::new();
        let on = map(vec![reading("card1-HDMI-A-1", true, "On")]);
        assert!(!step_dpms_observations(&mut prev, &on));
        // Source unavailable: baselines are discarded (mirrors the poller's
        // `prev.clear()` on a failed read).
        prev.clear();
        let off = map(vec![reading("card1-HDMI-A-1", true, "Off")]);
        assert!(
            !step_dpms_observations(&mut prev, &off),
            "a fresh read after an unavailable source must not manufacture a blank"
        );
    }

    #[test]
    fn absent_connector_is_reset_even_when_still_tracked_as_disconnected() {
        let mut prev = HashMap::new();
        let on = map(vec![reading("card1-HDMI-A-1", true, "On")]);
        assert!(!step_dpms_observations(&mut prev, &on));
        // The connector drops out of the snapshot, then reappears connected Off.
        let gone = HashMap::new();
        assert!(!step_dpms_observations(&mut prev, &gone));
        let off = map(vec![reading("card1-HDMI-A-1", true, "Off")]);
        assert!(!step_dpms_observations(&mut prev, &off));
    }
}
