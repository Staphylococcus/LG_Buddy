//! Saved TV metadata and stored-credential-only model reads.

use super::{report::safe_text, DiagnosticSection};
use crate::config::TvPlatform;
use crate::tvs::{EnvironmentTvsBackend, TvCredentialState, TvsBackend};

const MAX_TV_MODEL_BYTES: usize = 512;

pub(super) fn collect() -> DiagnosticSection {
    let backend = EnvironmentTvsBackend;
    collect_from_backend(&backend)
}

fn collect_from_backend(backend: &impl TvsBackend) -> DiagnosticSection {
    let profiles = match backend.read_profiles() {
        Ok(profiles) => profiles,
        Err(_) => {
            return DiagnosticSection::new(
                "TV observation",
                "TV profiles: could not read saved configuration or credential metadata.",
            )
        }
    };

    if profiles.is_empty() {
        return DiagnosticSection::new("TV observation", "No TV profile is configured.");
    }

    let mut body = String::new();
    for profile in profiles.iter().take(4) {
        let profile_id = profile.id().to_string();
        body.push_str("profile ");
        body.push_str(safe_text(&profile_id, 128).trim_end());
        body.push_str(": address=");
        body.push_str(&profile.address().to_string());
        body.push_str(", input=");
        body.push_str(profile.input_label());
        body.push_str(", platform=");
        body.push_str(profile.platform_label());
        body.push_str(", local credential state=");
        body.push_str(profile.credentials().label());
        body.push('\n');
        // Only native webOS enforces stored-credential-only authentication.
        // A compatibility credential file may lack a key for this TV, causing
        // its model read to initiate pairing instead.
        match (profile.platform(), profile.credentials()) {
            (TvPlatform::LgWebOs, TvCredentialState::Stored) => {
                match backend.read_model_name(profile) {
                    Ok(model) => {
                        body.push_str("model read: succeeded (model=");
                        body.push_str(safe_text(&model, MAX_TV_MODEL_BYTES).trim_end());
                        body.push_str(")\n");
                    }
                    Err(_) => body.push_str("model read: failed (connection or authentication)\n"),
                }
            }
            (TvPlatform::Bscpylgtv, _) => {
                body.push_str("model read: skipped (compatibility backend)\n")
            }
            _ => body.push_str("model read: skipped (no stored credential)\n"),
        }
    }
    DiagnosticSection::new("TV observation", body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tvs::TvsReadError;
    #[test]
    fn tv_diagnostics_only_probe_native_profiles_with_stored_credentials() {
        use crate::config::HdmiInput;
        use crate::tvs::TvProfile;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct ProfileBackend {
            profile: TvProfile,
            reads: AtomicUsize,
        }

        impl TvsBackend for ProfileBackend {
            fn read_profiles(&self) -> Result<Vec<TvProfile>, TvsReadError> {
                Ok(vec![self.profile.clone()])
            }

            fn read_model_name(&self, profile: &TvProfile) -> Result<String, TvsReadError> {
                assert_eq!(profile.platform(), TvPlatform::LgWebOs);
                assert_eq!(profile.credentials(), TvCredentialState::Stored);
                self.reads.fetch_add(1, Ordering::Relaxed);
                Ok("OLED fixture".to_string())
            }
        }

        for platform in [TvPlatform::Bscpylgtv, TvPlatform::LgWebOs] {
            for credentials in [
                TvCredentialState::Stored,
                TvCredentialState::LocalFile,
                TvCredentialState::Missing,
                TvCredentialState::Malformed,
                TvCredentialState::Unreadable,
                TvCredentialState::Unknown,
            ] {
                let backend = ProfileBackend {
                    profile: TvProfile::new(
                        "primary",
                        "Fixture TV",
                        "192.0.2.42".parse().unwrap(),
                        "aa:bb:cc:dd:ee:ff".parse().unwrap(),
                        HdmiInput::Hdmi1,
                        platform,
                        credentials,
                    ),
                    reads: AtomicUsize::new(0),
                };
                let section = collect_from_backend(&backend);
                if platform == TvPlatform::LgWebOs && credentials == TvCredentialState::Stored {
                    assert_eq!(backend.reads.load(Ordering::Relaxed), 1);
                    assert!(section.body().contains("model read: succeeded"));
                    assert!(section.body().contains("OLED fixture"));
                } else {
                    assert_eq!(backend.reads.load(Ordering::Relaxed), 0);
                    assert!(section.body().contains("model read: skipped"));
                    assert!(!section.body().contains("OLED fixture"));
                }
                if platform == TvPlatform::Bscpylgtv {
                    assert!(section.body().contains("skipped (compatibility backend)"));
                }
            }
        }
    }

    #[test]
    fn no_profiles_and_failed_profile_reads_do_not_probe_a_tv() {
        struct Backend {
            fail: bool,
        }
        impl TvsBackend for Backend {
            fn read_profiles(&self) -> Result<Vec<crate::tvs::TvProfile>, TvsReadError> {
                if self.fail {
                    Err(TvsReadError::internal("private-error"))
                } else {
                    Ok(Vec::new())
                }
            }
            fn read_model_name(&self, _: &crate::tvs::TvProfile) -> Result<String, TvsReadError> {
                panic!("no TV may be contacted without a usable profile");
            }
        }
        assert!(collect_from_backend(&Backend { fail: false })
            .body()
            .contains("No TV profile is configured"));
        let section = collect_from_backend(&Backend { fail: true });
        assert!(section
            .body()
            .contains("could not read saved configuration"));
        assert!(!section.body().contains("private-error"));
    }
}
