use super::*;
use crate::migration::{inspect_config, MigrationInspection, MonitoringChoice};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "lg-migrate-{}-{}",
            process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        let f = Self(p);
        fs::write(f.path(), "# retained\ntv_ip=192.0.2.4\ntv_mac=aa:bb:cc:dd:ee:ff\ninput=HDMI_1\nscreen_backend=swayidle\n").unwrap();
        f
    }
    fn path(&self) -> PathBuf {
        self.0.join("config.env")
    }
    fn token(&self) -> PathBuf {
        self.0.join("tvs/primary/access-token.json")
    }
    fn prepare(&self) -> (MigrationSnapshot, MigrationCandidate) {
        let s = MigrationSnapshot::capture(&self.path()).unwrap();
        let MigrationInspection::Required(plan) =
            inspect_config(&self.path(), s.contents()).unwrap()
        else {
            panic!()
        };
        (s, plan.select(Some(MonitoringChoice::Disabled)).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn token() -> PlatformAccessToken {
    PlatformAccessToken::new("prepared-secret").unwrap()
}

#[test]
fn combined_commit_publishes_both_changes_and_current_loader_accepts_it() {
    let f = Fixture::new();
    let (s, c) = f.prepare();
    let out = s
        .commit(&c, Some(&token()), &StepCancellation::default())
        .unwrap();
    assert!(!out.durability_warning);
    assert_eq!(fs::read_to_string(f.path()).unwrap(), c.rendered());
    assert!(crate::config::load_current_config(&f.path()).is_ok());
    assert!(fs::read_to_string(f.token())
        .unwrap()
        .contains("prepared-secret"));
}

#[test]
fn all_prepublication_faults_keep_stale_config_and_restore_token() {
    for point in [
        Point::StageWrite,
        Point::FileSync,
        Point::TokenPublish,
        Point::TokenSync,
        Point::BeforeConfig,
        Point::ConfigRename,
    ] {
        for existing in [false, true] {
            let f = Fixture::new();
            if existing {
                fs::create_dir_all(f.token().parent().unwrap()).unwrap();
                fs::write(f.token(), b"{\"access_token\":\"old\"}\n").unwrap();
            }
            let prior = fs::read(f.token()).ok();
            let (s, c) = f.prepare();
            let raw = fs::read(f.path()).unwrap();
            let result =
                s.commit_with(&c, Some(&token()), &StepCancellation::default(), &mut |p| {
                    if p == point {
                        Err(io::Error::other("injected"))
                    } else {
                        Ok(())
                    }
                });
            assert_eq!(result, Err(MigrationStoreError::Storage));
            assert_eq!(fs::read(f.path()).unwrap(), raw);
            assert_eq!(fs::read(f.token()).ok(), prior);
            assert!(matches!(
                crate::config::load_current_config(&f.path()),
                Err(crate::config::ConfigLoadError::Stale(_))
            ));
            assert!(fs::read_dir(&f.0).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
        }
    }
}
#[test]
fn directory_sync_failure_is_committed_and_never_rolls_back() {
    let f = Fixture::new();
    let (s, c) = f.prepare();
    let r = s
        .commit_with(&c, Some(&token()), &StepCancellation::default(), &mut |p| {
            if p == Point::DirectorySync {
                Err(io::Error::other("sync"))
            } else {
                Ok(())
            }
        })
        .unwrap();
    assert!(r.durability_warning);
    assert!(crate::config::load_current_config(&f.path()).is_ok());
    assert!(f.token().exists());
}
#[test]
fn cancellation_before_commit_publishes_nothing_and_commit_rejects_late_cancel() {
    let f = Fixture::new();
    let (s, c) = f.prepare();
    let gate = StepCancellation::default();
    assert!(gate.cancel());
    assert_eq!(
        s.commit(&c, Some(&token()), &gate),
        Err(MigrationStoreError::Cancelled)
    );
    assert!(!f.token().exists());
    let gate = StepCancellation::default();
    s.commit_with(&c, Some(&token()), &gate, &mut |p| {
        if p == Point::BeforeConfig {
            assert!(!gate.cancel());
        }
        Ok(())
    })
    .unwrap();
}
#[test]
fn changed_config_or_credential_rejects_old_plan() {
    let f = Fixture::new();
    let (s, c) = f.prepare();
    fs::write(f.path(), format!("{}# edit\n", s.contents())).unwrap();
    assert_eq!(
        s.commit(&c, Some(&token()), &StepCancellation::default()),
        Err(MigrationStoreError::ConfigurationChanged)
    );
    assert!(!f.token().exists());
    let (s, c) = f.prepare();
    fs::create_dir_all(f.token().parent().unwrap()).unwrap();
    fs::write(f.token(), "external").unwrap();
    assert_eq!(
        s.commit(&c, Some(&token()), &StepCancellation::default()),
        Err(MigrationStoreError::CredentialChanged)
    );
    assert_eq!(fs::read_to_string(f.token()).unwrap(), "external");
}
#[test]
fn rollback_conflict_preserves_external_credential_and_reports_failure() {
    let f = Fixture::new();
    let (s, c) = f.prepare();
    let r = s.commit_with(&c, Some(&token()), &StepCancellation::default(), &mut |p| {
        if p == Point::BeforeConfig {
            fs::write(f.token(), "external").unwrap();
            return Err(io::Error::other("fail"));
        }
        Ok(())
    });
    assert_eq!(r, Err(MigrationStoreError::RollbackFailed));
    assert_eq!(fs::read_to_string(f.token()).unwrap(), "external");
    assert_eq!(fs::read_to_string(f.path()).unwrap(), s.contents());
}
#[test]
fn ambiguous_rename_reconciles_committed_or_preserves_token_when_indeterminate() {
    for external in [false, true] {
        let f = Fixture::new();
        let (s, c) = f.prepare();
        let r = s.commit_with(&c, Some(&token()), &StepCancellation::default(), &mut |p| {
            if p == Point::RenameResult {
                if external {
                    fs::write(f.path(), "external").unwrap();
                }
                return Err(io::Error::other("ambiguous"));
            }
            Ok(())
        });
        if external {
            assert_eq!(r, Err(MigrationStoreError::CommitIndeterminate));
        } else {
            assert!(r.is_ok());
        }
        assert!(f.token().exists());
    }
}
#[test]
fn file_identity_readonly_and_symlink_are_checked() {
    let f = Fixture::new();
    let (s, c) = f.prepare();
    let other = f.0.join("replacement");
    fs::write(&other, s.contents()).unwrap();
    fs::rename(other, f.path()).unwrap();
    assert_eq!(
        s.commit(&c, Some(&token()), &StepCancellation::default()),
        Err(MigrationStoreError::ConfigurationChanged)
    );
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o400)).unwrap();
    assert!(MigrationSnapshot::capture(&f.path()).is_err());
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o600)).unwrap();
    let alias = f.0.join("alias");
    std::os::unix::fs::symlink(f.path(), &alias).unwrap();
    assert!(MigrationSnapshot::capture(&alias).is_err());
}

#[test]
fn shared_guard_excludes_settings_pairing_unpair_and_direct_token_writes() {
    let f = Fixture::new();
    let _guard = PairingLock::for_config(&f.path()).unwrap();
    let mut editor = ConfigEnvEditor::load(f.path()).unwrap();
    editor.set("screen_backend", SettingValue::Enum("auto"));
    assert!(editor.save().is_err());
    let store = PlatformAccessTokenStore::for_primary_profile(
        &f.path(),
        resolve_config_owner(&f.path()).unwrap(),
    )
    .unwrap();
    assert!(store.persist(&token()).is_err());
    let request =
        crate::pairing::PairingRequest::parse("192.0.2.4", "aa:bb:cc:dd:ee:ff", HdmiInput::Hdmi1)
            .unwrap();
    assert!(PairingStore::prepare_pairing(&f.path(), &request).is_err());
    let profile = crate::tvs::TvProfile::new(
        crate::tvs::TvId::primary(),
        "Primary TV",
        request.address(),
        request.mac(),
        request.input(),
        TvPlatform::Bscpylgtv,
        crate::tvs::TvCredentialState::Stored,
    );
    assert!(PairingStore::unpair_primary(&f.path(), &profile).is_err());
    assert!(!f.token().exists());
    let alias = f.0.join("alias.env");
    std::os::unix::fs::symlink(f.path(), &alias).unwrap();
    assert!(PairingLock::for_config(&alias).is_err());
}
#[test]
fn stale_settings_editor_cannot_overwrite_a_migration() {
    let f = Fixture::new();
    let mut old = ConfigEnvEditor::load(f.path()).unwrap();
    old.set("screen_backend", SettingValue::Enum("swayidle"));
    let (s, c) = f.prepare();
    s.commit(&c, Some(&token()), &StepCancellation::default())
        .unwrap();
    assert!(old.save().is_err());
    assert_eq!(fs::read_to_string(f.path()).unwrap(), c.rendered());
}
#[test]
fn unchanged_native_token_is_verified_without_replacement() {
    let f = Fixture::new();
    let store = PlatformAccessTokenStore::for_primary_profile(
        &f.path(),
        resolve_config_owner(&f.path()).unwrap(),
    )
    .unwrap();
    store.persist(&token()).unwrap();
    let before = fs::metadata(f.token()).unwrap();
    let (s, c) = f.prepare();
    assert_eq!(s.existing_token().unwrap(), Some(token()));
    s.commit(&c, Some(&token()), &StepCancellation::default())
        .unwrap();
    assert_eq!(before.ino(), fs::metadata(f.token()).unwrap().ino());
}
#[test]
fn rollback_io_failure_is_explicit_and_retains_recoverable_native_token() {
    let f = Fixture::new();
    let (s, c) = f.prepare();
    let r = s.commit_with(&c, Some(&token()), &StepCancellation::default(), &mut |p| {
        if matches!(p, Point::BeforeConfig | Point::Rollback) {
            Err(io::Error::other("fault"))
        } else {
            Ok(())
        }
    });
    assert_eq!(r, Err(MigrationStoreError::RollbackFailed));
    assert_eq!(fs::read_to_string(f.path()).unwrap(), s.contents());
    assert!(f.token().exists());
    let (retry, candidate) = f.prepare();
    retry
        .commit(
            &candidate,
            Some(&retry.existing_token().unwrap().unwrap()),
            &StepCancellation::default(),
        )
        .unwrap();
    assert!(crate::config::load_current_config(&f.path()).is_ok());
}

#[test]
fn interruption_child() {
    let Some(root) = std::env::var_os("LG_BUDDY_MIGRATION_CRASH_TEST_ROOT") else {
        return;
    };
    let f = Fixture(PathBuf::from(root));
    let (s, c) = f.prepare();
    let phase = std::env::var("LG_BUDDY_MIGRATION_CRASH_TEST_PHASE").unwrap();
    let point = match phase.as_str() {
        "before-token" => Point::TokenPublish,
        "between" => Point::BeforeConfig,
        "after-config" => Point::RenameResult,
        _ => panic!("unknown fixture phase"),
    };
    s.commit_with(&c, Some(&token()), &StepCancellation::default(), &mut |p| {
        if p == point {
            fs::write(f.0.join("ready"), b"ready").unwrap();
            loop {
                std::thread::park();
            }
        }
        Ok(())
    })
    .unwrap();
}
#[test]
fn killed_process_keeps_old_or_complete_config_and_restart_recovers() {
    for phase in ["before-token", "between", "after-config"] {
        let f = Fixture::new();
        let mut cmd = std::process::Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "pairing_store::migration::tests::interruption_child",
            "--test-threads=1",
        ])
        .env("LG_BUDDY_MIGRATION_CRASH_TEST_ROOT", &f.0)
        .env("LG_BUDDY_MIGRATION_CRASH_TEST_PHASE", phase);
        let result = crate::command::run_status_bounded(cmd, std::time::Duration::from_secs(1));
        assert!(result.timed_out);
        assert!(
            f.0.join("ready").exists(),
            "child reached the named boundary"
        );
        if phase == "after-config" {
            assert!(crate::config::load_current_config(&f.path()).is_ok());
            assert!(f.token().exists());
        } else {
            assert!(matches!(
                crate::config::load_current_config(&f.path()),
                Err(crate::config::ConfigLoadError::Stale(_))
            ));
            assert_eq!(f.token().exists(), phase == "between");
            let (s, c) = f.prepare();
            let prepared = s.existing_token().unwrap().unwrap_or_else(token);
            s.commit(&c, Some(&prepared), &StepCancellation::default())
                .unwrap();
            assert!(crate::config::load_current_config(&f.path()).is_ok());
        }
    }
}

#[test]
fn concurrent_readers_observe_only_complete_old_or_new_config() {
    let f = Fixture::new();
    let mut raw = fs::read_to_string(f.path()).unwrap();
    raw.push_str(&"# unrelated preserved comment\n".repeat(2000));
    fs::write(f.path(), &raw).unwrap();
    let (snapshot, candidate) = f.prepare();
    let new = candidate.rendered().as_bytes().to_vec();
    let old = raw.into_bytes();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let ready = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            assert_eq!(fs::read(f.path()).unwrap(), old);
            ready.wait();
            while !stop.load(Ordering::Acquire) {
                let bytes = fs::read(f.path()).unwrap();
                assert!(
                    bytes == old || bytes == new,
                    "reader saw partial publication"
                );
                std::thread::yield_now();
            }
            assert_eq!(fs::read(f.path()).unwrap(), new);
        });
        ready.wait();
        let result = snapshot.commit(&candidate, Some(&token()), &StepCancellation::default());
        stop.store(true, Ordering::Release);
        assert!(result.is_ok());
        reader.join().unwrap();
    });
}

#[test]
fn externally_published_config_after_token_write_never_gets_old_credential_restored() {
    let f = Fixture::new();
    let (snapshot, candidate) = f.prepare();
    let result = snapshot.commit_with(
        &candidate,
        Some(&token()),
        &StepCancellation::default(),
        &mut |p| {
            if p == Point::BeforeConfig {
                fs::write(f.path(), candidate.rendered()).unwrap();
            }
            Ok(())
        },
    );
    assert_eq!(result, Err(MigrationStoreError::CommitIndeterminate));
    assert!(crate::config::load_current_config(&f.path()).is_ok());
    assert!(fs::read_to_string(f.token())
        .unwrap()
        .contains("prepared-secret"));
}
