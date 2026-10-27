use super::*;
use crate::presentation::settings::SettingsPresentation;
use crate::settings::{
    ConfigEnvEditor, ServiceController, SettingsApplier, SettingsMutation, SettingsMutationFailure,
    SettingsStore, UserServiceState,
};
use crate::settings_view::{BehaviorSetting, SettingsApplication, SettingsIntent};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
    config: PathBuf,
    overlay: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-declarative-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let config = root.join("config.env");
        fs::write(&config, "tvs_primary_ip=192.0.2.10\ntvs_primary_mac=02:00:00:00:00:10\ntvs_primary_input=HDMI_1\ntvs_primary_platform=lg_webos\nscreen_idle_timeout=600\nscreen_idle_blank=disabled\n").unwrap();
        let overlay = binding_path(&config).unwrap();
        fs::write(root.join("credential-sentinel"), b"unchanged credential").unwrap();
        Self {
            root,
            config,
            overlay,
        }
    }
    fn declare(&self, source: DeclarativeSource, settings: Value) {
        fs::write(&self.overlay, serde_json::to_vec(&serde_json::json!({"version":1, "source":if source==DeclarativeSource::Nix {"nix"} else {"generic"}, "settings":settings})).unwrap()).unwrap();
        fs::set_permissions(&self.overlay, fs::Permissions::from_mode(0o644)).unwrap();
    }
    fn store(&self) -> SettingsStore {
        SettingsStore::load(&self.config).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct NoRuntime;
impl ServiceController for NoRuntime {
    fn user_service_state(&self, _: &str) -> Result<UserServiceState, SettingsError> {
        panic!("must reject before service access")
    }
    fn restart_user_service(&self, _: &str) -> Result<(), SettingsError> {
        panic!("must reject before restart")
    }
    fn enable_start_user_unit(
        &self,
        _: &str,
    ) -> Result<crate::settings::UserUnitEnableOutcome, SettingsError> {
        panic!("must reject before activation")
    }
    fn disable_stop_user_unit(&self, _: &str) -> Result<(), SettingsError> {
        panic!("must reject before stopping a service")
    }
}

#[test]
fn declaration_overrides_saved_values_for_all_shared_readers_without_materialization() {
    for source in [DeclarativeSource::Generic, DeclarativeSource::Nix] {
        let f = Fixture::new();
        let original = fs::read(&f.config).unwrap();
        f.declare(source, serde_json::json!({"screen.idle_timeout":300}));
        let setting = f.store().effective_by_name("screen.idle_timeout").unwrap();
        assert_eq!(
            setting.required_value().unwrap(),
            SettingValue::Integer(300)
        );
        assert_eq!(setting.managed_by(), Some(source));
        assert_eq!(
            crate::config::load_current_config(&f.config)
                .unwrap()
                .config
                .screen_idle_timeout,
            300
        );
        assert_eq!(
            crate::config::load_config(&f.config)
                .unwrap()
                .screen_idle_timeout,
            300
        );
        let crate::migration::automatic::MigrationOutcome::Current(current) =
            crate::migration::automatic::migrate_config(&f.config).unwrap()
        else {
            panic!("current data must not be migrated")
        };
        assert_eq!(current.config.screen_idle_timeout, 300);
        assert_eq!(fs::read(&f.config).unwrap(), original);
        assert_eq!(
            fs::read(f.root.join("credential-sentinel")).unwrap(),
            b"unchanged credential"
        );
        fs::remove_file(&f.overlay).unwrap();
        assert_eq!(
            f.store()
                .effective_by_name("screen.idle_timeout")
                .unwrap()
                .required_value()
                .unwrap(),
            SettingValue::Integer(600)
        );
    }
}

#[test]
fn stale_mutations_and_alternate_editor_writes_are_rejected_before_side_effects() {
    for (key, wanted, declared, storage, raw) in [
        (
            "screen.idle_timeout",
            "450",
            serde_json::json!(300),
            "screen_idle_timeout",
            SettingValue::Integer(450),
        ),
        (
            "screen.idle_blank",
            "enabled",
            serde_json::json!(false),
            "screen_idle_blank",
            SettingValue::Enum("enabled"),
        ),
        (
            "system.sleep_wake_policy",
            "enabled",
            serde_json::json!(false),
            "system_sleep_wake_policy",
            SettingValue::Enum("enabled"),
        ),
    ] {
        let f = Fixture::new();
        let stale = SettingsMutation::set(&f.store(), key, wanted).unwrap();
        let mut settings = serde_json::Map::new();
        settings.insert(key.into(), declared);
        f.declare(DeclarativeSource::Nix, Value::Object(settings));
        let before = fs::read(&f.config).unwrap();
        assert!(matches!(
            SettingsMutation::set(&f.store(), key, wanted),
            Err(SettingsError::ManagedSetting { .. })
        ));
        assert!(matches!(
            SettingsMutation::unset(&f.store(), key),
            Err(SettingsError::ManagedSetting { .. })
        ));
        assert!(matches!(
            crate::settings::execute_settings_mutation(
                &f.config,
                stale,
                &SettingsApplier::new(NoRuntime),
                &mut |_| {}
            ),
            Err(SettingsMutationFailure::Persistence(
                SettingsError::ManagedSetting { .. }
            ))
        ));
        assert!(matches!(
            crate::settings::execute_gui_settings_mutation(
                &f.config,
                stale,
                &SettingsApplier::new(NoRuntime),
                &mut |_| {}
            ),
            Err(SettingsMutationFailure::Validation(
                SettingsError::ManagedSetting { .. }
            ))
        ));
        let mut editor = ConfigEnvEditor::load(&f.config).unwrap();
        editor.set(storage, raw);
        assert!(editor.save().is_err());
        assert_eq!(fs::read(&f.config).unwrap(), before);
    }
}

#[test]
fn declaration_appearing_after_validation_blocks_commit_and_unrelated_values_still_save() {
    let f = Fixture::new();
    let mutation = SettingsMutation::set(&f.store(), "screen.idle_timeout", "450").unwrap();
    let before = fs::read(&f.config).unwrap();
    let result = crate::settings::execute_settings_mutation(
        &f.config,
        mutation,
        &SettingsApplier::new(NoRuntime),
        &mut |_| {
            f.declare(
                DeclarativeSource::Generic,
                serde_json::json!({"screen.idle_timeout":300}),
            );
        },
    );
    assert!(matches!(
        result,
        Err(SettingsMutationFailure::Persistence(
            SettingsError::ManagedSetting { .. }
        ))
    ));
    assert_eq!(fs::read(&f.config).unwrap(), before);
    let other = SettingsMutation::set(&f.store(), "updates.channel", "prerelease").unwrap();
    crate::settings::execute_settings_mutation(
        &f.config,
        other,
        &SettingsApplier::new(NoRuntime),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(
        f.store().raw_storage_value("screen_idle_timeout"),
        Some("600")
    );
    assert_eq!(
        f.store()
            .effective_by_name("screen.idle_timeout")
            .unwrap()
            .required_value()
            .unwrap(),
        SettingValue::Integer(300)
    );
    assert_eq!(
        f.store()
            .effective_by_name("updates.channel")
            .unwrap()
            .required_value()
            .unwrap(),
        SettingValue::Enum("prerelease")
    );
}

#[test]
fn invalid_dangling_writable_or_unsupported_sources_fail_closed_and_preserve_data() {
    let f = Fixture::new();
    let original = fs::read(&f.config).unwrap();
    for bytes in [
        r#"{"version":2,"settings":{}}"#,
        r#"{"version":1,"settings":{"screen.idle_timeout":-1}}"#,
        r#"{"version":1,"settings":{"screen.idle_timeout":300,"screen.idle_timeout":600}}"#,
        r#"{"version":1,"settings":{"tv.ip":"192.0.2.1"}}"#,
        r#"{"version":1,"settings":{"screen.idle_timeout":{}}}"#,
        r#"{"version":1,"settings":{},"shell":"exit 0"}"#,
        r#"{"version":1,"settings":{},"source":"unsupported"}"#,
        "{incomplete publication",
    ] {
        fs::write(&f.overlay, bytes).unwrap();
        assert!(SettingsStore::load(&f.config).is_err());
        assert!(crate::config::load_current_config(&f.config).is_err());
        let mut editor = ConfigEnvEditor::load(&f.config).unwrap();
        editor.set("updates_channel", SettingValue::Enum("prerelease"));
        assert!(editor.save().is_err());
        assert_eq!(fs::read(&f.config).unwrap(), original);
    }
    fs::remove_file(&f.overlay).unwrap();
    symlink(f.root.join("missing-generation.json"), &f.overlay).unwrap();
    assert!(SettingsStore::load(&f.config).is_err());
    fs::remove_file(&f.overlay).unwrap();
    f.declare(
        DeclarativeSource::Generic,
        serde_json::json!({"screen.idle_timeout":300}),
    );
    fs::set_permissions(&f.overlay, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(SettingsStore::load(&f.config).is_err());
    assert_eq!(fs::read(&f.config).unwrap(), original);
}

#[test]
fn unsafe_directories_oversized_files_and_nonregular_sources_are_rejected() {
    let f = Fixture::new();
    f.declare(DeclarativeSource::Generic, serde_json::json!({}));
    let before = fs::read(&f.config).unwrap();
    fs::set_permissions(&f.root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(SettingsStore::load(&f.config).is_err());
    fs::set_permissions(&f.root, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(SettingsStore::load(&f.config).is_ok());

    fs::File::create(&f.overlay)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    assert!(SettingsStore::load(&f.config).is_err());
    fs::remove_file(&f.overlay).unwrap();
    fs::create_dir(&f.overlay).unwrap();
    assert!(SettingsStore::load(&f.config).is_err());
    fs::remove_dir(&f.overlay).unwrap();

    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(f.overlay.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(SettingsStore::load(&f.config).is_err());
    assert_eq!(fs::read(&f.config).unwrap(), before);
}

#[test]
fn stable_binding_follows_generation_replacement_and_config_aliases() {
    let f = Fixture::new();
    f.declare(
        DeclarativeSource::Generic,
        serde_json::json!({"screen.idle_timeout":300}),
    );
    let first = f.root.join("generation-one.json");
    fs::rename(&f.overlay, &first).unwrap();
    symlink(&first, &f.overlay).unwrap();
    let alias = f.root.join("profile-config.env");
    symlink(&f.config, &alias).unwrap();
    assert_eq!(
        SettingsStore::load(&alias)
            .unwrap()
            .effective_by_name("screen.idle_timeout")
            .unwrap()
            .required_value()
            .unwrap(),
        SettingValue::Integer(300)
    );
    let replacement = f.root.join("replacement-link");
    let second = f.root.join("generation-two.json");
    fs::write(
        &second,
        r#"{"version":1,"settings":{"screen.idle_timeout":450}}"#,
    )
    .unwrap();
    symlink(&second, &replacement).unwrap();
    fs::rename(&replacement, &f.overlay).unwrap();
    assert_eq!(
        f.store()
            .effective_by_name("screen.idle_timeout")
            .unwrap()
            .required_value()
            .unwrap(),
        SettingValue::Integer(450)
    );
    assert_eq!(
        f.store().raw_storage_value("screen_idle_timeout"),
        Some("600")
    );
}

#[test]
fn managed_rows_stay_visible_and_locked_through_busy_refresh_and_stale_intents() {
    for source in [DeclarativeSource::Generic, DeclarativeSource::Nix] {
        let f = Fixture::new();
        f.declare(source, serde_json::json!({"screen.idle_timeout":300}));
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(
            opening.read_operation().unwrap(),
            Ok(SettingsPresentation::from_store(&f.store())
                .groups()
                .to_vec()),
        )
        .unwrap();
        for available in [false, true, false, true] {
            app.set_controls_available(available);
            assert_eq!(app.presentation().managed_notice_source(), Some(source));
            let row = app
                .presentation()
                .row(BehaviorSetting::ScreenIdleTimeout)
                .unwrap();
            assert!(!row.editor_enabled());
            assert!(!row.reset_action().unwrap().enabled());
            assert!(app
                .presentation()
                .row_visible(BehaviorSetting::ScreenIdleTimeout));
            assert!(app
                .handle_intent(SettingsIntent::Commit {
                    setting: BehaviorSetting::ScreenIdleTimeout,
                    value: "600".into()
                })
                .is_none());
            assert!(app
                .handle_intent(SettingsIntent::Reset(BehaviorSetting::ScreenIdleTimeout))
                .is_none());
        }
        let refresh = app.handle_intent(SettingsIntent::Refresh).unwrap();
        fs::remove_file(&f.overlay).unwrap();
        app.complete_read(
            refresh.read_operation().unwrap(),
            Ok(SettingsPresentation::from_store(&f.store())
                .groups()
                .to_vec()),
        )
        .unwrap();
        assert!(app.presentation().managed_notice_source().is_none());
        assert!(app
            .presentation()
            .row(BehaviorSetting::ScreenIdleTimeout)
            .unwrap()
            .editor_enabled());
    }
}

#[test]
fn late_mutation_completion_cannot_replace_a_newly_declared_row() {
    struct NotInstalled;
    impl ServiceController for NotInstalled {
        fn user_service_state(&self, _: &str) -> Result<UserServiceState, SettingsError> {
            Ok(UserServiceState::Missing)
        }
        fn restart_user_service(&self, _: &str) -> Result<(), SettingsError> {
            panic!("no installed service")
        }
        fn enable_start_user_unit(
            &self,
            _: &str,
        ) -> Result<crate::settings::UserUnitEnableOutcome, SettingsError> {
            panic!("no activation")
        }
        fn disable_stop_user_unit(&self, _: &str) -> Result<(), SettingsError> {
            panic!("no stop")
        }
    }
    let f = Fixture::new();
    let (mut app, opening) = SettingsApplication::open();
    app.complete_read(
        opening.read_operation().unwrap(),
        Ok(SettingsPresentation::from_store(&f.store())
            .groups()
            .to_vec()),
    )
    .unwrap();
    let pending = app
        .handle_intent(SettingsIntent::Commit {
            setting: BehaviorSetting::ScreenIdleTimeout,
            value: "450".into(),
        })
        .unwrap();
    let operation = pending.mutation_operation().unwrap().clone();
    let refresh = app.profile_changed();
    let mutation = SettingsMutation::set(&f.store(), "screen.idle_timeout", "450").unwrap();
    let saved = crate::settings::execute_settings_mutation(
        &f.config,
        mutation,
        &SettingsApplier::new(NotInstalled),
        &mut |_| {},
    )
    .unwrap();
    f.declare(
        DeclarativeSource::Nix,
        serde_json::json!({"screen.idle_timeout":300}),
    );
    app.complete_read(
        refresh.read_operation().unwrap(),
        Ok(SettingsPresentation::from_store(&f.store())
            .groups()
            .to_vec()),
    )
    .unwrap();
    app.complete_mutation(&operation, Ok(saved)).unwrap();
    let row = app
        .presentation()
        .row(BehaviorSetting::ScreenIdleTimeout)
        .unwrap();
    assert_eq!(row.managed_by(), Some(DeclarativeSource::Nix));
    assert_eq!(
        row.editor(),
        &crate::presentation::settings::SettingsEditor::Number { text: "300".into() }
    );
    assert!(!row.editor_enabled());
    assert!(!row.reset_action().unwrap().enabled());
}

#[test]
fn declared_backend_wins_over_an_environment_override() {
    let f = Fixture::new();
    f.declare(
        DeclarativeSource::Nix,
        serde_json::json!({"screen.backend":"auto"}),
    );
    assert_eq!(
        crate::backend::configured_backend_from_path(Some("wayland"), Some(&f.config)).unwrap(),
        crate::config::ScreenBackend::Auto
    );
    fs::remove_file(&f.overlay).unwrap();
    assert_eq!(
        crate::backend::configured_backend_from_path(Some("wayland"), Some(&f.config)).unwrap(),
        crate::config::ScreenBackend::Wayland
    );
}

#[test]
fn fresh_pairing_preserves_declarations_and_uses_effective_requested_behaviors() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let f = Fixture::new();
    fs::write(&f.config, "screen_idle_blank=disabled\n").unwrap();
    f.declare(
        DeclarativeSource::Generic,
        serde_json::json!({"screen.idle_blank":false,"system.sleep_wake_policy":false}),
    );
    let requested = crate::pairing_store::PairingStore::prepare(&f.config)
        .unwrap()
        .commit(
            "192.0.2.10".parse().unwrap(),
            "02:00:00:00:00:10".parse().unwrap(),
            crate::config::HdmiInput::Hdmi1,
            &crate::platform_access_token::PlatformAccessToken::new("new-test-token").unwrap(),
        )
        .unwrap();
    assert!(requested.is_empty());
    assert_eq!(
        f.store().raw_storage_value("system_sleep_wake_policy"),
        None
    );
    assert_eq!(
        f.store()
            .effective_by_name("system.sleep_wake_policy")
            .unwrap()
            .required_value()
            .unwrap(),
        SettingValue::Enum("disabled")
    );
    assert!(f.root.join("tvs/primary/access-token.json").is_file());
}

#[test]
fn root_reads_user_owned_declarations_without_materializing_or_bypassing_management() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let f = Fixture::new();
    f.declare(
        DeclarativeSource::Nix,
        serde_json::json!({"screen.idle_timeout":300}),
    );
    for path in [&f.config, &f.overlay] {
        let file = OpenOptions::new().read(true).open(path).unwrap();
        use std::os::fd::AsRawFd;
        assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 65534, 65534) }, 0);
        fs::set_permissions(path, fs::Permissions::from_mode(0o400)).unwrap();
    }
    let before = fs::read(&f.config).unwrap();
    let metadata = fs::metadata(&f.config).unwrap();
    assert_eq!(
        crate::config::load_current_config(&f.config)
            .unwrap()
            .config
            .screen_idle_timeout,
        300
    );
    assert!(matches!(
        SettingsMutation::set(&f.store(), "screen.idle_timeout", "600"),
        Err(SettingsError::ManagedSetting { .. })
    ));
    assert_eq!(fs::read(&f.config).unwrap(), before);
    let after = fs::metadata(&f.config).unwrap();
    assert_eq!(
        (
            metadata.uid(),
            metadata.mode(),
            metadata.mtime(),
            metadata.mtime_nsec()
        ),
        (after.uid(), after.mode(), after.mtime(), after.mtime_nsec())
    );
}
