//! Read-only observations from the running monitor, never permission inputs.

use std::fmt::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::config::ScreenBackend;
use crate::inhibition::{
    BlankingEvaluation, Inhibition, InhibitionPreferenceEvaluation, InhibitionSectionEvaluation,
    InhibitionStatus,
};
use crate::sources::desktop::{ActivityAdapter, ActivityStatus};

use super::activity::{ActivityContributions, ActivitySource};
use super::inactivity::InactivityEngine;

#[derive(Clone, Default)]
pub(crate) struct MonitorDiagnostics(Arc<Mutex<Snapshot>>);

#[derive(Default)]
struct Snapshot {
    updated: Option<Instant>,
    mode: String,
    configured: Option<ScreenBackend>,
    activity: Vec<(&'static str, ActivityStatus, Option<Instant>)>,
    inhibition: Option<BlankingEvaluation>,
    preference: Option<InhibitionPreferenceEvaluation>,
    next_action_at: Option<Instant>,
    timed_power_off_pending: bool,
}

impl MonitorDiagnostics {
    pub(crate) fn waiting(&self, configured: Option<ScreenBackend>, mode: impl Into<String>) {
        *self.0.lock().expect("monitor diagnostics") = Snapshot {
            updated: Some(Instant::now()),
            configured,
            mode: mode.into(),
            ..Snapshot::default()
        };
    }

    pub(crate) fn publish(
        &self,
        configured: ScreenBackend,
        adapters: &[(ActivitySource, Arc<dyn ActivityAdapter>)],
        contributions: &ActivityContributions,
        inhibition: Option<&Inhibition>,
        inactivity: &InactivityEngine,
    ) {
        let now = Instant::now();
        *self.0.lock().expect("monitor diagnostics") = Snapshot {
            updated: Some(now),
            mode: "running".into(),
            configured: Some(configured),
            activity: adapters
                .iter()
                .map(|(source, adapter)| {
                    (
                        source.name(),
                        adapter.status(),
                        contributions.latest_activity(*source),
                    )
                })
                .collect(),
            inhibition: inhibition.and_then(Inhibition::diagnostics).cloned(),
            preference: inhibition.map(Inhibition::preference_diagnostics),
            next_action_at: inactivity
                .time_until_action(now)
                .map(|remaining| now + remaining),
            timed_power_off_pending: inactivity.timed_power_off_pending(),
        };
    }

    /// All sections come from one captured runtime snapshot. Reading does not
    /// query inhibitors, drain activity, or feed any result back into policy.
    pub(crate) fn report(&self) -> (String, String, String) {
        let state = self.0.lock().expect("monitor diagnostics");
        let now = Instant::now();
        let mut context = format!(
            "monitor process: {}\nmode: {}\nsnapshot age: {}\n",
            std::process::id(),
            state.mode,
            age(state.updated, now)
        );
        if let Some(configured) = state.configured {
            writeln!(context, "configured integration: {}", configured.as_str()).unwrap();
            if std::env::var_os("LG_BUDDY_SCREEN_BACKEND").is_some() {
                context.push_str(
                    "configuration origin: LG_BUDDY_SCREEN_BACKEND environment override\n",
                );
            }
            if configured != ScreenBackend::Auto {
                writeln!(context, "legacy override: {}; native activity discovery is restricted until an explicit switch to automatic integration", configured.as_str()).unwrap();
            }
        }
        context.push_str("Snapshot and evaluation ages describe observations; they never grant permission for a later blanking attempt.\n");
        let mut activity = String::new();
        if let Some(deadline) = state.next_action_at {
            writeln!(
                activity,
                "next scheduled inactivity action: {}; due in {} ms",
                if state.timed_power_off_pending {
                    "post-blank power-off"
                } else {
                    "idle blanking evaluation"
                },
                deadline.saturating_duration_since(now).as_millis()
            )
            .unwrap();
        } else {
            activity.push_str("next scheduled inactivity action: none\n");
        }
        for (name, status, last_activity) in &state.activity {
            writeln!(
                activity,
                "source: {name}; available: {}; last contribution age: {}",
                status.is_available(),
                age(*last_activity, now)
            )
            .unwrap();
            if let ActivityStatus::Unavailable(reason) = status {
                writeln!(activity, "  absence detail: {reason}").unwrap();
            }
        }
        if state.activity.is_empty() {
            activity.push_str(if state.configured == Some(ScreenBackend::Swayidle) {
                "Explicit legacy swayidle supplies timeout/resume events; native activity adapters are not selected.\n"
            } else {
                "No native activity adapter is running.\n"
            });
        }
        activity.push_str(
            "Availability comes from the interface connection, independently of event silence.\n",
        );
        let mut inhibition = String::new();
        if let Some(preference) = state.preference {
            writeln!(
                inhibition,
                "preference: honor app inhibition={}; bypass={}",
                preference.diagnostics.honoring.as_str(),
                preference.bypass_inhibition
            )
            .unwrap();
        }
        if let Some(evaluation) = &state.inhibition {
            writeln!(
                inhibition,
                "evaluation age: {}\naggregate can_blank: {}\npull work pending: {}",
                age(Some(evaluation.evaluated_at), now),
                evaluation.can_blank,
                evaluation.checking
            )
            .unwrap();
            section(&mut inhibition, "push", evaluation.push.as_ref(), now);
            section(&mut inhibition, "pull", evaluation.pull.as_ref(), now);
            writeln!(
                inhibition,
                "inhibition release delay remaining: {} ms",
                evaluation
                    .release_not_before
                    .map_or(0, |at| at.saturating_duration_since(now).as_millis())
            )
            .unwrap();
        } else if state.configured == Some(ScreenBackend::Swayidle) {
            inhibition.push_str("Legacy override: swayidle always honors compositor inhibition; the native preference and sections do not apply.\n");
        } else {
            inhibition.push_str("No active blanking evaluation. Checks run when the idle deadline is due; activity and lifecycle changes discard the previous evaluation.\n");
        }
        inhibition.push_str("An absent source contributes no inhibitor. Sources omitted by preference bypass or pending work have no check result in this evaluation.\n");
        inhibition.push_str("The aggregate is inhibition permission; activity eligibility is evaluated separately.\n");
        (activity, inhibition, context)
    }
}

fn age(at: Option<Instant>, now: Instant) -> String {
    at.map_or_else(
        || "none observed".into(),
        |at| format!("{} ms", now.saturating_duration_since(at).as_millis()),
    )
}

fn section(
    output: &mut String,
    name: &str,
    section: Option<&InhibitionSectionEvaluation>,
    now: Instant,
) {
    let Some(section) = section else {
        writeln!(output, "{name}: not evaluated on this call").unwrap();
        return;
    };
    writeln!(output, "{name}: allowed={}", section.allowed).unwrap();
    for result in &section.contributions {
        let status = match &result.diagnostics.status {
            InhibitionStatus::Absent => "absent".into(),
            InhibitionStatus::Unavailable(reason) => format!("absent ({reason})"),
            InhibitionStatus::Clear => "clear".into(),
            InhibitionStatus::Inhibited => "inhibited".into(),
        };
        writeln!(
            output,
            "  source: {}; result: {status}; allowed: {}; observation age: {}; release age: {}",
            result.diagnostics.source,
            result.allowed,
            age(result.diagnostics.observed_at, now),
            age(result.diagnostics.last_release_at, now)
        )
        .unwrap();
    }
}
