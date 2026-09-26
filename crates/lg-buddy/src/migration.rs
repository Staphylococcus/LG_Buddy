//! Pure migration planning: inspect an already-read config snapshot, decide
//! what a migration requires, and render a validated candidate.
//!
//! This module owns no file I/O, no pairing, and no runtime gates. A produced
//! candidate is a validated *proposal*: it validates config syntax and
//! staleness against the same rules the runtime loader uses
//! (`config::parse_current_config`). It is not a proof that TV pairing or
//! capability/readiness checks passed, and not runtime authorisation. The
//! future executor performs those checks (after acknowledgement, before
//! publication) and owns publication.

use std::fmt;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use crate::config::{
    parse_config, parse_config_entries, parse_current_config, stale_config_reasons, ConfigError,
    ConfigLoadError, HdmiInput, MacAddress, ScreenIdleBlankPolicy, StaleConfigReason,
};
use crate::pairing::PairingRequest;
use crate::presentation::brightness::UserFacingError;
use crate::settings::{ConfigEnvEditor, SettingValue};

#[cfg(test)]
mod tests;

/// Result of inspecting a config snapshot.
#[derive(Debug)]
pub enum MigrationInspection {
    /// The snapshot is a current, parseable configuration; no migration.
    Current,
    /// The snapshot is stale and requires a migration plan.
    Required(MigrationPlan),
}

impl MigrationInspection {
    pub fn is_current(&self) -> bool {
        matches!(self, Self::Current)
    }
}

/// Error raised while inspecting a snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationInspectionError {
    /// A required profile field is missing from the snapshot.
    MissingRequiredKey(String),
    /// A required profile field has a value that does not parse.
    InvalidValue {
        key: String,
        value: String,
        expected: String,
    },
    /// `screen_idle_blank` is present with a value that is not `enabled`/`disabled`.
    InvalidScreenIdleBlank(String),
    /// The preserved TV profile fails native pairing validation and the
    /// migration requires pairing. Carries the same static guidance the
    /// pairing flow shows; it never contains config text.
    InvalidPairingProfile(UserFacingError),
}

impl fmt::Display for MigrationInspectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRequiredKey(key) => write!(f, "required field `{key}` is missing"),
            Self::InvalidValue {
                key,
                value,
                expected,
            } => write!(f, "field `{key}` has value `{value}`, expected {expected}"),
            Self::InvalidScreenIdleBlank(value) => write!(
                f,
                "screen_idle_blank has value `{value}`, expected `enabled` or `disabled`"
            ),
            Self::InvalidPairingProfile(error) => write!(
                f,
                "the preserved TV profile cannot be paired natively: {} — {}",
                error.summary(),
                error.detail()
            ),
        }
    }
}

/// Primary TV profile preserved from the existing config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TvProfile {
    address: Ipv4Addr,
    mac: MacAddress,
    input: HdmiInput,
}

impl TvProfile {
    pub fn address(&self) -> Ipv4Addr {
        self.address
    }

    pub fn mac(&self) -> MacAddress {
        self.mac
    }

    pub fn input(&self) -> HdmiInput {
        self.input
    }

    /// Build a validated native pairing request from the preserved profile,
    /// using the same validation rules as the normal pairing flow.
    pub fn pairing_request(&self) -> Result<PairingRequest, UserFacingError> {
        PairingRequest::parse(&self.address.to_string(), &self.mac.to_string(), self.input)
    }
}

/// The monitoring outcome the migration will produce, and the choice a caller
/// supplies when one is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitoringChoice {
    /// Write `screen_backend=auto` and preserve the existing idle-blanking
    /// setting (including its absence/default). Requires a native-monitoring
    /// check by the executor, after acknowledgement and before publication.
    Native,
    /// Write `screen_backend=auto` and `screen_idle_blank=disabled`. Requires
    /// no native-monitoring check.
    Disabled,
}

/// Whether the user must choose a `Native`/`Disabled` monitoring outcome for
/// the screen part of the migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenChoiceRequired {
    /// An explicit `Native`/`Disabled` selection is required; absence is a
    /// typed error, not an implicit default.
    Required,
    /// Existing `screen_idle_blank=disabled` makes the outcome fixed: `None`
    /// and `Disabled` produce it, `Native` is rejected.
    FixedDisabled,
    /// No stale `swayidle` backend: the screen choice is not applicable.
    NotApplicable,
}

/// A concrete migration plan for a stale config snapshot.
#[derive(Clone)]
pub struct MigrationPlan {
    /// Where the snapshot came from (context only; not re-read).
    path: PathBuf,
    /// The raw source text, retained privately for future conflict
    /// comparison. Never included in `Debug` output or diagnostics.
    raw: String,
    stale_reasons: Vec<StaleConfigReason>,
    /// Preserved primary TV identity (always present for a required plan).
    profile: TvProfile,
    /// Whether the plan requires native TV pairing (stale platform).
    requires_tv_pairing: bool,
    /// The screen-monitoring choice state.
    choice: ScreenChoiceRequired,
}

impl MigrationPlan {
    /// The stale reasons this plan addresses.
    pub fn stale_reasons(&self) -> &[StaleConfigReason] {
        &self.stale_reasons
    }

    /// The preserved primary TV identity.
    pub fn profile(&self) -> &TvProfile {
        &self.profile
    }

    /// Whether the plan requires the user to pair a native TV.
    pub fn requires_tv_pairing(&self) -> bool {
        self.requires_tv_pairing
    }

    /// The screen-monitoring choice state.
    pub fn screen_choice_required(&self) -> ScreenChoiceRequired {
        self.choice
    }

    /// Select a monitoring outcome and produce a validated candidate.
    ///
    /// `choice` must be `Some(Native)`/`Some(Disabled)` when the choice state
    /// is `Required`. For `FixedDisabled`, `None` and `Disabled` produce the
    /// disabled outcome and `Native` is rejected. For `NotApplicable` (TV-only
    /// migration), only `None` is valid; any choice is rejected as irrelevant.
    pub fn select(
        &self,
        choice: Option<MonitoringChoice>,
    ) -> Result<MigrationCandidate, MigrationSelectionError> {
        let screen = match (self.choice, choice) {
            (ScreenChoiceRequired::Required, Some(choice)) => choice.into(),
            (ScreenChoiceRequired::Required, None) => {
                return Err(MigrationSelectionError::ChoiceRequired);
            }
            (ScreenChoiceRequired::FixedDisabled, Some(MonitoringChoice::Native)) => {
                return Err(MigrationSelectionError::NativeRejected);
            }
            (ScreenChoiceRequired::FixedDisabled, Some(MonitoringChoice::Disabled)) => {
                ScreenWrite::Disabled
            }
            (ScreenChoiceRequired::FixedDisabled, None) => ScreenWrite::Disabled,
            (ScreenChoiceRequired::NotApplicable, Some(_)) => {
                return Err(MigrationSelectionError::IrrelevantChoice);
            }
            (ScreenChoiceRequired::NotApplicable, None) => ScreenWrite::None,
        };

        // Every successful select enforces the documented guarantee: the
        // rendered candidate passes the shared current-config contents
        // validator before it is returned. `validate_current` remains as a
        // convenience, but the invariant no longer depends on caller
        // discipline.
        let candidate = self.render(screen);
        parse_current_config(&candidate.rendered)
            .map_err(|_| MigrationSelectionError::InvalidCandidate)?;
        Ok(candidate)
    }

    fn render(&self, screen: ScreenWrite) -> MigrationCandidate {
        // Operate on a private clone of the source lines; the stored `raw`
        // and the caller's snapshot are left untouched.
        let mut editor = ConfigEnvEditor::parse(self.path.clone(), &self.raw);

        if self.requires_tv_pairing {
            editor.set("tvs_primary_platform", SettingValue::Enum("lg_webos"));
        }

        match screen {
            ScreenWrite::None => {}
            ScreenWrite::Native => {
                // Preserve the prior idle-blanking setting (including
                // absence/default) — do not touch screen_idle_blank.
                editor.set("screen_backend", SettingValue::Enum("auto"));
            }
            ScreenWrite::Disabled => {
                editor.set("screen_backend", SettingValue::Enum("auto"));
                editor.set("screen_idle_blank", SettingValue::Enum("disabled"));
            }
        }

        MigrationCandidate {
            path: self.path.clone(),
            rendered: editor.render(),
            requires_tv_pairing: self.requires_tv_pairing,
            requires_native_check: matches!(screen, ScreenWrite::Native),
        }
    }
}

impl fmt::Debug for MigrationPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MigrationPlan")
            .field("path", &self.path)
            .field("stale_reasons", &self.stale_reasons)
            .field("profile", &self.profile)
            .field("requires_tv_pairing", &self.requires_tv_pairing)
            .field("choice", &self.choice)
            // `raw` intentionally omitted: it may contain credentials.
            .finish()
    }
}

/// What the screen portion of a candidate should write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenWrite {
    None,
    Native,
    Disabled,
}

impl From<MonitoringChoice> for ScreenWrite {
    fn from(value: MonitoringChoice) -> Self {
        match value {
            MonitoringChoice::Native => Self::Native,
            MonitoringChoice::Disabled => Self::Disabled,
        }
    }
}

/// Error raised when `select` is called with an inappropriate choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationSelectionError {
    /// A choice is required but `None` was supplied.
    ChoiceRequired,
    /// `Native` was supplied but the outcome is already fixed as disabled.
    NativeRejected,
    /// A choice was supplied when none is applicable (TV-only migration).
    IrrelevantChoice,
    /// The rendered candidate failed the shared current-config validator.
    /// Omits parser diagnostics, which may contain raw config values.
    InvalidCandidate,
}

impl fmt::Display for MigrationSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChoiceRequired => {
                write!(f, "a monitoring choice (Native or Disabled) is required")
            }
            Self::NativeRejected => write!(
                f,
                "Native monitoring cannot be selected: screen_idle_blank is already disabled"
            ),
            Self::IrrelevantChoice => {
                write!(f, "a monitoring choice is not applicable to this migration")
            }
            Self::InvalidCandidate => {
                write!(f, "the rendered candidate is not a valid current config")
            }
        }
    }
}

/// A validated migration candidate: the rendered config text ready for
/// internal storage, plus the facts the future executor must honour.
/// Validation covers config syntax and staleness only; it does not prove
/// pairing/readiness completion and is not runtime authorisation.
#[derive(Clone, PartialEq, Eq)]
pub struct MigrationCandidate {
    path: PathBuf,
    rendered: String,
    requires_tv_pairing: bool,
    requires_native_check: bool,
}

impl fmt::Debug for MigrationCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MigrationCandidate")
            .field("path", &self.path)
            .field("requires_tv_pairing", &self.requires_tv_pairing)
            .field("requires_native_check", &self.requires_native_check)
            // `rendered` intentionally omitted: it is the full config text
            // and may contain unknown/credential-bearing keys.
            .finish()
    }
}

impl MigrationCandidate {
    /// The rendered candidate config text.
    pub fn rendered(&self) -> &str {
        &self.rendered
    }

    /// The config path this candidate is destined for.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether the candidate also requires native TV pairing.
    pub fn requires_tv_pairing(&self) -> bool {
        self.requires_tv_pairing
    }

    /// Whether the candidate requires a native-monitoring capability check:
    /// the future executor performs it after acknowledgement and before
    /// publication.
    pub fn requires_native_check(&self) -> bool {
        self.requires_native_check
    }

    /// Validate the rendered candidate against the shared current-config
    /// contents validator. Every produced candidate must pass.
    pub fn validate_current(&self) -> Result<crate::config::Config, ConfigLoadError> {
        crate::config::parse_current_config(&self.rendered)
    }
}

/// Inspect an already-read config snapshot at `path`.
///
/// Missing/unreadable file handling is the caller's job; this receives one
/// already-read text snapshot. Returns `Current` for a clean parseable config,
/// or a `Required` plan for a stale one.
pub fn inspect_config(
    path: &Path,
    contents: &str,
) -> Result<MigrationInspection, MigrationInspectionError> {
    // Stale recognition on the raw entries, before defaults are applied.
    let stale = stale_config_reasons(contents);

    // Validate the required profile fields (and extract the preserved
    // identity); malformed or missing fields fail inspection.
    let config = parse_config(contents).map_err(map_config_error)?;

    if stale.is_empty() {
        return Ok(MigrationInspection::Current);
    }

    let tv_stale = stale.iter().any(|reason| {
        matches!(
            reason,
            StaleConfigReason::MissingTvPlatform | StaleConfigReason::BscpylgtvPlatform
        )
    });
    let screen_stale = stale
        .iter()
        .any(|reason| matches!(reason, StaleConfigReason::SwayidleBackend));

    let raw = parse_config_entries(contents);
    // An explicitly invalid `screen_idle_blank` must be reported, not
    // defaulted to enabled/disabled — but only for an actual stale
    // swayidle backend, where the value is load-bearing. A current config
    // or a TV-only migration keeps the runtime's optional fallback
    // semantics for an unrelated value.
    if screen_stale {
        if let Some(value) = raw.get("screen_idle_blank") {
            if value.parse::<ScreenIdleBlankPolicy>().is_err() {
                return Err(MigrationInspectionError::InvalidScreenIdleBlank(
                    value.clone(),
                ));
            }
        }
    }
    let idle_disabled = raw
        .get("screen_idle_blank")
        .is_some_and(|value| value == "disabled");

    let choice = if screen_stale {
        if idle_disabled {
            ScreenChoiceRequired::FixedDisabled
        } else {
            ScreenChoiceRequired::Required
        }
    } else {
        ScreenChoiceRequired::NotApplicable
    };

    let profile = TvProfile {
        address: config.tv_ip,
        mac: config.tv_mac,
        input: config.input,
    };
    // A plan that requires native pairing must carry a pairable profile:
    // validate during inspection instead of deferring the parse failure to
    // an optional later caller.
    if tv_stale {
        profile
            .pairing_request()
            .map(|_| ())
            .map_err(MigrationInspectionError::InvalidPairingProfile)?;
    }

    Ok(MigrationInspection::Required(MigrationPlan {
        path: path.to_path_buf(),
        raw: contents.to_string(),
        stale_reasons: stale,
        profile,
        requires_tv_pairing: tv_stale,
        choice,
    }))
}

fn map_config_error(err: ConfigError) -> MigrationInspectionError {
    match err {
        ConfigError::MissingRequiredKey(key) => {
            MigrationInspectionError::MissingRequiredKey(key.to_string())
        }
        ConfigError::InvalidValue {
            key,
            value,
            expected,
        } => MigrationInspectionError::InvalidValue {
            key: key.to_string(),
            value,
            expected: expected.to_string(),
        },
        ConfigError::Io(err) => MigrationInspectionError::InvalidValue {
            key: String::from("<io>"),
            value: err.to_string(),
            expected: String::from("a readable config"),
        },
    }
}
