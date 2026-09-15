//! Current systemd service and timer state.

use super::command::{command_path, run_bounded, CommandResult, COMMAND_TIMEOUT};
use super::DiagnosticSection;

pub(super) const SCREEN_UNIT: &str = "LG_Buddy_screen.service";
pub(super) const UPDATE_TIMER: &str = "LG_Buddy_update_check.timer";
pub(super) const LIFECYCLE_UNIT: &str = "LG_Buddy_lifecycle.service";
pub(super) const KWIN_UNIT: &str = "LG_Buddy_kwin.service";

pub(super) fn collect() -> DiagnosticSection {
    let systemctl = command_path("LG_BUDDY_SYSTEMCTL", "systemctl");
    collect_with(|args| run_bounded(&systemctl, args, COMMAND_TIMEOUT))
}

fn collect_with(mut run: impl FnMut(&[&str]) -> CommandResult) -> DiagnosticSection {
    let mut body = String::new();
    for (scope, unit) in [
        (ServiceScope::User, SCREEN_UNIT),
        (ServiceScope::User, KWIN_UNIT),
        (ServiceScope::User, UPDATE_TIMER),
        (ServiceScope::User, "LG_Buddy_update_check.service"),
        (ServiceScope::System, "LG_Buddy.service"),
        (ServiceScope::System, LIFECYCLE_UNIT),
    ] {
        let observation = systemd_unit_observation(&mut run, scope, unit);
        body.push_str(&format!("{} {unit}: {}", scope.label(), observation.text));
        if observation.action_needed && unit != KWIN_UNIT {
            body.push_str(" [needs attention]");
        }
        body.push('\n');
    }
    DiagnosticSection::new("Services", body)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServiceScope {
    User,
    System,
}

impl ServiceScope {
    fn label(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::System => "system",
        }
    }
}

#[derive(Debug)]
struct UnitObservation {
    text: String,
    action_needed: bool,
}

fn systemd_unit_observation(
    run: &mut impl FnMut(&[&str]) -> CommandResult,
    scope: ServiceScope,
    unit: &str,
) -> UnitObservation {
    let mut args = Vec::new();
    if scope == ServiceScope::User {
        args.push("--user");
    }
    args.extend([
        "show",
        "--no-pager",
        "--property=LoadState,ActiveState,SubState,UnitFileState",
        unit,
    ]);

    let result = run(&args);
    if result.timed_out {
        return UnitObservation {
            text: "inspection unavailable (timed out)".to_string(),
            action_needed: false,
        };
    }
    if result.unavailable {
        return UnitObservation {
            text: "inspection unavailable (systemctl not found)".to_string(),
            action_needed: false,
        };
    }

    let values = parse_unit_properties(&result.stdout);
    if values.is_empty() {
        return UnitObservation {
            text: "inspection unavailable (unit state was not reported)".to_string(),
            action_needed: false,
        };
    }
    let load = allowlisted_unit_value(values.get("LoadState").map(String::as_str), "unknown");
    let active = allowlisted_unit_value(values.get("ActiveState").map(String::as_str), "unknown");
    let substate = allowlisted_unit_value(values.get("SubState").map(String::as_str), "unknown");
    let enabled =
        allowlisted_unit_value(values.get("UnitFileState").map(String::as_str), "unknown");
    let action_needed = matches!(load, "not-found" | "error" | "bad")
        || matches!(active, "failed")
        || matches!(enabled, "masked");
    UnitObservation {
        text: format!("load={load}, active={active}, substate={substate}, enabled={enabled}"),
        action_needed,
    }
}

fn parse_unit_properties(output: &[u8]) -> std::collections::BTreeMap<String, String> {
    let output = String::from_utf8_lossy(output);
    output
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| {
            matches!(
                *key,
                "LoadState" | "ActiveState" | "SubState" | "UnitFileState"
            )
        })
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn allowlisted_unit_value(value: Option<&str>, unknown: &'static str) -> &'static str {
    match value {
        Some("loaded") => "loaded",
        Some("not-found") => "not-found",
        Some("bad") => "bad",
        Some("active") => "active",
        Some("inactive") => "inactive",
        Some("failed") => "failed",
        Some("activating") => "activating",
        Some("deactivating") => "deactivating",
        Some("reloading") => "reloading",
        Some("running") => "running",
        Some("dead") => "dead",
        Some("exited") => "exited",
        Some("waiting") => "waiting",
        Some("auto-restart") => "auto-restart",
        Some("start-pre") => "start-pre",
        Some("start") => "start",
        Some("start-post") => "start-post",
        Some("stop") => "stop",
        Some("stop-sigterm") => "stop-sigterm",
        Some("stop-post") => "stop-post",
        Some("final-sigterm") => "final-sigterm",
        Some("listening") => "listening",
        Some("elapsed") => "elapsed",
        Some("plugged") => "plugged",
        Some("mounted") => "mounted",
        Some("condition") => "condition",
        Some("enabled") => "enabled",
        Some("disabled") => "disabled",
        Some("masked") => "masked",
        Some("static") => "static",
        Some("indirect") => "indirect",
        Some("generated") => "generated",
        Some("transient") => "transient",
        Some("linked") => "linked",
        Some(_) | None => unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unit_parser_keeps_only_typed_state_fields() {
        let values = parse_unit_properties(
            b"LoadState=loaded\nActiveState=failed\nSubState=dead\nUnitFileState=enabled\nMESSAGE=token=secret\n",
        );
        assert_eq!(values.get("LoadState").map(String::as_str), Some("loaded"));
        assert_eq!(
            values.get("ActiveState").map(String::as_str),
            Some("failed")
        );
        assert!(!values.contains_key("MESSAGE"));
        assert_eq!(
            allowlisted_unit_value(values.get("UnitFileState").map(String::as_str), "unknown"),
            "enabled"
        );
    }

    #[test]
    fn getter_reads_each_unit_in_its_scope_and_preserves_state_dimensions() {
        let mut calls = Vec::new();
        let section = collect_with(|args| {
            calls.push(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
            assert!(args.contains(&"show"));
            assert!(args.contains(&"--property=LoadState,ActiveState,SubState,UnitFileState"));
            let state = match *args.last().unwrap() {
                SCREEN_UNIT => "LoadState=loaded\nActiveState=active\nSubState=running\nUnitFileState=disabled\n",
                KWIN_UNIT => "LoadState=not-found\nActiveState=inactive\nSubState=dead\n",
                UPDATE_TIMER => "LoadState=loaded\nActiveState=active\nSubState=waiting\nUnitFileState=enabled\n",
                "LG_Buddy_update_check.service" => "LoadState=loaded\nActiveState=inactive\nSubState=dead\nUnitFileState=static\n",
                _ => "LoadState=loaded\nActiveState=failed\nSubState=dead\nUnitFileState=enabled\n",
            };
            CommandResult {
                stdout: state.as_bytes().to_vec(),
                ..CommandResult::default()
            }
        });
        assert_eq!(calls.len(), 6);
        assert_eq!(
            calls
                .iter()
                .map(|args| args.last().unwrap().as_str())
                .collect::<Vec<_>>(),
            [
                SCREEN_UNIT,
                KWIN_UNIT,
                UPDATE_TIMER,
                "LG_Buddy_update_check.service",
                "LG_Buddy.service",
                LIFECYCLE_UNIT
            ]
        );
        assert!(calls[..4].iter().all(|args| args[0] == "--user"));
        assert!(calls[4..]
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "--user")));
        assert!(section
            .body()
            .contains("active=active, substate=running, enabled=disabled\n"));
        assert!(section
            .body()
            .contains("active=inactive, substate=dead, enabled=static\n"));
        let kwin = section
            .body()
            .lines()
            .find(|line| line.contains(KWIN_UNIT))
            .unwrap();
        assert!(!kwin.contains("needs attention"));
        assert_eq!(section.body().matches("needs attention").count(), 2);
    }

    #[test]
    fn getter_reports_missing_command_timeout_and_missing_properties() {
        for (result, expected) in [
            (
                CommandResult {
                    unavailable: true,
                    ..CommandResult::default()
                },
                "systemctl not found",
            ),
            (
                CommandResult {
                    timed_out: true,
                    ..CommandResult::default()
                },
                "timed out",
            ),
            (CommandResult::default(), "unit state was not reported"),
        ] {
            let mut result = Some(result);
            let observation = systemd_unit_observation(
                &mut |_| result.take().unwrap(),
                ServiceScope::User,
                SCREEN_UNIT,
            );
            assert!(observation.text.contains(expected));
            assert!(!observation.action_needed);
        }
    }
}
