//! Headless table-driven tests for the migration planner. No filesystem, no
//! services: all fixtures are raw config text snapshots.

use std::path::PathBuf;

use super::{
    inspect_config, MigrationInspection, MigrationInspectionError, MigrationSelectionError,
    MonitoringChoice, ScreenChoiceRequired,
};
use crate::config::StaleConfigReason;

fn config_path() -> PathBuf {
    PathBuf::from("/home/user/.config/lg-buddy/config.env")
}

fn inspect(contents: &str) -> Result<MigrationInspection, MigrationInspectionError> {
    inspect_config(&config_path(), contents)
}

fn plan(contents: &str) -> super::MigrationPlan {
    match inspect(contents).expect("expected a required plan") {
        MigrationInspection::Required(plan) => plan,
        MigrationInspection::Current => panic!("expected a required plan, got Current"),
    }
}

/// A current, clean v2 config.
fn current_config() -> &'static str {
    "\
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
tvs_primary_platform=lg_webos
screen_backend=gnome
"
}

/// Missing platform (the common 1.x case) plus the required profile fields.
fn missing_platform() -> &'static str {
    "\
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
"
}

/// Explicit legacy platform.
fn bscpylgtv_platform() -> &'static str {
    "\
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
tvs_primary_platform=bscpylgtv
"
}

/// Screen-only: platform already lg_webos, stale swayidle backend, idle
/// blanking still enabled (so a choice is required).
fn screen_only() -> &'static str {
    "\
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
tvs_primary_platform=lg_webos
screen_backend=swayidle
"
}

/// Screen-only with idle blanking already disabled (fixed disabled outcome).
fn already_disabled() -> &'static str {
    "\
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
tvs_primary_platform=lg_webos
screen_backend=swayidle
screen_idle_blank=disabled
"
}

/// Combined: stale platform + stale swayidle backend, idle blanking enabled.
fn combined_stale() -> &'static str {
    "\
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
tvs_primary_platform=bscpylgtv
screen_backend=swayidle
"
}

#[test]
fn current_config_reports_current() {
    assert!(inspect(current_config())
        .expect("clean config")
        .is_current());
}

#[test]
fn missing_platform_requires_pairing_and_webos_candidate() {
    let plan = plan(missing_platform());
    assert!(plan.requires_tv_pairing());
    assert_eq!(
        plan.stale_reasons(),
        &[StaleConfigReason::MissingTvPlatform]
    );
    assert_eq!(
        plan.screen_choice_required(),
        ScreenChoiceRequired::NotApplicable
    );
    // The requirement stays true regardless of any screen selection.
    let candidate = plan.select(None).expect("TV-only select");
    assert!(candidate.requires_tv_pairing());
    let rendered = candidate.rendered();
    assert!(rendered.contains("tvs_primary_platform=lg_webos"));
    assert!(!rendered.contains("tvs_primary_platform=bscpylgtv"));
    // No screen writes in a TV-only migration.
    assert!(!rendered.contains("screen_backend=auto"));
    assert!(!candidate.requires_native_check());
    candidate
        .validate_current()
        .expect("candidate must validate");
}

#[test]
fn explicit_bscpylgtv_requires_pairing() {
    let plan = plan(bscpylgtv_platform());
    assert!(plan.requires_tv_pairing());
    assert_eq!(
        plan.stale_reasons(),
        &[StaleConfigReason::BscpylgtvPlatform]
    );
    let candidate = plan.select(None).expect("TV-only select");
    assert!(candidate.requires_tv_pairing());
    assert!(candidate
        .rendered()
        .contains("tvs_primary_platform=lg_webos"));
}

#[test]
fn screen_only_requires_explicit_choice() {
    let plan = plan(screen_only());
    assert!(!plan.requires_tv_pairing());
    assert_eq!(
        plan.screen_choice_required(),
        ScreenChoiceRequired::Required
    );

    // Absence is a typed choice-required error, not an implicit selection.
    assert_eq!(
        plan.select(None),
        Err(MigrationSelectionError::ChoiceRequired)
    );

    let native = plan
        .select(Some(MonitoringChoice::Native))
        .expect("native select");
    assert!(native.requires_native_check());
    let rendered = native.rendered();
    assert!(rendered.contains("screen_backend=auto"));
    // Native preserves the prior idle-blanking setting (here: absence).
    assert!(!rendered.contains("screen_idle_blank="));
    // A screen-only migration does not request TV pairing.
    assert!(!native.requires_tv_pairing());
    native.validate_current().expect("candidate must validate");
}

#[test]
fn screen_only_disabled_choice_writes_disabled() {
    let plan = plan(screen_only());
    let candidate = plan
        .select(Some(MonitoringChoice::Disabled))
        .expect("disabled select");
    assert!(!candidate.requires_native_check());
    let rendered = candidate.rendered();
    assert!(rendered.contains("screen_backend=auto"));
    assert!(rendered.contains("screen_idle_blank=disabled"));
    assert!(!candidate.requires_tv_pairing());
    candidate
        .validate_current()
        .expect("candidate must validate");
}

#[test]
fn combined_native_carries_both_requirements() {
    let plan = plan(combined_stale());
    assert!(plan.requires_tv_pairing());
    assert_eq!(
        plan.screen_choice_required(),
        ScreenChoiceRequired::Required
    );

    let candidate = plan
        .select(Some(MonitoringChoice::Native))
        .expect("native select");
    let rendered = candidate.rendered();
    assert!(rendered.contains("tvs_primary_platform=lg_webos"));
    assert!(rendered.contains("screen_backend=auto"));
    assert!(candidate.requires_tv_pairing());
    assert!(candidate.requires_native_check());
    candidate
        .validate_current()
        .expect("candidate must validate");
}

#[test]
fn combined_disabled_carries_tv_pairing_without_native_check() {
    let plan = plan(combined_stale());
    let candidate = plan
        .select(Some(MonitoringChoice::Disabled))
        .expect("disabled select");
    // Disabling must not drop the required TV pairing.
    assert!(candidate.requires_tv_pairing());
    assert!(!candidate.requires_native_check());
    let rendered = candidate.rendered();
    assert!(rendered.contains("tvs_primary_platform=lg_webos"));
    assert!(rendered.contains("screen_backend=auto"));
    assert!(rendered.contains("screen_idle_blank=disabled"));
    candidate
        .validate_current()
        .expect("candidate must validate");
}

#[test]
fn already_disabled_is_fixed_outcome() {
    let plan = plan(already_disabled());
    assert!(!plan.requires_tv_pairing());
    assert_eq!(
        plan.screen_choice_required(),
        ScreenChoiceRequired::FixedDisabled
    );

    // None and Disabled both produce the fixed disabled outcome.
    let via_none = plan.select(None).expect("fixed via None");
    assert!(via_none.rendered().contains("screen_idle_blank=disabled"));
    assert!(via_none.rendered().contains("screen_backend=auto"));
    assert!(!via_none.requires_native_check());

    let via_disabled = plan
        .select(Some(MonitoringChoice::Disabled))
        .expect("fixed via Disabled");
    assert!(via_disabled
        .rendered()
        .contains("screen_idle_blank=disabled"));

    // Native must be rejected against a fixed disabled outcome.
    assert_eq!(
        plan.select(Some(MonitoringChoice::Native)),
        Err(MigrationSelectionError::NativeRejected)
    );
    // The plan still carries a migration plan, not a silent no-op.
    assert!(!plan.stale_reasons().is_empty());
}

#[test]
fn tv_only_migration_rejects_irrelevant_choices() {
    let plan = plan(missing_platform());
    assert_eq!(
        plan.select(Some(MonitoringChoice::Native)),
        Err(MigrationSelectionError::IrrelevantChoice)
    );
    assert_eq!(
        plan.select(Some(MonitoringChoice::Disabled)),
        Err(MigrationSelectionError::IrrelevantChoice)
    );
    // And preserves all screen keys untouched (none present here).
    let candidate = plan.select(None).expect("TV-only select");
    assert!(!candidate.rendered().contains("screen_backend="));
}

#[test]
fn malformed_required_profile_fails_inspection() {
    let missing_ip = "tvs_primary_mac=02:11:22:33:44:55\ntvs_primary_input=HDMI_2\n";
    assert_eq!(
        inspect(missing_ip).unwrap_err(),
        MigrationInspectionError::MissingRequiredKey("tvs_primary_ip".to_string())
    );

    let bad_mac =
        "tvs_primary_ip=192.168.1.50\ntvs_primary_mac=not-a-mac\ntvs_primary_input=HDMI_2\n";
    assert!(matches!(
        inspect(bad_mac).unwrap_err(),
        MigrationInspectionError::InvalidValue { key, .. } if key == "tvs_primary_mac"
    ));
}

#[test]
fn invalid_explicit_idle_preference_is_reported() {
    let contents = "\
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
tvs_primary_platform=lg_webos
screen_backend=swayidle
screen_idle_blank=bogus
";
    assert_eq!(
        inspect(contents).unwrap_err(),
        MigrationInspectionError::InvalidScreenIdleBlank("bogus".to_string())
    );
}

#[test]
fn duplicate_last_key_wins_and_editor_replaces_last() {
    // Last-key wins: the last duplicate is swayidle, so stale detection
    // recognises the backend as stale even though an earlier line says auto.
    let contents = "\
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
tvs_primary_platform=lg_webos
screen_backend=auto
screen_backend=swayidle
";
    let plan = plan(contents);
    assert_eq!(
        plan.screen_choice_required(),
        ScreenChoiceRequired::Required
    );
    assert_eq!(plan.stale_reasons(), &[StaleConfigReason::SwayidleBackend]);

    let candidate = plan
        .select(Some(MonitoringChoice::Native))
        .expect("native select");
    let rendered = candidate.rendered();
    // The editor replaces the LAST occurrence only; the duplicate line is
    // preserved, not normalised away.
    assert!(!rendered.contains("swayidle"));
    assert_eq!(rendered.matches("screen_backend=").count(), 2);
    candidate
        .validate_current()
        .expect("candidate must validate");
}

#[test]
fn legacy_input_alias_is_preserved() {
    // TV-only (missing platform, no stale screen backend): legacy alias keys
    // are read via fallback, and the candidate keeps them verbatim.
    let contents = "\
tv_ip=192.168.1.50
tv_mac=02:11:22:33:44:55
input=HDMI_2
";
    let plan = plan(contents);
    assert!(plan.requires_tv_pairing());
    assert_eq!(
        plan.screen_choice_required(),
        ScreenChoiceRequired::NotApplicable
    );
    let candidate = plan.select(None).expect("TV-only select");
    // Legacy alias keys are preserved verbatim; only the platform is added.
    let rendered = candidate.rendered();
    assert!(rendered.contains("tv_ip=192.168.1.50"));
    assert!(rendered.contains("tv_mac=02:11:22:33:44:55"));
    assert!(rendered.contains("input=HDMI_2"));
    assert!(rendered.contains("tvs_primary_platform=lg_webos"));
    assert!(!rendered.contains("screen_backend="));
    candidate
        .validate_current()
        .expect("candidate must validate");
}

#[test]
fn unrelated_lines_comments_and_unknown_keys_are_preserved() {
    let contents = "\
# LG Buddy config
tvs_primary_ip=192.168.1.50
tvs_primary_mac=02:11:22:33:44:55
tvs_primary_input=HDMI_2
screen_backend=swayidle
screen_idle_timeout=900
system_sleep_wake_policy=disabled
updates.channel=prerelease
some_unknown_key=kept
";
    let plan = plan(contents);
    let candidate = plan
        .select(Some(MonitoringChoice::Native))
        .expect("native select");
    let rendered = candidate.rendered();
    for preserved in [
        "# LG Buddy config",
        "tvs_primary_ip=192.168.1.50",
        "screen_idle_timeout=900",
        "system_sleep_wake_policy=disabled",
        "updates.channel=prerelease",
        "some_unknown_key=kept",
        "tvs_primary_platform=lg_webos",
        "screen_backend=auto",
    ] {
        assert!(
            rendered.contains(preserved),
            "missing `{preserved}` in:\n{rendered}"
        );
    }
    candidate
        .validate_current()
        .expect("candidate must validate");
}

#[test]
fn inspecting_and_rendering_leave_source_untouched() {
    let contents = screen_only().to_string();
    let plan = plan(&contents);
    let before = contents.clone();
    let candidate = plan
        .select(Some(MonitoringChoice::Native))
        .expect("native select");
    let _ = candidate.rendered();
    assert_eq!(contents, before, "the source snapshot must be untouched");
}

#[test]
fn preserved_profile_builds_pairing_request() {
    let plan = plan(missing_platform());
    let profile = plan.profile();
    assert_eq!(profile.address().to_string(), "192.168.1.50");
    assert_eq!(profile.mac().to_string(), "02:11:22:33:44:55");
    assert_eq!(profile.input().as_str(), "HDMI_2");
    let request = profile
        .pairing_request()
        .expect("preserved profile must satisfy native pairing validation");
    assert_eq!(request.address().to_string(), "192.168.1.50");
}

#[test]
fn review_regression_current_keeps_optional_idle_fallback() {
    let contents = format!("{}screen_idle_blank=invalid\n", current_config());
    crate::config::parse_current_config(&contents).expect("runtime accepts optional fallback");
    assert!(inspect(&contents)
        .expect("current config must retain runtime parsing semantics")
        .is_current());
}

#[test]
fn review_regression_tv_only_preserves_unrelated_idle_value() {
    let contents = format!("{}screen_idle_blank=invalid\n", missing_platform());
    let candidate = plan(&contents).select(None).expect("TV-only migration");
    assert!(candidate.requires_tv_pairing());
    assert!(!candidate.requires_native_check());
    assert!(candidate.rendered().contains("screen_idle_blank=invalid\n"));
    candidate
        .validate_current()
        .expect("runtime accepts optional fallback");
}

#[test]
fn review_regression_debug_omits_source_and_rendered_config() {
    let marker = "private-value-must-never-appear-in-debug";
    let contents = format!("{}unknown_private_key={marker}\n", missing_platform());
    let plan = plan(&contents);
    let candidate = plan.select(None).expect("TV-only migration");
    assert!(
        candidate.rendered().contains(marker),
        "preserve unknown config values"
    );
    assert!(!format!("{plan:?}").contains(marker));
    assert!(!format!("{candidate:?}").contains(marker));
}

#[test]
fn review_regression_tv_migration_rejects_unpairable_profile_at_inspection() {
    for (original, invalid) in [
        ("192.168.1.50", "0.0.0.0"),
        ("192.168.1.50", "224.0.0.1"),
        ("192.168.1.50", "255.255.255.255"),
        ("02:11:22:33:44:55", "00:00:00:00:00:00"),
        ("02:11:22:33:44:55", "01:11:22:33:44:55"),
    ] {
        let contents = missing_platform().replace(original, invalid);
        let parsed = crate::config::parse_config(&contents).expect("syntactically valid profile");
        assert!(
            crate::pairing::PairingRequest::parse(
                &parsed.tv_ip.to_string(),
                &parsed.tv_mac.to_string(),
                parsed.input,
            )
            .is_err(),
            "fixture must violate native pairing validation"
        );
        assert!(
            inspect(&contents).is_err(),
            "invalid pairing profile accepted: {invalid}"
        );
    }
}
