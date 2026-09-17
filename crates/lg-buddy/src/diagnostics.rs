//! On-demand, read-only diagnostics for the graphical application.
//!
//! Each domain collects its own bounded section. This orchestrator only orders
//! those sections into a current snapshot followed by recent logs.

mod application;
use crate::command;
mod desktop;
mod inhibition;
mod journal;
mod monitor;
mod report;
mod services;
mod settings;
mod tv;

pub use report::{DiagnosticSection, DiagnosticsReport};

use std::time::{SystemTime, UNIX_EPOCH};

/// Synchronous collection runs behind the GUI's existing worker boundary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentDiagnosticsCollector;

impl EnvironmentDiagnosticsCollector {
    /// Collect current state without activating services or changing policy.
    pub fn collect(&self) -> DiagnosticsReport {
        let collected_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let mut sections = vec![desktop::collect()];
        sections.extend(monitor::collect());
        sections.extend([
            inhibition::collect(),
            settings::collect(),
            services::collect(),
            tv::collect(),
            application::collect(),
        ]);
        sections.extend(journal::collect());
        DiagnosticsReport::new(collected_at, sections)
    }
}
