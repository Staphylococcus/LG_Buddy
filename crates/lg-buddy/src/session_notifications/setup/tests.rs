use super::*;
use crate::{notifications::NotificationCloseReason, setup::published::SetupRequirement};
use std::{cell::RefCell, os::unix::fs::PermissionsExt, sync::Mutex};

struct RecordingBackend {
    gui_open: bool,
    gui_after_delivery: bool,
    owner: Result<String, String>,
    actions: bool,
    fail_notify: bool,
    fail_open: bool,
    fail_close: bool,
    notifications: RefCell<Vec<Notification>>,
    closes: RefCell<Vec<(String, NotificationId)>>,
    opens: RefCell<usize>,
    tokens: RefCell<Vec<Option<String>>>,
    queries: RefCell<usize>,
}

impl Default for RecordingBackend {
    fn default() -> Self {
        Self {
            gui_open: false,
            gui_after_delivery: false,
            owner: Ok(":1.10".into()),
            actions: true,
            fail_notify: false,
            fail_open: false,
            fail_close: false,
            notifications: RefCell::default(),
            closes: RefCell::default(),
            opens: RefCell::default(),
            tokens: RefCell::default(),
            queries: RefCell::default(),
        }
    }
}

impl AttentionBackend for RecordingBackend {
    fn gui_open(&self) -> Result<bool, String> {
        Ok(self.gui_open || (self.gui_after_delivery && !self.notifications.borrow().is_empty()))
    }
    fn owner(&self) -> Result<String, String> {
        *self.queries.borrow_mut() += 1;
        self.owner.clone()
    }
    fn actions_supported(&self, _: &str) -> Result<bool, String> {
        Ok(self.actions)
    }
    fn notify(&self, _: &str, notification: &Notification) -> Result<NotificationId, String> {
        self.notifications.borrow_mut().push(notification.clone());
        if self.fail_notify {
            Err("agent failed".into())
        } else {
            Ok(NotificationId(self.notifications.borrow().len() as u32))
        }
    }
    fn close(&self, owner: &str, id: NotificationId) -> Result<(), String> {
        self.closes.borrow_mut().push((owner.into(), id));
        if self.fail_close {
            Err("close failed".into())
        } else {
            Ok(())
        }
    }
    fn open_gui(&self, token: Option<&str>) -> Result<(), String> {
        *self.opens.borrow_mut() += 1;
        self.tokens.borrow_mut().push(token.map(str::to_owned));
        if self.fail_open {
            Err("GUI unavailable".into())
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Default)]
struct MemoryLedger(
    Arc<Mutex<BTreeSet<RequirementKey>>>,
    Arc<Mutex<Option<Outstanding>>>,
);

impl AttentionLedger for MemoryLedger {
    fn contains(&self, key: &RequirementKey) -> bool {
        self.0.lock().unwrap().contains(key)
    }
    fn record(&mut self, keys: &BTreeSet<RequirementKey>) -> io::Result<()> {
        self.0.lock().unwrap().extend(keys.iter().cloned());
        Ok(())
    }
    fn outstanding(&self) -> Option<Outstanding> {
        self.1.lock().unwrap().clone()
    }
    fn set_outstanding(&mut self, outstanding: Option<Outstanding>) -> io::Result<()> {
        *self.1.lock().unwrap() = outstanding;
        Ok(())
    }
}

fn snapshot(revision: u64, steps: &[(&str, bool)]) -> SetupSnapshot {
    SetupSnapshot {
        instance: "host".into(),
        revision,
        config: "/config/lg-buddy.toml".into(),
        status: if steps.is_empty() {
            SetupStatus::Complete
        } else {
            SetupStatus::Incomplete
        },
        requirements: steps
            .iter()
            .map(|(step, actionable)| SetupRequirement {
                recovery: crate::setup::recovery::SetupRecovery::default(),
                needs_attention: *actionable,
                step: (*step).into(),
                reason: format!("Complete {step}."),
                actionable: *actionable,
            })
            .collect(),
    }
}

#[test]
fn typed_concrete_failures_notify_once_but_uncertainty_and_unsupported_do_not() {
    use crate::setup::recovery::{
        RecoveryAction as Action, RecoveryCause as Cause, RepairBoundary as Boundary, SetupRecovery,
    };
    for (cause, action, expected) in [
        (Cause::InputRequired, Action::ProvideInput, true),
        (
            Cause::InvalidConfiguration,
            Action::CorrectConfiguration,
            true,
        ),
        (Cause::InvalidEnvironment, Action::RepairExternally, true),
        (Cause::MissingIntegration, Action::Repair, true),
        (Cause::MissingPayload, Action::RepairExternally, true),
        (Cause::AuthorizationDenied, Action::Retry, true),
        (Cause::IncompatibleState, Action::RepairExternally, true),
        (Cause::ManagedInstallation, Action::RepairExternally, true),
        (Cause::TemporaryFailure, Action::Retry, false),
        (Cause::VerifierUnavailable, Action::RestartSession, false),
        (Cause::Busy, Action::Recheck, false),
        (Cause::Unverified, Action::Retry, false),
        (
            Cause::UnsupportedInstallation,
            Action::RepairExternally,
            false,
        ),
        (Cause::Unknown, Action::Unknown, false),
    ] {
        let mut state = snapshot(1, &[("services", false)]);
        state.requirements[0].needs_attention = true;
        state.requirements[0].recovery =
            SetupRecovery::new(cause, Boundary::SystemConfiguration, action);
        let mut attention = Attention::new(
            "host".into(),
            RecordingBackend::default(),
            MemoryLedger::default(),
        );
        attention.observe(&state).unwrap();
        attention.observe(&state).unwrap();
        assert_eq!(
            attention.backend.notifications.borrow().len(),
            usize::from(expected),
            "{cause:?}"
        );
        assert_eq!(*attention.backend.opens.borrow(), 0);
        if !expected {
            assert_eq!(*attention.backend.queries.borrow(), 0);
        }
        state.status = SetupStatus::Complete;
        state.revision = 2;
        attention.observe(&state).unwrap();
        assert_eq!(
            attention.backend.closes.borrow().len(),
            usize::from(expected)
        );
    }
}

fn attention() -> Attention<RecordingBackend, MemoryLedger> {
    Attention::new(
        "host".into(),
        RecordingBackend::default(),
        MemoryLedger::default(),
    )
}

fn action(id: u32) -> NotificationSignal {
    NotificationSignal::ActionInvoked {
        id: NotificationId(id),
        action_key: COMPLETE_SETUP.into(),
    }
}

#[test]
fn actionable_installation_requirements_use_one_safe_gui_action() {
    let mut attention = attention();
    let mut state = snapshot(
        1,
        &[("pairing", true), ("services", true), ("plasma", true)],
    );
    state.requirements[0].reason = "Pair <TV> & finish setup.".into();
    attention.observe(&state).unwrap();
    let notifications = attention.backend.notifications.borrow();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].summary, "LG Buddy needs attention");
    assert!(notifications[0]
        .body
        .contains("Pair &lt;TV&gt; &amp; finish setup."));
    assert_eq!(
        notifications[0].actions,
        vec![
            NotificationAction {
                key: DEFAULT_ACTION.into(),
                label: "Complete setup".into(),
            },
            NotificationAction {
                key: COMPLETE_SETUP.into(),
                label: "Complete setup".into(),
            }
        ]
    );
    assert_eq!(*attention.backend.opens.borrow(), 0);
}

#[test]
fn notification_body_activation_opens_the_same_gui_only_once() {
    let mut attention = attention();
    attention
        .observe(&snapshot(1, &[("services", true)]))
        .unwrap();
    let default = NotificationSignal::ActionInvoked {
        id: NotificationId(1),
        action_key: DEFAULT_ACTION.into(),
    };
    attention.signal(":1.20", default.clone()).unwrap();
    assert_eq!(*attention.backend.opens.borrow(), 0);
    attention.signal(":1.10", default).unwrap();
    attention.signal(":1.10", action(1)).unwrap();
    assert_eq!(*attention.backend.opens.borrow(), 1);
    assert!(attention.pending.is_none());
}

#[test]
fn unchecked_complete_and_nonactionable_results_do_not_contact_agent() {
    let mut attention = attention();
    let mut unchecked = snapshot(0, &[("pairing", true)]);
    unchecked.status = SetupStatus::Unchecked;
    for state in [
        unchecked,
        snapshot(1, &[("offline", false)]),
        snapshot(2, &[]),
    ] {
        attention.observe(&state).unwrap();
    }
    assert_eq!(*attention.backend.queries.borrow(), 0);
    assert!(attention.backend.notifications.borrow().is_empty());
    assert_eq!(*attention.backend.opens.borrow(), 0);
}

#[test]
fn gui_suppression_survives_cancellation_and_host_restart() {
    let ledger = MemoryLedger::default();
    let mut first = Attention::new(
        "host".into(),
        RecordingBackend {
            gui_open: true,
            ..Default::default()
        },
        ledger.clone(),
    );
    first.observe(&snapshot(1, &[("pairing", true)])).unwrap();
    assert!(first.backend.notifications.borrow().is_empty());
    let mut restarted = Attention::new("host".into(), RecordingBackend::default(), ledger);
    restarted
        .observe(&snapshot(4, &[("pairing", true)]))
        .unwrap();
    assert!(restarted.backend.notifications.borrow().is_empty());
    assert_eq!(*restarted.backend.queries.borrow(), 0);
}

#[test]
fn dismissal_and_agent_restart_do_not_repeat_same_requirement() {
    let mut attention = attention();
    let state = snapshot(1, &[("services", true)]);
    attention.observe(&state).unwrap();
    attention
        .signal(
            ":1.10",
            NotificationSignal::Closed {
                id: NotificationId(1),
                reason: NotificationCloseReason::Dismissed,
            },
        )
        .unwrap();
    attention.backend.owner = Ok(":1.20".into());
    attention
        .observe(&snapshot(2, &[("services", true)]))
        .unwrap();
    assert_eq!(attention.backend.notifications.borrow().len(), 1);
    let mut restarted = Attention::new(
        "host".into(),
        RecordingBackend::default(),
        attention.ledger.clone(),
    );
    restarted.observe(&state).unwrap();
    assert!(restarted.backend.notifications.borrow().is_empty());
}

#[test]
fn resolution_retires_notification_and_stale_results_cannot_recreate_it() {
    let mut attention = attention();
    attention
        .observe(&snapshot(2, &[("services", true)]))
        .unwrap();
    attention.observe(&snapshot(3, &[])).unwrap();
    assert_eq!(
        *attention.backend.closes.borrow(),
        vec![(":1.10".into(), NotificationId(1))]
    );
    attention
        .observe(&snapshot(2, &[("plasma", true)]))
        .unwrap();
    let mut previous_host = snapshot(100, &[("pairing", true)]);
    previous_host.instance = "old-host".into();
    attention.observe(&previous_host).unwrap();
    assert_eq!(attention.backend.notifications.borrow().len(), 1);
    // A queued action after repair still does ordinary GUI activation, once.
    attention.signal(":1.10", action(1)).unwrap();
    attention.signal(":1.10", action(1)).unwrap();
    assert_eq!(*attention.backend.opens.borrow(), 1);
    assert_eq!(attention.backend.closes.borrow().len(), 1);
}

#[test]
fn action_identity_and_close_before_action_are_preserved() {
    let mut attention = attention();
    attention
        .observe(&snapshot(1, &[("pairing", true)]))
        .unwrap();
    attention.signal(":1.20", action(1)).unwrap();
    attention.signal(":1.10", action(99)).unwrap();
    attention
        .signal(
            ":1.10",
            NotificationSignal::ActionInvoked {
                id: NotificationId(1),
                action_key: "other".into(),
            },
        )
        .unwrap();
    assert_eq!(*attention.backend.opens.borrow(), 0);
    attention
        .signal(
            ":1.10",
            NotificationSignal::Closed {
                id: NotificationId(1),
                reason: NotificationCloseReason::ClosedByCall,
            },
        )
        .unwrap();
    attention.signal(":1.10", action(1)).unwrap();
    attention.signal(":1.10", action(1)).unwrap();
    assert_eq!(*attention.backend.opens.borrow(), 1);
    assert!(attention.pending.is_none());
    assert!(attention.backend.closes.borrow().is_empty());
}

#[test]
fn new_requirement_can_notify_without_repeating_old_requirements() {
    let mut attention = attention();
    attention
        .observe(&snapshot(1, &[("pairing", true)]))
        .unwrap();
    attention
        .observe(&snapshot(2, &[("pairing", true), ("plasma", true)]))
        .unwrap();
    let notifications = attention.backend.notifications.borrow();
    assert_eq!(notifications.len(), 2);
    assert_eq!(notifications[1].body, "Complete plasma.");
    drop(notifications);
    attention
        .observe(&snapshot(3, &[("plasma", false)]))
        .unwrap();
    assert!(attention.pending.is_none());
}

#[test]
fn delivery_failures_never_launch_gui_and_accepted_attempts_do_not_repeat() {
    for backend in [
        RecordingBackend {
            owner: Err("no agent".into()),
            ..Default::default()
        },
        RecordingBackend {
            actions: false,
            ..Default::default()
        },
        RecordingBackend {
            fail_notify: true,
            ..Default::default()
        },
    ] {
        let mut attention = Attention::new("host".into(), backend, MemoryLedger::default());
        let state = snapshot(1, &[("services", true)]);
        assert!(attention.observe(&state).is_err());
        assert_eq!(*attention.backend.opens.borrow(), 0);
        if attention.backend.owner.is_ok() {
            attention.observe(&state).unwrap();
            assert!(attention.backend.notifications.borrow().len() <= 1);
        }
    }
}

#[test]
fn gui_activation_failure_does_not_reprompt_or_accept_replayed_action() {
    let mut attention = attention();
    attention.backend.fail_open = true;
    let state = snapshot(1, &[("pairing", true)]);
    attention.observe(&state).unwrap();
    assert!(attention.signal(":1.10", action(1)).is_err());
    attention.signal(":1.10", action(1)).unwrap();
    attention.observe(&state).unwrap();
    assert_eq!(*attention.backend.opens.borrow(), 1);
    assert_eq!(attention.backend.notifications.borrow().len(), 1);
}

struct RuntimeDir(PathBuf);

impl RuntimeDir {
    fn new() -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = env::temp_dir().join(format!(
            "lg-buddy-attention-test-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}

impl Drop for RuntimeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn runtime_ledger_is_private_persistent_and_scoped_to_login() {
    let runtime = RuntimeDir::new();
    let key = RequirementKey {
        config: "/config".into(),
        step: "pairing".into(),
        reason: "Pair TV.".into(),
    };
    let mut ledger = RuntimeLedger::open(&runtime.0, "login-1".into()).unwrap();
    ledger.record(&BTreeSet::from([key.clone()])).unwrap();
    assert_eq!(fs::metadata(&ledger.path).unwrap().mode() & 0o777, 0o600);
    assert!(RuntimeLedger::open(&runtime.0, "login-1".into()).is_err());
    drop(ledger);
    let ledger = RuntimeLedger::open(&runtime.0, "login-1".into()).unwrap();
    assert!(ledger.contains(&key));
    drop(ledger);
    let mut ledger = RuntimeLedger::open(&runtime.0, "login-2".into()).unwrap();
    assert!(!ledger.contains(&key));
    ledger.record(&BTreeSet::from([key.clone()])).unwrap();
    drop(ledger);
    assert!(RuntimeLedger::open(&runtime.0, "login-2".into())
        .unwrap()
        .contains(&key));
}

#[test]
fn runtime_ledger_rejects_unsafe_files_and_directories() {
    let runtime = RuntimeDir::new();
    let path = runtime.0.join("lg-buddy-setup-attention.json");
    std::os::unix::fs::symlink("/dev/null", &path).unwrap();
    assert!(RuntimeLedger::open(&runtime.0, "login".into()).is_err());
    fs::remove_file(&path).unwrap();
    fs::write(&path, "{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(RuntimeLedger::open(&runtime.0, "login".into()).is_err());
    fs::remove_file(&path).unwrap();
    fs::set_permissions(&runtime.0, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(RuntimeLedger::open(&runtime.0, "login".into()).is_err());
}

#[test]
fn failed_ledger_claim_does_not_deliver_notification() {
    struct Unwritable;
    impl AttentionLedger for Unwritable {
        fn contains(&self, _: &RequirementKey) -> bool {
            false
        }
        fn record(&mut self, _: &BTreeSet<RequirementKey>) -> io::Result<()> {
            Err(io::Error::other("read-only runtime"))
        }
        fn outstanding(&self) -> Option<Outstanding> {
            None
        }
        fn set_outstanding(&mut self, _: Option<Outstanding>) -> io::Result<()> {
            Err(io::Error::other("read-only runtime"))
        }
    }
    let mut attention = Attention::new("host".into(), RecordingBackend::default(), Unwritable);
    assert!(attention
        .observe(&snapshot(1, &[("services", true)]))
        .is_err());
    assert!(attention.backend.notifications.borrow().is_empty());
    assert_eq!(*attention.backend.opens.borrow(), 0);
}

#[test]
fn gui_opening_during_delivery_retires_notice_without_activation_or_reprompt() {
    let mut attention = attention();
    attention.backend.gui_after_delivery = true;
    let state = snapshot(1, &[("pairing", true)]);
    attention.observe(&state).unwrap();
    assert!(attention.pending.is_none());
    assert_eq!(attention.backend.closes.borrow().len(), 1);
    attention.backend.gui_after_delivery = false;
    attention.observe(&state).unwrap();
    assert_eq!(attention.backend.notifications.borrow().len(), 1);
    assert_eq!(*attention.backend.opens.borrow(), 0);
}

#[test]
fn failed_close_does_not_consume_a_new_requirement_before_delivery() {
    let mut attention = attention();
    attention
        .observe(&snapshot(1, &[("pairing", true)]))
        .unwrap();
    attention.backend.fail_close = true;
    let next = snapshot(2, &[("plasma", true)]);
    assert!(attention.observe(&next).is_err());
    attention.backend.fail_close = false;
    attention.observe(&next).unwrap();
    assert_eq!(attention.backend.notifications.borrow().len(), 2);
    assert_eq!(
        attention.backend.notifications.borrow()[1].body,
        "Complete plasma."
    );
}

#[test]
fn runtime_ledger_rejects_fifo_without_waiting_for_a_writer() {
    use std::os::unix::ffi::OsStrExt;
    let runtime = RuntimeDir::new();
    let path = std::ffi::CString::new(
        runtime
            .0
            .join("lg-buddy-setup-attention.json")
            .as_os_str()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(RuntimeLedger::open(&runtime.0, "login".into()).is_err());
}

#[test]
fn partial_repair_retires_only_the_requirements_displayed_in_the_notice() {
    let mut attention = attention();
    attention
        .observe(&snapshot(1, &[("pairing", true)]))
        .unwrap();
    attention
        .observe(&snapshot(2, &[("pairing", true), ("plasma", true)]))
        .unwrap();
    assert_eq!(
        attention.backend.notifications.borrow()[1].body,
        "Complete plasma."
    );
    attention
        .observe(&snapshot(3, &[("pairing", true)]))
        .unwrap();
    assert!(attention.pending.is_none());
    assert_eq!(
        attention.backend.closes.borrow().last().unwrap().1,
        NotificationId(2)
    );
    attention
        .observe(&snapshot(4, &[("pairing", true)]))
        .unwrap();
    assert_eq!(attention.backend.notifications.borrow().len(), 2);
}

#[test]
fn resolving_one_of_multiple_displayed_requirements_withdraws_stale_body() {
    let mut attention = attention();
    attention
        .observe(&snapshot(1, &[("pairing", true), ("plasma", true)]))
        .unwrap();
    attention
        .observe(&snapshot(2, &[("pairing", true)]))
        .unwrap();
    assert!(attention.pending.is_none());
    assert_eq!(
        *attention.backend.closes.borrow(),
        vec![(":1.10".into(), NotificationId(1))]
    );
    assert_eq!(attention.backend.notifications.borrow().len(), 1);
}

#[test]
fn daemon_restart_withdraws_outstanding_notice_without_reprompting() {
    let mut first = attention();
    let state = snapshot(1, &[("pairing", true)]);
    first.observe(&state).unwrap();
    let mut restarted = Attention::new(
        "host".into(),
        RecordingBackend::default(),
        first.ledger.clone(),
    );
    let mut unchecked = snapshot(0, &[]);
    unchecked.status = SetupStatus::Unchecked;
    restarted.observe(&unchecked).unwrap();
    assert_eq!(
        *restarted.backend.closes.borrow(),
        vec![(":1.10".into(), NotificationId(1))]
    );
    restarted.observe(&state).unwrap();
    restarted.observe(&snapshot(2, &[])).unwrap();
    assert!(restarted.backend.notifications.borrow().is_empty());
    assert!(restarted.ledger.outstanding().is_none());
    restarted.signal(":1.10", action(1)).unwrap();
    assert_eq!(*restarted.backend.opens.borrow(), 0);
}

#[test]
fn daemon_restart_does_not_close_reused_id_on_replacement_agent() {
    let mut first = attention();
    first.observe(&snapshot(1, &[("pairing", true)])).unwrap();
    let mut restarted = Attention::new(
        "host".into(),
        RecordingBackend {
            owner: Ok(":1.20".into()),
            ..Default::default()
        },
        first.ledger.clone(),
    );
    restarted.observe(&snapshot(2, &[])).unwrap();
    assert!(restarted.backend.closes.borrow().is_empty());
    assert!(restarted.ledger.outstanding().is_none());
}

#[test]
fn activation_token_matches_owner_and_id_and_is_consumed_once() {
    let mut attention = attention();
    attention
        .observe(&snapshot(1, &[("pairing", true)]))
        .unwrap();
    let token = |id, token: &str| NotificationSignal::ActivationToken {
        id: NotificationId(id),
        token: token.into(),
    };
    attention.signal(":1.20", token(1, "forged-owner")).unwrap();
    attention.signal(":1.10", token(99, "wrong-id")).unwrap();
    assert!(attention.pending.as_ref().unwrap().token.is_none());
    attention.signal(":1.10", token(1, "valid-token")).unwrap();
    attention
        .signal(
            ":1.10",
            NotificationSignal::Closed {
                id: NotificationId(1),
                reason: NotificationCloseReason::ClosedByCall,
            },
        )
        .unwrap();
    attention.signal(":1.10", action(1)).unwrap();
    attention
        .signal(":1.10", token(1, "replayed-token"))
        .unwrap();
    attention.signal(":1.10", action(1)).unwrap();
    assert_eq!(
        *attention.backend.tokens.borrow(),
        vec![Some("valid-token".into())]
    );
    assert_eq!(*attention.backend.opens.borrow(), 1);
}

#[test]
fn runtime_ledger_persists_outstanding_identity_and_forgets_it_on_a_new_login() {
    let runtime = RuntimeDir::new();
    let receipt = Outstanding {
        owner: ":1.10".into(),
        id: 42,
    };
    let mut ledger = RuntimeLedger::open(&runtime.0, "login-1".into()).unwrap();
    ledger.set_outstanding(Some(receipt.clone())).unwrap();
    drop(ledger);
    let ledger = RuntimeLedger::open(&runtime.0, "login-1".into()).unwrap();
    assert_eq!(ledger.outstanding(), Some(receipt));
    drop(ledger);
    assert!(RuntimeLedger::open(&runtime.0, "login-2".into())
        .unwrap()
        .outstanding()
        .is_none());
}

#[test]
fn runtime_ledger_accepts_existing_key_only_format() {
    let runtime = RuntimeDir::new();
    let path = runtime.0.join("lg-buddy-setup-attention.json");
    fs::write(&path, r#"{"login":"login-1","seen":[]}"#).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(RuntimeLedger::open(&runtime.0, "login-1".into())
        .unwrap()
        .outstanding()
        .is_none());
}
