use super::*;
use crate::config::ScreenBackend;
use std::{fs, os::unix::fs::PermissionsExt, sync::Mutex};
struct Fixture(PathBuf);
impl Fixture {
    fn new(platform: Option<&str>, screen: &str, idle: Option<&str>) -> Self {
        let path = std::env::temp_dir().join(format!(
            "lg-flow-{}-{}",
            std::process::id(),
            next_revision().0
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let mut raw=format!("# retain\ntv_ip=192.0.2.4\ntv_mac=aa:bb:cc:dd:ee:ff\ninput=HDMI_1\nscreen_backend={screen}\n");
        if let Some(p) = platform {
            raw.push_str(&format!("tvs_primary_platform={p}\n"));
        }
        if let Some(i) = idle {
            raw.push_str(&format!("screen_idle_blank={i}\n"));
        }
        fs::write(path.join("config.env"), raw).unwrap();
        Self(path)
    }
    fn path(&self) -> PathBuf {
        self.0.join("config.env")
    }
    fn flow(&self, fake: Fake) -> MigrationFlow {
        MigrationFlow::with_preparation(&self.path(), &self.0.join("setup.lock"), Box::new(fake))
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Fake {
    calls: Arc<Mutex<Vec<&'static str>>>,
    pair_failure: Option<PairingFailure>,
    native_failure: bool,
}
impl Preparation for Fake {
    fn pair(
        &mut self,
        op: &PairingOperation,
        _: Option<&PlatformAccessToken>,
        progress: &mut dyn FnMut(PairingStage),
    ) -> Result<PlatformAccessToken, PairingFailure> {
        assert_eq!(op.request().address().to_string(), "192.0.2.4");
        self.calls.lock().unwrap().push("pair");
        progress(PairingStage::Verifying);
        if let Some(e) = self.pair_failure {
            Err(e)
        } else {
            Ok(PlatformAccessToken::new("prepared").unwrap())
        }
    }
    fn native(&mut self, _: &AtomicBool) -> Result<BackendResolution, NativeReadinessError> {
        self.calls.lock().unwrap().push("native");
        if self.native_failure {
            Err(NativeReadinessError::Unavailable)
        } else {
            Ok(BackendResolution::selected(ScreenBackend::Wayland, None))
        }
    }
}
#[test]
fn required_outcome_matrix_commits_and_reloads_without_services() {
    for (platform, screen, idle, choice, want) in [
        (None, "auto", None, None, vec!["pair"]),
        (Some("bscpylgtv"), "auto", None, None, vec!["pair"]),
        (
            Some("lg_webos"),
            "swayidle",
            None,
            Some(MonitoringChoice::Native),
            vec!["native"],
        ),
        (
            Some("lg_webos"),
            "swayidle",
            None,
            Some(MonitoringChoice::Disabled),
            vec![],
        ),
        (
            None,
            "swayidle",
            None,
            Some(MonitoringChoice::Native),
            vec!["pair", "native"],
        ),
        (
            None,
            "swayidle",
            None,
            Some(MonitoringChoice::Disabled),
            vec!["pair"],
        ),
        (None, "swayidle", Some("disabled"), None, vec!["pair"]),
        (Some("lg_webos"), "swayidle", Some("disabled"), None, vec![]),
    ] {
        let f = Fixture::new(platform, screen, idle);
        let fake = Fake::default();
        let calls = fake.calls.clone();
        let mut flow = f.flow(fake);
        assert!(calls.lock().unwrap().is_empty());
        let attempt = flow.acknowledge(flow.revision(), choice).unwrap();
        assert!(calls.lock().unwrap().is_empty());
        let completed = flow.execute(attempt, &mut |_| {}).unwrap();
        assert!(completed.startup.is_ok());
        assert_eq!(*calls.lock().unwrap(), want);
        assert!(!completed.durability_warning);
        assert!(load_current_config(&f.path()).is_ok());
        let bytes = fs::read_to_string(f.path()).unwrap();
        assert!(bytes.contains("# retain"));
    }
}
#[test]
fn native_failure_can_retry_disabled_but_cannot_bypass_tv_verification() {
    let f = Fixture::new(None, "swayidle", None);
    let fake = Fake {
        native_failure: true,
        ..Fake::default()
    };
    let calls = fake.calls.clone();
    let mut flow = f.flow(fake);
    let original = fs::read(f.path()).unwrap();
    let a = flow
        .acknowledge(flow.revision(), Some(MonitoringChoice::Native))
        .unwrap();
    assert_eq!(
        flow.execute(a, &mut |_| {}).err().unwrap().failure,
        MigrationFailure::NativeMonitoring
    );
    assert_eq!(fs::read(f.path()).unwrap(), original);
    assert!(!f.0.join("tvs").exists());
    let a = flow
        .acknowledge(flow.revision(), Some(MonitoringChoice::Disabled))
        .unwrap();
    assert!(flow.execute(a, &mut |_| {}).unwrap().startup.is_ok());
    assert_eq!(*calls.lock().unwrap(), vec!["pair", "native", "pair"]);
}
#[test]
fn disabled_pairing_failure_retains_stale_gate_and_reports_legacy_value() {
    let f = Fixture::new(Some("bscpylgtv"), "swayidle", None);
    let mut flow = f.flow(Fake {
        pair_failure: Some(PairingFailure::Verification),
        ..Fake::default()
    });
    let raw = fs::read(f.path()).unwrap();
    let a = flow
        .acknowledge(flow.revision(), Some(MonitoringChoice::Disabled))
        .unwrap();
    let e = flow.execute(a, &mut |_| {}).err().unwrap();
    assert_eq!(
        e.failure,
        MigrationFailure::Pairing(PairingFailure::Verification)
    );
    assert!(e.to_string().contains("bscpylgtv"));
    assert!(e.to_string().to_ascii_lowercase().contains("retry"));
    assert_eq!(fs::read(f.path()).unwrap(), raw);
    assert!(!f.0.join("tvs").exists());
}
#[test]
fn accepted_cancel_wins_phase_failure_and_never_publishes() {
    let f = Fixture::new(None, "swayidle", None);
    let mut flow = f.flow(Fake {
        pair_failure: Some(PairingFailure::Connection),
        ..Fake::default()
    });
    let a = flow
        .acknowledge(flow.revision(), Some(MonitoringChoice::Native))
        .unwrap();
    let cancel = a.cancellation();
    let e = flow
        .execute(a, &mut |p| {
            if p.stage == MigrationStage::Pairing(PairingStage::Verifying) {
                assert!(cancel.cancel());
            }
        })
        .err()
        .unwrap();
    assert_eq!(e.failure, MigrationFailure::Cancelled);
    assert!(!f.0.join("tvs").exists());
    assert!(!cancel.can_cancel());
}
#[test]
fn no_acknowledgement_no_effects_and_old_attempt_cannot_publish() {
    let f = Fixture::new(None, "swayidle", None);
    let fake = Fake::default();
    let calls = fake.calls.clone();
    let mut flow = f.flow(fake);
    let revision = flow.revision();
    assert_eq!(
        flow.acknowledge(revision, None).err().unwrap().failure,
        MigrationFailure::InvalidChoice
    );
    let a = flow
        .acknowledge(revision, Some(MonitoringChoice::Native))
        .unwrap();
    let old_cancel = a.cancellation();
    let b = flow
        .acknowledge(flow.revision(), Some(MonitoringChoice::Disabled))
        .unwrap();
    assert_eq!(
        flow.execute(a, &mut |_| panic!("stale progress"))
            .err()
            .unwrap()
            .failure,
        MigrationFailure::StaleAttempt
    );
    assert!(calls.lock().unwrap().is_empty());
    assert!(!old_cancel.cancel());
    assert!(flow.execute(b, &mut |_| {}).is_ok());
}
#[test]
fn mutation_during_preparation_rejects_config_and_same_lease_excludes_setup() {
    let f = Fixture::new(None, "swayidle", None);
    let mut flow = f.flow(Fake::default());
    assert!(FlowLock::acquire(&f.0.join("setup.lock")).is_err());
    let a = flow
        .acknowledge(flow.revision(), Some(MonitoringChoice::Disabled))
        .unwrap();
    let result = flow.execute(a, &mut |p| {
        if p.stage == MigrationStage::Pairing(PairingStage::Verifying) {
            let mut editor = crate::settings::ConfigEnvEditor::load(f.path()).unwrap();
            editor.set("unrelated", crate::settings::SettingValue::Enum("saved"));
            editor.save().unwrap();
        }
    });
    assert_eq!(
        result.err().unwrap().failure,
        MigrationFailure::ConfigurationChanged
    );
    assert!(fs::read_to_string(f.path())
        .unwrap()
        .contains("unrelated=saved"));
    assert!(!f.0.join("tvs").exists());
}

#[test]
fn startup_failure_after_publication_is_committed_without_credential_rollback() {
    let f = Fixture::new(None, "swayidle", None);
    let mut flow = f.flow(Fake::default());
    let attempt = flow
        .acknowledge(flow.revision(), Some(MonitoringChoice::Disabled))
        .unwrap();
    let cancel = attempt.cancellation();
    let completion = flow
        .execute_with_startup(attempt, &mut |_| {}, |path| {
            assert!(load_current_config(path).is_ok());
            Err(MigrationStartupFailure::ReloadFailed)
        })
        .unwrap();
    assert!(matches!(
        completion.startup,
        Err(MigrationStartupFailure::ReloadFailed)
    ));
    assert!(load_current_config(&f.path()).is_ok());
    assert!(f.0.join("tvs/primary/access-token.json").exists());
    assert!(!cancel.cancel());
}

#[test]
fn cancellation_at_each_prepublication_stage_leaves_config_stale() {
    for stage in [
        None,
        Some(MigrationStage::NativeMonitoring),
        Some(MigrationStage::Committing),
    ] {
        let f = Fixture::new(None, "swayidle", None);
        let mut flow = f.flow(Fake::default());
        let original = fs::read(f.path()).unwrap();
        let attempt = flow
            .acknowledge(flow.revision(), Some(MonitoringChoice::Native))
            .unwrap();
        let cancel = attempt.cancellation();
        if stage.is_none() {
            assert!(cancel.cancel());
        }
        let result = flow.execute(attempt, &mut |p| {
            if stage == Some(p.stage) {
                assert!(cancel.cancel());
            }
        });
        assert_eq!(result.err().unwrap().failure, MigrationFailure::Cancelled);
        assert_eq!(fs::read(f.path()).unwrap(), original);
        assert!(!f.0.join("tvs").exists());
    }
}

#[test]
fn frontend_plan_exposes_identity_and_choices_without_unknown_config_values() {
    let f = Fixture::new(None, "swayidle", None);
    let mut raw = fs::read_to_string(f.path()).unwrap();
    raw.push_str("private_unknown_key=keep-this-secret-out-of-frontends\n");
    fs::write(f.path(), raw).unwrap();
    let flow = f.flow(Fake::default());
    let summary = flow.plan();
    assert_eq!(summary.profile.address().to_string(), "192.0.2.4");
    assert_eq!(
        summary.screen_choice,
        super::super::ScreenChoiceRequired::Required
    );
    assert!(summary.requires_tv_pairing);
    assert!(!format!("{summary:?}").contains("keep-this-secret"));
}

#[test]
fn completion_and_cancellation_release_setup_lease_even_if_frontend_retains_flow() {
    for cancel in [false, true] {
        let f = Fixture::new(None, "swayidle", None);
        let mut flow = f.flow(Fake::default());
        let attempt = flow
            .acknowledge(flow.revision(), Some(MonitoringChoice::Disabled))
            .unwrap();
        if cancel {
            assert!(attempt.cancellation().cancel());
        }
        let result = flow.execute(attempt, &mut |_| {});
        assert_eq!(result.is_ok(), !cancel);
        let _next_setup = FlowLock::acquire(&f.0.join("setup.lock"))
            .expect("startup can now acquire the shared setup lease");
        assert_eq!(
            flow.acknowledge(flow.revision(), Some(MonitoringChoice::Disabled))
                .err()
                .unwrap()
                .failure,
            MigrationFailure::StaleAttempt
        );
    }
}

#[test]
fn invalid_profile_reports_failed_field_stale_value_and_recovery() {
    let f = Fixture::new(None, "swayidle", None);
    let original = fs::read_to_string(f.path())
        .unwrap()
        .replace("192.0.2.4", "invalid-address");
    fs::write(f.path(), &original).unwrap();
    let error = MigrationFlow::with_preparation(
        &f.path(),
        &f.0.join("setup.lock"),
        Box::new(Fake::default()),
    )
    .err()
    .unwrap();
    let message = error.to_string();
    assert!(message.contains("tvs_primary_ip"));
    assert!(message.contains("swayidle"));
    assert!(message.contains("reopen migration"));
    assert_eq!(fs::read_to_string(f.path()).unwrap(), original);
    assert!(!f.0.join("tvs").exists());
}

#[test]
fn corrupt_native_credential_reports_repair_without_pairing_or_legacy_import() {
    for credential in ["not JSON", "{}", "{\"access_token\":\"\"}"] {
        let f = Fixture::new(None, "swayidle", None);
        let original = fs::read(f.path()).unwrap();
        let token_path = f.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().unwrap()).unwrap();
        fs::write(&token_path, credential).unwrap();
        let legacy = f.0.join(".aiopylgtv.sqlite");
        fs::write(&legacy, "legacy user-owned bytes").unwrap();
        let fake = Fake::default();
        let calls = fake.calls.clone();
        let mut flow = f.flow(fake);
        let attempt = flow
            .acknowledge(flow.revision(), Some(MonitoringChoice::Disabled))
            .unwrap();
        let error = flow.execute(attempt, &mut |_| {}).err().unwrap();
        assert_eq!(error.failure, MigrationFailure::CredentialInvalid);
        assert!(error.to_string().contains("malformed"));
        assert!(error.to_string().contains("reopen migration"));
        assert!(calls.lock().unwrap().is_empty());
        assert_eq!(fs::read(f.path()).unwrap(), original);
        assert_eq!(fs::read_to_string(token_path).unwrap(), credential);
        assert_eq!(
            fs::read_to_string(legacy).unwrap(),
            "legacy user-owned bytes"
        );
    }
}
