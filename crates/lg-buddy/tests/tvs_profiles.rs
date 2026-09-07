mod support;

use std::fs;

use lg_buddy::config::{HdmiInput, TvPlatform};
use lg_buddy::platform_access_token::PlatformAccessTokenStore;
use lg_buddy::tvs::{EnvironmentTvsBackend, TvCredentialState, TvsBackend};
use support::{MockBscpylgtv, TestConfigFile, TestEnv};

#[test]
fn environment_backend_reads_primary_profile_and_only_local_token_metadata() {
    let config = TestConfigFile::new("tvs-native-profile");
    config.write_sample("HDMI_3");
    config.set_value("tvs_primary_platform", "lg_webos");
    let token_store = PlatformAccessTokenStore::for_primary_profile(
        config.path(),
        lg_buddy::auth::resolve_config_owner(config.path()).expect("owner"),
    )
    .expect("token store");
    fs::create_dir_all(
        token_store
            .token_path()
            .parent()
            .expect("profile directory"),
    )
    .expect("profile directory");
    fs::write(token_store.token_path(), "{\"access_token\":\"secret\"}\n").expect("token");
    let original = fs::read(config.path()).expect("original config");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    let profiles = EnvironmentTvsBackend
        .read_profiles()
        .expect("configured profile");
    assert_eq!(profiles.len(), 1);
    let profile = &profiles[0];
    assert_eq!(profile.id().as_str(), "primary");
    assert_eq!(profile.name(), "Primary TV");
    assert_eq!(
        profile.address(),
        "192.0.2.42".parse::<std::net::Ipv4Addr>().expect("address")
    );
    assert_eq!(profile.mac().to_string(), "aa:bb:cc:dd:ee:ff");
    assert_eq!(profile.input(), HdmiInput::Hdmi3);
    assert_eq!(profile.platform(), TvPlatform::LgWebOs);
    assert_eq!(profile.credentials(), TvCredentialState::Stored);
    assert!(TvCredentialState::Stored
        .description()
        .contains("does not establish current access"));
    assert_eq!(
        fs::read(config.path()).expect("config after read"),
        original
    );
}

#[test]
fn environment_backend_returns_zero_profiles_without_tv_configuration() {
    let config = TestConfigFile::new("tvs-empty");
    config.write_contents("screen_backend=auto\n");
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());

    let profiles = EnvironmentTvsBackend
        .read_profiles()
        .expect("empty configuration");
    assert!(profiles.is_empty());
}

#[test]
fn native_model_read_without_a_token_keeps_configuration_and_credentials_unchanged() {
    let config = TestConfigFile::new("tvs-native-model-unpaired");
    config.write_sample("HDMI_3");
    config.set_value("tvs_primary_platform", "lg_webos");
    let original = fs::read(config.path()).expect("saved config");
    let token_store = PlatformAccessTokenStore::for_primary_profile(
        config.path(),
        lg_buddy::auth::resolve_config_owner(config.path()).unwrap(),
    )
    .unwrap();
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    let profiles = EnvironmentTvsBackend
        .read_profiles()
        .expect("saved profile");
    assert_eq!(profiles[0].credentials(), TvCredentialState::Missing);
    assert!(EnvironmentTvsBackend.read_model_name(&profiles[0]).is_err());
    assert!(
        !token_store.token_path().exists(),
        "model lookup must not pair or create a token"
    );
    assert_eq!(
        fs::read(config.path()).expect("config after read"),
        original
    );
}

#[test]
fn legacy_profile_loading_is_local_and_model_read_is_separate() {
    let mock = MockBscpylgtv::new("tvs-legacy-profile");
    let wrapper = mock.command_wrapper("tvs-legacy-profile-wrapper");
    let config = TestConfigFile::new("tvs-legacy-profile-config");
    config.write_sample("HDMI_2");
    fs::write(
        config
            .path()
            .parent()
            .expect("config directory")
            .join(".aiopylgtv.sqlite"),
        b"legacy credential metadata",
    )
    .expect("legacy credential file");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    let profiles = EnvironmentTvsBackend
        .read_profiles()
        .expect("legacy profile");

    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0].platform(), TvPlatform::Bscpylgtv);
    assert_eq!(profiles[0].credentials(), TvCredentialState::LocalFile);
    assert!(mock.calls().is_empty(), "profile read must stay local");

    let original = fs::read(config.path()).expect("saved config");
    assert_eq!(
        EnvironmentTvsBackend
            .read_model_name(&profiles[0])
            .expect("live model"),
        "OLED42C2"
    );
    assert_eq!(
        fs::read(config.path()).expect("config after model read"),
        original
    );
    let calls = mock.calls();
    assert_eq!(
        calls.len(),
        1,
        "model read only performs one system-information request"
    );
    assert_eq!(calls[0].command, "get_system_info");
}
