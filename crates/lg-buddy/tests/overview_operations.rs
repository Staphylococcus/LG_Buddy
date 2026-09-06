mod support;

use lg_buddy::audio::{AudioOperation, AudioWriteFailure};
use lg_buddy::commands::run_volume;
use lg_buddy::overview::{AudioReadFailure, EnvironmentOverviewBackend, OverviewBackend};
use lg_buddy::tv::{CurrentVolume, VolumeLevel};
use lg_buddy::VolumeCommand;
use support::{MockBscpylgtv, TestConfigFile, TestEnv};

#[test]
fn overview_reads_configured_identity_and_independent_tv_capabilities() {
    let mock = MockBscpylgtv::new("overview-reads");
    mock.set_backlight(62);
    mock.set_volume(24);
    mock.set_muted(true);
    let wrapper = mock.command_wrapper("overview-reads-wrapper");
    let config = TestConfigFile::new("overview-reads-config");
    config.write_sample("HDMI_2");
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    let backend = EnvironmentOverviewBackend;

    let identity = backend.read_summary().expect("primary TV identity");
    assert_eq!(identity.address().to_string(), "192.0.2.42");
    assert_eq!(identity.input(), lg_buddy::config::HdmiInput::Hdmi2);
    assert!(
        mock.calls().is_empty(),
        "identity does not need a TV request"
    );
    assert_eq!(
        backend.read_brightness().expect("brightness").as_percent(),
        62
    );
    let audio = backend.read_audio().expect("audio");
    assert_eq!(
        audio.volume(),
        CurrentVolume::Level(VolumeLevel::new(24).unwrap())
    );
    assert!(audio.is_muted());

    mock.queue_error("get_picture_settings", 1, "planned brightness failure");
    assert!(backend.read_brightness().is_err());
    assert_eq!(
        backend
            .read_audio()
            .expect("audio survives brightness failure"),
        audio
    );
    mock.queue_error("get_audio_status", 1, "planned audio failure");
    assert!(backend.read_audio().is_err());
    assert_eq!(
        backend
            .read_brightness()
            .expect("brightness survives audio failure")
            .as_percent(),
        62
    );
}

#[test]
fn overview_and_cli_set_volume_then_unmute_through_the_same_tv_contract() {
    let mock = MockBscpylgtv::new("overview-volume");
    mock.set_muted(true);
    let wrapper = mock.command_wrapper("overview-volume-wrapper");
    let config = TestConfigFile::new("overview-volume-config");
    config.write_sample("HDMI_2");
    let before = std::fs::read(config.path()).unwrap();
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    let volume = VolumeLevel::new(37).unwrap();

    EnvironmentOverviewBackend
        .write_audio(AudioOperation::SetVolumeAndUnmute(volume))
        .expect("Overview volume application");
    let overview_calls = mock.calls();
    assert_eq!(
        overview_calls
            .iter()
            .map(|call| call.command.as_str())
            .collect::<Vec<_>>(),
        ["set_volume", "set_mute"]
    );
    assert_eq!(mock.state_snapshot().volume, 37);
    assert!(!mock.state_snapshot().muted);

    mock.set_muted(true);
    run_volume(&mut Vec::new(), VolumeCommand::Set(volume)).expect("CLI volume application");
    assert_eq!(
        &mock.calls()[overview_calls.len()..],
        overview_calls.as_slice()
    );
    assert_eq!(mock.state_snapshot().volume, 37);
    assert!(!mock.state_snapshot().muted);
    assert_eq!(std::fs::read(config.path()).unwrap(), before);
}

#[test]
fn failed_unmute_reports_the_volume_that_was_already_applied() {
    let mock = MockBscpylgtv::new("overview-partial-audio");
    mock.set_muted(true);
    mock.queue_error("set_mute", 1, "planned mute failure");
    let wrapper = mock.command_wrapper("overview-partial-audio-wrapper");
    let config = TestConfigFile::new("overview-partial-audio-config");
    config.write_sample("HDMI_2");
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    let volume = VolumeLevel::new(42).unwrap();

    let error = EnvironmentOverviewBackend
        .write_audio(AudioOperation::SetVolumeAndUnmute(volume))
        .expect_err("unmute fails after volume application");
    assert_eq!(error.failure(), AudioWriteFailure::UnmuteAfterVolume);
    assert_eq!(error.volume_applied(), Some(volume));
    assert_eq!(mock.state_snapshot().volume, 42);
    assert!(mock.state_snapshot().muted);
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
        AudioReadFailure::CredentialsUnavailable
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
