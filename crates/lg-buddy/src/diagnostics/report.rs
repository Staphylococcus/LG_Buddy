//! Report sections, rendering, bounds, and redaction. No host reads.

pub(super) const MAX_SECTION_BYTES: usize = 8 * 1024;
const MAX_REPORT_BYTES: usize = 32 * 1024;
const MAX_SESSION_FINDINGS: usize = 24;
pub(super) const MAX_SESSION_FINDING_BYTES: usize = 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    Snapshot,
    Logs,
}

/// One bounded, sanitized diagnostics section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSection {
    title: &'static str,
    body: String,
    kind: SectionKind,
}

impl DiagnosticSection {
    pub fn new(title: &'static str, body: impl Into<String>) -> Self {
        Self {
            title,
            body: safe_text(&body.into(), MAX_SECTION_BYTES),
            kind: SectionKind::Snapshot,
        }
    }

    pub(super) fn log(title: &'static str, body: impl Into<String>) -> Self {
        Self {
            kind: SectionKind::Logs,
            ..Self::new(title, body)
        }
    }

    pub fn title(&self) -> &'static str {
        self.title
    }

    pub fn body(&self) -> &str {
        &self.body
    }
}

/// A complete diagnostics snapshot ready for display, copying, or saving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsReport {
    text: String,
    collected_at: String,
    sections: Vec<DiagnosticSection>,
}

impl DiagnosticsReport {
    pub fn new(collected_at_unix_seconds: u64, sections: Vec<DiagnosticSection>) -> Self {
        let collected_at = format_utc_timestamp(collected_at_unix_seconds);
        let text = render_report(&collected_at, &sections);
        Self {
            text,
            collected_at,
            sections,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn collected_at(&self) -> &str {
        &self.collected_at
    }

    pub fn sections(&self) -> &[DiagnosticSection] {
        &self.sections
    }

    /// Add findings owned by the GUI session without allowing them to become
    /// an unbounded or credential-bearing extension of the report.
    pub fn with_session_findings(mut self, findings: &[String]) -> Self {
        if findings.is_empty() {
            return self;
        }

        let mut body = String::new();
        for finding in findings.iter().take(MAX_SESSION_FINDINGS) {
            let finding = safe_text(finding, MAX_SESSION_FINDING_BYTES);
            if finding.is_empty() {
                continue;
            }
            body.push_str("- ");
            body.push_str(&finding);
            body.push('\n');
        }
        if body.is_empty() {
            return self;
        }

        self.sections
            .push(DiagnosticSection::log("GUI session events", body));
        self.text = render_report(&self.collected_at, &self.sections);
        self
    }
}
pub(super) fn safe_text(input: &str, max_bytes: usize) -> String {
    let mut result = String::new();
    for line in input.lines() {
        let line = if contains_secret_marker(line) {
            "[Credential-bearing output omitted]".to_string()
        } else {
            let sanitized = line
                .chars()
                .map(|character| {
                    if character == '\t' || character == ' ' || !character.is_control() {
                        character
                    } else {
                        ' '
                    }
                })
                .filter(|character| !matches!(*character, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
                .collect::<String>();
            redact_urls(&sanitized)
        };
        if line.is_empty() && result.is_empty() {
            continue;
        }
        result.push_str(&line);
        result.push('\n');
        if result.len() > max_bytes {
            break;
        }
    }
    truncate_string(result, max_bytes)
}

fn redact_urls(line: &str) -> String {
    if !line.contains("://") {
        return line.into();
    }
    line.split_whitespace()
        .map(|part| {
            if part.contains("://") {
                "[URL omitted]"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_secret_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "password",
        "passwd",
        "token",
        "client-key",
        "client_key",
        "secret",
        "cookie",
        "authorization:",
        "bearer ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn truncate_string(mut value: String, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value;
    }
    const MARKER: &str = "\n[Output truncated]";
    if max_bytes <= MARKER.len() {
        value.truncate(max_bytes);
        return value;
    }
    let mut boundary = max_bytes - MARKER.len();
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value.push_str(MARKER);
    value
}

fn render_report(collected_at: &str, sections: &[DiagnosticSection]) -> String {
    let mut text = String::new();
    text.push_str("LG Buddy diagnostics\n");
    text.push_str("Collected at: ");
    text.push_str(collected_at);
    text.push_str("\n\n");
    for (kind, heading) in [
        (SectionKind::Snapshot, "Current snapshot"),
        (SectionKind::Logs, "Recent logs"),
    ] {
        if !sections.iter().any(|section| section.kind == kind) {
            continue;
        }
        text.push_str(heading);
        text.push_str("\n================\n\n");
        for section in sections.iter().filter(|section| section.kind == kind) {
            text.push_str(section.title());
            text.push_str(":\n");
            text.push_str(section.body());
            if !section.body().ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
        }
    }
    truncate_string(text, MAX_REPORT_BYTES)
}
pub(super) fn format_utc_timestamp(seconds: u64) -> String {
    const SECONDS_PER_DAY: u64 = 86_400;
    let days = seconds / SECONDS_PER_DAY;
    let day_seconds = seconds % SECONDS_PER_DAY;

    // Gregorian civil date conversion, using only integer arithmetic so
    // diagnostics does not need a date/time dependency.
    let z = days as i128 + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = month_part + if month_part < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };

    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_is_bounded_and_session_findings_are_sanitized() {
        let report = DiagnosticsReport::new(
            123,
            vec![DiagnosticSection::new(
                "Test",
                format!("token=secret\n{}", "x".repeat(MAX_SECTION_BYTES + 100)),
            )],
        )
        .with_session_findings(&["permission denied".to_string(), "bearer abc".to_string()]);

        assert_eq!(report.collected_at(), "1970-01-01T00:02:03Z");
        assert!(report.text().len() <= MAX_REPORT_BYTES);
        assert!(!report.text().contains("secret"));
        assert!(!report.text().contains("bearer abc"));
        assert!(report.text().contains("Credential-bearing output omitted"));
    }
    #[test]
    fn safe_text_replaces_controls_and_credential_lines() {
        let text = safe_text("ok\npassword=hunter2\n\u{202e}evil", 1024);
        assert!(text.contains("ok"));
        assert!(text.contains("Credential-bearing output omitted"));
        assert!(!text.contains("hunter2"));
        assert!(!text.contains('\u{202e}'));
    }
    #[test]
    fn report_timestamp_is_human_readable_utc() {
        assert_eq!(format_utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc_timestamp(86_400), "1970-01-02T00:00:00Z");
        assert_eq!(format_utc_timestamp(1_609_459_200), "2021-01-01T00:00:00Z");
    }

    #[test]
    fn report_orders_snapshot_before_logs_and_keeps_session_events_in_logs() {
        let report = DiagnosticsReport::new(
            0,
            vec![
                DiagnosticSection::log("Journal", "past service action"),
                DiagnosticSection::new("Snapshot", "current state"),
            ],
        )
        .with_session_findings(&["past GUI action".into()]);
        let (snapshot, logs) = report.text().split_once("Recent logs").unwrap();
        assert!(snapshot.contains("Current snapshot"));
        assert!(snapshot.contains("current state"));
        assert!(!snapshot.contains("past"));
        assert!(logs.contains("past service action"));
        assert!(logs.contains("past GUI action"));
    }

    #[test]
    fn section_and_report_limits_handle_multibyte_text() {
        let sections = (0..10)
            .map(|_| DiagnosticSection::new("Large", "界".repeat(MAX_SECTION_BYTES)))
            .collect::<Vec<_>>();
        assert!(sections
            .iter()
            .all(|section| section.body().len() <= MAX_SECTION_BYTES));
        let report = DiagnosticsReport::new(0, sections);
        assert!(report.text().len() <= MAX_REPORT_BYTES);
        assert!(report.text().ends_with("[Output truncated]"));
    }
}
