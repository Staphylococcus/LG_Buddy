//! Acknowledged foreground migration. Hosts present the plan once, obtain an
//! attempt and cancellation handle, and execute on a worker. Only this owner
//! can turn in-memory checks into publication; a candidate alone cannot do so.
use super::{
    inspect_config, MigrationCandidate, MigrationInspection, MigrationPlan, MonitoringChoice,
};
use crate::{
    backend::{
        readiness::{check_native_readiness, NativeReadinessContext, NativeReadinessError},
        BackendResolution,
    },
    config::{load_current_config, CurrentConfig, StaleConfigReason},
    pairing::{
        prepare_webos_pairing_in_memory, PairingError, PairingFailure, PairingOperation,
        PairingStage,
    },
    pairing_store::migration::{MigrationSnapshot, MigrationStoreError},
    platform_access_token::PlatformAccessToken,
    setup::{lock::FlowLock, StepCancellation},
    web_os::WebOsEndpoint,
};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

/// Opaque revision; a response from an older plan cannot authorize this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationRevision(u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationFailure {
    Busy,
    AlreadyCurrent,
    InvalidConfiguration,
    InvalidChoice,
    StaleAttempt,
    Cancelled,
    Pairing(PairingFailure),
    NativeMonitoring,
    ConflictingOverride,
    ConfigurationChanged,
    CredentialChanged,
    CredentialInvalid,
    Storage,
    RollbackFailed,
    CommitIndeterminate,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationError {
    pub failure: MigrationFailure,
    pub stale_reasons: Vec<StaleConfigReason>,
    pub inspection: Option<super::MigrationInspectionError>,
}
impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for reason in &self.stale_reasons {
            write!(f, "{}; ", reason.message())?;
        }
        if let Some(inspection) = &self.inspection {
            return write!(
                f,
                "{inspection}; repair the saved profile and reopen migration"
            );
        }
        if let MigrationFailure::Pairing(failure) = self.failure {
            let guidance = PairingError::new(failure).presentation();
            return write!(
                f,
                "{}: {} Retry migration after resolving this check.",
                guidance.summary(),
                guidance.detail()
            );
        }
        f.write_str(match self.failure {
            MigrationFailure::Busy => "another setup or migration is open; finish or close it and retry",
            MigrationFailure::AlreadyCurrent => "configuration is already current; continue normal startup",
            MigrationFailure::InvalidConfiguration => "the existing TV profile or configuration is invalid; repair its saved values and reopen migration",
            MigrationFailure::InvalidChoice => "select an available monitoring outcome before confirming migration",
            MigrationFailure::StaleAttempt => "this confirmation belongs to an older plan; review the current plan and confirm again",
            MigrationFailure::Cancelled => "migration cancelled before publication; retry when ready",
            MigrationFailure::Pairing(_) => "native TV authentication or verification failed; turn on the saved TV, accept its pairing prompt if shown, and retry",
            MigrationFailure::NativeMonitoring => "native GNOME/Wayland readiness failed; repair the desktop integration and retry, or choose disabled idle blanking",
            MigrationFailure::ConflictingOverride => "native monitoring conflicts with LG_BUDDY_SCREEN_BACKEND; remove the override and retry, or choose disabled idle blanking",
            MigrationFailure::ConfigurationChanged => "configuration changed during migration; reopen migration and review the new settings",
            MigrationFailure::CredentialChanged => "the native credential changed during migration; reopen migration and retry verification",
            MigrationFailure::CredentialInvalid => "the saved native TV credential is malformed; repair or remove tvs/primary/access-token.json, then reopen migration to pair again",
            MigrationFailure::Storage => "migration storage check or write failed; run as the config owner, ensure writable non-symlink config and credentials, and retry",
            MigrationFailure::RollbackFailed => "config was not published but credential restoration failed; inspect the native credential and retry migration before startup",
            MigrationFailure::CommitIndeterminate => "publication could not be determined; inspect the config and native credential before retrying or starting",
        })
    }
}
impl std::error::Error for MigrationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationStage {
    Pairing(PairingStage),
    NativeMonitoring,
    Committing,
    Complete,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationProgress {
    pub revision: MigrationRevision,
    pub stage: MigrationStage,
    pub can_cancel: bool,
}

/// A cancellation is accepted only before the atomic commit gate wins. This
/// handle never waits for the filesystem, probe subprocess, or TV connection.
#[derive(Clone)]
pub struct MigrationCancellation {
    gate: StepCancellation,
    stop: Arc<AtomicBool>,
}
impl MigrationCancellation {
    fn new() -> Self {
        Self {
            gate: StepCancellation::default(),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn cancel(&self) -> bool {
        if self.gate.cancel() {
            self.stop.store(true, Ordering::Release);
            true
        } else {
            false
        }
    }
    pub fn can_cancel(&self) -> bool {
        self.gate.can_cancel()
    }
}

/// Produced only by explicit acknowledgement. No public success flags, token,
/// or candidate can be supplied by a frontend.
pub struct MigrationAttempt {
    revision: MigrationRevision,
    candidate: MigrationCandidate,
    cancellation: MigrationCancellation,
}
impl MigrationAttempt {
    pub fn cancellation(&self) -> MigrationCancellation {
        self.cancellation.clone()
    }
}

/// Publication has already happened for every value of this type. Failure to
/// reload is a startup problem, never an invitation to undo the migration.
pub struct MigrationCompletion {
    pub durability_warning: bool,
    pub native_backend: Option<BackendResolution>,
    pub startup: Result<CurrentConfig, MigrationStartupFailure>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationStartupFailure {
    ReloadFailed,
}

trait Preparation: Send {
    fn pair(
        &mut self,
        operation: &PairingOperation,
        token: Option<&PlatformAccessToken>,
        progress: &mut dyn FnMut(PairingStage),
    ) -> Result<PlatformAccessToken, PairingFailure>;
    fn native(&mut self, stop: &AtomicBool) -> Result<BackendResolution, NativeReadinessError>;
}
struct ForegroundPreparation(NativeReadinessContext);
impl Preparation for ForegroundPreparation {
    fn pair(
        &mut self,
        operation: &PairingOperation,
        token: Option<&PlatformAccessToken>,
        progress: &mut dyn FnMut(PairingStage),
    ) -> Result<PlatformAccessToken, PairingFailure> {
        prepare_webos_pairing_in_memory(
            operation,
            WebOsEndpoint::wss(operation.request().address()),
            token,
            progress,
        )
        .map_err(|e| e.failure())
    }
    fn native(&mut self, stop: &AtomicBool) -> Result<BackendResolution, NativeReadinessError> {
        check_native_readiness(&self.0, stop)
    }
}

/// Safe frontend snapshot. The executor retains the full source and candidate
/// privately; unknown configuration keys cannot be rendered through this view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSummary {
    pub stale_reasons: Vec<StaleConfigReason>,
    pub profile: super::TvProfile,
    pub requires_tv_pairing: bool,
    pub screen_choice: super::ScreenChoiceRequired,
}

pub struct MigrationFlow {
    path: PathBuf,
    snapshot: MigrationSnapshot,
    plan: MigrationPlan,
    revision: MigrationRevision,
    pending: Option<MigrationCancellation>,
    finished: bool,
    preparation: Box<dyn Preparation>,
    lease: Option<FlowLock>,
}
fn next_revision() -> MigrationRevision {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    MigrationRevision(NEXT.fetch_add(1, Ordering::Relaxed))
}
impl MigrationFlow {
    /// Open in the desktop user session. Uses the same stable per-user lease
    /// as onboarding. Merely opening never pairs, probes, or publishes.
    pub fn open(path: &Path, context: NativeReadinessContext) -> Result<Self, MigrationError> {
        let lock = PathBuf::from(format!("/run/user/{}/lg-buddy-onboarding.lock", unsafe {
            libc::geteuid()
        }));
        let path = std::path::absolute(path).map_err(|_| MigrationError {
            failure: MigrationFailure::Storage,
            stale_reasons: Vec::new(),
            inspection: None,
        })?;
        Self::with_preparation(&path, &lock, Box::new(ForegroundPreparation(context)))
    }
    fn with_preparation(
        path: &Path,
        lock: &Path,
        preparation: Box<dyn Preparation>,
    ) -> Result<Self, MigrationError> {
        let error = |failure| MigrationError {
            failure,
            stale_reasons: Vec::new(),
            inspection: None,
        };
        let lease = FlowLock::try_acquire(lock).map_err(|e| {
            error(if e.kind() == std::io::ErrorKind::WouldBlock {
                MigrationFailure::Busy
            } else {
                MigrationFailure::Storage
            })
        })?;
        let snapshot = MigrationSnapshot::capture(path).map_err(|e| error(store_failure(e)))?;
        let plan = match inspect_config(path, snapshot.contents()).map_err(|inspection| {
            MigrationError {
                failure: MigrationFailure::InvalidConfiguration,
                stale_reasons: crate::config::stale_config_reasons(snapshot.contents()),
                inspection: Some(inspection),
            }
        })? {
            MigrationInspection::Current => return Err(error(MigrationFailure::AlreadyCurrent)),
            MigrationInspection::Required(plan) => plan,
        };
        Ok(Self {
            path: path.into(),
            snapshot,
            plan,
            revision: next_revision(),
            pending: None,
            finished: false,
            preparation,
            lease: Some(lease),
        })
    }
    pub fn plan(&self) -> MigrationSummary {
        MigrationSummary {
            stale_reasons: self.plan.stale_reasons().to_vec(),
            profile: *self.plan.profile(),
            requires_tv_pairing: self.plan.requires_tv_pairing(),
            screen_choice: self.plan.screen_choice_required(),
        }
    }
    pub fn revision(&self) -> MigrationRevision {
        self.revision
    }
    fn error(&self, failure: MigrationFailure) -> MigrationError {
        MigrationError {
            failure,
            stale_reasons: self.plan.stale_reasons().to_vec(),
            inspection: None,
        }
    }

    /// This call is the one application confirmation for both TV and screen
    /// changes. The TV may separately ask for permission on its own display.
    pub fn acknowledge(
        &mut self,
        revision: MigrationRevision,
        choice: Option<MonitoringChoice>,
    ) -> Result<MigrationAttempt, MigrationError> {
        if self.finished || revision != self.revision {
            return Err(self.error(MigrationFailure::StaleAttempt));
        }
        let candidate = self
            .plan
            .select(choice)
            .map_err(|_| self.error(MigrationFailure::InvalidChoice))?;
        if let Some(old) = self.pending.take() {
            old.cancel();
        }
        self.revision = next_revision();
        let cancellation = MigrationCancellation::new();
        self.pending = Some(cancellation.clone());
        Ok(MigrationAttempt {
            revision: self.revision,
            candidate,
            cancellation,
        })
    }

    /// Consumes one acknowledged attempt. Failed native checks allow a new
    /// acknowledgement choosing Disabled; TV verification still runs if needed.
    pub fn execute(
        &mut self,
        attempt: MigrationAttempt,
        progress: &mut dyn FnMut(MigrationProgress),
    ) -> Result<MigrationCompletion, MigrationError> {
        self.execute_with_startup(attempt, progress, |path| {
            load_current_config(path).map_err(|_| MigrationStartupFailure::ReloadFailed)
        })
    }

    fn execute_with_startup(
        &mut self,
        attempt: MigrationAttempt,
        progress: &mut dyn FnMut(MigrationProgress),
        startup: impl FnOnce(&Path) -> Result<CurrentConfig, MigrationStartupFailure>,
    ) -> Result<MigrationCompletion, MigrationError> {
        if self.finished
            || attempt.revision != self.revision
            || self
                .pending
                .as_ref()
                .is_none_or(|c| !c.gate.same_attempt(&attempt.cancellation.gate))
        {
            return Err(self.error(MigrationFailure::StaleAttempt));
        }
        let cancel = &attempt.cancellation;
        let mut emit = |stage| {
            progress(MigrationProgress {
                revision: attempt.revision,
                stage,
                can_cancel: cancel.can_cancel(),
            })
        };
        let result = (|| {
            if cancel.gate.is_cancelled() {
                return Err(MigrationFailure::Cancelled);
            }
            let token = if attempt.candidate.requires_tv_pairing() {
                let stored = self.snapshot.existing_token().map_err(store_failure)?;
                let request = self
                    .plan
                    .profile()
                    .pairing_request()
                    .map_err(|_| MigrationFailure::InvalidConfiguration)?;
                let operation = PairingOperation::for_setup(request, cancel.gate.clone());
                emit(MigrationStage::Pairing(PairingStage::Connecting));
                Some(
                    self.preparation
                        .pair(&operation, stored.as_ref(), &mut |s| {
                            emit(MigrationStage::Pairing(s))
                        })
                        .map_err(MigrationFailure::Pairing)?,
                )
            } else {
                None
            };
            if cancel.gate.is_cancelled() {
                return Err(MigrationFailure::Cancelled);
            }
            let native_backend = if attempt.candidate.requires_native_check() {
                emit(MigrationStage::NativeMonitoring);
                Some(self.preparation.native(&cancel.stop).map_err(|e| match e {
                    NativeReadinessError::Cancelled => MigrationFailure::Cancelled,
                    NativeReadinessError::ConflictingOverride => {
                        MigrationFailure::ConflictingOverride
                    }
                    NativeReadinessError::Unavailable => MigrationFailure::NativeMonitoring,
                })?)
            } else {
                None
            };
            if cancel.gate.is_cancelled() {
                return Err(MigrationFailure::Cancelled);
            }
            emit(MigrationStage::Committing);
            let committed = self
                .snapshot
                .commit(&attempt.candidate, token.as_ref(), &cancel.gate)
                .map_err(store_failure)?;
            self.finished = true;
            // Hosts continue their existing startup path from a freshly loaded
            // CurrentConfig; readiness is not a perpetual runtime guarantee.
            let startup = startup(&self.path);
            // Normal startup may next need onboarding. A retained completed
            // flow must not keep that setup lease busy.
            self.lease.take();
            emit(MigrationStage::Complete);
            Ok(MigrationCompletion {
                durability_warning: committed.durability_warning,
                native_backend,
                startup,
            })
        })();
        self.pending = None;
        self.revision = next_revision();
        // Observe cancellation before sealing a failed preparation. A stop
        // accepted during a failing TV/probe phase outranks that phase error.
        let was_cancelled = cancel.gate.finish_and_was_cancelled();
        let result = if result.is_err() && was_cancelled {
            Err(MigrationFailure::Cancelled)
        } else {
            result
        };
        if matches!(
            result,
            Err(MigrationFailure::Cancelled
                | MigrationFailure::ConfigurationChanged
                | MigrationFailure::CredentialChanged
                | MigrationFailure::CredentialInvalid
                | MigrationFailure::Storage
                | MigrationFailure::RollbackFailed
                | MigrationFailure::CommitIndeterminate)
        ) {
            // Disk conflicts/uncertain state require a fresh inspection. A
            // cancelled attempt is terminal, like the shared onboarding flow.
            self.finished = true;
            self.lease.take();
        }
        result.map_err(|e| self.error(e))
    }
}
impl Drop for MigrationFlow {
    fn drop(&mut self) {
        if let Some(cancel) = self.pending.take() {
            cancel.cancel();
        }
    }
}
fn store_failure(error: MigrationStoreError) -> MigrationFailure {
    match error {
        MigrationStoreError::Cancelled => MigrationFailure::Cancelled,
        MigrationStoreError::ConfigurationChanged => MigrationFailure::ConfigurationChanged,
        MigrationStoreError::CredentialChanged => MigrationFailure::CredentialChanged,
        MigrationStoreError::CredentialInvalid => MigrationFailure::CredentialInvalid,
        MigrationStoreError::Storage => MigrationFailure::Storage,
        MigrationStoreError::RollbackFailed => MigrationFailure::RollbackFailed,
        MigrationStoreError::CommitIndeterminate => MigrationFailure::CommitIndeterminate,
    }
}

#[cfg(test)]
mod tests;
