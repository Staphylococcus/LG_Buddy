//! On-demand diagnostics workflow. Hosts execute reads, clipboard writes and
//! file selection; the application owns the report and export snapshot.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::diagnostics::{DiagnosticsReport, EnvironmentDiagnosticsCollector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticsIntent {
    Open,
    Refresh,
    Close,
    Copy,
    Save,
    SaveDestination {
        request: DiagnosticsSaveRequest,
        path: Option<PathBuf>,
    },
    SaveSelectionFailed(DiagnosticsSaveRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsReadOperation {
    id: u64,
    session_findings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticsSaveRequest(u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsSaveOperation {
    request: DiagnosticsSaveRequest,
    path: PathBuf,
    report: DiagnosticsReport,
}

impl DiagnosticsSaveOperation {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn text(&self) -> &str {
        self.report.text()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsError(String);

impl DiagnosticsError {
    pub fn collection_stopped() -> Self {
        Self("Collection stopped unexpectedly. Refresh to try again.".into())
    }

    pub fn save_failed() -> Self {
        Self("Could not save the report. Choose a writable location with enough free space and try again.".into())
    }
}

pub trait DiagnosticsBackend: Send + Sync + 'static {
    fn collect(&self) -> Result<DiagnosticsReport, DiagnosticsError>;

    fn save(&self, operation: &DiagnosticsSaveOperation) -> Result<(), DiagnosticsError> {
        save_report(operation.path(), operation.text()).map_err(|_| DiagnosticsError::save_failed())
    }
}

#[derive(Debug, Default)]
pub struct EnvironmentDiagnosticsBackend;

impl DiagnosticsBackend for EnvironmentDiagnosticsBackend {
    fn collect(&self) -> Result<DiagnosticsReport, DiagnosticsError> {
        Ok(EnvironmentDiagnosticsCollector.collect())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiagnosticsPresentation {
    visible: bool,
    collecting: bool,
    saving: bool,
    choosing_save: bool,
    report: Option<DiagnosticsReport>,
    error: Option<String>,
}

impl DiagnosticsPresentation {
    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn collecting(&self) -> bool {
        self.collecting
    }

    pub fn saving(&self) -> bool {
        self.saving
    }

    pub fn report_text(&self) -> Option<&str> {
        self.report.as_ref().map(DiagnosticsReport::text)
    }

    pub fn collected_at(&self) -> Option<&str> {
        self.report.as_ref().map(DiagnosticsReport::collected_at)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn can_refresh(&self) -> bool {
        self.visible && !self.collecting && !self.saving && !self.choosing_save
    }

    pub fn can_export(&self) -> bool {
        self.can_refresh() && self.report.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsTransition {
    presentation: DiagnosticsPresentation,
    read: Option<DiagnosticsReadOperation>,
    choose_save: Option<DiagnosticsSaveRequest>,
    save: Option<DiagnosticsSaveOperation>,
    clipboard_text: Option<String>,
    toast: Option<&'static str>,
}

impl DiagnosticsTransition {
    pub fn presentation(&self) -> &DiagnosticsPresentation {
        &self.presentation
    }

    pub fn read_operation(&self) -> Option<&DiagnosticsReadOperation> {
        self.read.as_ref()
    }

    pub fn save_request(&self) -> Option<DiagnosticsSaveRequest> {
        self.choose_save
    }

    pub fn save_operation(&self) -> Option<&DiagnosticsSaveOperation> {
        self.save.as_ref()
    }

    pub fn clipboard_text(&self) -> Option<&str> {
        self.clipboard_text.as_deref()
    }

    pub fn toast(&self) -> Option<&str> {
        self.toast
    }
}

#[derive(Default)]
pub struct DiagnosticsApplication {
    presentation: DiagnosticsPresentation,
    next_id: u64,
    pending_read: Option<u64>,
    pending_save: Option<(DiagnosticsSaveRequest, DiagnosticsReport)>,
    closed: bool,
    recent_findings: Vec<String>,
}

impl DiagnosticsApplication {
    /// Only application-owned user-facing summaries belong here. Protocol
    /// frames and arbitrary subprocess/configuration output must not be retained.
    pub(crate) fn record_failure(&mut self, context: &str, message: &str) {
        let finding: String = format!("{context}: {message}").chars().take(1024).collect();
        if self.recent_findings.contains(&finding) {
            return;
        }
        self.recent_findings.push(finding);
        if self.recent_findings.len() > 12 {
            self.recent_findings.remove(0);
        }
    }

    pub fn handle_intent(&mut self, intent: DiagnosticsIntent) -> Option<DiagnosticsTransition> {
        if self.closed {
            return None;
        }
        match intent {
            DiagnosticsIntent::Open if !self.presentation.visible => {
                self.presentation.visible = true;
                Some(self.begin_read())
            }
            DiagnosticsIntent::Refresh if self.presentation.can_refresh() => {
                Some(self.begin_read())
            }
            DiagnosticsIntent::Close if self.presentation.visible => {
                self.presentation.visible = false;
                self.presentation.collecting = false;
                self.presentation.saving = false;
                self.presentation.choosing_save = false;
                self.pending_read = None;
                self.pending_save = None;
                Some(self.transition())
            }
            DiagnosticsIntent::Copy if self.presentation.can_export() => {
                let mut transition = self.transition();
                transition.clipboard_text = self.presentation.report_text().map(str::to_string);
                transition.toast = Some("Diagnostic report copied");
                Some(transition)
            }
            DiagnosticsIntent::Save if self.presentation.can_export() => {
                let request = DiagnosticsSaveRequest(self.next_id);
                self.next_id += 1;
                self.pending_save = Some((request, self.presentation.report.clone()?));
                self.presentation.choosing_save = true;
                self.presentation.error = None;
                let mut transition = self.transition();
                transition.choose_save = Some(request);
                Some(transition)
            }
            DiagnosticsIntent::SaveDestination { request, path }
                if self.presentation.choosing_save && self.save_matches(request) =>
            {
                self.presentation.choosing_save = false;
                let mut transition = self.transition();
                if let Some(path) = path {
                    self.presentation.saving = true;
                    transition.presentation = self.presentation.clone();
                    transition.save = Some(DiagnosticsSaveOperation {
                        request,
                        path,
                        report: self.pending_save.as_ref()?.1.clone(),
                    });
                } else {
                    self.pending_save = None;
                }
                Some(transition)
            }
            DiagnosticsIntent::SaveSelectionFailed(request)
                if self.presentation.choosing_save && self.save_matches(request) =>
            {
                self.pending_save = None;
                self.presentation.choosing_save = false;
                self.presentation.error = Some(
                    "Could not select a local save location. Try again or copy the report.".into(),
                );
                Some(self.transition())
            }
            _ => None,
        }
    }

    pub fn complete_read(
        &mut self,
        operation: &DiagnosticsReadOperation,
        result: Result<DiagnosticsReport, DiagnosticsError>,
    ) -> Option<DiagnosticsTransition> {
        if self.closed || self.pending_read != Some(operation.id) {
            return None;
        }
        self.pending_read = None;
        self.presentation.collecting = false;
        match result {
            Ok(report) => {
                self.presentation.report =
                    Some(report.with_session_findings(&operation.session_findings))
            }
            Err(error) => self.presentation.error = Some(error.0),
        }
        Some(self.transition())
    }

    pub fn complete_save(
        &mut self,
        operation: &DiagnosticsSaveOperation,
        result: Result<(), DiagnosticsError>,
    ) -> Option<DiagnosticsTransition> {
        if self.closed || !self.presentation.saving || !self.save_matches(operation.request) {
            return None;
        }
        self.pending_save = None;
        self.presentation.saving = false;
        let mut transition = self.transition();
        match result {
            Ok(()) => transition.toast = Some("Diagnostic report saved"),
            Err(error) => {
                self.presentation.error = Some(error.0);
                transition.presentation = self.presentation.clone();
            }
        }
        Some(transition)
    }

    pub fn shutdown(&mut self) {
        self.closed = true;
        self.pending_read = None;
        self.pending_save = None;
    }

    fn save_matches(&self, request: DiagnosticsSaveRequest) -> bool {
        self.pending_save
            .as_ref()
            .is_some_and(|(pending, _)| *pending == request)
    }

    fn begin_read(&mut self) -> DiagnosticsTransition {
        let operation = DiagnosticsReadOperation {
            id: self.next_id,
            session_findings: self.recent_findings.clone(),
        };
        self.next_id += 1;
        self.pending_read = Some(operation.id);
        self.presentation.collecting = true;
        self.presentation.error = None;
        let mut transition = self.transition();
        transition.read = Some(operation);
        transition
    }

    fn transition(&self) -> DiagnosticsTransition {
        DiagnosticsTransition {
            presentation: self.presentation.clone(),
            read: None,
            choose_save: None,
            save: None,
            clipboard_text: None,
            toast: None,
        }
    }
}

fn save_report(path: &Path, text: &str) -> std::io::Result<()> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(
        ".lg-buddy-diagnostics-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| {
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::DiagnosticSection;

    fn report(label: &str) -> DiagnosticsReport {
        DiagnosticsReport::new(1_000, vec![DiagnosticSection::new("Application", label)])
    }

    fn ready(app: &mut DiagnosticsApplication) -> DiagnosticsTransition {
        let opened = app.handle_intent(DiagnosticsIntent::Open).unwrap();
        app.complete_read(opened.read_operation().unwrap(), Ok(report("fixture")))
            .unwrap()
    }

    #[test]
    fn collection_is_on_demand_and_close_rejects_stale_results() {
        let mut app = DiagnosticsApplication::default();
        assert!(!app.presentation.visible());
        assert!(app.pending_read.is_none());
        let opening = app.handle_intent(DiagnosticsIntent::Open).unwrap();
        assert!(!opening.presentation().can_export());
        assert!(app.handle_intent(DiagnosticsIntent::Refresh).is_none());
        app.handle_intent(DiagnosticsIntent::Close).unwrap();
        let reopening = app.handle_intent(DiagnosticsIntent::Open).unwrap();
        assert!(app
            .complete_read(opening.read_operation().unwrap(), Ok(report("old")))
            .is_none());
        let completed = app
            .complete_read(reopening.read_operation().unwrap(), Ok(report("new")))
            .unwrap();
        assert!(completed
            .presentation()
            .report_text()
            .unwrap()
            .contains("new"));
        app.shutdown();
        assert!(app.handle_intent(DiagnosticsIntent::Open).is_none());
    }

    #[test]
    fn copy_and_save_use_the_visible_report_and_cancel_is_quiet() {
        let mut app = DiagnosticsApplication::default();
        let loaded = ready(&mut app);
        let copy = app.handle_intent(DiagnosticsIntent::Copy).unwrap();
        assert_eq!(copy.clipboard_text(), loaded.presentation().report_text());
        let choosing = app.handle_intent(DiagnosticsIntent::Save).unwrap();
        assert!(app.handle_intent(DiagnosticsIntent::Refresh).is_none());
        let cancelled = app
            .handle_intent(DiagnosticsIntent::SaveDestination {
                request: choosing.save_request().unwrap(),
                path: None,
            })
            .unwrap();
        assert!(cancelled.presentation().error().is_none());
        assert!(cancelled.presentation().can_export());
        let choosing = app.handle_intent(DiagnosticsIntent::Save).unwrap();
        let saving = app
            .handle_intent(DiagnosticsIntent::SaveDestination {
                request: choosing.save_request().unwrap(),
                path: Some(PathBuf::from("report.txt")),
            })
            .unwrap();
        assert_eq!(
            Some(saving.save_operation().unwrap().text()),
            copy.clipboard_text()
        );
        assert!(saving.presentation().saving());
        let failed = app
            .complete_save(
                saving.save_operation().unwrap(),
                Err(DiagnosticsError::save_failed()),
            )
            .unwrap();
        assert!(failed.presentation().can_export());
        assert!(failed.presentation().error().is_some());
        assert_eq!(failed.presentation().report_text(), copy.clipboard_text());
    }

    #[test]
    fn failed_refresh_keeps_the_previous_report_available_for_export() {
        let mut app = DiagnosticsApplication::default();
        let loaded = ready(&mut app);
        let refresh = app.handle_intent(DiagnosticsIntent::Refresh).unwrap();
        let failed = app
            .complete_read(
                refresh.read_operation().unwrap(),
                Err(DiagnosticsError::collection_stopped()),
            )
            .unwrap();
        assert!(failed.presentation().can_export());
        assert!(failed.presentation().error().is_some());
        assert_eq!(
            failed.presentation().report_text(),
            loaded.presentation().report_text()
        );
    }

    #[test]
    fn cancelled_file_dialog_cannot_save_after_reopening() {
        let mut app = DiagnosticsApplication::default();
        ready(&mut app);
        let choosing = app.handle_intent(DiagnosticsIntent::Save).unwrap();
        app.handle_intent(DiagnosticsIntent::Close).unwrap();
        ready(&mut app);
        assert!(app
            .handle_intent(DiagnosticsIntent::SaveDestination {
                request: choosing.save_request().unwrap(),
                path: Some(PathBuf::from("obsolete.txt")),
            })
            .is_none());
    }

    #[test]
    fn export_replaces_a_report_atomically_and_preserves_destination_on_failure() {
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-diagnostics-export-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let destination = root.join("report.txt");
        fs::write(&destination, "previous report").unwrap();
        save_report(&destination, report("safe report").text()).unwrap();
        assert_eq!(
            fs::read_to_string(&destination).unwrap(),
            report("safe report").text()
        );
        let directory = root.join("directory");
        fs::create_dir(&directory).unwrap();
        assert!(save_report(&directory, "replacement").is_err());
        assert!(directory.is_dir());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }
}
