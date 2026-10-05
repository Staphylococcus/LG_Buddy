//! Narrow, consented corrections reuse the normal configuration editor and lock.
use super::{
    recovery::{RecoveryAction, RecoveryCause, RepairBoundary, SetupRecovery},
    StepCancellation, StepFailure, StepInput, StepResponse,
};
use crate::{
    config::HdmiInput,
    pairing::PairingRequest,
    presentation::brightness::UserFacingError,
    settings::{ConfigEnvEditor, ConfigEnvReader, SettingValue, SettingsStore},
};
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) fn load(path: &Path) -> Result<(SettingsStore, ConfigEnvEditor, [u8; 32]), StepFailure> {
    let editor = ConfigEnvEditor::load(path).map_err(external_failure)?;
    let contents = editor.render();
    for line in contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let Some((key, _)) = line.split_once('=') else {
            return Err(external_failure(
                "configuration contains unsupported syntax",
            ));
        };
        if key.trim().is_empty()
            || !key
                .trim()
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(external_failure(
                "configuration contains an unsupported key syntax",
            ));
        }
    }
    let version = Sha256::digest(contents.as_bytes()).into();
    Ok((
        SettingsStore::from_reader(ConfigEnvReader::parse(path, &contents)),
        editor,
        version,
    ))
}

pub(super) fn tv_input(path: &Path) -> StepResponse {
    match load(path) {
        Ok((store, _, revision)) => {
            let raw = |primary, legacy| {
                store
                    .raw_storage_value(primary)
                    .or_else(|| store.raw_storage_value(legacy))
                    .unwrap_or("")
                    .to_owned()
            };
            StepResponse::InputRequired(StepInput::CorrectTv {
                address: raw("tvs_primary_ip", "tv_ip"),
                mac: raw("tvs_primary_mac", "tv_mac"),
                input: raw("tvs_primary_input", "input")
                    .parse()
                    .unwrap_or(HdmiInput::Hdmi1),
                revision,
            })
        }
        Err(error) => StepResponse::Failed(error),
    }
}

pub(super) fn update_input(path: &Path) -> StepResponse {
    match load(path) {
        Ok((_, _, revision)) => {
            StepResponse::InputRequired(StepInput::UpdatePreference { revision })
        }
        Err(error) => StepResponse::Failed(error),
    }
}

pub(super) fn correct_tv(
    path: &Path,
    request: PairingRequest,
    revision: [u8; 32],
    cancellation: &StepCancellation,
) -> StepResponse {
    let result = correct(path, revision, cancellation, |editor| {
        editor.set("tvs_primary_ip", SettingValue::Ipv4(request.address()));
        editor.set("tvs_primary_mac", SettingValue::MacAddress(request.mac()));
        editor.set(
            "tvs_primary_input",
            SettingValue::Enum(request.input().as_str()),
        );
        let store = SettingsStore::from_reader(ConfigEnvReader::parse(path, &editor.render()));
        if store
            .raw_storage_value("tvs_primary_platform")
            .is_some_and(|value| value != "lg_webos" && value != "bscpylgtv")
        {
            editor.set("tvs_primary_platform", SettingValue::Enum("lg_webos"));
        }
    });
    match result {
        Ok(()) => super::pairing::inspect(path),
        Err(response) => response,
    }
}

pub(super) fn correct_updates(
    path: &Path,
    enabled: bool,
    revision: [u8; 32],
    cancellation: &StepCancellation,
) -> StepResponse {
    match correct(path, revision, cancellation, |editor| {
        editor.set(
            "updates_auto_check",
            SettingValue::Enum(if enabled { "enabled" } else { "disabled" }),
        );
    }) {
        Ok(()) => StepResponse::Complete,
        Err(response) => response,
    }
}

fn correct(
    path: &Path,
    revision: [u8; 32],
    cancellation: &StepCancellation,
    edit: impl FnOnce(&mut ConfigEnvEditor),
) -> Result<(), StepResponse> {
    let (_, mut editor, current) = load(path).map_err(StepResponse::Failed)?;
    if current != revision {
        return Err(StepResponse::Failed(StepFailure {
            presentation: UserFacingError::new(
                "Configuration changed",
                "Recheck setup before saving the correction.",
            ),
            diagnostic: "configuration correction version mismatch".into(),
            recovery: SetupRecovery::new(
                RecoveryCause::InvalidConfiguration,
                RepairBoundary::UserInput,
                RecoveryAction::Recheck,
            ),
            retryable: true,
        }));
    }
    if !cancellation.begin() {
        return Err(StepResponse::Cancelled);
    }
    edit(&mut editor);
    let result = editor
        .save()
        .map_err(|error| StepResponse::Failed(external_failure(error)));
    cancellation.finish();
    result
}

fn external_failure(error: impl ToString) -> StepFailure {
    StepFailure {
        presentation: UserFacingError::new("Configuration needs external repair", "LG Buddy cannot safely edit this configuration. Restore readable, supported key=value syntax and user access, then recheck setup. No settings or credentials were reset."),
        diagnostic: error.to_string(),
        recovery: SetupRecovery::new(RecoveryCause::InvalidConfiguration, RepairBoundary::SystemConfiguration, RecoveryAction::RepairExternally), retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new(contents: impl AsRef<[u8]>) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "lg-buddy-correction-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("config.env"), contents).unwrap();
            Self(root)
        }
        fn path(&self) -> PathBuf {
            self.0.join("config.env")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn correction_preserves_credential_and_unrelated_configuration() {
        let fixture = Fixture::new("# Keep this comment\ntvs_primary_ip=broken\ntvs_primary_mac=02:11:22:33:44:55\ntvs_primary_input=HDMI_2\ntvs_primary_platform=lg_webos\nscreen_idle_blank=disabled\nunknown_setting=unchanged\n");
        let token = fixture.0.join("tvs/primary/access-token.json");
        fs::create_dir_all(token.parent().unwrap()).unwrap();
        fs::write(&token, r#"{"access_token":"private-existing-key"}"#).unwrap();
        let token_bytes = fs::read(&token).unwrap();
        let StepResponse::InputRequired(StepInput::CorrectTv { revision, .. }) =
            tv_input(&fixture.path())
        else {
            panic!("correction required")
        };
        let request =
            PairingRequest::parse("192.0.2.42", "02:11:22:33:44:55", HdmiInput::Hdmi2).unwrap();
        let response = correct_tv(
            &fixture.path(),
            request,
            revision,
            &StepCancellation::default(),
        );
        assert_eq!(response, StepResponse::Complete);
        let contents = fs::read_to_string(fixture.path()).unwrap();
        for preserved in [
            "# Keep this comment",
            "screen_idle_blank=disabled",
            "unknown_setting=unchanged",
            "tvs_primary_platform=lg_webos",
        ] {
            assert!(contents.contains(preserved));
        }
        assert!(contents.contains("tvs_primary_ip=192.0.2.42"));
        assert_eq!(fs::read(token).unwrap(), token_bytes);
    }

    #[test]
    fn stale_or_cancelled_correction_never_writes() {
        let fixture = Fixture::new("updates_auto_check=typo\n");
        let (_, _, revision) = load(&fixture.path()).unwrap();
        let cancellation = StepCancellation::default();
        assert!(cancellation.cancel());
        assert_eq!(
            correct_updates(&fixture.path(), false, revision, &cancellation),
            StepResponse::Cancelled
        );
        fs::write(
            fixture.path(),
            "updates_auto_check=typo\nunknown_setting=new-value\n",
        )
        .unwrap();
        let before = fs::read(fixture.path()).unwrap();
        let StepResponse::Failed(error) = correct_updates(
            &fixture.path(),
            false,
            revision,
            &StepCancellation::default(),
        ) else {
            panic!("stale correction rejected")
        };
        assert_eq!(error.recovery.action, RecoveryAction::Recheck);
        assert_eq!(fs::read(fixture.path()).unwrap(), before);
    }

    #[test]
    fn unsupported_syntax_and_unreadable_bytes_are_not_reset() {
        for bytes in [
            b"updates_auto_check=typo\nsource /another/config\n".as_slice(),
            b"updates_auto_check=typo\n\xff".as_slice(),
        ] {
            let fixture = Fixture::new(bytes);
            let StepResponse::Failed(error) = update_input(&fixture.path()) else {
                panic!("external repair required")
            };
            assert!(!error.recovery.can_repair_here());
            assert_eq!(error.recovery.action, RecoveryAction::RepairExternally);
            assert_eq!(fs::read(fixture.path()).unwrap(), bytes);
        }
    }

    #[test]
    fn update_correction_preserves_other_fields_and_requires_a_new_revision() {
        let fixture = Fixture::new("updates_auto_check=typo\nscreen_idle_blank=disabled\n");
        let (_, _, revision) = load(&fixture.path()).unwrap();
        assert_eq!(
            correct_updates(
                &fixture.path(),
                false,
                revision,
                &StepCancellation::default()
            ),
            StepResponse::Complete
        );
        assert_eq!(
            fs::read_to_string(fixture.path()).unwrap(),
            "updates_auto_check=disabled\nscreen_idle_blank=disabled\n"
        );
        assert!(matches!(
            correct_updates(
                &fixture.path(),
                true,
                revision,
                &StepCancellation::default()
            ),
            StepResponse::Failed(_)
        ));
    }
}
