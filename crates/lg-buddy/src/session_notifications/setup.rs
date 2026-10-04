//! Attention consumes published setup state; it never probes or requests setup.
use super::current_bus_name_owner;
use crate::notifications::{
    self, Notification, NotificationAction, NotificationId, NotificationSignal, Notifier,
    NOTIFICATION_SERVICE,
};
use crate::setup::{
    assessment::SetupStatus,
    published::{PublishedSetup, SetupSnapshot},
};
use dbus::blocking::Connection;
use dbus::channel::MatchingReceiver;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, VecDeque},
    env,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

const GUI_BUS_NAME: &str = "io.github.staphylococcus.LGBuddy";
const COMPLETE_SETUP: &str = "complete-setup";
const POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct RequirementKey {
    config: PathBuf,
    step: String,
    reason: String,
}

trait AttentionBackend {
    fn gui_open(&self) -> Result<bool, String>;
    fn owner(&self) -> Result<String, String>;
    fn actions_supported(&self, owner: &str) -> Result<bool, String>;
    fn notify(&self, owner: &str, notification: &Notification) -> Result<NotificationId, String>;
    fn close(&self, owner: &str, id: NotificationId) -> Result<(), String>;
    fn open_gui(&self, token: Option<&str>) -> Result<(), String>;
}

trait AttentionLedger {
    fn contains(&self, key: &RequirementKey) -> bool;
    fn record(&mut self, keys: &BTreeSet<RequirementKey>) -> io::Result<()>;
    fn outstanding(&self) -> Option<Outstanding>;
    fn set_outstanding(&mut self, outstanding: Option<Outstanding>) -> io::Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Outstanding {
    owner: String,
    id: u32,
}

struct Pending {
    owner: String,
    id: NotificationId,
    requirements: BTreeSet<RequirementKey>,
    token: Option<String>,
}

struct Attention<B, L> {
    instance: String,
    revision: u64,
    backend: B,
    ledger: L,
    pending: Option<Pending>,
    recently_closed: VecDeque<Pending>,
}

impl<B: AttentionBackend, L: AttentionLedger> Attention<B, L> {
    fn new(instance: String, backend: B, ledger: L) -> Self {
        Self {
            instance,
            revision: 0,
            backend,
            ledger,
            pending: None,
            recently_closed: VecDeque::new(),
        }
    }

    fn observe(&mut self, snapshot: &SetupSnapshot) -> Result<(), String> {
        if snapshot.instance != self.instance || snapshot.revision < self.revision {
            return Ok(());
        }
        self.revision = snapshot.revision;
        self.reconcile_previous_host()?;
        if snapshot.status == SetupStatus::Unchecked {
            return Ok(());
        }
        let requirements: BTreeSet<_> = snapshot
            .requirements
            .iter()
            .filter(|item| {
                snapshot.status == SetupStatus::Incomplete
                    && ((item.needs_attention && item.recovery.warrants_notification())
                        || (item.recovery == crate::setup::recovery::SetupRecovery::default()
                            && item.actionable))
            })
            .map(|item| RequirementKey {
                config: snapshot.config.clone(),
                step: item.step.clone(),
                reason: item.reason.clone(),
            })
            .collect();
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.requirements.is_subset(&requirements))
        {
            self.retire()?;
        }
        if requirements.is_empty() {
            return Ok(());
        }
        let unseen: BTreeSet<_> = requirements
            .iter()
            .filter(|key| !self.ledger.contains(key))
            .cloned()
            .collect();
        if unseen.is_empty() && self.pending.is_none() {
            return Ok(());
        }
        if self.backend.gui_open()? {
            self.ledger
                .record(&requirements)
                .map_err(|e| e.to_string())?;
            self.retire()?;
            return Ok(());
        }
        if unseen.is_empty() {
            return Ok(());
        }
        let owner = self.backend.owner()?;
        let actions_supported = self.backend.actions_supported(&owner)?;
        self.retire()?;
        // Record before delivery, so a restart cannot repeat an accepted attempt.
        self.ledger
            .record(&requirements)
            .map_err(|e| e.to_string())?;
        if !actions_supported {
            return Err(
                "setup notification actions are unavailable; open LG Buddy to complete setup"
                    .into(),
            );
        }
        let body = unseen
            .iter()
            .map(|key| escape_body(&key.reason))
            .collect::<Vec<_>>()
            .join("\n");
        let mut notification = Notification::new("LG Buddy needs attention", body);
        notification.actions.push(NotificationAction {
            key: COMPLETE_SETUP.into(),
            label: "Complete setup".into(),
        });
        let id = self.backend.notify(&owner, &notification)?;
        self.pending = Some(Pending {
            owner: owner.clone(),
            id,
            requirements: unseen,
            token: None,
        });
        if let Err(error) = self
            .ledger
            .set_outstanding(Some(Outstanding { owner, id: id.0 }))
        {
            self.retire()?;
            return Err(error.to_string());
        }
        // Opening the GUI can race the transport call, but never changes admission.
        if self.backend.gui_open()? {
            self.retire()?;
        }
        Ok(())
    }

    fn reconcile_previous_host(&mut self) -> Result<(), String> {
        if self.pending.is_some() {
            return Ok(());
        }
        if let Some(outstanding) = self.ledger.outstanding() {
            // An old connection cannot receive directed actions. Withdraw its notice,
            // but retain seen requirements so restarting never repeats the prompt.
            if self.backend.owner()? == outstanding.owner {
                self.backend
                    .close(&outstanding.owner, NotificationId(outstanding.id))?;
            }
            self.ledger
                .set_outstanding(None)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn retire(&mut self) -> Result<(), String> {
        if let Some(pending) = self.pending.as_ref() {
            // Never close a reused ID belonging to a replacement notification server.
            if self.backend.owner().ok().as_deref() == Some(&pending.owner) {
                self.backend.close(&pending.owner, pending.id)?;
            }
        }
        if self.pending.is_some() {
            self.ledger
                .set_outstanding(None)
                .map_err(|e| e.to_string())?;
        }
        if let Some(pending) = self.pending.take() {
            self.remember_closed(pending);
        }
        Ok(())
    }

    fn remember_closed(&mut self, pending: Pending) {
        if self.recently_closed.len() == super::RECENTLY_CLOSED_NOTIFICATION_LIMIT {
            self.recently_closed.pop_front();
        }
        self.recently_closed.push_back(pending);
    }

    // The receiver has verified the sender; owner also pins its generation.
    fn signal(&mut self, owner: &str, signal: NotificationSignal) -> Result<(), String> {
        let (id, action) = match signal {
            NotificationSignal::ActivationToken { id, token } => {
                if let Some(pending) = self
                    .pending
                    .iter_mut()
                    .chain(self.recently_closed.iter_mut())
                    .find(|pending| pending.owner == owner && pending.id == id)
                {
                    pending.token = Some(token);
                }
                return Ok(());
            }
            NotificationSignal::ActionInvoked { id, action_key } => (id, Some(action_key)),
            NotificationSignal::Closed { id, .. } => (id, None),
        };
        let matches = |pending: &Pending| pending.owner == owner && pending.id == id;
        if action.is_none() {
            if self.pending.as_ref().is_some_and(matches) {
                let pending = self.pending.take().unwrap();
                self.remember_closed(pending);
                self.ledger
                    .set_outstanding(None)
                    .map_err(|e| e.to_string())?;
            }
            return Ok(());
        }
        if action.as_deref() != Some(COMPLETE_SETUP) {
            return Ok(());
        }
        let active = self.pending.as_ref().is_some_and(matches);
        let pending = if active {
            self.pending.take()
        } else {
            self.recently_closed
                .iter()
                .position(matches)
                .and_then(|index| self.recently_closed.remove(index))
        };
        if let Some(pending) = pending {
            let launched = self.backend.open_gui(pending.token.as_deref());
            let retired = (|| {
                // Retired IDs may already have been reused by the same server.
                if active {
                    if self.backend.owner().ok().as_deref() == Some(&pending.owner) {
                        self.backend.close(&pending.owner, pending.id)?;
                    }
                    self.ledger
                        .set_outstanding(None)
                        .map_err(|e| e.to_string())?;
                }
                Ok(())
            })();
            return launched.and(retired);
        }
        Ok(())
    }
}

fn escape_body(reason: &str) -> String {
    reason
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

struct DesktopBackend(Connection);

impl AttentionBackend for DesktopBackend {
    fn gui_open(&self) -> Result<bool, String> {
        let proxy = self.0.with_proxy(
            super::DBUS_SERVICE_NAME,
            super::DBUS_OBJECT_PATH,
            Duration::from_secs(1),
        );
        let (open,): (bool,) = proxy
            .method_call(super::DBUS_INTERFACE, "NameHasOwner", (GUI_BUS_NAME,))
            .map_err(|e| e.to_string())?;
        Ok(open)
    }
    fn owner(&self) -> Result<String, String> {
        current_bus_name_owner(&self.0, NOTIFICATION_SERVICE).map_err(|e| e.to_string())
    }
    fn actions_supported(&self, owner: &str) -> Result<bool, String> {
        notifications::for_owner(&self.0, owner)
            .capabilities()
            .map(|caps| caps.actions)
            .map_err(|e| e.to_string())
    }
    fn notify(&self, owner: &str, notification: &Notification) -> Result<NotificationId, String> {
        notifications::for_owner(&self.0, owner)
            .notify(notification)
            .map_err(|e| e.to_string())
    }
    fn close(&self, owner: &str, id: NotificationId) -> Result<(), String> {
        notifications::for_owner(&self.0, owner)
            .close(id)
            .map_err(|e| e.to_string())
    }
    fn open_gui(&self, token: Option<&str>) -> Result<(), String> {
        // Reuse ordinary installed-GUI activation, with no setup arguments or shell.
        let token = token.map(str::to_owned);
        thread::spawn(move || {
            if let Err(error) =
                crate::commands::run_overview_with_activation_token(token.as_deref())
            {
                eprintln!("LG Buddy Session: could not open GUI from setup notification: {error}");
            }
        });
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct LedgerData {
    login: String,
    seen: BTreeSet<RequirementKey>,
    #[serde(default)]
    outstanding: Option<Outstanding>,
}

struct RuntimeLedger {
    path: PathBuf,
    data: LedgerData,
    _lock: crate::setup::lock::FlowLock,
}

impl RuntimeLedger {
    fn open(runtime: &Path, login: String) -> io::Result<Self> {
        let lock = crate::setup::lock::FlowLock::try_acquire(
            &runtime.join("lg-buddy-setup-attention.lock"),
        )?;
        let path = runtime.join("lg-buddy-setup-attention.json");
        let mut data = LedgerData {
            login,
            seen: BTreeSet::new(),
            outstanding: None,
        };
        match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => {
                let metadata = file.metadata()?;
                if !metadata.is_file()
                    || metadata.uid() != unsafe { libc::geteuid() }
                    || metadata.mode() & 0o077 != 0
                    || metadata.len() > 1024 * 1024
                {
                    return Err(io::Error::other("unsafe setup notification ledger"));
                }
                let previous: LedgerData = serde_json::from_reader(file)?;
                if previous.login == data.login {
                    data = previous;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(Self {
            path,
            data,
            _lock: lock,
        })
    }
    fn save(&mut self, next: LedgerData) -> io::Result<()> {
        let bytes = serde_json::to_vec(&next)?;
        for attempt in 0..100 {
            let temp = self.path.with_file_name(format!(
                ".lg-buddy-setup-attention.{}.{attempt}.tmp",
                std::process::id()
            ));
            let mut file = match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let result = (|| {
                file.write_all(&bytes)?;
                file.sync_all()?;
                fs::rename(&temp, &self.path)
            })();
            if let Err(error) = result {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
            self.data = next;
            return Ok(());
        }
        Err(io::Error::other(
            "could not persist setup notification deduplication",
        ))
    }
}

impl AttentionLedger for RuntimeLedger {
    fn contains(&self, key: &RequirementKey) -> bool {
        self.data.seen.contains(key)
    }

    fn record(&mut self, keys: &BTreeSet<RequirementKey>) -> io::Result<()> {
        if keys.is_subset(&self.data.seen) {
            return Ok(());
        }
        let mut next = self.data.clone();
        next.seen.extend(keys.iter().cloned());
        self.save(next)
    }

    fn outstanding(&self) -> Option<Outstanding> {
        self.data.outstanding.clone()
    }

    fn set_outstanding(&mut self, outstanding: Option<Outstanding>) -> io::Result<()> {
        if self.data.outstanding == outstanding {
            return Ok(());
        }
        let mut next = self.data.clone();
        next.outstanding = outstanding;
        self.save(next)
    }
}

fn login_key(connection: &Connection) -> Result<String, String> {
    let proxy = connection.with_proxy(
        super::DBUS_SERVICE_NAME,
        super::DBUS_OBJECT_PATH,
        Duration::from_secs(1),
    );
    let (bus_id,): (String,) = proxy
        .method_call(super::DBUS_INTERFACE, "GetId", ())
        .map_err(|e| e.to_string())?;
    let mut system = crate::session_bus::new_system_bus_client().map_err(|e| e.to_string())?;
    let session = crate::sources::linux::logind::resolve_current_graphical_session(
        &mut system,
        env::var("XDG_SESSION_ID").ok().as_deref(),
        unsafe { libc::geteuid() },
    )
    .map_err(|e| e.to_string())?;
    Ok(format!("{bus_id}/{}", session.id))
}

fn register_signals(
    connection: &Connection,
    signals: mpsc::Sender<(String, NotificationSignal)>,
) -> Result<(), String> {
    let rule = dbus::message::MatchRule::new()
        .with_type(dbus::message::MessageType::Signal)
        .with_interface(notifications::NOTIFICATION_INTERFACE)
        .with_path(notifications::NOTIFICATION_PATH);
    // Accept both broadcasts and signals addressed to the persistent Notify caller.
    connection
        .add_match_no_cb(&rule.match_str())
        .map_err(|e| e.to_string())?;
    connection.start_receive(
        rule,
        Box::new(move |message, conn| {
            if let Ok(signal) = crate::session_bus::bus_signal_from_dbus_message(message) {
                if let Some(parsed) = notifications::parse_notification_signal(&signal) {
                    if let Ok(owners) = super::trusted_notification_signal_senders(conn) {
                        if super::notification_signal_sender_is_trusted(&signal, &owners) {
                            let _ = signals.send((owners[0].clone(), parsed));
                        }
                    }
                }
            }
            true
        }),
    );
    Ok(())
}

pub(super) struct AttentionWorker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for AttentionWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub(super) fn spawn(published: PublishedSetup, host_stop: Arc<AtomicBool>) -> AttentionWorker {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let handle = thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let mut last_startup_error = None;
            let (connection, ledger) = loop {
                if thread_stop.load(Ordering::SeqCst) || host_stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                let initialized = (|| -> Result<_, String> {
                    let connection = Connection::new_session().map_err(|e| e.to_string())?;
                    let login = login_key(&connection)?;
                    let runtime = env::var_os("XDG_RUNTIME_DIR")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() }))
                        });
                    let ledger = RuntimeLedger::open(&runtime, login).map_err(|e| e.to_string())?;
                    Ok((connection, ledger))
                })();
                match initialized {
                    Ok(initialized) => break initialized,
                    Err(error) => {
                        if last_startup_error.as_ref() != Some(&error) {
                            eprintln!("LG Buddy Session: setup notifications unavailable: {error}");
                        }
                        last_startup_error = Some(error);
                        for _ in 0..4 {
                            if thread_stop.load(Ordering::SeqCst)
                                || host_stop.load(Ordering::SeqCst)
                            {
                                return Ok(());
                            }
                            thread::sleep(POLL_INTERVAL);
                        }
                    }
                }
            };
            let (signals, incoming) = mpsc::channel();
            register_signals(&connection, signals)?;
            let mut attention = Attention::new(
                published.snapshot().instance,
                DesktopBackend(connection),
                ledger,
            );
            let mut last_error = None;
            while !thread_stop.load(Ordering::SeqCst) && !host_stop.load(Ordering::SeqCst) {
                let result = attention.observe(&published.snapshot());
                if let Err(error) = &result {
                    if last_error.as_ref() != Some(error) {
                        eprintln!("LG Buddy Session: setup notification unavailable: {error}");
                    }
                }
                last_error = result.err();
                attention
                    .backend
                    .0
                    .process(POLL_INTERVAL)
                    .map_err(|e| e.to_string())?;
                for (owner, signal) in incoming.try_iter() {
                    if let Err(error) = attention.signal(&owner, signal) {
                        eprintln!("LG Buddy Session: setup notification action failed: {error}");
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            eprintln!("LG Buddy Session: setup notifications unavailable: {error}");
        }
    });
    AttentionWorker {
        stop,
        handle: Some(handle),
    }
}

#[cfg(test)]
mod tests;
