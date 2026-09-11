mod support;

use std::fs;

use lg_buddy::config::load_config;
use lg_buddy::inhibition::evaluate_inhibition_preference;
use lg_buddy::presentation::settings::{
    SettingsEditStatus, SettingsFeedbackSeverity, SettingsPresentation, SettingsStatus,
};
use lg_buddy::settings::{
    SettingsCommand, SettingsCommandRunner, SettingsMutationFailure, SettingsMutationOutcome,
    SettingsStore,
};
use lg_buddy::settings_view::{
    BehaviorSetting, EnvironmentSettingsBackend, SettingsApplication, SettingsBackend,
    SettingsIntent, SettingsTransition,
};
use support::{ExecutableScript, TestConfigFile, TestEnv};

#[test]
fn inhibition_preference_follows_persisted_settings_and_existing_restart_application() {
    let config = TestConfigFile::new("inhibition-preference-settings");
    config.write_contents("tv_ip=192.168.1.42\ntv_mac=aa:bb:cc:dd:ee:ff\ninput=HDMI_1\n");
    let restart_snapshot = config.path().with_extension("applied");
    let systemctl = ExecutableScript::new(
        "inhibition-preference-systemctl",
        "systemctl",
        r#"#!/bin/sh
[ "$1" = "--user" ] || exit 23
case "$2" in
  cat|is-active|is-enabled) exit 0 ;;
  restart)
    [ "$3" = "LG_Buddy_screen.service" ] || exit 23
    cp "$LG_BUDDY_CONFIG" "$LG_BUDDY_TEST_RESTART_SNAPSHOT"
    ;;
  *) exit 23 ;;
esac
"#,
    );
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_SYSTEMCTL", systemctl.path());
    env.set("LG_BUDDY_TEST_RESTART_SNAPSHOT", &restart_snapshot);
    env.remove("LG_BUDDY_SKIP_SYSTEMD_ACTIONS");
    let key = "screen.honor_idle_inhibitors";
    assert!(evaluate_inhibition_preference(&load_config(config.path()).unwrap()).bypass_inhibition);

    for (value, bypass) in [
        (Some("enabled"), false),
        (Some("disabled"), true),
        (Some("enabled"), false),
        (None, true),
    ] {
        let command = match value {
            Some(value) => SettingsCommand::Set {
                key: key.into(),
                value: value.into(),
            },
            None => SettingsCommand::Unset(key.into()),
        };
        let runner = SettingsCommandRunner::new(SettingsStore::load(config.path()).unwrap());
        let mut output = Vec::new();
        runner.run(command, &mut output).unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("apply: restarted LG_Buddy_screen.service"));
        // The normal apply path sees the new file, including resetting to the
        // default. Evaluate what the restarted monitor would actually load.
        assert_eq!(
            fs::read(&restart_snapshot).unwrap(),
            fs::read(config.path()).unwrap()
        );
        let result = evaluate_inhibition_preference(&load_config(&restart_snapshot).unwrap());
        assert_eq!(result.bypass_inhibition, bypass);
        let effective = SettingsStore::load(config.path())
            .unwrap()
            .effective_by_name(key)
            .unwrap();
        assert_eq!(
            effective.value().unwrap().to_string(),
            result.diagnostics.honoring.as_str()
        );
        if value.is_none() {
            assert!(!fs::read_to_string(config.path())
                .unwrap()
                .contains("screen_honor_idle_inhibitors="));
        }
    }
}

const BEHAVIOR_CONFIG: &str = "screen_backend=auto
screen_idle_blank=enabled
screen_honor_idle_inhibitors=disabled
screen_idle_timeout=300
screen_restore_policy=conservative
system_sleep_wake_policy=enabled
updates_auto_check=enabled
updates_channel=stable
";

fn row(
    presentation: &SettingsPresentation,
    setting: BehaviorSetting,
) -> &lg_buddy::presentation::settings::SettingsRow {
    presentation
        .groups()
        .iter()
        .flat_map(|group| group.rows())
        .find(|row| row.setting() == setting)
        .expect("settings presentation contains requested row")
}

fn open_ready(
    app: &mut SettingsApplication,
    opening: SettingsTransition,
    backend: &EnvironmentSettingsBackend,
) -> SettingsTransition {
    let read = opening.read_operation().expect("opening read operation");
    let groups = backend.read_settings().expect("read settings without a TV");
    app.complete_read(read, Ok(groups))
        .expect("complete settings read")
}

fn run_mutation(
    app: &mut SettingsApplication,
    backend: &EnvironmentSettingsBackend,
    intent: SettingsIntent,
) -> SettingsTransition {
    let started = app.handle_intent(intent).expect("start settings mutation");
    let operation = started
        .mutation_operation()
        .expect("mutation operation")
        .clone();
    let outcome = backend
        .write_setting(operation.clone(), &mut |_| {})
        .expect("settings mutation succeeds");
    app.complete_mutation(
        &operation,
        Ok::<SettingsMutationOutcome, SettingsMutationFailure>(outcome),
    )
    .expect("complete settings mutation")
}

#[test]
fn environment_backend_edits_all_behavior_settings_without_a_tv_and_matches_cli() {
    let gui_config = TestConfigFile::new("settings-operations-gui");
    let cli_config = TestConfigFile::new("settings-operations-cli");
    gui_config.write_contents(BEHAVIOR_CONFIG);
    cli_config.write_contents(BEHAVIOR_CONFIG);

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", gui_config.path());
    env.set("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1");

    let backend = EnvironmentSettingsBackend;
    let (mut app, opening) = SettingsApplication::open();
    let ready = open_ready(&mut app, opening, &backend);
    assert!(matches!(
        ready.presentation().status(),
        SettingsStatus::Ready
    ));
    assert_eq!(
        ready
            .presentation()
            .groups()
            .iter()
            .map(|group| group.rows().len())
            .sum::<usize>(),
        8,
        "the Settings view is independent of TV availability"
    );

    let edits = [
        (
            SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenBackend,
                value: "wayland".to_string(),
            },
            "screen.backend",
            "wayland",
        ),
        (
            SettingsIntent::SetEnabled {
                setting: BehaviorSetting::ScreenIdleBlank,
                enabled: false,
            },
            "screen.idle_blank",
            "disabled",
        ),
        (
            SettingsIntent::SetEnabled {
                setting: BehaviorSetting::ScreenHonorIdleInhibitors,
                enabled: true,
            },
            "screen.honor_idle_inhibitors",
            "enabled",
        ),
        (
            SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenIdleTimeout,
                value: "600".to_string(),
            },
            "screen.idle_timeout",
            "600",
        ),
        (
            SettingsIntent::Commit {
                setting: BehaviorSetting::ScreenRestorePolicy,
                value: "aggressive".to_string(),
            },
            "screen.restore_policy",
            "aggressive",
        ),
        (
            SettingsIntent::SetEnabled {
                setting: BehaviorSetting::SystemSleepWakePolicy,
                enabled: false,
            },
            "system.sleep_wake_policy",
            "disabled",
        ),
        (
            SettingsIntent::SetEnabled {
                setting: BehaviorSetting::UpdatesAutoCheck,
                enabled: false,
            },
            "updates.auto_check",
            "disabled",
        ),
        (
            SettingsIntent::Commit {
                setting: BehaviorSetting::UpdatesChannel,
                value: "prerelease".to_string(),
            },
            "updates.channel",
            "prerelease",
        ),
    ];

    for (intent, key, value) in edits {
        let setting = match &intent {
            SettingsIntent::SetEnabled { setting, .. } | SettingsIntent::Commit { setting, .. } => {
                *setting
            }
            _ => unreachable!("the edit table contains only setting writes"),
        };
        let transition = run_mutation(&mut app, &backend, intent);
        assert_eq!(
            row(transition.presentation(), setting).edit_status(),
            SettingsEditStatus::Applied
        );

        env.set("LG_BUDDY_CONFIG", cli_config.path());
        let runner = SettingsCommandRunner::new(SettingsStore::load(cli_config.path()).unwrap());
        runner
            .run(
                SettingsCommand::Set {
                    key: key.to_string(),
                    value: value.to_string(),
                },
                &mut Vec::new(),
            )
            .expect("CLI settings write");
        env.set("LG_BUDDY_CONFIG", gui_config.path());

        assert_eq!(
            fs::read(gui_config.path()).unwrap(),
            fs::read(cli_config.path()).unwrap(),
            "GUI and CLI persist the same bytes for {key}"
        );
    }
}

#[test]
fn reset_removes_a_setting_override_without_a_tv() {
    let config = TestConfigFile::new("settings-reset");
    config.write_contents("screen_backend=wayland\n");
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1");

    let backend = EnvironmentSettingsBackend;
    let (mut app, opening) = SettingsApplication::open();
    open_ready(&mut app, opening, &backend);
    let transition = run_mutation(
        &mut app,
        &backend,
        SettingsIntent::Reset(BehaviorSetting::ScreenBackend),
    );

    assert!(!fs::read_to_string(config.path())
        .unwrap()
        .contains("screen_backend="));
    let setting = row(transition.presentation(), BehaviorSetting::ScreenBackend);
    assert_eq!(setting.value_label(), "Automatic");
    assert_eq!(setting.source_label(), "Default");
}

#[test]
fn invalid_edit_preserves_config_bytes_and_effective_value() {
    let config = TestConfigFile::new("settings-invalid-edit");
    config.write_contents("screen_idle_timeout=300\n");
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_SKIP_SYSTEMD_ACTIONS", "1");

    let backend = EnvironmentSettingsBackend;
    let (mut app, opening) = SettingsApplication::open();
    open_ready(&mut app, opening, &backend);
    let before = fs::read(config.path()).unwrap();
    let started = app
        .handle_intent(SettingsIntent::Commit {
            setting: BehaviorSetting::ScreenIdleTimeout,
            value: "not-a-number".to_string(),
        })
        .expect("start invalid edit");
    let operation = started
        .mutation_operation()
        .expect("mutation operation")
        .clone();
    let failure = backend
        .write_setting(operation.clone(), &mut |_| {})
        .expect_err("invalid value is rejected before persistence");
    let transition = app
        .complete_mutation(&operation, Err::<SettingsMutationOutcome, _>(failure))
        .expect("complete rejected mutation");

    assert_eq!(fs::read(config.path()).unwrap(), before);
    let setting = row(
        transition.presentation(),
        BehaviorSetting::ScreenIdleTimeout,
    );
    assert_eq!(setting.value_label(), "300 seconds");
    assert_eq!(setting.edit_status(), SettingsEditStatus::ValidationFailed);
    assert_eq!(
        setting.feedback().map(|feedback| feedback.severity()),
        Some(SettingsFeedbackSeverity::Error)
    );
}

#[test]
fn apply_failure_keeps_saved_value_and_retry_does_not_rewrite() {
    let config = TestConfigFile::new("settings-apply-failure-gui");
    let cli_config = TestConfigFile::new("settings-apply-failure-cli");
    config.write_contents("screen_idle_timeout=300\n");
    cli_config.write_contents("screen_idle_timeout=300\n");
    let systemctl = ExecutableScript::new(
        "settings-systemctl-failure",
        "systemctl",
        r##"#!/bin/sh
case "$2" in
  cat|is-active|is-enabled) exit 0 ;;
  restart) exit 23 ;;
esac
exit 23
"##,
    );
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_SYSTEMCTL", systemctl.path());
    env.remove("LG_BUDDY_SKIP_SYSTEMD_ACTIONS");

    let backend = EnvironmentSettingsBackend;
    let (mut app, opening) = SettingsApplication::open();
    open_ready(&mut app, opening, &backend);
    let transition = run_mutation(
        &mut app,
        &backend,
        SettingsIntent::Commit {
            setting: BehaviorSetting::ScreenIdleTimeout,
            value: "600".to_string(),
        },
    );
    let setting = row(
        transition.presentation(),
        BehaviorSetting::ScreenIdleTimeout,
    );
    assert_eq!(setting.edit_status(), SettingsEditStatus::ApplyFailed);
    assert_eq!(
        setting.feedback().map(|feedback| feedback.severity()),
        Some(SettingsFeedbackSeverity::Warning)
    );
    assert!(setting
        .retry_apply_action()
        .is_some_and(|action| action.enabled()));
    assert!(fs::read_to_string(config.path())
        .unwrap()
        .contains("screen_idle_timeout=600"));

    env.set("LG_BUDDY_CONFIG", cli_config.path());
    let runner = SettingsCommandRunner::new(SettingsStore::load(cli_config.path()).unwrap());
    assert!(runner
        .run(
            SettingsCommand::Set {
                key: "screen.idle_timeout".to_string(),
                value: "600".to_string(),
            },
            &mut Vec::new(),
        )
        .is_err());
    env.set("LG_BUDDY_CONFIG", config.path());
    assert_eq!(
        fs::read(config.path()).unwrap(),
        fs::read(cli_config.path()).unwrap()
    );

    let saved = fs::read(config.path()).unwrap();
    let retry_started = app
        .handle_intent(SettingsIntent::RetryApply(
            BehaviorSetting::ScreenIdleTimeout,
        ))
        .expect("start retry apply");
    let retry_operation = retry_started
        .mutation_operation()
        .expect("retry mutation operation")
        .clone();
    let retry_outcome = backend
        .write_setting(retry_operation.clone(), &mut |_| {})
        .expect("retry application returns an outcome");
    assert!(!retry_outcome.change().file_changed());
    app.complete_mutation(
        &retry_operation,
        Ok::<SettingsMutationOutcome, SettingsMutationFailure>(retry_outcome),
    )
    .expect("complete retry apply");
    assert_eq!(fs::read(config.path()).unwrap(), saved);
}

#[test]
fn sleep_activation_authorizes_before_saving_and_reuses_the_active_service() {
    use lg_buddy::settings::{SettingsError, SettingsMutationStage};
    let config = TestConfigFile::new("settings-authorization");
    let original = "system_sleep_wake_policy=disabled\n";
    config.write_contents(original);
    let install_root = config.path().parent().unwrap().join("installed");
    for (name, contents) in [
        (
            "usr/lib/lg-buddy/config-path",
            config.path().to_str().unwrap(),
        ),
        ("etc/systemd/system/LG_Buddy_lifecycle.service", "fixture"),
        (
            "etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle",
            "fixture",
        ),
    ] {
        let path = install_root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    let authorization = ExecutableScript::new(
        "settings-pkexec",
        "pkexec",
        r#"#!/bin/sh
case "$(cat "$LG_BUDDY_CONFIG")" in
  *system_sleep_wake_policy=disabled*) ;;
  *) exit 99 ;;
esac
printf '%s\n' "$@" >> "$LG_BUDDY_TEST_AUTH_LOG"
exit "$LG_BUDDY_TEST_AUTH_EXIT"
"#,
    );
    let systemctl = ExecutableScript::new(
        "settings-systemctl",
        "systemctl",
        r#"#!/bin/sh
[ "$*" = 'is-active --quiet LG_Buddy_lifecycle.service' ] || exit 99
[ "$LG_BUDDY_TEST_LIFECYCLE_ACTIVE" = 1 ]
"#,
    );
    let log = install_root.join("authorization.log");
    let mut env = TestEnv::new();
    let path = std::env::join_paths(
        [
            authorization.path().parent().unwrap().to_path_buf(),
            systemctl.path().parent().unwrap().to_path_buf(),
        ]
        .into_iter()
        .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    env.set("PATH", path);
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_INSTALL_ROOT", &install_root);
    env.set("LG_BUDDY_SYSTEMCTL", systemctl.path());
    env.set("LG_BUDDY_TEST_AUTH_LOG", &log);
    env.set("LG_BUDDY_TEST_LIFECYCLE_ACTIVE", "0");
    env.remove("LG_BUDDY_SKIP_SYSTEMD_ACTIONS");
    let backend = EnvironmentSettingsBackend;
    let (mut app, opening) = SettingsApplication::open();
    open_ready(&mut app, opening, &backend);

    for (code, intent) in [
        (
            "126",
            SettingsIntent::SetEnabled {
                setting: BehaviorSetting::SystemSleepWakePolicy,
                enabled: true,
            },
        ),
        (
            "127",
            SettingsIntent::Reset(BehaviorSetting::SystemSleepWakePolicy),
        ),
    ] {
        env.set("LG_BUDDY_TEST_AUTH_EXIT", code);
        let started = app.handle_intent(intent).unwrap();
        let operation = started.mutation_operation().unwrap().clone();
        let mut stages = Vec::new();
        let result = backend.write_setting(operation.clone(), &mut |stage| stages.push(stage));
        assert!(matches!(
            &result,
            Err(SettingsMutationFailure::Activation(_))
        ));
        if code == "126" {
            assert!(matches!(
                &result,
                Err(SettingsMutationFailure::Activation(
                    SettingsError::ActivationCancelled
                ))
            ));
        }
        assert!(!stages.contains(&SettingsMutationStage::Persisting));
        assert_eq!(fs::read_to_string(config.path()).unwrap(), original);
        let finished = app.complete_mutation(&operation, result).unwrap();
        let toggle = row(
            finished.presentation(),
            BehaviorSetting::SystemSleepWakePolicy,
        );
        assert_eq!(toggle.value_label(), "Disabled");
        assert!(toggle.retry_apply_action().is_none());
        assert_eq!(toggle.feedback().is_none(), code == "126");
    }

    env.set("LG_BUDDY_TEST_AUTH_EXIT", "0");
    let enabled = run_mutation(
        &mut app,
        &backend,
        SettingsIntent::SetEnabled {
            setting: BehaviorSetting::SystemSleepWakePolicy,
            enabled: true,
        },
    );
    assert_eq!(
        row(
            enabled.presentation(),
            BehaviorSetting::SystemSleepWakePolicy
        )
        .value_label(),
        "Enabled"
    );
    assert!(fs::read_to_string(config.path())
        .unwrap()
        .contains("system_sleep_wake_policy=enabled"));
    let calls = fs::read_to_string(&log).unwrap();
    let arguments: Vec<_> = calls.lines().collect();
    assert_eq!(arguments.len(), 12);
    for call in arguments.as_chunks::<4>().0 {
        assert_eq!(call[0], "--disable-internal-agent");
        assert!(
            matches!(
                call[1],
                "/usr/bin/systemctl" | "/run/current-system/sw/bin/systemctl"
            ),
            "privileged activation must ignore PATH and LG_BUDDY_SYSTEMCTL: {call:?}"
        );
        assert_eq!(&call[2..], &["start", "LG_Buddy_lifecycle.service"]);
    }

    run_mutation(
        &mut app,
        &backend,
        SettingsIntent::SetEnabled {
            setting: BehaviorSetting::SystemSleepWakePolicy,
            enabled: false,
        },
    );
    env.set("LG_BUDDY_TEST_LIFECYCLE_ACTIVE", "1");
    env.set("LG_BUDDY_TEST_AUTH_EXIT", "99");
    run_mutation(
        &mut app,
        &backend,
        SettingsIntent::SetEnabled {
            setting: BehaviorSetting::SystemSleepWakePolicy,
            enabled: true,
        },
    );
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        calls,
        "an active lifecycle service must not ask again"
    );
}
