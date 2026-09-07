mod support;

use lg_buddy::config::HdmiInput;
use lg_buddy::settings::{ConfigEnvEditor, SettingsCommand, SettingsCommandRunner, SettingsStore};
use lg_buddy::tvs::{
    EnvironmentTvsBackend, TvsApplication, TvsBackend, TvsIntent, TvsManagementOutcome,
};
use std::fs;
use support::{TestConfigFile, TestEnv};

#[test]
fn input_edit_reuses_cli_persistence_without_pairing_or_tv_commands() {
    let config = TestConfigFile::new("tvs-input-edit");
    config.write_sample("HDMI_1");
    let before = fs::read_to_string(config.path()).unwrap();
    let cli_config = TestConfigFile::new("tvs-input-cli");
    cli_config.write_contents(&before);
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    let backend = EnvironmentTvsBackend;
    let (mut app, opening) = TvsApplication::open();
    let profiles = backend.read_profiles().unwrap();
    app.complete_read(opening.read_operation().unwrap(), Ok(profiles))
        .unwrap();
    let start = app
        .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
        .unwrap();
    let operation = start.management_operation().unwrap();
    let outcome = backend.manage(operation).unwrap();
    assert!(matches!(outcome, TvsManagementOutcome::InputChanged(_)));
    let saved = app.complete_management(operation, Ok(outcome)).unwrap();
    assert_eq!(
        saved.presentation().selected_profile().unwrap().input(),
        HdmiInput::Hdmi3
    );
    let runner = SettingsCommandRunner::new(SettingsStore::load(cli_config.path()).unwrap());
    runner
        .run(
            SettingsCommand::Set {
                key: "tv.input".into(),
                value: "HDMI_3".into(),
            },
            &mut Vec::new(),
        )
        .unwrap();
    assert_eq!(
        fs::read(config.path()).unwrap(),
        fs::read(cli_config.path()).unwrap()
    );
}

#[test]
fn input_edit_rejects_a_changed_profile_before_saving() {
    let config = TestConfigFile::new("tvs-input-stale");
    config.write_sample("HDMI_1");
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    let backend = EnvironmentTvsBackend;
    let (mut app, opening) = TvsApplication::open();
    app.complete_read(
        opening.read_operation().unwrap(),
        Ok(backend.read_profiles().unwrap()),
    )
    .unwrap();
    let start = app
        .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
        .unwrap();
    config.set_value("tvs_primary_ip", "192.0.2.99");
    let before = fs::read(config.path()).unwrap();
    assert!(backend
        .manage(start.management_operation().unwrap())
        .is_err());
    assert_eq!(fs::read(config.path()).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn input_write_failure_preserves_configuration_and_selection() {
    // Resource limits and signal dispositions affect the whole process, so
    // exercise real short writes in a child running only this test.
    const CHILD: &str = "LG_BUDDY_TEST_INPUT_SHORT_WRITE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "input_write_failure_preserves_configuration_and_selection",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "short-write test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    let config = TestConfigFile::new("tvs-input-short-write");
    config.write_sample("HDMI_1");
    config.append_line(&format!("# {}", "padding".repeat(250)));
    config.append_line("updates_auto_check=disabled");
    let before = fs::read(config.path()).unwrap();
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    let backend = EnvironmentTvsBackend;
    let (mut app, opening) = TvsApplication::open();
    app.complete_read(
        opening.read_operation().unwrap(),
        Ok(backend.read_profiles().unwrap()),
    )
    .unwrap();
    let start = app
        .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
        .unwrap();
    let operation = start.management_operation().unwrap();
    let runner = SettingsCommandRunner::new(SettingsStore::load(config.path()).unwrap());
    unsafe {
        // The first write can succeed partially; subsequent writes return EFBIG.
        assert_ne!(libc::signal(libc::SIGXFSZ, libc::SIG_IGN), libc::SIG_ERR);
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut limit), 0);
        limit.rlim_cur = 512;
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &limit), 0);
    }

    let error = backend.manage(operation).unwrap_err();
    assert_eq!(error.presentation().summary(), "Could not save HDMI input");
    let failed = app.complete_management(operation, Err(error)).unwrap();
    assert_eq!(
        failed.presentation().selected_profile().unwrap().input(),
        HdmiInput::Hdmi1
    );
    assert!(failed.presentation().input_enabled());
    assert!(failed.toast_message().is_none());
    assert_eq!(fs::read(config.path()).unwrap(), before);
    assert!(runner
        .run(
            SettingsCommand::Set {
                key: "tv.input".into(),
                value: "HDMI_3".into(),
            },
            &mut Vec::new(),
        )
        .is_err());
    assert_eq!(fs::read(config.path()).unwrap(), before);
    assert_eq!(
        fs::read_dir(config.path().parent().unwrap())
            .unwrap()
            .count(),
        1,
        "failed saves must clean up their temporary files"
    );
}

#[cfg(unix)]
#[test]
fn input_edit_preserves_symlink_target_permissions_and_owner() {
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    let config = TestConfigFile::new("tvs-input-symlink");
    config.write_sample("HDMI_1");
    fs::set_permissions(config.path(), fs::Permissions::from_mode(0o640)).unwrap();
    let before = fs::metadata(config.path()).unwrap();
    let link = config.path().parent().unwrap().join("links/config.env");
    fs::create_dir(link.parent().unwrap()).unwrap();
    symlink("../config.env", &link).unwrap();
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", &link);
    let backend = EnvironmentTvsBackend;
    let (mut app, opening) = TvsApplication::open();
    app.complete_read(
        opening.read_operation().unwrap(),
        Ok(backend.read_profiles().unwrap()),
    )
    .unwrap();
    let start = app
        .handle_intent(TvsIntent::SetInput(HdmiInput::Hdmi3))
        .unwrap();
    backend
        .manage(start.management_operation().unwrap())
        .unwrap();

    assert_eq!(
        fs::read_link(link).unwrap(),
        std::path::Path::new("../config.env")
    );
    assert!(fs::read_to_string(config.path())
        .unwrap()
        .contains("tvs_primary_input=HDMI_3"));
    let after = fs::metadata(config.path()).unwrap();
    assert_eq!(after.mode(), before.mode());
    assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
}

#[cfg(unix)]
#[test]
fn settings_save_does_not_replace_read_only_configuration() {
    use std::os::unix::fs::PermissionsExt;

    if unsafe { libc::geteuid() } == 0 {
        return; // Root can write read-only files, including with the original writer.
    }
    let config = TestConfigFile::new("settings-read-only");
    config.write_sample("HDMI_1");
    let before = fs::read(config.path()).unwrap();
    fs::set_permissions(config.path(), fs::Permissions::from_mode(0o400)).unwrap();
    let runner = SettingsCommandRunner::new(SettingsStore::load(config.path()).unwrap());
    assert!(runner
        .run(
            SettingsCommand::Set {
                key: "tv.input".into(),
                value: "HDMI_3".into(),
            },
            &mut Vec::new(),
        )
        .is_err());
    assert_eq!(fs::read(config.path()).unwrap(), before);
}

#[test]
fn settings_save_creates_missing_configuration_and_parent_directory() {
    let config = TestConfigFile::new("settings-new-config");
    let path = config.path().parent().unwrap().join("nested/config.env");
    let contents = "updates_channel=prerelease\n";
    ConfigEnvEditor::parse(&path, contents).save().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), contents);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}
