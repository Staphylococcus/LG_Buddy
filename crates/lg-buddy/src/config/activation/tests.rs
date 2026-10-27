use super::*;
use crate::settings::{SettingValue, SettingsError, UserServiceState, UserUnitEnableOutcome};
use std::cell::Cell;
use std::os::unix::fs::PermissionsExt;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-activation-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn config(&self) -> PathBuf {
        self.0.join("user/config.env")
    }
    fn record(&self) -> PathBuf {
        self.0.join("user/config-path")
    }
    fn generation(&self) -> PathBuf {
        let path = self.0.join("generation.json");
        if path.exists() {
            return path;
        }
        fs::write(
            &path,
            br#"{"version":1,"settings":{"screen.idle_timeout":300}}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        path
    }
    fn publish(&self) {
        publish_user(
            &self.config(),
            &self.record(),
            Some(&self.generation()),
            &self.0,
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Services {
    config: PathBuf,
    active: bool,
    unavailable: bool,
    fail_restart: bool,
    restarts: Cell<usize>,
    reads: Cell<usize>,
}
impl Services {
    fn new(config: PathBuf) -> Self {
        Self {
            config,
            active: true,
            unavailable: false,
            fail_restart: false,
            restarts: Cell::new(0),
            reads: Cell::new(0),
        }
    }
}
impl ServiceController for Services {
    fn user_service_config_path(&self, unit: &str) -> Result<PathBuf, SettingsError> {
        assert_eq!(unit, SCREEN);
        self.reads.set(self.reads.get() + 1);
        if self.unavailable {
            Err(SettingsError::Activation {
                message: "fixture manager unavailable".into(),
            })
        } else {
            Ok(self.config.clone())
        }
    }
    fn user_service_is_active(&self, unit: &str) -> Result<bool, SettingsError> {
        assert_eq!(unit, SCREEN);
        Ok(self.active)
    }
    fn restart_user_service(&self, unit: &str) -> Result<(), SettingsError> {
        assert_eq!(unit, SCREEN);
        let _lock = PairingLock::for_config(&self.config)
            .expect("refresh must release the publication lock");
        self.restarts.set(self.restarts.get() + 1);
        assert_eq!(
            Declarations::load(&self.config)
                .unwrap()
                .unwrap()
                .value("screen.idle_timeout"),
            Some(SettingValue::Integer(300))
        );
        if self.fail_restart {
            Err(SettingsError::Apply {
                message: "fixture restart failed".into(),
            })
        } else {
            Ok(())
        }
    }
    fn user_service_state(&self, _: &str) -> Result<UserServiceState, SettingsError> {
        panic!("must not enable or start an inactive service")
    }
    fn enable_start_user_unit(&self, _: &str) -> Result<UserUnitEnableOutcome, SettingsError> {
        panic!("must not enable units")
    }
    fn disable_stop_user_unit(&self, _: &str) -> Result<(), SettingsError> {
        panic!("must not disable units")
    }
}

#[test]
fn fresh_machine_binding_is_regular_idempotent_and_does_not_create_user_state() {
    let f = Fixture::new();
    let root = f.0.join("machine");
    publish_machine(&root, &f.config()).unwrap();
    let record = root.join("etc/lg-buddy/config-path");
    let metadata = fs::symlink_metadata(&record).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.mode() & 0o7777, 0o644);
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(binding::installed(&root).unwrap(), Some(f.config()));
    assert!(!f.config().parent().unwrap().exists());
    publish_machine(&root, &f.config()).unwrap();
    assert_eq!(fs::metadata(&record).unwrap().ino(), metadata.ino());
    assert!(publish_machine(&root, &f.0.join("another/config.env")).is_err());
    assert_eq!(fs::metadata(&record).unwrap().ino(), metadata.ino());
    assert_eq!(binding::installed(&root).unwrap(), Some(f.config()));
}

#[test]
fn machine_publication_rejects_redirected_records_and_configs() {
    for config_redirect in [true, false] {
        let f = Fixture::new();
        let root = f.0.join("machine");
        let sentinel = f.0.join("sentinel");
        fs::write(&sentinel, b"retained").unwrap();
        let redirected = if config_redirect {
            f.config()
        } else {
            root.join("etc/lg-buddy/config-path")
        };
        fs::create_dir_all(redirected.parent().unwrap()).unwrap();
        symlink(&sentinel, &redirected).unwrap();
        assert!(publish_machine(&root, &f.config()).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"retained");
        assert!(fs::symlink_metadata(&redirected)
            .unwrap()
            .file_type()
            .is_symlink());
    }
}

#[test]
fn machine_publication_preserves_a_conflicting_legacy_selection() {
    let f = Fixture::new();
    let root = f.0.join("machine");
    let legacy = root.join(binding::LEGACY_BINDING.trim_start_matches('/'));
    fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    fs::write(&legacy, b"/other/config.env\n").unwrap();
    assert!(publish_machine(&root, &f.config()).is_err());
    assert_eq!(fs::read(&legacy).unwrap(), b"/other/config.env\n");
    assert!(!root.join("etc/lg-buddy/config-path").exists());
    assert!(!f.config().exists());
}

#[test]
fn user_publication_refuses_config_aliases_of_activation_artifacts() {
    let f = Fixture::new();
    let marker = PairingLock::marker_path(&f.record());
    assert!(publish_user(&marker, &f.record(), Some(&f.generation()), &f.0).is_err());
    assert!(!marker.exists());
    fs::create_dir_all(f.config().parent().unwrap()).unwrap();
    let sentinel = f.0.join("sentinel");
    fs::write(&sentinel, b"retained").unwrap();
    fs::hard_link(&sentinel, f.config()).unwrap();
    assert!(publish_user(&f.config(), &f.record(), Some(&f.generation()), &f.0).is_err());
    assert_eq!(fs::read(&sentinel).unwrap(), b"retained");
    assert!(!f.record().exists());
}

#[test]
fn invalid_or_writable_candidate_does_not_publish_discovery_or_create_config_parents() {
    for valid in [true, false] {
        let f = Fixture::new();
        let path = f.0.join("invalid.json");
        fs::write(
            &path,
            if valid {
                br#"{"version":1,"settings":{}}"#.as_slice()
            } else {
                b"invalid".as_slice()
            },
        )
        .unwrap();
        if !valid {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        }
        assert!(publish_user(&f.config(), &f.record(), Some(&path), &f.0).is_err());
        assert!(!f.record().exists());
        assert!(!f.config().parent().unwrap().exists());
    }
}

#[test]
fn owned_dangling_generation_can_be_repaired_without_weakening_selector_rules() {
    let f = Fixture::new();
    f.publish();
    let overlay = super::super::declarative::binding_path(&f.config()).unwrap();
    fs::remove_file(&overlay).unwrap();
    symlink(f.0.join("missing.json"), &overlay).unwrap();
    assert!(Declarations::load(&f.config()).is_err());
    f.publish();
    assert_eq!(
        Declarations::load(&f.config())
            .unwrap()
            .unwrap()
            .value("screen.idle_timeout"),
        Some(SettingValue::Integer(300))
    );
}

#[test]
fn first_pairing_after_activation_keeps_declared_behavior_out_of_preferences() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let f = Fixture::new();
    f.publish();
    assert!(!f.config().exists());
    crate::pairing_store::PairingStore::prepare(&f.config())
        .unwrap()
        .commit(
            "192.0.2.10".parse().unwrap(),
            "02:00:00:00:00:10".parse().unwrap(),
            crate::config::HdmiInput::Hdmi1,
            &crate::platform_access_token::PlatformAccessToken::new("verified-fixture-token")
                .unwrap(),
        )
        .unwrap();
    let current = crate::config::load_current_config(&f.config()).unwrap();
    assert_eq!(current.config.screen_idle_timeout, 300);
    assert_eq!(
        current.config.tv_platform,
        crate::config::TvPlatform::LgWebOs
    );
    let preferences = crate::settings::SettingsStore::load(f.config()).unwrap();
    assert_eq!(preferences.raw_storage_value("screen_idle_timeout"), None);
    let token = f
        .config()
        .parent()
        .unwrap()
        .join("tvs/primary/access-token.json");
    let metadata = fs::metadata(token).unwrap();
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.mode() & 0o777, 0o600);
}

#[test]
fn refresh_observes_published_policy_releases_lock_and_retries_a_failed_restart() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let f = Fixture::new();
    f.publish();
    let mut services = Services::new(f.config());
    services.fail_restart = true;
    assert!(refresh_user(&f.config(), &services, &f.0).is_err());
    assert!(!f.config().exists());
    assert_eq!(
        binding::read(&f.record(), unsafe { libc::geteuid() }).unwrap(),
        Some(f.config())
    );
    services.fail_restart = false;
    refresh_user(&f.config(), &services, &f.0).unwrap();
    assert_eq!(services.restarts.get(), 2);
}

#[test]
fn refresh_does_not_start_an_inactive_service_or_touch_a_mismatched_unavailable_unit() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let f = Fixture::new();
    f.publish();
    let mut services = Services::new(f.config());
    services.active = false;
    refresh_user(&f.config(), &services, &f.0).unwrap();
    assert_eq!(services.restarts.get(), 0);
    services.active = true;
    services.config = f.0.join("other.env");
    assert!(refresh_user(&f.config(), &services, &f.0).is_err());
    services.config = f.config();
    services.unavailable = true;
    assert!(refresh_user(&f.config(), &services, &f.0).is_err());
    assert_eq!(services.restarts.get(), 0);
    fs::remove_file(f.generation()).unwrap();
    assert!(refresh_user(&f.config(), &services, &f.0).is_err());
    assert_eq!(services.reads.get(), 3);
}

#[test]
fn root_machine_publication_never_creates_or_claims_user_state() {
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let f = Fixture::new();
    let directory = f.config().parent().unwrap().to_path_buf();
    fs::create_dir(&directory).unwrap();
    let path = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::chown(path.as_ptr(), 65534, 65534) }, 0);
    let before = fs::metadata(&directory).unwrap();
    let root = f.0.join("machine");
    publish_machine(&root, &f.config()).unwrap();
    let record = root.join("etc/lg-buddy/config-path");
    assert_eq!(fs::metadata(&record).unwrap().uid(), 0);
    assert!(!f.config().exists());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
    assert_eq!(fs::metadata(&directory).unwrap().uid(), before.uid());
    assert!(publish_user(&f.config(), &f.record(), Some(&f.generation()), &f.0).is_err());
    let services = Services::new(f.config());
    assert!(refresh_user(&f.config(), &services, &f.0).is_err());
    assert_eq!(services.reads.get(), 0);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
}
