//! KWin's native inhibition contribution, available when the optional bridge loads.
//! Provisioning is separate; each decision reads the compositor's current state.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::inhibition::{InhibitionEvaluation, InhibitionState, PullInhibitionAdapter};
use crate::session_bus::{
    get_name_owner, new_session_bus_client, BusMethodCall, SessionBusClient, SessionBusError,
};

pub const SERVICE: &str = "io.github.staphylococcus.LGBuddy.KWinInhibition";
pub const PATH: &str = "/io/github/staphylococcus/LGBuddy/KWinInhibition";
pub const INTERFACE: &str = "io.github.staphylococcus.LGBuddy.KWinInhibition1";

pub struct KWinInhibition {
    // History is diagnostic only. Every request obtains a fresh answer.
    state: InhibitionState,
    owner: Option<String>,
}

impl Default for KWinInhibition {
    fn default() -> Self {
        Self {
            state: InhibitionState::new("kwin"),
            owner: None,
        }
    }
}

impl PullInhibitionAdapter for KWinInhibition {
    fn query(&mut self, cancelled: &AtomicBool) -> Option<InhibitionEvaluation> {
        if cancelled.load(Ordering::SeqCst) {
            return None;
        }
        // Connect per request; the next check also handles late service
        // startup or connection recovery without a background monitor.
        let result = new_session_bus_client().and_then(|mut bus| read(&mut bus, cancelled));
        self.complete(result, cancelled)
    }
}

impl KWinInhibition {
    fn complete(
        &mut self,
        result: Result<Option<(String, bool, Instant)>, SessionBusError>,
        cancelled: &AtomicBool,
    ) -> Option<InhibitionEvaluation> {
        if cancelled.load(Ordering::SeqCst) {
            self.owner = None;
            self.state.unavailable("inhibition check cancelled");
            return None;
        }
        match result {
            Ok(Some((owner, inhibited, observed_at))) => {
                if self.owner.as_ref() != Some(&owner) {
                    // A new owner's clear answer is not an observed release by
                    // the old owner. Neither is recovery after a failed check.
                    self.state.unavailable("KWin bridge owner changed");
                }
                self.owner = Some(owner);
                self.state.observe(inhibited, observed_at);
            }
            Ok(None) => {
                self.owner = None;
                self.state.absent();
            }
            Err(error) => {
                self.owner = None;
                self.state.unavailable(error);
            }
        }
        Some(self.state.evaluate())
    }
}

fn read(
    bus: &mut impl SessionBusClient,
    cancelled: &AtomicBool,
) -> Result<Option<(String, bool, Instant)>, SessionBusError> {
    // Discovery does not activate KWin bridge on a desktop where it is absent.
    if !bus.name_has_owner(SERVICE)? || cancelled.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let owner = get_name_owner(bus, SERVICE)?;
    if cancelled.load(Ordering::SeqCst) {
        return Ok(None);
    }
    let inhibited = bus
        .call_method(BusMethodCall::new(&owner, PATH, INTERFACE, "IsInhibited"))?
        .single_bool()?;
    let observed_at = Instant::now();
    if cancelled.load(Ordering::SeqCst) {
        return Ok(None);
    }
    if get_name_owner(bus, SERVICE)? != owner {
        return Err(SessionBusError::Transport(
            "KWin bridge owner changed during check".into(),
        ));
    }
    Ok(Some((owner, inhibited, observed_at)))
}
