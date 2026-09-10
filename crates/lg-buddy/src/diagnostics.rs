//! On-demand, read-only diagnostics for the graphical application.
//!
//! This module deliberately produces a small, typed snapshot.  It does not
//! read raw configuration or journal messages into the report, and it never
//! starts a service, changes a setting, or attempts TV pairing.

use std::collections::BTreeSet;
use std::env;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::ScreenBackend;
use crate::settings::{ConfigPathResolver, SettingValue, SettingsStore};
use crate::tvs::{EnvironmentTvsBackend, TvCredentialState, TvsBackend};
use crate::version::VersionInfo;

const MAX_SECTION_BYTES: usize = 8 * 1024;
const MAX_REPORT_BYTES: usize = 32 * 1024;
const MAX_COMMAND_BYTES: usize = 16 * 1024;
const MAX_SESSION_FINDINGS: usize = 24;
const MAX_SESSION_FINDING_BYTES: usize = 1024;
const MAX_TV_MODEL_BYTES: usize = 512;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);

const SCREEN_UNIT: &str = "LG_Buddy_screen.service";
const UPDATE_TIMER: &str = "LG_Buddy_update_check.timer";
const LIFECYCLE_UNIT: &str = "LG_Buddy_lifecycle.service";

/// One bounded, sanitized diagnostics section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticSection {
    title: &'static str,
    body: String,
}

impl DiagnosticSection {
    pub fn new(title: &'static str, body: impl Into<String>) -> Self {
        Self {
            title,
            body: safe_text(&body.into(), MAX_SECTION_BYTES),
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
            .push(DiagnosticSection::new("Session findings", body));
        self.text = render_report(&self.collected_at, &self.sections);
        self
    }
}

/// Production diagnostics collector.  Collection is synchronous so the GUI
/// can put it behind its existing worker boundary.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EnvironmentDiagnosticsCollector;

impl EnvironmentDiagnosticsCollector {
    /// Collect a useful partial report even when individual host interfaces
    /// are unavailable. External command probes have a finite timeout; the
    /// backend section uses conservative session observations rather than
    /// calling an unbounded compositor/DBus probe.
    pub fn collect(&self) -> DiagnosticsReport {
        let collected_at = current_unix_seconds();
        DiagnosticsReport::new(
            collected_at,
            vec![
                application_section(),
                settings_section(),
                backend_section(),
                services_section(),
                tv_section(),
                journal_section(),
                recovery_section(),
            ],
        )
    }
}

fn application_section() -> DiagnosticSection {
    let version = VersionInfo::current();
    let mut body = String::new();
    body.push_str("version: ");
    body.push_str(version.version());
    body.push('\n');
    body.push_str("channel: ");
    body.push_str(version.channel().as_str());
    body.push('\n');
    body.push_str("commit: ");
    body.push_str(version.commit().unwrap_or("unknown"));
    body.push('\n');
    body.push_str("target OS: ");
    body.push_str(env::consts::OS);
    body.push('\n');
    body.push_str("target architecture: ");
    body.push_str(env::consts::ARCH);
    body.push('\n');
    DiagnosticSection::new("Application and build", body)
}

fn settings_section() -> DiagnosticSection {
    let path = match ConfigPathResolver::resolve_from_env() {
        Ok(path) => path,
        Err(_) => {
            return DiagnosticSection::new(
                "Effective settings",
                "Settings are unavailable: no configuration path could be resolved.\nAction: configure a TV, then retry diagnostics.",
            )
        }
    };

    let store = match SettingsStore::load(&path) {
        Ok(store) => store,
        Err(_) => {
            return DiagnosticSection::new(
                "Effective settings",
                "Settings are unavailable: the configuration could not be read.\nAction: check the configuration access and retry diagnostics.",
            )
        }
    };

    settings_section_from_store(&store)
}

fn settings_section_from_store(store: &SettingsStore) -> DiagnosticSection {
    let mut body = String::new();
    body.push_str("configuration: resolved and readable\n");
    let mut invalid = false;
    for setting in store.all_effective() {
        body.push_str(setting.key_name());
        body.push_str(" = ");
        match (setting.value(), setting.invalid_value()) {
            (_, Some(_)) => {
                invalid = true;
                body.push_str("invalid (raw value omitted; expected ");
                body.push_str(&setting.definition().value_type().expected());
                body.push(')');
            }
            (Some(value), None) => body.push_str(&safe_setting_value(value)),
            (None, None) => body.push_str("missing"),
        }
        body.push_str(" [source: ");
        body.push_str(setting.source().as_str());
        body.push_str("]\n");
    }
    if setting_value(store, "screen.idle_blank") == Some("disabled") {
        body.push_str("Finding: screen.idle_blank is intentionally disabled; screen blanking is not expected.\n");
    }
    if setting_value(store, "system.sleep_wake_policy") == Some("disabled") {
        body.push_str("Finding: system.sleep_wake_policy is intentionally disabled; TV sleep/wake control is not expected.\n");
    }
    if invalid {
        body.push_str("Finding: one or more settings are invalid; behavior using them is not confirmed.\nAction: change the affected setting to an accepted value in Settings.\n");
    }
    DiagnosticSection::new("Effective settings", body)
}

fn backend_section() -> DiagnosticSection {
    let session = match env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => "wayland",
        Ok("x11") => "x11",
        Ok(_) => "unknown session type",
        Err(_) => "session type unavailable",
    };
    let mut body = format!("session type: {session}\n");

    let store = ConfigPathResolver::resolve_from_env()
        .ok()
        .and_then(|path| SettingsStore::load(path).ok());
    let override_value = env::var("LG_BUDDY_SCREEN_BACKEND").ok();
    match diagnostic_configured_backend(override_value.as_deref(), store.as_ref()) {
        Ok(configured) => {
            body.push_str("configured backend: ");
            body.push_str(configured.as_str());
            body.push('\n');
            body.push_str(&conservative_backend_observation(configured, session));
        }
        Err(error) => {
            body.push_str("configured backend: invalid or unavailable\n");
            body.push_str("configuration finding: ");
            body.push_str(error);
            body.push('\n');
            body.push_str("Action: choose auto, gnome, wayland, or swayidle in Settings.\n");
        }
    }
    DiagnosticSection::new("Desktop capability and fallback", body)
}

fn diagnostic_configured_backend(
    override_value: Option<&str>,
    store: Option<&SettingsStore>,
) -> Result<ScreenBackend, &'static str> {
    if let Some(value) = override_value {
        return value
            .parse()
            .map_err(|_| "invalid backend override (raw value omitted)");
    }
    let store = store.ok_or("screen.backend could not be read")?;
    setting_value(store, "screen.backend")
        .and_then(|value| value.parse().ok())
        .ok_or("screen.backend is invalid (raw value omitted)")
}

fn conservative_backend_observation(configured: ScreenBackend, session: &str) -> String {
    let mut body = String::new();
    body.push_str("swayidle fallback command: ");
    body.push_str(if command_available("swayidle") {
        "available in PATH\n"
    } else {
        "not found in PATH\n"
    });

    body.push_str("GNOME session interfaces: ");
    let gnome = gnome_interface_observation();
    body.push_str(&gnome);

    match configured {
        ScreenBackend::Wayland => {
            if session == "wayland" {
                body.push_str("capability observation: Wayland session matches the configured backend; compositor idle capability was not probed.\n");
            } else {
                body.push_str("capability observation: configured Wayland backend does not match the current session type.\n");
            }
        }
        ScreenBackend::Gnome => {
            body.push_str("capability observation: GNOME session interfaces above are the bounded availability check; this does not prove a running screen service is using GNOME.\n");
        }
        ScreenBackend::Swayidle => {
            if !command_available("swayidle") {
                body.push_str("capability finding: swayidle command was not found in PATH.\nAction: install swayidle or choose auto, gnome, or wayland in Settings, then use Retry apply.\n");
            }
        }
        ScreenBackend::Auto => {
            body.push_str("capability observation: automatic selection is configured; GNOME names and swayidle availability above are bounded observations. Native Wayland registry probing was omitted because its roundtrip is not bounded and may consume an inherited socket.\n");
        }
    }
    body.push_str("These are current capability observations, not proof that a running service is using the backend.\n");
    body
}

fn gnome_interface_observation() -> String {
    let command = if command_available("gdbus") {
        Some("gdbus")
    } else if command_available("busctl") {
        Some("busctl")
    } else {
        None
    };
    let Some(command) = command else {
        return "unavailable (neither gdbus nor busctl is installed)\n".to_string();
    };

    let names = [
        ("GNOME Shell", "org.gnome.Shell"),
        ("GNOME ScreenSaver", "org.gnome.ScreenSaver"),
        ("GNOME IdleMonitor", "org.gnome.Mutter.IdleMonitor"),
    ];
    let mut body = format!("using {command}: ");
    for (index, (label, name)) in names.iter().enumerate() {
        if index > 0 {
            body.push_str(", ");
        }
        body.push_str(label);
        body.push('=');
        match gnome_name_has_owner(command, name) {
            Some(true) => body.push_str("owner-present"),
            Some(false) => body.push_str("no-owner"),
            None => body.push_str("unavailable"),
        }
    }
    body.push('\n');
    body
}

fn gnome_name_has_owner(command: &str, name: &str) -> Option<bool> {
    let args = if command == "gdbus" {
        vec![
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            name,
        ]
    } else {
        vec![
            "--user",
            "--no-pager",
            "--quiet",
            "call",
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            "s",
            name,
        ]
    };
    let result = run_bounded(&PathBuf::from(command), &args, COMMAND_TIMEOUT);
    if result.timed_out || result.unavailable || result.status != Some(0) {
        return None;
    }
    let output = String::from_utf8_lossy(&result.stdout).to_ascii_lowercase();
    if output.contains("true") {
        Some(true)
    } else if output.contains("false") {
        Some(false)
    } else {
        None
    }
}

fn services_section() -> DiagnosticSection {
    let systemctl = command_path("LG_BUDDY_SYSTEMCTL", "systemctl");
    let mut body = String::new();
    body.push_str("State fields are read independently: load, active, substate, and enabled.\n");
    let mut action_needed = false;
    let mut failed_units = Vec::new();
    let timer_expectation = match setting_is_enabled("updates.auto_check") {
        Some(true) => "expected enabled while updates.auto_check=enabled",
        Some(false) => "intentionally disabled by updates.auto_check=disabled",
        None => {
            "expected timer state is unavailable because updates.auto_check is invalid or missing"
        }
    };

    for (scope, unit, expectation) in [
        (
            ServiceScope::User,
            SCREEN_UNIT,
            "long-running monitor; active status is separate from the idle blanking policy",
        ),
        (ServiceScope::User, UPDATE_TIMER, timer_expectation),
        (
            ServiceScope::User,
            "LG_Buddy_update_check.service",
            "oneshot worker; inactive between timer fires is expected",
        ),
        (
            ServiceScope::System,
            "LG_Buddy.service",
            "startup/shutdown oneshot; inactive before a startup run is expected",
        ),
        (
            ServiceScope::System,
            LIFECYCLE_UNIT,
            "long-running sleep/wake monitor; active status is separate from the sleep/wake policy",
        ),
    ] {
        let observation = systemd_unit_observation(&systemctl, scope, unit);
        body.push_str(scope.label());
        body.push(' ');
        body.push_str(unit);
        body.push_str(": ");
        body.push_str(&observation.text);
        body.push_str(" (expectation: ");
        body.push_str(expectation);
        body.push(')');
        body.push('\n');
        if observation.action_needed {
            action_needed = true;
            failed_units.push(unit);
        }
    }
    if action_needed {
        for unit in failed_units {
            match unit {
                SCREEN_UNIT => body.push_str("Action for screen service: if screen blanking is wanted, enable screen.idle_blank in Settings and choose Retry apply after fixing the service.\n"),
                UPDATE_TIMER | "LG_Buddy_update_check.service" => body.push_str("Action for update checks: choose the updates.auto_check setting in Settings and choose Retry apply after fixing the timer or worker.\n"),
                LIFECYCLE_UNIT => body.push_str("Action for sleep/wake: if TV sleep/wake control is wanted, enable system.sleep_wake_policy in Settings and choose Retry apply after fixing the lifecycle service.\n"),
                "LG_Buddy.service" => body.push_str("Action for startup service: inspect the installed startup unit before relying on automatic startup/shutdown handling.\n"),
                _ => {}
            }
        }
    }
    DiagnosticSection::new("Relevant service and timer state", body)
}

fn setting_is_enabled(key: &str) -> Option<bool> {
    let path = ConfigPathResolver::resolve_from_env().ok()?;
    let store = SettingsStore::load(path).ok()?;
    match setting_value(&store, key) {
        Some("enabled") => Some(true),
        Some("disabled") => Some(false),
        _ => None,
    }
}

fn setting_value(store: &SettingsStore, key: &str) -> Option<&'static str> {
    store.effective_by_name(key).ok()?.value()?.as_enum()
}

fn tv_section() -> DiagnosticSection {
    let backend = EnvironmentTvsBackend;
    tv_section_from_backend(&backend)
}

fn tv_section_from_backend(backend: &impl TvsBackend) -> DiagnosticSection {
    let profiles = match backend.read_profiles() {
        Ok(profiles) => profiles,
        Err(_) => {
            return DiagnosticSection::new(
                "TV observation",
                "TV profile inspection is unavailable: saved configuration or local credential metadata could not be read.\nAction: check the saved TV settings, or unpair and pair again.\nNo credential or protocol data was collected.",
            )
        }
    };

    if profiles.is_empty() {
        return DiagnosticSection::new(
            "TV observation",
            "No TV profile is configured. Connectivity and runtime behavior were not tested.\nAction: pair a TV from the TVs page.",
        );
    }

    let mut body = String::new();
    for profile in profiles.iter().take(4) {
        let profile_id = profile.id().to_string();
        body.push_str("profile ");
        body.push_str(&safe_text(&profile_id, 128));
        body.push_str(": address=");
        body.push_str(&profile.address().to_string());
        body.push_str(", input=");
        body.push_str(profile.input_label());
        body.push_str(", platform=");
        body.push_str(profile.platform_label());
        body.push_str(", local credential state=");
        body.push_str(profile.credentials().label());
        body.push('\n');
        body.push_str("credential observation: ");
        body.push_str(credential_observation(profile.credentials()));
        body.push('\n');

        // The existing backend uses stored-credential-only authentication and a
        // three-second model-read timeout.  It never starts pairing here.
        match profile.credentials() {
            TvCredentialState::Stored | TvCredentialState::LocalFile => {
                match backend.read_model_name(profile) {
                    Ok(model) => {
                        body.push_str("credential-scoped model read: succeeded (model=");
                        body.push_str(&safe_text(&model, MAX_TV_MODEL_BYTES));
                        body.push_str(")\n");
                    }
                    Err(_) => body.push_str(
                        "credential-scoped model read: unavailable; connectivity or authentication was not confirmed\n",
                    ),
                }
            }
            _ => body.push_str(
                "credential-scoped model read: skipped because no usable local credential was observed\n",
            ),
        }
    }
    body.push_str("A reachable TV or successful model read does not prove screen automation or sleep/wake behavior.\n");
    DiagnosticSection::new("TV observation", body)
}

fn credential_observation(state: TvCredentialState) -> &'static str {
    match state {
        TvCredentialState::Stored => {
            "A native credential is stored locally; current TV access is not established."
        }
        TvCredentialState::Missing => "No local credential was found.",
        TvCredentialState::LocalFile => {
            "A compatibility credential file is present locally; authentication is not verified."
        }
        TvCredentialState::Malformed => {
            "A local credential is malformed; authentication is not verified."
        }
        TvCredentialState::Unreadable => {
            "A local credential could not be read; authentication is not verified."
        }
        TvCredentialState::Unknown => "The local credential state could not be determined.",
    }
}

fn journal_section() -> DiagnosticSection {
    let journalctl = command_path("LG_BUDDY_JOURNALCTL", "journalctl");
    let mut body = String::new();
    let mut any_available = false;
    let mut actions = BTreeSet::new();

    for (scope, units) in [
        (ServiceScope::User, [SCREEN_UNIT, UPDATE_TIMER]),
        (ServiceScope::System, [LIFECYCLE_UNIT, ""]),
    ] {
        let units = units.iter().copied().filter(|unit| !unit.is_empty());
        let mut args = Vec::new();
        if scope == ServiceScope::User {
            args.push("--user");
        }
        for unit in units {
            args.push("-u");
            args.push(unit);
        }
        args.extend(["--no-pager", "--quiet", "--lines=20", "--output=cat"]);

        match run_bounded(&journalctl, &args, COMMAND_TIMEOUT) {
            CommandResult {
                status: Some(_),
                stdout,
                timed_out: false,
                unavailable: false,
            } => {
                any_available = true;
                let classes = classify_journal(&stdout);
                body.push_str(scope.label());
                body.push_str(" journal (up to 20 entries; capture capped at 16 KiB): ");
                if stdout.is_empty() {
                    body.push_str("no accessible entries; the unit may have no history or journal access may be restricted");
                } else if classes.is_empty() {
                    body.push_str(
                        "no classified failure markers in the accessible captured entries",
                    );
                } else {
                    body.push_str("failure markers observed: ");
                    body.push_str(&classes.join(", "));
                    actions.insert("Use the affected setting's Retry apply action after fixing the reported failure.");
                }
                body.push('\n');
            }
            result if result.timed_out => {
                body.push_str(scope.label());
                body.push_str(" journal: unavailable (timed out)\n");
            }
            _ => {
                body.push_str(scope.label());
                body.push_str(" journal: unavailable (journal access failed)\n");
            }
        }
    }
    body.push_str(
        "Raw journal messages are omitted; only conservative failure classes are retained.\n",
    );
    if !any_available {
        body.push_str("Recent failure details could not be inspected in this session.\n");
    }
    for action in actions {
        body.push_str("Action: ");
        body.push_str(action);
        body.push('\n');
    }
    DiagnosticSection::new("Recent failure observations", body)
}

fn recovery_section() -> DiagnosticSection {
    DiagnosticSection::new(
        "Known recovery actions",
        "Invalid settings: change the affected value in Settings.\nScreen service failure: fix the service, enable screen.idle_blank if desired, then choose Retry apply.\nSleep/wake service failure: fix the lifecycle service, enable system.sleep_wake_policy if desired, then choose Retry apply.\nUpdate timer failure: fix the timer or worker, then choose Retry apply for updates.auto_check.\nTV authentication or saved-profile failure: unpair the TV and pair it again.\nDiagnostics never performs these recovery actions automatically.",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceScope {
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
    systemctl: &PathBuf,
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

    let result = run_bounded(systemctl, &args, COMMAND_TIMEOUT);
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

fn classify_journal(output: &[u8]) -> Vec<&'static str> {
    let text = String::from_utf8_lossy(output).to_ascii_lowercase();
    let mut classes = BTreeSet::new();
    for line in text.lines() {
        if line.contains("failed") || line.contains("error") || line.contains("panic") {
            classes.insert("error");
        }
        if line.contains("timeout") || line.contains("timed out") {
            classes.insert("timeout");
        }
        if line.contains("denied") || line.contains("permission") {
            classes.insert("permission");
        }
        if line.contains("refused") || line.contains("unreachable") {
            classes.insert("connectivity");
        }
        if line.contains("authentication") || line.contains("unauthorized") {
            classes.insert("authentication");
        }
    }
    classes.into_iter().collect()
}

fn command_path(variable: &str, fallback: &str) -> PathBuf {
    env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

#[derive(Debug)]
struct CommandResult {
    status: Option<i32>,
    stdout: Vec<u8>,
    timed_out: bool,
    unavailable: bool,
}

fn run_bounded(program: &PathBuf, args: &[&str], timeout: Duration) -> CommandResult {
    let mut child = match Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return CommandResult {
                status: None,
                stdout: Vec::new(),
                timed_out: false,
                unavailable: true,
            }
        }
    };

    let deadline = Instant::now() + timeout;
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = thread::spawn(move || read_bounded_stdout(stdout, deadline));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok().and_then(|status| status.code());
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // The reader uses the same absolute deadline as the child. In
    // particular, a descendant that inherited stdout cannot make this join
    // wait beyond the collection timeout.
    let stdout = reader.join().unwrap_or_default();
    CommandResult {
        status,
        stdout,
        timed_out,
        unavailable: false,
    }
}

fn read_bounded_stdout(mut stdout: ChildStdout, deadline: Instant) -> Vec<u8> {
    #[cfg(unix)]
    {
        let fd = stdout.as_raw_fd();
        // A nonblocking descriptor lets this reader honor the deadline even
        // after the direct child exits while a descendant retains the pipe.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Vec::new();
        }

        let mut output = Vec::new();
        let mut buffer = [0_u8; 1024];
        while output.len() < MAX_COMMAND_BYTES + 1 {
            match stdout.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let remaining = MAX_COMMAND_BYTES + 1 - output.len();
                    output.extend_from_slice(&buffer[..read.min(remaining)]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
        output
    }

    #[cfg(not(unix))]
    {
        // LG Buddy's supported hosts are Unix. Keep a capped fallback for
        // other targets; the child is still terminated by the parent deadline.
        let mut limited = stdout.take((MAX_COMMAND_BYTES + 1) as u64);
        let mut output = Vec::new();
        let _ = limited.read_to_end(&mut output);
        output
    }
}

fn safe_setting_value(value: SettingValue) -> String {
    // SettingValue can only contain typed, registry-validated values.  Keep
    // this helper explicit so future secret-bearing setting types cannot be
    // accidentally rendered by a broad Debug or raw-config formatter.
    match value {
        SettingValue::Enum(value) => value.to_string(),
        SettingValue::Integer(value) => value.to_string(),
        SettingValue::Ipv4(value) => value.to_string(),
        SettingValue::MacAddress(value) => value.to_string(),
    }
}

fn command_available(command: &str) -> bool {
    if command.contains(std::path::MAIN_SEPARATOR) {
        return std::path::Path::new(command).is_file();
    }
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|directory| directory.join(command).is_file()))
        .unwrap_or(false)
}

fn safe_text(input: &str, max_bytes: usize) -> String {
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
    for section in sections {
        text.push_str(section.title());
        text.push_str(":\n");
        text.push_str(section.body());
        if !section.body().ends_with('\n') {
            text.push('\n');
        }
        text.push('\n');
    }
    truncate_string(text, MAX_REPORT_BYTES)
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn format_utc_timestamp(seconds: u64) -> String {
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
    use crate::settings::{ConfigEnvReader, SettingsStore};
    use crate::tvs::{TvsBackend, TvsReadError};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

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
    fn journal_classification_does_not_return_messages() {
        let classes =
            classify_journal(b"failed: token=secret\nconnection refused\npermission denied\n");
        assert_eq!(classes, vec!["connectivity", "error", "permission"]);
        assert!(!classes.iter().any(|class| class.contains("secret")));
    }

    #[test]
    fn safe_text_replaces_controls_and_credential_lines() {
        let text = safe_text("ok\npassword=hunter2\n\u{202e}evil", 1024);
        assert!(text.contains("ok"));
        assert!(text.contains("Credential-bearing output omitted"));
        assert!(!text.contains("hunter2"));
        assert!(!text.contains('\u{202e}'));
    }

    #[cfg(unix)]
    fn fixture_script(name: &str, body: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lg-buddy-diagnostics-{name}-{}",
            std::process::id()
        ));
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("fixture script");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("script mode");
        path
    }

    #[cfg(unix)]
    #[test]
    fn bounded_command_caps_output() {
        let path = fixture_script(
            "large-output",
            "i=0; while [ $i -lt 2000 ]; do printf 'safe-state\\n'; i=$((i + 1)); done",
        );
        let started = Instant::now();
        let result = run_bounded(&path, &[], COMMAND_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(result.stdout.len() <= MAX_COMMAND_BYTES + 1);
        fs::remove_file(path).expect("remove fixture");
    }

    #[cfg(unix)]
    #[test]
    fn bounded_command_does_not_join_a_descendant_holding_stdout() {
        let path = fixture_script("inherited-pipe", "(sleep 30) & exit 0");
        let started = Instant::now();
        let result = run_bounded(&path, &[], COMMAND_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(!result.unavailable);
        fs::remove_file(path).expect("remove fixture");
    }

    #[test]
    fn report_timestamp_is_human_readable_utc() {
        assert_eq!(format_utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc_timestamp(86_400), "1970-01-02T00:00:00Z");
        assert_eq!(format_utc_timestamp(1_609_459_200), "2021-01-01T00:00:00Z");
    }

    #[test]
    fn partial_sections_explain_missing_configuration() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/fixture/config.env",
            "screen_idle_blank=credential-secret\n",
        ));
        let settings = settings_section_from_store(&store);
        assert!(settings.body().contains("screen.idle_blank = invalid"));
        assert!(settings.body().contains("raw value omitted"));
        assert!(!settings.body().contains("credential-secret"));

        struct EmptyTvs;
        impl TvsBackend for EmptyTvs {
            fn read_profiles(&self) -> Result<Vec<crate::tvs::TvProfile>, TvsReadError> {
                Ok(Vec::new())
            }

            fn read_model_name(
                &self,
                _profile: &crate::tvs::TvProfile,
            ) -> Result<String, TvsReadError> {
                Err(TvsReadError::internal("model read was not expected"))
            }
        }

        struct FailingTvs;
        impl TvsBackend for FailingTvs {
            fn read_profiles(&self) -> Result<Vec<crate::tvs::TvProfile>, TvsReadError> {
                Err(TvsReadError::internal("credential-secret"))
            }

            fn read_model_name(
                &self,
                _profile: &crate::tvs::TvProfile,
            ) -> Result<String, TvsReadError> {
                Err(TvsReadError::internal("credential-secret"))
            }
        }

        let empty_tv = tv_section_from_backend(&EmptyTvs);
        assert!(empty_tv.body().contains("No TV profile is configured"));
        let failed_tv = tv_section_from_backend(&FailingTvs);
        assert!(failed_tv.body().contains("inspection is unavailable"));
        assert!(!failed_tv.body().contains("credential-secret"));
    }

    #[test]
    fn backend_setting_remains_visible_when_an_unrelated_setting_is_invalid() {
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/fixture/config.env",
            "screen_backend=gnome\ntvs_primary_ip=invalid\n",
        ));
        assert!(store
            .effective_by_name("tv.ip")
            .unwrap()
            .invalid_value()
            .is_some());
        assert_eq!(
            diagnostic_configured_backend(None, Some(&store)),
            Ok(ScreenBackend::Gnome)
        );
        assert_eq!(
            diagnostic_configured_backend(Some("wayland"), Some(&store)),
            Ok(ScreenBackend::Wayland)
        );
        let invalid = SettingsStore::from_reader(ConfigEnvReader::parse(
            "/fixture/config.env",
            "screen_backend=private-invalid-value\n",
        ));
        assert!(diagnostic_configured_backend(None, Some(&invalid)).is_err());
        assert!(
            diagnostic_configured_backend(Some("private-invalid-value"), Some(&store)).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn systemd_fixture_distinguishes_enabled_and_active_states() {
        let path = fixture_script(
            "systemd-states",
            "unit=\"$4\"; [ \"$1\" = \"--user\" ] && unit=\"$5\"; case \"$unit\" in active.service) printf 'LoadState=loaded\\nActiveState=active\\nSubState=running\\nUnitFileState=disabled\\n' ;; inactive.service) printf 'LoadState=loaded\\nActiveState=inactive\\nSubState=dead\\nUnitFileState=enabled\\n' ;; timer.service) printf 'LoadState=loaded\\nActiveState=active\\nSubState=waiting\\nUnitFileState=enabled\\n' ;; esac",
        );
        let active = systemd_unit_observation(&path, ServiceScope::System, "active.service");
        assert_eq!(
            active.text,
            "load=loaded, active=active, substate=running, enabled=disabled"
        );
        assert!(!active.action_needed);

        let inactive = systemd_unit_observation(&path, ServiceScope::System, "inactive.service");
        assert_eq!(
            inactive.text,
            "load=loaded, active=inactive, substate=dead, enabled=enabled"
        );
        assert!(!inactive.action_needed);

        let timer = systemd_unit_observation(&path, ServiceScope::User, "timer.service");
        assert_eq!(
            timer.text,
            "load=loaded, active=active, substate=waiting, enabled=enabled"
        );
        assert!(!timer.action_needed);
        fs::remove_file(path).expect("remove fixture");
    }
}
