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

#[cfg(test)]
mod tests {
    use super::super::tests::{cached_notification, release_info, TEST_NOW};
    use super::super::UpdateChannel;
    use super::{
        evaluate_update_notification_policy, UpdateNotificationDecision,
        UpdateNotificationPolicyInput, UpdateNotificationReason, UpdateNotificationSkipReason,
    };

    #[test]
    fn notification_policy_skips_when_notification_was_not_requested() {
        let latest = release_info(
            "1.1.1",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
        );

        let decision = evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: false,
            update_available: true,
            latest: &latest,
            last_notification: None,
        });

        assert_eq!(
            decision,
            UpdateNotificationDecision::Skip {
                reason: UpdateNotificationSkipReason::NotRequested
            }
        );
    }

    #[test]
    fn notification_policy_skips_when_no_update_is_available() {
        let latest = release_info(
            "1.1.0",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.0",
        );

        let decision = evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: true,
            update_available: false,
            latest: &latest,
            last_notification: None,
        });

        assert_eq!(
            decision,
            UpdateNotificationDecision::Skip {
                reason: UpdateNotificationSkipReason::NoUpdateAvailable
            }
        );
    }

    #[test]
    fn notification_policy_skips_when_latest_release_was_already_shown() {
        let latest = release_info(
            "1.1.1",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
        );
        let last_notification = cached_notification(
            "1.1.1",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
            TEST_NOW - 1,
        );

        let decision = evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: true,
            update_available: true,
            latest: &latest,
            last_notification: Some(&last_notification),
        });

        assert_eq!(
            decision,
            UpdateNotificationDecision::Skip {
                reason: UpdateNotificationSkipReason::AlreadyShownForRelease
            }
        );
    }

    #[test]
    fn notification_policy_notifies_when_latest_release_has_not_been_shown() {
        let latest = release_info(
            "1.1.2",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.2",
        );
        let last_notification = cached_notification(
            "1.1.1",
            UpdateChannel::Stable,
            "https://github.test/releases/tag/v1.1.1",
            TEST_NOW - 1,
        );

        let decision = evaluate_update_notification_policy(UpdateNotificationPolicyInput {
            notify_requested: true,
            update_available: true,
            latest: &latest,
            last_notification: Some(&last_notification),
        });

        assert_eq!(
            decision,
            UpdateNotificationDecision::Notify {
                reason: UpdateNotificationReason::NewRelease
            }
        );
    }
}
