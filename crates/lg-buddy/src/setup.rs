//! One-time activation after GUI pairing, resumable across application launches.

use std::fs::{self, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::presentation::brightness::UserFacingError;
use crate::settings::{
    ConfigPathResolver, ServiceController, SettingValue, SettingsStore,
    SystemdUserServiceController, UserUnitEnableOutcome,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupTask {
    Inspect,
    Activate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetupOperation {
    id: u64,
    task: SetupTask,
}

impl SetupOperation {
    pub fn task(&self) -> SetupTask {
        self.task
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupOutcome {
    Inspected { pending: bool },
    Activated,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupError(UserFacingError);

impl SetupError {
    pub fn new(summary: &str, detail: &str) -> Self {
        Self(UserFacingError::new(summary, detail))
    }

    pub fn stopped() -> Self {
        Self::new(
            "Setup was interrupted",
            "Your saved TV and settings are intact. Retry setup to finish activation.",
        )
    }

    fn storage() -> Self {
        Self::new(
            "Could not resume setup",
            "Check access to your LG Buddy configuration folder, then retry setup.",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SetupPresentation {
    #[default]
    Idle,
    Activating,
    Failed(UserFacingError),
}

impl SetupPresentation {
    pub fn busy(&self) -> bool {
        matches!(self, Self::Activating)
    }

    pub fn error(&self) -> Option<&UserFacingError> {
        match self {
            Self::Failed(error) => Some(error),
            _ => None,
        }
    }

    pub fn retry_available(&self) -> bool {
        self.error().is_some()
    }

    pub fn title(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Activating => Some("Finishing setup…"),
            Self::Failed(error) => Some(error.summary()),
        }
    }
}

pub trait SetupBackend: Send + Sync + 'static {
    fn run(&self, operation: &SetupOperation) -> Result<SetupOutcome, SetupError>;
}

#[derive(Debug, Default)]
pub struct EnvironmentSetupBackend;

impl SetupBackend for EnvironmentSetupBackend {
    fn run(&self, operation: &SetupOperation) -> Result<SetupOutcome, SetupError> {
        if unsafe { libc::geteuid() } == 0 {
            return Err(SetupError::new("Run LG Buddy as your normal user", "Setup keeps your TV and settings owned by your account. Only system service activation requests authorization."));
        }
        let path = ConfigPathResolver::resolve_from_env().map_err(|_| SetupError::storage())?;
        match operation.task {
            SetupTask::Inspect => Ok(SetupOutcome::Inspected {
                pending: pending_at(&path).map_err(|_| SetupError::storage())?,
            }),
            SetupTask::Activate => {
                verify_installed_config(&path)?;
                activate_at(
                    &path,
                    &SystemdUserServiceController::from_env(),
                    start_system_services,
                )?;
                Ok(SetupOutcome::Activated)
            }
        }
    }
}

fn verify_installed_config(path: &Path) -> Result<(), SetupError> {
    let root = std::env::var_os("LG_BUDDY_INSTALL_ROOT")
        .filter(|root| !root.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let pointer = fs::read_to_string(root.join("usr/lib/lg-buddy/config-path"));
    let matching = pointer
        .ok()
        .and_then(|text| {
            let target = text.lines().find(|line| !line.trim().is_empty())?;
            Some(fs::canonicalize(target).ok()? == fs::canonicalize(path).ok()?)
        })
        .unwrap_or(false);
    if matching {
        Ok(())
    } else {
        Err(SetupError::new("Setup needs the installed application", "Install LG Buddy for this configuration, then reopen it and retry setup. Your saved TV and settings are intact."))
    }
}

pub(crate) fn pending_path(config_path: &Path) -> PathBuf {
    let mut path = config_path.as_os_str().to_os_string();
    path.push(".setup-pending");
    PathBuf::from(path)
}

fn pending_at(config_path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(pending_path(config_path)) {
        Ok(metadata) if metadata.is_file() && metadata.uid() == unsafe { libc::geteuid() } => {
            Ok(true)
        }
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid setup marker",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Publish this before the paired profile so a crash never loses activation intent.
pub(crate) fn mark_pending(config_path: &Path) -> io::Result<()> {
    if pending_at(config_path)? {
        return Ok(());
    }
    let path = pending_path(config_path);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.sync_all()?;
    if let Some(parent) = path.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn activate_at(
    path: &Path,
    services: &impl ServiceController,
    start_system: impl FnOnce() -> Result<(), SetupError>,
) -> Result<(), SetupError> {
    if !pending_at(path).map_err(|_| SetupError::storage())? {
        return Err(SetupError::storage());
    }
    let store = SettingsStore::load(path).map_err(|_| SetupError::storage())?;
    for key in [
        "tv.ip",
        "tv.mac",
        "tv.input",
        "tv.platform",
        "screen.backend",
        "screen.idle_blank",
        "screen.idle_timeout",
        "screen.restore_policy",
        "system.sleep_wake_policy",
        "updates.channel",
    ] {
        store.effective_by_name(key).and_then(|value| value.required_value()).map_err(|_| {
            SetupError::new("Check your saved settings", "Setup needs a valid TV profile and behavior settings. Correct them in Settings or TVs, then retry setup.")
        })?;
    }
    let auto_check = store
        .effective_by_name("updates.auto_check")
        .and_then(|value| value.required_value())
        .map_err(|_| {
            SetupError::new(
                "Check automatic update checks",
                "Correct this setting in Settings, then retry setup.",
            )
        })?;
    if services.systemd_actions_disabled() {
        return Err(SetupError::new("Service activation was skipped", "Service operations are disabled for this run. Restart LG Buddy with service operations enabled, then retry setup."));
    }
    // The installed payload already owns these fixed system units. Pairing and
    // all per-user operations remain outside the authorization boundary.
    start_system()?;
    services
        .reload_user_manager()
        .map_err(|_| user_activation_error())?;
    services
        .enable_start_user_unit("LG_Buddy_screen.service")
        .map_err(|_| user_activation_error())?;
    // The monitor also owns session notifications when idle blanking is off.
    // Restart applies a new pairing even if an older monitor was still running.
    services
        .restart_user_service("LG_Buddy_screen.service")
        .map_err(|_| user_activation_error())?;
    if auto_check == SettingValue::Enum("enabled") {
        if services
            .enable_start_user_unit("LG_Buddy_update_check.timer")
            .map_err(|_| user_activation_error())?
            == UserUnitEnableOutcome::Enabled
        {
            services
                .restart_user_service("LG_Buddy_update_check.timer")
                .map_err(|_| user_activation_error())?;
        }
    } else {
        services
            .disable_stop_user_unit("LG_Buddy_update_check.timer")
            .map_err(|_| user_activation_error())?;
    }
    fs::remove_file(pending_path(path)).map_err(|_| SetupError::storage())?;
    Ok(())
}

fn user_activation_error() -> SetupError {
    SetupError::new("Could not finish service activation", "Your TV and settings are saved. Check that the installed user services and your desktop session are available, then retry setup.")
}

fn start_system_services() -> Result<(), SetupError> {
    start_system_services_with(
        std::ffi::OsStr::new("pkexec"),
        std::ffi::OsStr::new("/usr/bin/systemctl"),
    )
}

fn start_system_services_with(
    authorization: &std::ffi::OsStr,
    systemctl: &std::ffi::OsStr,
) -> Result<(), SetupError> {
    let status = Command::new(authorization).arg("--disable-internal-agent").arg(systemctl)
        .args(["start", "LG_Buddy.service", "LG_Buddy_lifecycle.service"])
        .stdin(std::process::Stdio::null())
        .status().map_err(|_| SetupError::new("Could not request authorization", "A desktop authorization agent and pkexec are required to activate the installed system services. Retry setup when they are available."))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(126) => Err(SetupError::new("Authorization was cancelled", "Your TV and settings are saved. Retry setup when you are ready to authorize service activation.")),
        _ => Err(SetupError::new("Could not activate system services", "Your TV and settings are saved. Check the installation and desktop authorization agent, then retry setup.")),
    }
}

pub(crate) struct SetupApplication {
    presentation: SetupPresentation,
    active: Option<SetupOperation>,
    required: bool,
    inspected: bool,
    next_id: u64,
}

impl SetupApplication {
    pub(crate) fn open() -> (Self, SetupOperation) {
        let operation = SetupOperation {
            id: 1,
            task: SetupTask::Inspect,
        };
        (
            Self {
                presentation: SetupPresentation::Idle,
                active: Some(operation),
                required: false,
                inspected: false,
                next_id: 2,
            },
            operation,
        )
    }

    pub(crate) fn presentation(&self) -> &SetupPresentation {
        &self.presentation
    }

    pub(crate) fn paired(&mut self) {
        self.active = None;
        self.inspected = true;
        self.required = true;
        self.presentation = SetupPresentation::Idle;
    }

    pub(crate) fn unpaired(&mut self) {
        self.presentation = SetupPresentation::Idle;
    }

    pub(crate) fn complete(
        &mut self,
        operation: &SetupOperation,
        result: Result<SetupOutcome, SetupError>,
    ) -> bool {
        if self.active.as_ref() != Some(operation) {
            return false;
        }
        self.active = None;
        match (operation.task, result) {
            (SetupTask::Inspect, Ok(SetupOutcome::Inspected { pending })) => {
                self.required = pending;
                self.inspected = true;
                self.presentation = SetupPresentation::Idle;
            }
            (SetupTask::Activate, Ok(SetupOutcome::Activated)) => {
                self.required = false;
                self.presentation = SetupPresentation::Idle;
            }
            (_, Err(error)) => self.presentation = SetupPresentation::Failed(error.0),
            _ => self.presentation = SetupPresentation::Failed(SetupError::stopped().0),
        }
        true
    }

    pub(crate) fn start_if_needed(&mut self, has_profile: bool) -> Option<SetupOperation> {
        if !has_profile
            || !self.required
            || self.active.is_some()
            || self.presentation.error().is_some()
        {
            return None;
        }
        Some(self.start(SetupTask::Activate))
    }

    pub(crate) fn retry(&mut self, has_profile: bool) -> Option<SetupOperation> {
        if self.active.is_some() || !self.presentation.retry_available() {
            return None;
        }
        if self.inspected && !has_profile {
            return None;
        }
        Some(self.start(if self.inspected {
            SetupTask::Activate
        } else {
            SetupTask::Inspect
        }))
    }

    fn start(&mut self, task: SetupTask) -> SetupOperation {
        let operation = SetupOperation {
            id: self.next_id,
            task,
        };
        self.next_id += 1;
        self.active = Some(operation);
        self.presentation = SetupPresentation::Activating;
        operation
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{SettingsError, UserServiceState};
    use std::cell::{Cell, RefCell};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    const PROFILE: &str = "tvs_primary_ip=192.0.2.10\ntvs_primary_mac=02:11:22:33:44:55\ntvs_primary_input=HDMI_1\ntvs_primary_platform=lg_webos\n";

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-setup-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn config(&self, extra: &str) -> PathBuf {
            let path = self.0.join("config.env");
            fs::write(&path, format!("{PROFILE}{extra}")).unwrap();
            path
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct Services {
        calls: RefCell<Vec<String>>,
        fail: Cell<bool>,
        skip: bool,
    }
    impl ServiceController for Services {
        fn systemd_actions_disabled(&self) -> bool {
            self.skip
        }
        fn reload_user_manager(&self) -> Result<(), SettingsError> {
            self.calls.borrow_mut().push("reload".into());
            Ok(())
        }
        fn user_service_state(&self, _: &str) -> Result<UserServiceState, SettingsError> {
            unreachable!()
        }
        fn restart_user_service(&self, service: &str) -> Result<(), SettingsError> {
            self.calls.borrow_mut().push(format!("restart {service}"));
            if self.fail.replace(false) {
                Err(SettingsError::Apply {
                    message: "fixture service failure".into(),
                })
            } else {
                Ok(())
            }
        }
        fn enable_start_user_unit(
            &self,
            unit: &str,
        ) -> Result<UserUnitEnableOutcome, SettingsError> {
            self.calls.borrow_mut().push(format!("enable {unit}"));
            Ok(UserUnitEnableOutcome::Enabled)
        }
        fn disable_stop_user_unit(&self, unit: &str) -> Result<(), SettingsError> {
            self.calls.borrow_mut().push(format!("disable {unit}"));
            Ok(())
        }
    }

    #[test]
    fn activation_uses_defaults_without_rewriting_pairing_or_preferences() {
        let fixture = Fixture::new();
        let path = fixture.config("# retain user annotations\n");
        let before = fs::read(&path).unwrap();
        assert!(!pending_at(&path).unwrap());
        mark_pending(&path).unwrap();
        assert_eq!(
            fs::metadata(pending_path(&path))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let services = Services::default();
        let authorized = Cell::new(false);
        activate_at(&path, &services, || {
            authorized.set(true);
            Ok(())
        })
        .unwrap();
        assert!(authorized.get());
        assert_eq!(
            *services.calls.borrow(),
            [
                "reload",
                "enable LG_Buddy_screen.service",
                "restart LG_Buddy_screen.service",
                "enable LG_Buddy_update_check.timer",
                "restart LG_Buddy_update_check.timer"
            ]
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!pending_at(&path).unwrap());
    }

    #[test]
    fn failure_keeps_pending_state_and_saved_choices_for_retry() {
        let fixture = Fixture::new();
        let path = fixture.config(
            "screen_idle_blank=disabled\nupdates_auto_check=disabled\nscreen_idle_timeout=720\n",
        );
        let before = fs::read(&path).unwrap();
        mark_pending(&path).unwrap();
        let services = Services::default();
        services.fail.set(true);
        assert!(activate_at(&path, &services, || Ok(())).is_err());
        assert!(pending_at(&path).unwrap());
        activate_at(&path, &services, || Ok(())).unwrap();
        assert!(!pending_at(&path).unwrap());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(services
            .calls
            .borrow()
            .iter()
            .any(|call| call == "restart LG_Buddy_screen.service"));
        assert_eq!(
            services.calls.borrow().last().unwrap(),
            "disable LG_Buddy_update_check.timer"
        );
    }

    #[test]
    fn activation_requires_pending_intent_and_valid_configuration_before_authorization() {
        let fixture = Fixture::new();
        let path = fixture.config("");
        let services = Services::default();
        assert!(activate_at(&path, &services, || panic!("no setup requested")).is_err());
        mark_pending(&path).unwrap();
        fs::write(&path, "updates_auto_check=enabled\n").unwrap();
        assert!(activate_at(&path, &services, || panic!("no TV paired")).is_err());
        assert!(pending_at(&path).unwrap());
        assert!(services.calls.borrow().is_empty());
    }

    #[test]
    fn skipped_or_declined_activation_never_clears_pending_state() {
        let fixture = Fixture::new();
        let path = fixture.config("");
        mark_pending(&path).unwrap();
        assert!(activate_at(
            &path,
            &Services {
                skip: true,
                ..Services::default()
            },
            || panic!("skipped")
        )
        .is_err());
        let services = Services::default();
        assert!(activate_at(&path, &services, || Err(SetupError::stopped())).is_err());
        assert!(pending_at(&path).unwrap());
        assert!(services.calls.borrow().is_empty());
    }

    #[test]
    fn marker_does_not_follow_symlinks() {
        let fixture = Fixture::new();
        let path = fixture.config("");
        let before = fs::read(&path).unwrap();
        std::os::unix::fs::symlink(&path, pending_path(&path)).unwrap();
        assert!(mark_pending(&path).is_err());
        assert!(pending_at(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn graphical_authorization_is_limited_to_starting_installed_system_units() {
        let fixture = Fixture::new();
        let script = fixture.0.join("authorize");
        let output = fixture.0.join("arguments");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nexit 126\n",
                output.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let error = start_system_services_with(
            script.as_os_str(),
            std::ffi::OsStr::new("/usr/bin/systemctl"),
        )
        .unwrap_err();
        assert_eq!(error.0.summary(), "Authorization was cancelled");
        assert_eq!(fs::read_to_string(output).unwrap(), "--disable-internal-agent\n/usr/bin/systemctl\nstart\nLG_Buddy.service\nLG_Buddy_lifecycle.service\n");
    }
}
