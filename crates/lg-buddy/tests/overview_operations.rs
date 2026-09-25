mod support;
#[allow(dead_code)] // Shared process fixture; methods it does not use belong to other binaries.
#[path = "cucumber_support/webos.rs"]
mod web_os;

mod auth {
    pub use lg_buddy::auth::SystemUser;
}
mod platform_access_token {
    pub use lg_buddy::platform_access_token::{PlatformAccessToken, PlatformAccessTokenStore};
}

use lg_buddy::audio::{AudioOperation, AudioWriteFailure};
use lg_buddy::commands::run_volume;
use lg_buddy::overview::{EnvironmentOverviewBackend, OverviewBackend};
use lg_buddy::tv::{CurrentVolume, VolumeLevel};
use lg_buddy::VolumeCommand;
use std::fs;
use support::{TestConfigFile, TestEnv};

/// Native webOS fixture: TV at 127.0.0.1:3001 with a stored platform-access token.
fn native_webos_tv(config: &TestConfigFile) -> web_os::MockWebOsTv {
    let tv =
        web_os::MockWebOsTv::with_version(web_os::MockWebOsVersion::WebOs24Version92261, "HDMI_2");
    config.set_value("tvs_primary_ip", "127.0.0.1");
    config.set_value("tvs_primary_platform", "lg_webos");
    let token_dir = config.path().parent().unwrap().join("tvs/primary");
    fs::create_dir_all(&token_dir).unwrap();
    fs::write(
        token_dir.join("access-token.json"),
        r#"{"access_token": "webos-test-access-token"}"#,
    )
    .unwrap();
    tv
}

#[test]
fn overview_reads_configured_identity_and_independent_tv_capabilities() {
    let config = TestConfigFile::new("overview-reads-config");
    config.write_sample("HDMI_2");
    let mut env = TestEnv::new();
    let tv = native_webos_tv(&config);
    env.set("LG_BUDDY_CONFIG", config.path());
    let backend = EnvironmentOverviewBackend;

    // Identity comes from the stored config; no TV request is required.
    let identity = backend.read_summary().expect("primary TV identity");
    assert_eq!(identity.address().to_string(), "127.0.0.1");
    assert_eq!(identity.input(), lg_buddy::config::HdmiInput::Hdmi2);
    let initial_uris = tv.snapshot().request_uris.len();
    assert_eq!(initial_uris, 0, "identity must not touch the TV");

    // Brightness reads the native backlight (HDMI_2 defaults to 90).
    assert_eq!(
        backend.read_brightness().expect("brightness").as_percent(),
        90
    );

    // Audio reads the native state (volume 20, unmuted by default).
    let audio = backend.read_audio().expect("audio");
    assert_eq!(
        audio.volume(),
        CurrentVolume::Level(VolumeLevel::new(20).unwrap())
    );
    assert!(!audio.is_muted());

    tv.reject_request(Some("ssap://settings/getSystemSettings"));
    assert!(backend.read_brightness().is_err());
    assert_eq!(
        backend
            .read_audio()
            .expect("audio survives brightness failure"),
        audio
    );
    tv.reject_request(Some("ssap://audio/getStatus"));
    assert!(backend.read_audio().is_err());
    assert_eq!(
        backend
            .read_brightness()
            .expect("brightness survives audio failure")
            .as_percent(),
        90
    );
    tv.reject_request(None);
}

#[test]
fn overview_and_cli_set_volume_then_unmute_through_the_same_tv_contract() {
    let config = TestConfigFile::new("overview-volume-config");
    config.write_sample("HDMI_2");
    let mut env = TestEnv::new();
    let tv = native_webos_tv(&config);
    let before = fs::read(config.path()).unwrap();
    env.set("LG_BUDDY_CONFIG", config.path());
    let volume = VolumeLevel::new(37).unwrap();

    tv.set_muted(true);
    // Overview path: set volume then unmute through the same TV contract.
    EnvironmentOverviewBackend
        .write_audio(AudioOperation::SetVolumeAndUnmute(volume))
        .expect("Overview volume application");
    assert_eq!(tv.snapshot().volume, 37);
    assert!(!tv.snapshot().muted);
    // Volume-then-unmute is two distinct operations in a fixed order.
    let snap = tv.snapshot();
    let overview_audio_uris: Vec<String> = snap
        .request_uris
        .iter()
        .filter(|uri| uri.contains("setVolume") || uri.contains("setMute"))
        .cloned()
        .collect();
    assert_eq!(
        overview_audio_uris,
        vec![
            "ssap://audio/setVolume".to_string(),
            "ssap://audio/setMute".to_string()
        ]
    );

    let overview_end = snap.request_uris.len();
    // CLI path: same operation, same native contract.
    tv.set_muted(true);
    run_volume(&mut Vec::new(), VolumeCommand::Set(volume)).expect("CLI volume application");
    assert_eq!(tv.snapshot().volume, 37);
    assert!(!tv.snapshot().muted);
    let cli_audio_uris: Vec<_> = tv.snapshot().request_uris[overview_end..]
        .iter()
        .filter(|uri| uri.contains("setVolume") || uri.contains("setMute"))
        .cloned()
        .collect();
    assert_eq!(cli_audio_uris, overview_audio_uris);
    assert_eq!(fs::read(config.path()).unwrap(), before);
}

#[test]
fn failed_unmute_reports_the_volume_that_was_already_applied() {
    let config = TestConfigFile::new("overview-partial-audio-config");
    config.write_sample("HDMI_2");
    let mut env = TestEnv::new();
    let tv = native_webos_tv(&config);
    env.set("LG_BUDDY_CONFIG", config.path());
    tv.set_muted(true);
    tv.reject_set_mute();
    let volume = VolumeLevel::new(42).unwrap();

    let error = EnvironmentOverviewBackend
        .write_audio(AudioOperation::SetVolumeAndUnmute(volume))
        .expect_err("unmute fails after volume application");
    assert_eq!(error.failure(), AudioWriteFailure::UnmuteAfterVolume);
    assert_eq!(error.volume_applied(), Some(volume));
    assert_eq!(tv.snapshot().volume, 42);
    assert!(tv.snapshot().muted);
}

#[test]
fn overview_native_reads_require_stored_credentials_without_pairing() {
    let config = TestConfigFile::new("overview-unpaired-native");
    config.write_sample("HDMI_2");
    config.set_value("tvs_primary_platform", "lg_webos");
    let before = std::fs::read(config.path()).unwrap();
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());

    let backend = EnvironmentOverviewBackend;
    assert_eq!(
        backend
            .read_audio()
            .expect_err("audio needs credentials")
            .failure(),
        lg_buddy::overview::AudioReadFailure::CredentialsUnavailable
    );
    assert_eq!(
        backend
            .read_brightness()
            .expect_err("brightness needs credentials")
            .failure(),
        lg_buddy::brightness::BrightnessReadFailure::CredentialsUnavailable
    );
    assert_eq!(
        backend
            .write_audio(AudioOperation::SetMuted(false))
            .expect_err("mute needs credentials")
            .failure(),
        AudioWriteFailure::CredentialsUnavailable
    );
    assert_eq!(std::fs::read(config.path()).unwrap(), before);
}
