mod support;
#[allow(dead_code)]
#[path = "cucumber_support/webos.rs"]
mod web_os;
mod auth {
    pub use lg_buddy::auth::SystemUser;
}
mod platform_access_token {
    pub use lg_buddy::platform_access_token::{PlatformAccessToken, PlatformAccessTokenStore};
}

use lg_buddy::audio::{AudioOperation, AudioWriteFailure};
use lg_buddy::brightness::{BrightnessReadFailure, BrightnessWriteFailure};
use lg_buddy::config::{load_current_config, TvPlatform};
use lg_buddy::events::RuntimeEvent;
use lg_buddy::overview::{
    AudioReadFailure, EnvironmentOverviewBackend, OverviewBackend, OverviewSummaryFailure,
};
use lg_buddy::session::runner::{RuntimeActionExecutor, SessionActionExecutor};
use lg_buddy::tv::OledBrightness;
use lg_buddy::tvs::{EnvironmentTvsBackend, TvsBackend, TvsReadFailure};
use lg_buddy::{run_command, Command, RunError};
use std::fs;
use support::{ExecutableScript, MockBscpylgtv, RuntimeStateLayout, TestConfigFile, TestEnv};

fn native_tv(config: &TestConfigFile) -> web_os::MockWebOsTv {
    let tv =
        web_os::MockWebOsTv::with_version(web_os::MockWebOsVersion::WebOs24Version92261, "HDMI_2");
    config.set_value("tvs_primary_ip", "127.0.0.1");
    let token_dir = config.path().parent().unwrap().join("tvs/primary");
    fs::create_dir_all(&token_dir).unwrap();
    fs::write(
        token_dir.join("access-token.json"),
        r#"{"access_token":"webos-test-access-token"}"#,
    )
    .unwrap();
    tv
}

fn assert_stale_foreground(platform: Option<&str>, backend: Option<&str>, reason: &str) {
    let mut env = TestEnv::new();
    let config = TestConfigFile::new("migration-foreground");
    config.write_sample("HDMI_2");
    if let Some(platform) = platform {
        config.set_value("tvs_primary_platform", platform);
    }
    if let Some(backend) = backend {
        config.set_value("screen_backend", backend);
    }
    let native = native_tv(&config);
    let legacy = MockBscpylgtv::new("migration-legacy-tv");
    let wrapper = legacy.command_wrapper("migration-legacy-wrapper");
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    let original = fs::read(config.path()).unwrap();
    let overview = EnvironmentOverviewBackend;

    let summary = overview.read_summary().unwrap_err();
    assert_eq!(summary.failure(), OverviewSummaryFailure::MigrationRequired);
    assert!(summary.to_string().contains(reason), "{summary}");
    assert_eq!(
        overview.read_brightness().unwrap_err().failure(),
        BrightnessReadFailure::MigrationRequired
    );
    assert_eq!(
        overview
            .write_brightness(OledBrightness::new(50).unwrap())
            .unwrap_err()
            .failure(),
        BrightnessWriteFailure::MigrationRequired
    );
    assert_eq!(
        overview.read_audio().unwrap_err().failure(),
        AudioReadFailure::MigrationRequired
    );
    assert_eq!(
        overview
            .write_audio(AudioOperation::SetMuted(true))
            .unwrap_err()
            .failure(),
        AudioWriteFailure::MigrationRequired
    );

    // The GUI may still inspect the local profile, but its automatic model
    // lookup must be gated, including on refresh after configuration changes.
    let profiles = EnvironmentTvsBackend
        .read_profiles()
        .expect("local profile stays readable");
    assert_eq!(
        EnvironmentTvsBackend
            .read_model_name(&profiles[0])
            .unwrap_err()
            .failure(),
        TvsReadFailure::MigrationRequired
    );
    assert_eq!(native.snapshot().connection_count, 0);
    assert!(native.snapshot().request_uris.is_empty());
    assert!(legacy.calls().is_empty());
    assert_eq!(fs::read(config.path()).unwrap(), original);
}

#[test]
fn missing_platform_blocks_all_foreground_tv_operations() {
    assert_stale_foreground(None, None, "tvs_primary_platform is not set");
}

#[test]
fn explicit_legacy_platform_blocks_all_foreground_tv_operations() {
    assert_stale_foreground(Some("bscpylgtv"), None, "tvs_primary_platform=bscpylgtv");
}

#[test]
fn swayidle_alone_blocks_all_foreground_tv_operations() {
    assert_stale_foreground(
        Some("lg_webos"),
        Some("swayidle"),
        "screen_backend=swayidle",
    );
}

#[test]
fn current_foreground_uses_native_tv_and_rechecks_config_on_refresh() {
    let mut env = TestEnv::new();
    let config = TestConfigFile::new("migration-current");
    config.write_sample("HDMI_2");
    config.set_value("tvs_primary_platform", "lg_webos");
    let native = native_tv(&config);
    env.set("LG_BUDDY_CONFIG", config.path());
    let overview = EnvironmentOverviewBackend;
    overview.read_summary().unwrap();
    assert_eq!(overview.read_brightness().unwrap().as_percent(), 90);
    assert!(!overview.read_audio().unwrap().is_muted());
    overview
        .write_audio(AudioOperation::SetMuted(true))
        .unwrap();
    assert!(native.snapshot().muted);
    let profiles = EnvironmentTvsBackend.read_profiles().unwrap();
    assert_eq!(
        EnvironmentTvsBackend.read_model_name(&profiles[0]).unwrap(),
        "OLED42C2"
    );
    assert!(native
        .snapshot()
        .request_uris
        .contains(&"ssap://system/getSystemInfo".to_string()));
    let before = native.snapshot();
    config.set_value("screen_backend", "swayidle");
    assert_eq!(
        overview.read_audio().unwrap_err().failure(),
        AudioReadFailure::MigrationRequired
    );
    assert_eq!(
        overview
            .write_audio(AudioOperation::SetMuted(false))
            .unwrap_err()
            .failure(),
        AudioWriteFailure::MigrationRequired
    );
    assert_eq!(
        EnvironmentTvsBackend
            .read_model_name(&profiles[0])
            .unwrap_err()
            .failure(),
        TvsReadFailure::MigrationRequired
    );
    assert_eq!(native.snapshot().request_uris, before.request_uris);
    assert_eq!(native.snapshot().connection_count, before.connection_count);
}

#[test]
fn stale_cached_profile_vs_current_file_blocks_model_lookup_before_any_tv_work() {
    let mut env = TestEnv::new();
    let config = TestConfigFile::new("migration-recheck-legacy");
    config.write_sample("HDMI_2");
    config.set_value("tvs_primary_platform", "bscpylgtv");
    let legacy = MockBscpylgtv::new("migration-recheck-legacy-tv");
    let wrapper = legacy.command_wrapper("migration-recheck-legacy-wrapper");
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    let native = native_tv(&config);

    // The profile a running GUI cached while the file was still legacy.
    let profiles = EnvironmentTvsBackend
        .read_profiles()
        .expect("legacy profile stays readable");
    assert_eq!(profiles[0].platform(), TvPlatform::Bscpylgtv);

    // The operator migrated the file in place, but the cached profile is stale.
    config.set_value("tvs_primary_platform", "lg_webos");

    let result = EnvironmentTvsBackend.read_model_name(&profiles[0]);
    assert_eq!(
        result.unwrap_err().failure(),
        TvsReadFailure::ProfileChanged,
        "an obsolete cached profile must not drive a model lookup against the migrated file"
    );
    assert_eq!(native.snapshot().connection_count, 0);
    assert!(native.snapshot().request_uris.is_empty());
    assert!(legacy.calls().is_empty());
}

#[test]
fn changed_native_target_prevents_cached_model_lookup() {
    let mut env = TestEnv::new();
    let config = TestConfigFile::new("migration-changed-target");
    config.write_sample("HDMI_2");
    config.set_value("tvs_primary_platform", "lg_webos");
    let native = native_tv(&config);
    env.set("LG_BUDDY_CONFIG", config.path());
    let profiles = EnvironmentTvsBackend.read_profiles().unwrap();
    let original = fs::read(config.path()).unwrap();

    for (key, value) in [
        ("tvs_primary_ip", "127.0.0.2"),
        ("tvs_primary_mac", "aa:bb:cc:dd:ee:01"),
    ] {
        config.set_value(key, value);
        assert_eq!(
            EnvironmentTvsBackend
                .read_model_name(&profiles[0])
                .unwrap_err()
                .failure(),
            TvsReadFailure::ProfileChanged,
            "changed {key} must invalidate the queued model lookup"
        );
        assert_eq!(native.snapshot().connection_count, 0);
        assert!(native.snapshot().request_uris.is_empty());
        fs::write(config.path(), &original).unwrap();
    }

    // Restoring the same target permits a model lookup again.
    assert_eq!(
        EnvironmentTvsBackend.read_model_name(&profiles[0]).unwrap(),
        "OLED42C2"
    );
}

#[test]
fn missing_or_invalid_current_config_prevents_cached_model_lookup() {
    let mut env = TestEnv::new();
    let config = TestConfigFile::new("migration-invalid-current");
    config.write_sample("HDMI_2");
    config.set_value("tvs_primary_platform", "lg_webos");
    let native = native_tv(&config);
    env.set("LG_BUDDY_CONFIG", config.path());
    let profiles = EnvironmentTvsBackend.read_profiles().unwrap();
    config.set_value("tvs_primary_ip", "not-an-address");
    assert_eq!(
        EnvironmentTvsBackend
            .read_model_name(&profiles[0])
            .unwrap_err()
            .failure(),
        TvsReadFailure::InvalidConfiguration
    );
    fs::remove_file(config.path()).unwrap();
    assert_eq!(
        EnvironmentTvsBackend
            .read_model_name(&profiles[0])
            .unwrap_err()
            .failure(),
        TvsReadFailure::NotConfigured
    );
    assert_eq!(native.snapshot().connection_count, 0);
    assert!(native.snapshot().request_uris.is_empty());
}

#[test]
fn invalid_utf8_config_prevents_model_lookup_for_a_cached_profile() {
    let mut env = TestEnv::new();
    let config = TestConfigFile::new("migration-bad-utf8");
    config.write_sample("HDMI_2");
    config.set_value("tvs_primary_platform", "lg_webos");
    let legacy = MockBscpylgtv::new("migration-bad-utf8-tv");
    let wrapper = legacy.command_wrapper("migration-bad-utf8-wrapper");
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    let native = native_tv(&config);

    let profiles = EnvironmentTvsBackend
        .read_profiles()
        .expect("current profile is readable before the file is corrupted");
    assert_eq!(profiles[0].platform(), TvPlatform::LgWebOs);

    let mut bytes = fs::read(config.path()).expect("config bytes");
    bytes.push(0xff);
    fs::write(config.path(), &bytes).expect("corrupt config");

    let result = EnvironmentTvsBackend.read_model_name(&profiles[0]);
    assert_eq!(
        result.unwrap_err().failure(),
        TvsReadFailure::InvalidConfiguration,
        "an unreadable configuration must prevent TV work"
    );
    assert_eq!(native.snapshot().connection_count, 0);
    assert!(native.snapshot().request_uris.is_empty());
    assert!(legacy.calls().is_empty());
}

#[test]
fn stale_sleep_stops_before_journal_tv_or_marker_work() {
    let mut env = TestEnv::new();
    let config = TestConfigFile::new("migration-sleep");
    config.write_sample("HDMI_2");
    let legacy = MockBscpylgtv::new("migration-sleep-tv");
    let wrapper = legacy.command_wrapper("migration-sleep-wrapper");
    let journal = ExecutableScript::new("migration-journal", "journalctl", "#!/bin/sh\nprintf called >> \"$LG_BUDDY_TEST_JOURNAL_LOG\"\nprintf '%s\\n' 'manager: sleep: sleep requested'\n");
    let journal_log = config.path().with_file_name("journal.log");
    let runtime = RuntimeStateLayout::new("migration-sleep-state");
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_JOURNALCTL", journal.path());
    env.set("LG_BUDDY_TEST_JOURNAL_LOG", &journal_log);
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
    assert!(matches!(
        run_command(Command::Sleep, &mut Vec::new()),
        Err(RunError::MigrationRequired(_))
    ));
    assert!(!journal_log.exists());
    assert!(legacy.calls().is_empty());
    runtime.assert_system_marker_absent();
}

#[test]
fn stale_reload_during_before_sleep_refuses_before_any_tv_work() {
    let mut env = TestEnv::new();
    let config = TestConfigFile::new("migration-sleep-reload");
    config.write_sample("HDMI_2");
    config.set_value("tvs_primary_platform", "lg_webos");
    let legacy = MockBscpylgtv::new("migration-sleep-reload-tv");
    let wrapper = legacy.command_wrapper("migration-sleep-reload-wrapper");
    let runtime = RuntimeStateLayout::new("migration-sleep-reload-state");
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
    let native = native_tv(&config);

    // A retained session executor validates the current, native configuration.
    let mut executor = RuntimeActionExecutor::default();
    assert!(load_current_config(config.path())
        .map(|current| current.config.tv_platform)
        .is_ok_and(|platform| platform == TvPlatform::LgWebOs));

    // The operator flips the file to legacy while the executor is retained.
    config.set_value("tvs_primary_platform", "bscpylgtv");

    let event = RuntimeEvent::from_command(Command::SleepPre).unwrap();
    assert!(
        matches!(
            executor.before_sleep(event),
            Err(RunError::MigrationRequired(_))
        ),
        "a stale reload must refuse, not fall back to the legacy adapter"
    );

    // No native connection was opened and the legacy adapter was never driven.
    assert_eq!(native.snapshot().connection_count, 0);
    assert!(native.snapshot().request_uris.is_empty());
    assert!(legacy.calls().is_empty());
    runtime.assert_system_marker_absent();
}
