// Update notification policy: the decision/skip-reason taxonomy, `evaluate_update_notification_policy`, and the render helpers that
// produce the `notification:` user-facing lines. Moved verbatim from updates.rs; the items the parent orchestrator and the colocated tests
// reach through the parent, so they are promoted to `pub(super)`.
use super::{CachedUpdateNotification, ReleaseInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateNotificationReason {
    NewRelease,
}

impl UpdateNotificationReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::NewRelease => "new release",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateNotificationSkipReason {
    NotRequested,
    NoUpdateAvailable,
    AlreadyShownForRelease,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpdateNotificationDecision {
    Notify {
        reason: UpdateNotificationReason,
    },
    Skip {
        reason: UpdateNotificationSkipReason,
    },
}

#[derive(Debug, Clone, Copy)]
pub(super) struct UpdateNotificationPolicyInput<'a> {
    pub(super) notify_requested: bool,
    pub(super) update_available: bool,
    pub(super) latest: &'a ReleaseInfo,
    pub(super) last_notification: Option<&'a CachedUpdateNotification>,
}

pub(super) fn evaluate_update_notification_policy(
    input: UpdateNotificationPolicyInput<'_>,
) -> UpdateNotificationDecision {
    if !input.notify_requested {
        return UpdateNotificationDecision::Skip {
            reason: UpdateNotificationSkipReason::NotRequested,
        };
    }

    if !input.update_available {
        return UpdateNotificationDecision::Skip {
            reason: UpdateNotificationSkipReason::NoUpdateAvailable,
        };
    }

    if input
        .last_notification
        .is_some_and(|notification| notification.matches_release(input.latest))
    {
        return UpdateNotificationDecision::Skip {
            reason: UpdateNotificationSkipReason::AlreadyShownForRelease,
        };
    }

    UpdateNotificationDecision::Notify {
        reason: UpdateNotificationReason::NewRelease,
    }
}

pub(super) fn render_update_notification_sent(reason: UpdateNotificationReason) -> String {
    format!("notification: sent ({})\n", reason.as_str())
}

pub(super) fn render_update_notification_failure(reason: UpdateNotificationReason) -> String {
    format!("notification: failed ({})\n", reason.as_str())
}

pub(super) fn render_update_notification_skip(
    reason: UpdateNotificationSkipReason,
    latest: &ReleaseInfo,
) -> String {
    let reason = match reason {
        UpdateNotificationSkipReason::NotRequested => "not requested".to_string(),
        UpdateNotificationSkipReason::NoUpdateAvailable => "no update available".to_string(),
        UpdateNotificationSkipReason::AlreadyShownForRelease => {
            format!("already shown for {}", latest.version())
        }
    };

    format!("notification: skipped ({reason})\n")
}
