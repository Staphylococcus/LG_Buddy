//! Bounded, timestamped observations from application-lifetime activity adapters.
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Instant;

use super::inactivity::InactivityObservation;
use super::{SessionEvent, SessionObservation};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum ActivitySource {
    Gnome,
    Wayland,
}

impl ActivitySource {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Gnome => "gnome",
            Self::Wayland => "wayland",
        }
    }
}

#[derive(Debug, Default)]
struct Contribution {
    latest: [Option<Instant>; 3],
    pending: [Option<(InactivityObservation, Instant)>; 3],
}

#[derive(Debug, Default)]
pub(crate) struct ActivityContributions {
    sources: Mutex<BTreeMap<ActivitySource, Contribution>>,
}

impl ActivityContributions {
    pub(crate) fn publish(&self, source: ActivitySource, observation: SessionObservation) {
        let (kind, at) = match observation {
            SessionObservation::Inactivity {
                observation,
                observed_at,
                ..
            } => (observation, observed_at),
            SessionObservation::Event {
                event, observed_at, ..
            } => match event {
                SessionEvent::Active => (InactivityObservation::ProviderActive, observed_at),
                SessionEvent::WakeRequested => (InactivityObservation::WakeRequested, observed_at),
                // Native idle is never a blanking authority. Lock and lifecycle
                // observations have their independent sources in the runner.
                _ => return,
            },
        };
        let index = match kind {
            InactivityObservation::DesktopActivityObserved
            | InactivityObservation::UserActivityObserved => 0,
            InactivityObservation::ProviderActive => 1,
            InactivityObservation::WakeRequested => 2,
        };
        let mut sources = self.sources.lock().expect("activity sources");
        let state = sources.entry(source).or_default();
        if state.latest[index].is_none_or(|latest| at > latest) {
            state.latest[index] = Some(at);
            state.pending[index] = Some((kind, at));
        }
    }

    pub(crate) fn drain(&self) -> Vec<(ActivitySource, InactivityObservation, Instant)> {
        let mut observations = Vec::new();
        for (&source, state) in self.sources.lock().expect("activity sources").iter_mut() {
            for slot in &mut state.pending {
                if let Some((observation, at)) = slot.take() {
                    observations.push((source, observation, at));
                }
            }
        }
        observations.sort_by_key(|(_, _, at)| *at);
        observations
    }

    pub(crate) fn latest_activity(&self, source: ActivitySource) -> Option<Instant> {
        self.sources
            .lock()
            .expect("activity sources")
            .get(&source)
            .and_then(|state| state.latest.iter().flatten().max().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventSource;
    use std::time::Duration;

    fn input(at: Instant) -> SessionObservation {
        SessionObservation::Inactivity {
            observation: InactivityObservation::DesktopActivityObserved,
            source: EventSource::DesktopSession,
            observed_at: at,
        }
    }

    #[test]
    fn overlapping_sources_preserve_times_and_do_not_overwrite_newer_input() {
        let sources = ActivityContributions::default();
        let start = Instant::now();
        let latest = start + Duration::from_secs(1);
        sources.publish(ActivitySource::Gnome, input(latest));
        sources.publish(ActivitySource::Gnome, input(start));
        sources.publish(ActivitySource::Wayland, input(latest));
        assert_eq!(
            sources.drain(),
            vec![
                (
                    ActivitySource::Gnome,
                    InactivityObservation::DesktopActivityObserved,
                    latest
                ),
                (
                    ActivitySource::Wayland,
                    InactivityObservation::DesktopActivityObserved,
                    latest
                ),
            ]
        );
        assert!(sources.drain().is_empty());
        sources.publish(ActivitySource::Gnome, input(start));
        assert!(sources.drain().is_empty());
        assert_eq!(sources.latest_activity(ActivitySource::Gnome), Some(latest));
    }

    #[test]
    fn first_observation_is_delivered_without_registration_or_readiness() {
        let sources = ActivityContributions::default();
        let at = Instant::now();
        sources.publish(ActivitySource::Wayland, input(at));
        assert_eq!(
            sources.drain(),
            vec![(
                ActivitySource::Wayland,
                InactivityObservation::DesktopActivityObserved,
                at,
            )]
        );
    }
}
