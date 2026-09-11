//! Bounded activity contributions from independently connected native sources.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SourceInstance {
    source: ActivitySource,
    generation: u64,
}

#[derive(Debug, Default)]
struct Contribution {
    generation: u64,
    connected: bool,
    latest: [Option<Instant>; 3],
    pending: [Option<(InactivityObservation, Instant)>; 3],
    diagnostic: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct ActivityContributions {
    sources: Mutex<BTreeMap<ActivitySource, Contribution>>,
    last_delivered: Mutex<Option<Instant>>,
}

#[derive(Debug)]
pub(crate) struct ActivitySnapshot {
    pub source: ActivitySource,
    pub generation: u64,
    pub connected: bool,
    pub latest_activity: Option<Instant>,
    pub diagnostic: Option<String>,
}

impl ActivityContributions {
    pub(crate) fn begin(&self, source: ActivitySource) -> SourceInstance {
        let mut sources = self.sources.lock().expect("activity sources");
        let state = sources.entry(source).or_default();
        state.generation += 1;
        state.connected = false;
        state.pending = [None; 3];
        state.latest = [None; 3];
        SourceInstance {
            source,
            generation: state.generation,
        }
    }

    pub(crate) fn connected(&self, instance: SourceInstance) {
        self.update(instance, |state| {
            state.connected = true;
            state.diagnostic = None;
        });
    }

    pub(crate) fn unavailable(&self, instance: SourceInstance, reason: &str) {
        self.update(instance, |state| {
            state.connected = false;
            state.pending = [None; 3];
            state.diagnostic = Some(reason.chars().take(512).collect());
        });
    }

    fn update(&self, instance: SourceInstance, update: impl FnOnce(&mut Contribution)) {
        if let Some(state) = self
            .sources
            .lock()
            .expect("activity sources")
            .get_mut(&instance.source)
        {
            if state.generation == instance.generation {
                update(state);
            }
        }
    }

    pub(crate) fn publish(&self, instance: SourceInstance, observation: SessionObservation) {
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
        self.update(instance, |state| {
            if state.connected && state.latest[index].is_none_or(|latest| at > latest) {
                state.latest[index] = Some(at);
                state.pending[index] = Some((kind, at));
            }
        });
    }

    pub(crate) fn drain(&self) -> Vec<(SourceInstance, InactivityObservation, Instant)> {
        let mut observations = Vec::new();
        for (&source, state) in self.sources.lock().expect("activity sources").iter_mut() {
            for slot in &mut state.pending {
                if let Some((observation, at)) = slot.take() {
                    observations.push((
                        SourceInstance {
                            source,
                            generation: state.generation,
                        },
                        observation,
                        at,
                    ));
                }
            }
        }
        observations.sort_by_key(|(_, _, at)| *at);
        observations
    }

    /// Recheck the instance immediately before delivery: a previous TV action
    /// may have blocked while this source disconnected or was replaced.
    pub(crate) fn accept(&self, instance: SourceInstance, at: Instant) -> bool {
        let sources = self.sources.lock().expect("activity sources");
        if !sources
            .get(&instance.source)
            .is_some_and(|state| state.connected && state.generation == instance.generation)
        {
            return false;
        }
        // Native adapters describe the same desktop activity domain. A duplicate
        // or older contribution never becomes new input because delivery was late.
        let mut latest = self.last_delivered.lock().expect("delivered activity");
        if latest.is_some_and(|latest| at <= latest) {
            return false;
        }
        *latest = Some(at);
        true
    }

    pub(crate) fn snapshot(&self) -> Vec<ActivitySnapshot> {
        self.sources
            .lock()
            .expect("activity sources")
            .iter()
            .map(|(&source, state)| ActivitySnapshot {
                source,
                generation: state.generation,
                connected: state.connected,
                latest_activity: state.latest.iter().flatten().max().copied(),
                diagnostic: state.diagnostic.clone(),
            })
            .collect()
    }

    pub(crate) fn has_connected_source(&self) -> bool {
        self.sources
            .lock()
            .expect("activity sources")
            .values()
            .any(|state| state.connected)
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

    fn deliver(sources: &ActivityContributions) -> Vec<(InactivityObservation, Instant)> {
        sources
            .drain()
            .into_iter()
            .filter(|(instance, _, at)| sources.accept(*instance, *at))
            .map(|(_, observation, at)| (observation, at))
            .collect()
    }

    #[test]
    fn overlapping_sources_preserve_times_and_do_not_overwrite_newer_input() {
        let sources = ActivityContributions::default();
        let gnome = sources.begin(ActivitySource::Gnome);
        let wayland = sources.begin(ActivitySource::Wayland);
        sources.connected(gnome);
        sources.connected(wayland);
        let start = Instant::now();
        let latest = start + Duration::from_secs(1);
        sources.publish(gnome, input(latest));
        sources.publish(gnome, input(start));
        sources.publish(wayland, input(latest));
        assert_eq!(
            deliver(&sources),
            vec![(InactivityObservation::DesktopActivityObserved, latest)]
        );
        assert!(sources.drain().is_empty());
        sources.publish(gnome, input(start));
        assert!(sources.drain().is_empty());
    }

    #[test]
    fn source_loss_invalidates_pending_input_without_affecting_other_sources() {
        let sources = ActivityContributions::default();
        let gnome = sources.begin(ActivitySource::Gnome);
        let wayland = sources.begin(ActivitySource::Wayland);
        sources.connected(gnome);
        sources.connected(wayland);
        let at = Instant::now();
        sources.publish(gnome, input(at));
        sources.unavailable(gnome, "owner lost");
        sources.publish(gnome, input(at + Duration::from_secs(1)));
        assert!(sources.has_connected_source());
        assert!(sources.drain().is_empty());
        let replacement = sources.begin(ActivitySource::Gnome);
        sources.connected(replacement);
        sources.unavailable(gnome, "late error");
        sources.publish(gnome, input(at + Duration::from_secs(2)));
        assert!(sources.drain().is_empty());
        sources.publish(replacement, input(at + Duration::from_secs(1)));
        assert_eq!(sources.drain().len(), 1);
        assert!(sources.snapshot().iter().all(|source| source.connected));
    }

    #[test]
    fn quiet_connections_remain_available_and_reports_are_bounded() {
        let sources = ActivityContributions::default();
        let instance = sources.begin(ActivitySource::Gnome);
        sources.connected(instance);
        assert!(sources.drain().is_empty());
        assert!(sources.has_connected_source());
        sources.unavailable(instance, &"x".repeat(1000));
        assert!(!sources.has_connected_source());
        assert_eq!(
            sources.snapshot()[0].diagnostic.as_ref().unwrap().len(),
            512
        );
    }
    #[test]
    fn a_batch_taken_before_replacement_cannot_revive_its_source() {
        let sources = ActivityContributions::default();
        let old = sources.begin(ActivitySource::Gnome);
        sources.connected(old);
        sources.publish(old, input(Instant::now()));
        let batch = sources.drain();
        let new = sources.begin(ActivitySource::Gnome);
        sources.connected(new);
        assert!(!sources.accept(batch[0].0, batch[0].2));
        sources.publish(new, input(Instant::now()));
        assert_eq!(deliver(&sources).len(), 1);
    }
}
