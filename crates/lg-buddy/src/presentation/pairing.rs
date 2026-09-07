use super::brightness::UserFacingError;
use crate::config::HdmiInput;
use crate::pairing::{PairingDraft, PairingStage};

/// A pairing form or foreground operation, without credentials or I/O policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingPresentation {
    draft: PairingDraft,
    stage: PairingStage,
    error: Option<UserFacingError>,
}

impl PairingPresentation {
    pub(crate) fn new(
        draft: PairingDraft,
        stage: PairingStage,
        error: Option<UserFacingError>,
    ) -> Self {
        Self {
            draft,
            stage,
            error,
        }
    }

    pub fn stage(&self) -> PairingStage {
        self.stage
    }
    pub fn address(&self) -> &str {
        &self.draft.address
    }
    pub fn mac(&self) -> &str {
        &self.draft.mac
    }
    pub fn input(&self) -> HdmiInput {
        self.draft.input
    }
    pub fn error(&self) -> Option<&UserFacingError> {
        self.error.as_ref()
    }
    pub fn can_submit(&self) -> bool {
        matches!(self.stage, PairingStage::Editing | PairingStage::Failed)
    }
    pub fn can_cancel(&self) -> bool {
        self.stage != PairingStage::Saving
    }

    /// Completed workflow phases, not an estimate of elapsed or remaining time.
    /// Successful completion closes pairing and is confirmed by the success toast.
    pub fn progress_fraction(&self) -> Option<f64> {
        match self.stage {
            PairingStage::Editing | PairingStage::Failed => None,
            PairingStage::Connecting => Some(0.0),
            PairingStage::WaitingForConfirmation => Some(0.25),
            PairingStage::Verifying => Some(0.5),
            PairingStage::Saving => Some(0.75),
        }
    }

    pub fn title(&self) -> &str {
        match self.stage {
            PairingStage::Editing => "Pair a TV",
            PairingStage::Connecting => "Connecting to TV",
            PairingStage::WaitingForConfirmation => "Confirm on Your TV",
            PairingStage::Verifying => "Verifying TV Access",
            PairingStage::Saving => "Saving TV",
            PairingStage::Failed => "Could Not Pair TV",
        }
    }

    pub fn description(&self) -> &str {
        match self.stage {
            PairingStage::Editing => "Turn on your TV and connect it to the same network. Find its IP and MAC addresses in the TV’s network settings.\n\nRequired: Enable TV On With Mobile / Wake-on-LAN.\nStrongly recommended: Use a static IP address and enable Always Ready.",
            PairingStage::Connecting => "Keep the TV on while LG Buddy connects.",
            PairingStage::WaitingForConfirmation => "Use your TV remote to allow LG Buddy’s connection request. You have one minute to respond.",
            PairingStage::Verifying => "Checking that LG Buddy can read the TV’s power, sound, and brightness controls.",
            PairingStage::Saving => "Saving the verified TV and its access token.",
            PairingStage::Failed => "No TV configuration was saved. Check the details below and try again.",
        }
    }
}
