//! Bounded current-boot service logs, kept separate from current state.

use super::command::{command_path, run_bounded, CommandResult, COMMAND_TIMEOUT};
use super::report::{
    format_utc_timestamp, safe_text, MAX_SECTION_BYTES, MAX_SESSION_FINDING_BYTES,
};
use super::services::{ServiceScope, KWIN_UNIT, LIFECYCLE_UNIT, SCREEN_UNIT, UPDATE_TIMER};
use super::DiagnosticSection;

const MAX_LOG_ENTRIES: usize = 40;

pub(super) fn collect() -> Vec<DiagnosticSection> {
    let journalctl = command_path("LG_BUDDY_JOURNALCTL", "journalctl");
    collect_with(|args| run_bounded(&journalctl, args, COMMAND_TIMEOUT))
}

fn collect_with(mut run: impl FnMut(&[&str]) -> CommandResult) -> Vec<DiagnosticSection> {
    let mut sections = Vec::new();
    for (scope, units) in [
        (
            ServiceScope::User,
            [
                SCREEN_UNIT,
                KWIN_UNIT,
                UPDATE_TIMER,
                "LG_Buddy_update_check.service",
            ]
            .as_slice(),
        ),
        (
            ServiceScope::System,
            [LIFECYCLE_UNIT, "LG_Buddy.service"].as_slice(),
        ),
    ] {
        let mut args = Vec::new();
        if scope == ServiceScope::User {
            args.push("--user");
        }
        for unit in units {
            args.extend(["-u", unit]);
        }
        args.extend([
            "--boot",
            "--no-pager",
            "--quiet",
            "--lines=40",
            "--reverse",
            // Keep long fields intact for our own redaction and size limits.
            "--all",
            "--output=json",
            "--output-fields=__REALTIME_TIMESTAMP,_SYSTEMD_USER_UNIT,_SYSTEMD_UNIT,USER_UNIT,UNIT,MESSAGE",
        ]);
        let result = run(&args);
        let body = if result.timed_out {
            "Log read timed out".into()
        } else if result.unavailable || !(result.succeeded() || result.stopped_at_output_limit()) {
            "Logs unavailable".into()
        } else {
            let mut body = journal_entries(&result.stdout, units);
            if result.truncated {
                // Reserve room so the capture limit remains visible even when
                // the rendered entries also fill the section's budget.
                const MARKER: &str = "[Log capture reached 16 KiB]\n";
                body = safe_text(&body, MAX_SECTION_BYTES - MARKER.len() - 1);
                if !body.ends_with('\n') {
                    body.push('\n');
                }
                body.push_str(MARKER);
            }
            body
        };
        sections.push(DiagnosticSection::log(
            match scope {
                ServiceScope::User => "User services (current boot, latest 40 entries)",
                ServiceScope::System => "System services (current boot, latest 40 entries)",
            },
            body,
        ));
    }
    sections
}

fn journal_entries(output: &[u8], units: &[&str]) -> String {
    let mut body = String::new();
    let mut count = 0;
    for line in String::from_utf8_lossy(output).lines() {
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(unit) = entry
            .get("USER_UNIT")
            .or_else(|| entry.get("UNIT"))
            .or_else(|| entry.get("_SYSTEMD_USER_UNIT"))
            .or_else(|| entry.get("_SYSTEMD_UNIT"))
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        if !units.contains(&unit) {
            continue;
        }
        let Some(message) = entry.get("MESSAGE").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(timestamp) = entry
            .get("__REALTIME_TIMESTAMP")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<u64>().ok())
        else {
            continue;
        };
        body.push_str(&format!(
            "{} {unit}\n",
            format_utc_timestamp(timestamp / 1_000_000)
        ));
        // Redact before truncation, so a marker near the end of a long line
        // cannot be lost before its credential-bearing content is examined.
        for line in safe_text(message, MAX_SESSION_FINDING_BYTES).lines() {
            body.push_str("  ");
            body.push_str(line);
            body.push('\n');
        }
        count += 1;
        if count == MAX_LOG_ENTRIES || body.len() >= MAX_SECTION_BYTES {
            break;
        }
    }
    if body.is_empty() {
        body.push_str("No entries available\n");
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn journal_entries_keep_timestamps_units_and_messages_but_redact_credentials() {
        let entry = |unit: &str, message: &str| {
            serde_json::json!({
                "__REALTIME_TIMESTAMP": "1000000",
                "_SYSTEMD_USER_UNIT": unit,
                "MESSAGE": message,
            })
            .to_string()
        };
        let input = [
            entry(SCREEN_UNIT, "Screen blank command succeeded."),
            entry(
                KWIN_UNIT,
                "load failed\nclient-key=private-value\nhttps://private.example/a",
            ),
            entry("unrelated.service", "unrelated-message"),
            // Service manager records carry UNIT rather than the process's unit.
            serde_json::json!({"__REALTIME_TIMESTAMP": "2000000", "USER_UNIT": SCREEN_UNIT,
                "_SYSTEMD_UNIT": "user@1000.service", "MESSAGE": "Started screen monitor."})
            .to_string(),
        ]
        .join("\n");
        let text = journal_entries(input.as_bytes(), &[SCREEN_UNIT, KWIN_UNIT]);
        assert!(text.contains("1970-01-01T00:00:01Z LG_Buddy_screen.service"));
        assert!(text.contains("Screen blank command succeeded."));
        assert!(text.contains("load failed"));
        assert!(text.contains("Started screen monitor."));
        assert!(!text.contains("private-value"));
        assert!(!text.contains("private.example"));
        assert!(!text.contains("unrelated-message"));
        assert!(text.contains("[Credential-bearing output omitted]"));
        let repeated = input.repeat(100);
        let section = DiagnosticSection::log(
            "Logs",
            journal_entries(repeated.as_bytes(), &[SCREEN_UNIT, KWIN_UNIT]),
        );
        assert!(section.body().len() <= MAX_SECTION_BYTES);
    }

    #[cfg(unix)]
    fn result(status: i32, stdout: Vec<u8>) -> CommandResult {
        use std::os::unix::process::ExitStatusExt;
        CommandResult {
            status: Some(std::process::ExitStatus::from_raw(status)),
            stdout,
            ..CommandResult::default()
        }
    }

    #[cfg(unix)]
    #[test]
    fn getters_scope_and_bound_each_journal_without_spawning_commands() {
        let mut calls = Vec::new();
        let sections = collect_with(|args| {
            calls.push(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
            for required in [
                "--boot",
                "--lines=40",
                "--reverse",
                "--all",
                "--output=json",
            ] {
                assert!(args.contains(&required));
            }
            let unit = if args.contains(&"--user") {
                KWIN_UNIT
            } else {
                LIFECYCLE_UNIT
            };
            result(0, serde_json::json!({"__REALTIME_TIMESTAMP":"1000000", "UNIT":unit, "MESSAGE":"Observed."}).to_string().into_bytes())
        });
        assert_eq!(calls.len(), 2);
        let units = |args: &[String]| {
            args.windows(2)
                .filter(|pair| pair[0] == "-u")
                .map(|pair| pair[1].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            units(&calls[0]),
            [
                SCREEN_UNIT,
                KWIN_UNIT,
                UPDATE_TIMER,
                "LG_Buddy_update_check.service"
            ]
        );
        assert_eq!(units(&calls[1]), [LIFECYCLE_UNIT, "LG_Buddy.service"]);
        assert!(sections[0].body().contains(KWIN_UNIT));
        assert!(sections[1].body().contains(LIFECYCLE_UNIT));
    }

    #[cfg(unix)]
    #[test]
    fn long_messages_are_retained_with_local_truncation_and_redaction() {
        let message = format!("Detailed failure: {}", "x".repeat(5 * 1024));
        let sensitive = format!(
            "private context {} client-key=private-value",
            "x".repeat(5 * 1024)
        );
        let sections = collect_with(|args| {
            let unit = if args.contains(&"--user") {
                SCREEN_UNIT
            } else {
                LIFECYCLE_UNIT
            };
            let entries = [&message, &sensitive].map(|message| {
                // journalctl emits null for these oversized fields unless
                // --all is requested, before our renderer sees the message.
                let message = args.contains(&"--all").then_some(message);
                serde_json::json!({"__REALTIME_TIMESTAMP":"1000000", "UNIT":unit, "MESSAGE":message}).to_string()
            });
            result(0, entries.join("\n").into_bytes())
        });
        for section in sections {
            assert!(section.body().contains("Detailed failure:"));
            assert!(section.body().contains("[Output truncated]"));
            assert!(section
                .body()
                .contains("[Credential-bearing output omitted]"));
            assert!(!section.body().contains("private context"));
            assert!(!section.body().contains("private-value"));
            assert!(!section.body().contains("No entries available"));
            assert!(section.body().len() <= MAX_SECTION_BYTES);
        }
    }

    #[cfg(unix)]
    #[test]
    fn capture_limit_preserves_complete_entries_after_sigpipe() {
        use super::super::command::MAX_COMMAND_BYTES;
        let entry = serde_json::json!({"__REALTIME_TIMESTAMP":"1000000", "UNIT":SCREEN_UNIT, "MESSAGE":"Captured before the limit."}).to_string() + "\n";
        let mut output = entry
            .repeat(MAX_COMMAND_BYTES / entry.len() + 1)
            .into_bytes();
        output.truncate(MAX_COMMAND_BYTES); // Last JSON record is incomplete.
        let sections = collect_with(|_| CommandResult {
            truncated: true,
            ..result(libc::SIGPIPE, output.clone())
        });
        assert!(sections[0].body().contains("Captured before the limit."));
        assert!(sections[0].body().contains("[Log capture reached 16 KiB]"));
        assert!(!sections[0].body().contains("Logs unavailable"));
        assert!(sections[0].body().len() <= MAX_SECTION_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn capture_marker_survives_the_rendered_section_limit() {
        let entry = serde_json::json!({"__REALTIME_TIMESTAMP":"1000000", "UNIT":SCREEN_UNIT, "MESSAGE":"x".repeat(900)}).to_string() + "\n";
        let sections = collect_with(|_| CommandResult {
            truncated: true,
            ..result(0, entry.repeat(12).into_bytes())
        });
        assert!(sections[0]
            .body()
            .ends_with("[Log capture reached 16 KiB]\n"));
        assert!(sections[0].body().len() <= MAX_SECTION_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn command_failures_are_not_mistaken_for_capture_truncation() {
        for status in [1 << 8, libc::SIGTERM] {
            let sections = collect_with(|_| CommandResult {
                truncated: true,
                ..result(status, Vec::new())
            });
            assert!(sections
                .iter()
                .all(|section| section.body() == "Logs unavailable\n"));
        }
        let sections = collect_with(|_| result(libc::SIGPIPE, Vec::new()));
        assert!(sections[0].body().contains("Logs unavailable"));
        let sections = collect_with(|_| CommandResult {
            timed_out: true,
            ..CommandResult::default()
        });
        assert!(sections[0].body().contains("Log read timed out"));
        let sections = collect_with(|_| CommandResult {
            unavailable: true,
            ..CommandResult::default()
        });
        assert!(sections[0].body().contains("Logs unavailable"));
        let sections = collect_with(|_| result(0, Vec::new()));
        assert!(sections[0].body().contains("No entries available"));
    }
}
