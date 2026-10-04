//! Recovery facts are classified where the domain failure is known, not from text.
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryCause {
    InputRequired,
    InvalidConfiguration,
    InvalidEnvironment,
    MissingIntegration,
    MissingPayload,
    AuthorizationDenied,
    VerifierUnavailable,
    IncompatibleState,
    TemporaryFailure,
    ManagedInstallation,
    UnsupportedInstallation,
    Busy,
    Unverified,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairBoundary {
    UserInput,
    LocalSetup,
    SessionService,
    Installation,
    SystemConfiguration,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    ProvideInput,
    CorrectConfiguration,
    Repair,
    Retry,
    RestartSession,
    RepairExternally,
    Recheck,
    Wait,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupRecovery {
    pub cause: RecoveryCause,
    pub boundary: RepairBoundary,
    pub action: RecoveryAction,
}

impl SetupRecovery {
    pub const fn new(
        cause: RecoveryCause,
        boundary: RepairBoundary,
        action: RecoveryAction,
    ) -> Self {
        Self {
            cause,
            boundary,
            action,
        }
    }

    pub fn can_repair_here(self) -> bool {
        self.cause != RecoveryCause::Unknown
            && matches!(
                self.boundary,
                RepairBoundary::LocalSetup | RepairBoundary::UserInput
            )
            && matches!(
                self.action,
                RecoveryAction::Repair | RecoveryAction::ProvideInput | RecoveryAction::Retry
            )
    }

    pub fn needs_attention(self) -> bool {
        self.cause != RecoveryCause::Busy
    }

    /// Checking after an external remedy does not claim to perform that remedy.
    pub fn check_label(self) -> &'static str {
        if self.action == RecoveryAction::Retry {
            "Retry"
        } else {
            "Recheck"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{StepInput, StepResponse};

    #[test]
    fn input_repair_and_running_states_have_distinct_recovery_facts() {
        let input = StepResponse::InputRequired(StepInput::Pairing { saved: None })
            .recovery()
            .unwrap();
        assert_eq!(input.action, RecoveryAction::ProvideInput);
        assert!(input.can_repair_here());
        let repair = StepResponse::ActionRequired {
            explanation: "Repair services",
            requires_authorization: true,
        }
        .recovery()
        .unwrap();
        assert_eq!(repair.action, RecoveryAction::Repair);
        assert!(repair.can_repair_here());
        let running = StepResponse::Running {
            message: "Working",
            cancelable: false,
        }
        .recovery()
        .unwrap();
        assert_eq!(running.action, RecoveryAction::Wait);
        assert!(!running.needs_attention());
        assert!(!running.can_repair_here());
        assert!(StepResponse::Complete.recovery().is_none());
        assert!(StepResponse::NotApplicable.recovery().is_none());
    }

    #[test]
    fn unknown_recovery_is_attention_not_permission_to_execute() {
        let recovery = SetupRecovery::default();
        assert!(recovery.needs_attention());
        assert!(!recovery.can_repair_here());
        assert!(!SetupRecovery::new(
            RecoveryCause::Unknown,
            RepairBoundary::LocalSetup,
            RecoveryAction::Retry
        )
        .can_repair_here());
    }
}
