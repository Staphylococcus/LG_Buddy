use super::*;
use crate::config::{load_current_config, ScreenBackend, ScreenIdleBlankPolicy, TvPlatform};
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc, Barrier,
};

const PROFILE: &str = "tv_ip=192.0.2.4\ntv_mac=02:11:22:33:44:55\ninput=HDMI_2\n";
const LEGACY: &str = "# keep me\ntv_ip=192.0.2.4\ntv_mac=02:11:22:33:44:55\ninput=HDMI_2\ntvs_primary_platform=bscpylgtv\nscreen_backend=swayidle\nunknown_setting=private-sample\n";

struct Fixture(PathBuf);
impl Fixture {
    fn new(contents: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lg-automatic-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self(dir);
        fs::write(fixture.path(), contents).unwrap();
        fixture
    }
    fn path(&self) -> PathBuf {
        self.0.join("config.env")
    }
    fn token(&self) -> PathBuf {
        self.0.join("tvs/primary/access-token.json")
    }
    fn contents(&self) -> String {
        fs::read_to_string(self.path()).unwrap()
    }
    fn no_staged_files(&self) {
        assert!(fs::read_dir(&self.0).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn current_config() -> String {
    format!("{PROFILE}tvs_primary_platform=lg_webos\nscreen_backend=auto\n")
}
fn converted(outcome: MigrationOutcome) -> CurrentConfig {
    match outcome {
        MigrationOutcome::Converted {
            current,
            durability_warning: false,
        } => current,
        other => panic!("expected converted config: {other:?}"),
    }
}
fn fingerprint(path: &Path) -> (u64, u64, u32, u32, u32, i64, i64) {
    let m = fs::metadata(path).unwrap();
    (
        m.dev(),
        m.ino(),
        m.uid(),
        m.gid(),
        m.mode(),
        m.mtime(),
        m.mtime_nsec(),
    )
}

#[test]
fn legacy_matrix_preserves_preferences_and_needs_no_preparation() {
    for platform in [None, Some("bscpylgtv"), Some("lg_webos")] {
        for backend in ["swayidle", "auto", "gnome"] {
            for idle in [None, Some("enabled"), Some("disabled")] {
                let mut raw = format!("{PROFILE}screen_backend={backend}\n# keep\nunknown=keep\n");
                if let Some(p) = platform {
                    raw.push_str(&format!("tvs_primary_platform={p}\n"));
                }
                if let Some(i) = idle {
                    raw.push_str(&format!("screen_idle_blank={i}\n"));
                }
                let f = Fixture::new(&raw);
                let outcome = migrate_config(&f.path()).unwrap();
                let needs_change = platform != Some("lg_webos") || backend == "swayidle";
                let current = if needs_change {
                    converted(outcome)
                } else {
                    let MigrationOutcome::Current(current) = outcome else {
                        panic!("already current")
                    };
                    assert_eq!(f.contents(), raw);
                    current
                };
                assert_eq!(current.config.tv_platform, TvPlatform::LgWebOs);
                assert_eq!(
                    current.config.screen_backend,
                    if backend == "gnome" {
                        ScreenBackend::Gnome
                    } else {
                        ScreenBackend::Auto
                    }
                );
                assert_eq!(
                    current.config.screen_idle_blank,
                    if idle == Some("disabled") {
                        ScreenIdleBlankPolicy::Disabled
                    } else {
                        ScreenIdleBlankPolicy::Enabled
                    }
                );
                assert!(f.contents().starts_with(PROFILE));
                assert!(f.contents().contains("# keep\nunknown=keep\n"));
                assert_eq!(
                    parse_config_entries(&f.contents())
                        .get("screen_idle_blank")
                        .map(String::as_str),
                    idle
                );
                assert!(load_current_config(&f.path()).is_ok());
                assert!(!f.0.join("tvs").exists());
                f.no_staged_files();
            }
        }
    }
}

#[test]
fn fresh_inputs_are_not_legacy_profiles_and_create_nothing() {
    for raw in [
        "",
        "# empty\n",
        "screen_idle_blank=disabled\nupdates_auto_check=disabled\nunknown=kept\n",
    ] {
        let f = Fixture::new(raw);
        assert!(matches!(
            migrate_config(&f.path()),
            Ok(MigrationOutcome::Unconfigured)
        ));
        assert_eq!(f.contents(), raw);
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
    }
    let f = Fixture::new("");
    fs::remove_file(f.path()).unwrap();
    assert!(matches!(
        migrate_config(&f.path()),
        Ok(MigrationOutcome::Unconfigured)
    ));
    let nested = f.0.join("missing/config.env");
    assert!(matches!(
        migrate_config(&nested),
        Ok(MigrationOutcome::Unconfigured)
    ));
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 0);
}

#[test]
fn invalid_or_partial_input_fails_without_rewriting_or_exposing_values() {
    for raw in [
        "tv_ip=private-sample\n".to_owned(),
        "screen_backend=swayidle\n".to_owned(),
        "tvs_primary_platform=bscpylgtv\n".to_owned(),
        format!("{PROFILE}tvs_primary_platform=unknown-private-sample\n"),
        format!("{LEGACY}screen_idle_blank=private-sample\n"),
        LEGACY.replace("input=HDMI_2", "input=private-sample"),
        LEGACY.replace("tv_mac=02:11:22:33:44:55", "tv_mac=private-sample"),
        LEGACY.replace("tv_ip=192.0.2.4", "tv_ip=0.0.0.0"),
    ] {
        let f = Fixture::new(&raw);
        let error = migrate_config(&f.path()).unwrap_err();
        assert_eq!(error, AutomaticMigrationError::InvalidConfiguration);
        assert!(!format!("{error} {error:?}").contains("private-sample"));
        assert_eq!(f.contents(), raw);
        assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
    }
}

#[test]
fn duplicate_keys_and_alias_precedence_use_existing_config_semantics() {
    let raw = format!("{LEGACY}tvs_primary_ip=192.0.2.8\nscreen_backend=auto\nscreen_backend=swayidle\ntvs_primary_platform=bscpylgtv\nscreen_idle_blank=disabled\n");
    let f = Fixture::new(&raw);
    let current = converted(migrate_config(&f.path()).unwrap());
    assert_eq!(current.config.tv_ip.to_string(), "192.0.2.8");
    assert_eq!(
        f.contents(),
        raw.rsplit_once("tvs_primary_platform=bscpylgtv")
            .map(|(a, b)| format!("{a}tvs_primary_platform=lg_webos{b}"))
            .unwrap()
            .rsplit_once("screen_backend=swayidle")
            .map(|(a, b)| format!("{a}screen_backend=auto{b}"))
            .unwrap()
    );
    assert_eq!(
        parse_config_entries(&f.contents())["screen_idle_blank"],
        "disabled"
    );
}

#[test]
fn current_readonly_config_and_managed_link_are_true_noops() {
    let f = Fixture::new(&current_config());
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o444)).unwrap();
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o555)).unwrap();
    let before = fingerprint(&f.path());
    assert!(matches!(
        migrate_config(&f.path()),
        Ok(MigrationOutcome::Current(_))
    ));
    assert_eq!(fingerprint(&f.path()), before);
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o700)).unwrap();
    let link = f.0.join("managed.env");
    symlink(f.path(), &link).unwrap();
    assert!(matches!(
        migrate_config(&link),
        Ok(MigrationOutcome::Current(_))
    ));
    assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
    assert_eq!(fingerprint(&f.path()), before);
}

#[test]
fn repeated_conversion_does_not_rewrite_and_restored_legacy_converts_again() {
    let f = Fixture::new(LEGACY);
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o640)).unwrap();
    let before = fs::metadata(f.path()).unwrap();
    converted(migrate_config(&f.path()).unwrap());
    let first = fingerprint(&f.path());
    assert_eq!(
        (first.2, first.3, first.4 & 0o7777),
        (before.uid(), before.gid(), 0o640)
    );
    assert!(matches!(
        migrate_config(&f.path()),
        Ok(MigrationOutcome::Current(_))
    ));
    assert_eq!(fingerprint(&f.path()), first);
    fs::write(f.path(), LEGACY).unwrap();
    converted(migrate_config(&f.path()).unwrap());
    assert!(load_current_config(&f.path()).is_ok());
}

#[test]
fn credentials_are_untouched_and_missing_or_corrupt_tokens_still_need_pairing() {
    for token in [
        None,
        Some("not-json"),
        Some("{\"access_token\":\"secret-test\"}\n"),
    ] {
        let f = Fixture::new(LEGACY);
        let legacy = f.0.join(".aiopylgtv.sqlite");
        fs::write(&legacy, b"opaque old credentials").unwrap();
        let legacy_before = fingerprint(&legacy);
        let token_before = token.map(|bytes| {
            fs::create_dir_all(f.token().parent().unwrap()).unwrap();
            fs::write(f.token(), bytes).unwrap();
            fingerprint(&f.token())
        });
        converted(migrate_config(&f.path()).unwrap());
        assert_eq!(fs::read_to_string(f.token()).ok().as_deref(), token);
        assert_eq!(
            f.token().exists().then(|| fingerprint(&f.token())),
            token_before
        );
        assert_eq!(fs::read(&legacy).unwrap(), b"opaque old credentials");
        assert_eq!(fingerprint(&legacy), legacy_before);
        if token.is_none() {
            assert!(!f.0.join("tvs").exists());
        }
        let status = crate::setup::pairing::inspect(&f.path());
        if token.is_some_and(|t| t.starts_with('{')) {
            assert_eq!(status, crate::setup::StepResponse::Complete);
        } else {
            assert!(matches!(
                status,
                crate::setup::StepResponse::InputRequired(crate::setup::StepInput::Pairing {
                    saved: Some(_)
                })
            ));
        }
    }
}

#[test]
fn inaccessible_or_unsafe_credential_tree_does_not_block_config_conversion() {
    for kind in ["unreadable", "symlink", "not-directory"] {
        let f = Fixture::new(LEGACY);
        let tvs = f.0.join("tvs");
        match kind {
            "unreadable" => {
                fs::create_dir(&tvs).unwrap();
                fs::set_permissions(&tvs, fs::Permissions::from_mode(0o000)).unwrap();
            }
            "symlink" => symlink(f.0.join("absent-credentials"), &tvs).unwrap(),
            _ => fs::write(&tvs, "not a directory").unwrap(),
        }
        converted(migrate_config(&f.path()).unwrap());
        match kind {
            "unreadable" => {
                assert_eq!(fs::metadata(&tvs).unwrap().mode() & 0o777, 0);
                fs::set_permissions(&tvs, fs::Permissions::from_mode(0o700)).unwrap();
                assert_eq!(fs::read_dir(&tvs).unwrap().count(), 0);
            }
            "symlink" => assert_eq!(fs::read_link(&tvs).unwrap(), f.0.join("absent-credentials")),
            _ => assert_eq!(fs::read_to_string(&tvs).unwrap(), "not a directory"),
        }
    }
}

#[test]
fn stale_readonly_symlink_and_nonregular_configs_are_refused() {
    let f = Fixture::new(LEGACY);
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o444)).unwrap();
    assert_eq!(
        migrate_config(&f.path()).unwrap_err(),
        AutomaticMigrationError::Storage
    );
    assert_eq!(f.contents(), LEGACY);
    fs::set_permissions(f.path(), fs::Permissions::from_mode(0o600)).unwrap();
    let link = f.0.join("alias.env");
    symlink(f.path(), &link).unwrap();
    assert_eq!(
        migrate_config(&link).unwrap_err(),
        AutomaticMigrationError::Storage
    );
    assert_eq!(f.contents(), LEGACY);
    let dangling = f.0.join("dangling.env");
    symlink(f.0.join("absent"), &dangling).unwrap();
    assert_eq!(
        migrate_config(&dangling).unwrap_err(),
        AutomaticMigrationError::Storage
    );
    assert_eq!(
        migrate_config(&f.0).unwrap_err(),
        AutomaticMigrationError::Storage
    );
}

#[test]
fn non_utf8_config_path_is_supported() {
    use std::os::unix::ffi::OsStringExt;
    let f = Fixture::new(LEGACY);
    let path =
        f.0.join(std::ffi::OsString::from_vec(b"config-\xff.env".to_vec()));
    fs::rename(f.path(), &path).unwrap();
    assert_eq!(converted(migrate_config(&path).unwrap()).path, path);
}

#[test]
fn two_concurrent_startups_publish_once_and_share_the_current_result() {
    let f = Fixture::new(LEGACY);
    let barrier = Arc::new(Barrier::new(2));
    let path = f.path();
    let outcomes = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    migrate_with(&path, &mut |p| {
                        if p == Point::BeforeLock {
                            barrier.wait();
                        }
                        Ok(())
                    })
                    .unwrap()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, MigrationOutcome::Converted { .. }))
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| matches!(o, MigrationOutcome::Current(_)))
            .count(),
        1
    );
    assert!(load_current_config(&f.path()).is_ok());
    f.no_staged_files();
}

#[test]
fn existing_writer_lock_is_respected_then_latest_contents_are_reinspected() {
    let f = Fixture::new(LEGACY);
    let guard = PairingLock::for_config(&f.path()).unwrap();
    let (tx, rx) = mpsc::channel();
    let path = f.path();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            migrate_with(&path, &mut |p| {
                if p == Point::BeforeLock {
                    tx.send(()).unwrap();
                }
                Ok(())
            })
        });
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // This writer owns the same lock used by settings and token writers.
        fs::write(f.path(), current_config()).unwrap();
        let before = fingerprint(&f.path());
        drop(guard);
        assert!(matches!(
            worker.join().unwrap(),
            Ok(MigrationOutcome::Current(_))
        ));
        assert_eq!(fingerprint(&f.path()), before);
    });
}

#[test]
fn occupied_writer_lock_has_a_bounded_failure_and_does_not_write() {
    let f = Fixture::new(LEGACY);
    let _guard = PairingLock::for_config(&f.path()).unwrap();
    let started = Instant::now();
    assert_eq!(
        migrate_config(&f.path()).unwrap_err(),
        AutomaticMigrationError::Busy
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(f.contents(), LEGACY);
    f.no_staged_files();
}

#[test]
fn external_edit_before_publication_wins_over_prepared_conversion() {
    for replace_inode in [false, true] {
        let f = Fixture::new(LEGACY);
        let external = LEGACY.replace("192.0.2.4", "192.0.2.8");
        let result = migrate_with(&f.path(), &mut |p| {
            if p == Point::BeforePublish {
                if replace_inode {
                    let replacement = f.0.join("replacement");
                    fs::write(&replacement, &external)?;
                    fs::rename(replacement, f.path())?;
                } else {
                    fs::write(f.path(), &external)?;
                }
            }
            Ok(())
        });
        assert_eq!(
            result.unwrap_err(),
            AutomaticMigrationError::ConfigurationChanged
        );
        assert_eq!(f.contents(), external);
        assert!(!f.0.join("tvs").exists());
        f.no_staged_files();
    }
}

#[test]
fn prepublication_failures_preserve_source_and_remove_only_owned_staging() {
    for fail in [
        Point::Write(WritePoint::StageWrite),
        Point::Write(WritePoint::FileSync),
        Point::BeforePublish,
        Point::Write(WritePoint::ConfigRename),
    ] {
        let f = Fixture::new(LEGACY);
        let before = fingerprint(&f.path());
        let result = migrate_with(&f.path(), &mut |p| {
            if p == fail {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        });
        assert_eq!(
            result.unwrap_err(),
            AutomaticMigrationError::Storage,
            "{fail:?}"
        );
        assert_eq!(f.contents(), LEGACY);
        assert_eq!(fingerprint(&f.path()), before);
        assert!(!f.0.join("tvs").exists());
        f.no_staged_files();
        converted(migrate_config(&f.path()).unwrap());
    }
}

#[test]
fn ambiguous_publication_reconciles_success_or_reports_indeterminate_without_rollback() {
    for external in [false, true] {
        let f = Fixture::new(LEGACY);
        let result = migrate_with(&f.path(), &mut |p| {
            if p == Point::Write(WritePoint::RenameResult) {
                if external {
                    fs::write(f.path(), "external")?;
                }
                return Err(io::Error::other("ambiguous rename"));
            }
            Ok(())
        });
        if external {
            assert_eq!(
                result.unwrap_err(),
                AutomaticMigrationError::CommitIndeterminate
            );
            assert_eq!(f.contents(), "external");
        } else {
            converted(result.unwrap());
        }
        assert!(!f.0.join("tvs").exists());
    }
}

#[test]
fn committed_durability_and_reload_errors_never_restore_legacy_bytes() {
    for fail in [Point::Write(WritePoint::DirectorySync), Point::Reload] {
        let f = Fixture::new(LEGACY);
        let result = migrate_with(&f.path(), &mut |p| {
            if p == fail {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        });
        if fail == Point::Reload {
            assert_eq!(
                result.unwrap_err(),
                AutomaticMigrationError::CommittedReloadFailed
            );
        } else {
            assert!(matches!(
                result,
                Ok(MigrationOutcome::Converted {
                    durability_warning: true,
                    ..
                })
            ));
        }
        assert!(load_current_config(&f.path()).is_ok());
        assert!(!f.0.join("tvs").exists());
        let before = fingerprint(&f.path());
        assert!(matches!(
            migrate_config(&f.path()),
            Ok(MigrationOutcome::Current(_))
        ));
        assert_eq!(fingerprint(&f.path()), before);
    }
}

#[test]
fn intervening_postcommit_edit_is_preserved_but_not_admitted_for_startup() {
    let f = Fixture::new(LEGACY);
    let external = current_config().replace("192.0.2.4", "192.0.2.8");
    let result = migrate_with(&f.path(), &mut |p| {
        if p == Point::Reload {
            fs::write(f.path(), &external)?;
        }
        Ok(())
    });
    assert_eq!(
        result.unwrap_err(),
        AutomaticMigrationError::CommittedReloadFailed
    );
    assert_eq!(f.contents(), external);
}

#[test]
fn crash_child() {
    let Some(path) = std::env::var_os("LG_BUDDY_AUTOMATIC_CRASH_CONFIG") else {
        return;
    };
    let after = std::env::var("LG_BUDDY_AUTOMATIC_CRASH_AFTER").unwrap() == "yes";
    migrate_with(Path::new(&path), &mut |p| {
        if p == Point::Write(if after {
            WritePoint::RenameResult
        } else {
            WritePoint::FileSync
        }) {
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
        }
        Ok(())
    })
    .unwrap();
    panic!("fixture did not terminate at its crash point");
}

#[test]
fn process_death_before_and_after_rename_is_recoverable_on_restart() {
    use std::os::unix::process::ExitStatusExt;
    for after in [false, true] {
        let f = Fixture::new(LEGACY);
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["migration::automatic::tests::crash_child", "--exact"])
            .env("LG_BUDDY_AUTOMATIC_CRASH_CONFIG", f.path())
            .env(
                "LG_BUDDY_AUTOMATIC_CRASH_AFTER",
                if after { "yes" } else { "no" },
            );
        let result = crate::command::run_status_bounded(command, Duration::from_secs(5));
        assert!(!result.timed_out);
        assert_eq!(result.status.and_then(|s| s.signal()), Some(libc::SIGKILL));
        if after {
            let before = fingerprint(&f.path());
            assert!(matches!(
                migrate_config(&f.path()),
                Ok(MigrationOutcome::Current(_))
            ));
            assert_eq!(fingerprint(&f.path()), before);
        } else {
            assert_eq!(f.contents(), LEGACY);
            converted(migrate_config(&f.path()).unwrap());
        }
        assert!(load_current_config(&f.path()).is_ok());
        assert!(!f.0.join("tvs").exists());
    }
}

#[test]
fn nonowner_refusal_child() {
    let Some(path) = std::env::var_os("LG_BUDDY_AUTOMATIC_NONOWNER_CONFIG") else {
        return;
    };
    assert_ne!(unsafe { libc::geteuid() }, 0);
    assert_eq!(
        migrate_config(Path::new(&path)).unwrap_err(),
        AutomaticMigrationError::Storage
    );
}

#[test]
fn root_and_foreign_owned_configs_cannot_be_converted() {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let f = Fixture::new(LEGACY);
    assert_eq!(
        migrate_config(&f.path()).unwrap_err(),
        AutomaticMigrationError::Storage
    );
    assert_eq!(f.contents(), LEGACY);
    fs::set_permissions(&f.0, fs::Permissions::from_mode(0o755)).unwrap();
    let file = fs::File::open(f.path()).unwrap();
    assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 65533, 65533) }, 0);
    assert_eq!(
        migrate_config(&f.path()).unwrap_err(),
        AutomaticMigrationError::Storage
    );
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "migration::automatic::tests::nonowner_refusal_child",
            "--exact",
        ])
        .env("LG_BUDDY_AUTOMATIC_NONOWNER_CONFIG", f.path())
        .uid(65534)
        .gid(65534);
    let result = crate::command::run_bounded_command(command, Duration::from_secs(5));
    assert!(
        result.succeeded(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(f.contents(), LEGACY);
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
}

#[test]
fn ordinary_read_only_loader_does_not_migrate() {
    let f = Fixture::new(LEGACY);
    assert!(matches!(
        load_current_config(&f.path()),
        Err(crate::config::ConfigLoadError::Stale(_))
    ));
    assert_eq!(f.contents(), LEGACY);
    assert_eq!(fs::read_dir(&f.0).unwrap().count(), 1);
}

#[test]
fn relative_path_child() {
    if std::env::var_os("LG_BUDDY_AUTOMATIC_RELATIVE_PATH").is_none() {
        return;
    }
    assert_eq!(
        converted(migrate_config(Path::new("config.env")).unwrap()).path,
        Path::new("config.env")
    );
}

#[test]
fn relative_path_conversion_syncs_the_current_directory() {
    let f = Fixture::new(LEGACY);
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "migration::automatic::tests::relative_path_child",
            "--exact",
        ])
        .env("LG_BUDDY_AUTOMATIC_RELATIVE_PATH", "yes")
        .current_dir(&f.0);
    let result = crate::command::run_bounded_command(command, Duration::from_secs(5));
    assert!(
        result.succeeded(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(load_current_config(&f.path()).is_ok());
}
