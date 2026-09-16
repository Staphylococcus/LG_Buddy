use std::env;
use std::fmt;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::Duration;
use std::{fs::File, sync::Arc};

use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;

use super::SettingsError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsApplyOutcome {
    Restarted { service: &'static str },
    Enabled { unit: &'static str },
    EnabledStarted { unit: &'static str },
    DisabledStopped { unit: &'static str },
    NotInstalled { service: &'static str },
    InactiveDisabled { service: &'static str },
    Skipped { reason: String },
    NoActionRequired,
}

impl fmt::Display for SettingsApplyOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Restarted { service } => write!(f, "restarted {service}"),
            Self::Enabled { unit } => write!(f, "enabled {unit}"),
            Self::EnabledStarted { unit } => write!(f, "enabled and started {unit}"),
            Self::DisabledStopped { unit } => write!(f, "disabled and stopped {unit}"),
            Self::NotInstalled { service } => {
                write!(
                    f,
                    "{service} is not installed; change applies when it is installed"
                )
            }
            Self::InactiveDisabled { service } => write!(
                f,
                "{service} is inactive and disabled; change applies when it is started"
            ),
            Self::Skipped { reason } => write!(f, "{reason}"),
            Self::NoActionRequired => write!(f, "no runtime apply action required"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserServiceState {
    Missing,
    InactiveDisabled,
    ActiveOrEnabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserUnitEnableOutcome {
    Enabled,
    EnabledStarted,
}

pub trait ServiceController {
    /// Scope subprocess supervision to an onboarding operation. Controllers
    /// without subprocesses can use the default implementation.
    fn with_command_lock<T>(&self, _lock: Arc<File>, operation: impl FnOnce(&Self) -> T) -> T
    where
        Self: Sized,
    {
        operation(self)
    }

    fn systemd_actions_disabled(&self) -> bool {
        false
    }

    fn user_service_state(&self, service: &str) -> Result<UserServiceState, SettingsError>;

    fn user_service_config_path(&self, _service: &str) -> Result<PathBuf, SettingsError> {
        Err(SettingsError::Activation {
            message: "the screen service's configuration could not be inspected".to_string(),
        })
    }

    /// The coarse state above intentionally preserves the CLI's historical
    /// behavior. GUI onboarding also needs to distinguish an enabled but
    /// currently stopped unit before publishing an enabled setting.
    fn user_service_is_active(&self, service: &str) -> Result<bool, SettingsError> {
        Ok(matches!(
            self.user_service_state(service)?,
            UserServiceState::ActiveOrEnabled
        ))
    }

    fn restart_user_service(&self, service: &str) -> Result<(), SettingsError>;

    fn stop_user_service(&self, _service: &str) -> Result<(), SettingsError> {
        Err(SettingsError::Activation {
            message: "user service stop is unavailable".into(),
        })
    }

    fn enable_start_user_unit(&self, unit: &str) -> Result<UserUnitEnableOutcome, SettingsError>;

    fn disable_stop_user_unit(&self, unit: &str) -> Result<(), SettingsError>;

    fn user_unit_is_enabled(&self, unit: &str) -> Result<bool, SettingsError> {
        Ok(matches!(
            self.user_service_state(unit)?,
            UserServiceState::ActiveOrEnabled
        ))
    }
    fn system_unit_is_enabled(&self, _unit: &str) -> Result<bool, SettingsError> {
        Err(SettingsError::Activation {
            message: "system service enablement could not be checked".into(),
        })
    }
    fn reload_user_units(&self) -> Result<(), SettingsError> {
        Err(SettingsError::Activation {
            message: "user service reload is unavailable".into(),
        })
    }
    fn system_service_config_path(&self, _service: &str) -> Result<PathBuf, SettingsError> {
        Err(SettingsError::Activation {
            message: "system service configuration could not be checked".into(),
        })
    }
    fn repair_system_services(
        &self,
        _config: &Path,
        _interactive: bool,
    ) -> Result<(), SettingsError> {
        Err(SettingsError::Activation {
            message: "system service repair is unavailable".into(),
        })
    }

    fn system_lifecycle_is_active(&self) -> Result<bool, SettingsError> {
        Ok(false)
    }

    fn start_system_lifecycle(&self) -> Result<(), SettingsError> {
        Err(SettingsError::Apply {
            message: "system lifecycle service activation is unavailable".to_string(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SystemdUserServiceController {
    command_path: PathBuf,
    skip_systemd_actions: bool,
    command_lock: Option<Arc<File>>,
}

impl Default for SystemdUserServiceController {
    fn default() -> Self {
        Self::from_env()
    }
}

impl SystemdUserServiceController {
    pub fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_SYSTEMCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("systemctl")),
            skip_systemd_actions: env_truthy("LG_BUDDY_SKIP_SYSTEMD_ACTIONS"),
            command_lock: None,
        }
    }

    fn user_systemctl_status(&self, args: &[&str]) -> io::Result<bool> {
        ProcessCommand::new(&self.command_path)
            .arg("--user")
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
    }

    fn systemctl_status(&self, args: &[&str]) -> io::Result<bool> {
        ProcessCommand::new(&self.command_path)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
    }

    fn run_user_systemctl(&self, args: &[&str]) -> Result<(), SettingsError> {
        let output =
            crate::setup::lock::command_with_lock(&self.command_path, self.command_lock.as_ref())
                .arg("--user")
                .args(args)
                .output()
                .map_err(|err| SettingsError::Apply {
                    message: format!("could not run systemctl: {err}"),
                })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(SettingsError::Apply {
                message: format_command_failure(
                    output.status.code(),
                    &output.stdout,
                    &output.stderr,
                ),
            })
        }
    }
}

impl ServiceController for SystemdUserServiceController {
    fn with_command_lock<T>(&self, lock: Arc<File>, operation: impl FnOnce(&Self) -> T) -> T {
        let mut controller = self.clone();
        controller.command_lock = Some(lock);
        operation(&controller)
    }

    fn systemd_actions_disabled(&self) -> bool {
        self.skip_systemd_actions
    }

    fn user_service_state(&self, service: &str) -> Result<UserServiceState, SettingsError> {
        if !self
            .user_systemctl_status(&["cat", service])
            .unwrap_or(false)
        {
            return Ok(UserServiceState::Missing);
        }

        let active = self
            .user_systemctl_status(&["is-active", "--quiet", service])
            .unwrap_or(false);
        let enabled = self
            .user_systemctl_status(&["is-enabled", "--quiet", service])
            .unwrap_or(false);

        if active || enabled {
            Ok(UserServiceState::ActiveOrEnabled)
        } else {
            Ok(UserServiceState::InactiveDisabled)
        }
    }

    fn user_service_is_active(&self, service: &str) -> Result<bool, SettingsError> {
        Ok(self
            .user_systemctl_status(&["is-active", "--quiet", service])
            .unwrap_or(false))
    }

    fn user_service_config_path(&self, service: &str) -> Result<PathBuf, SettingsError> {
        configured_service_path(user_manager_connection(), service)
    }

    fn system_service_config_path(&self, service: &str) -> Result<PathBuf, SettingsError> {
        configured_service_path(dbus::blocking::Connection::new_system(), service)
    }

    fn restart_user_service(&self, service: &str) -> Result<(), SettingsError> {
        self.run_user_systemctl(&["restart", service])
    }

    fn stop_user_service(&self, service: &str) -> Result<(), SettingsError> {
        self.run_user_systemctl(&["stop", service])
    }

    fn enable_start_user_unit(&self, unit: &str) -> Result<UserUnitEnableOutcome, SettingsError> {
        self.run_user_systemctl(&["enable", unit])?;
        if self
            .user_systemctl_status(&["is-active", "--quiet", "graphical-session.target"])
            .unwrap_or(false)
        {
            self.run_user_systemctl(&["start", unit])?;
            Ok(UserUnitEnableOutcome::EnabledStarted)
        } else {
            Ok(UserUnitEnableOutcome::Enabled)
        }
    }

    fn disable_stop_user_unit(&self, unit: &str) -> Result<(), SettingsError> {
        self.run_user_systemctl(&["disable", "--now", unit])
    }

    fn user_unit_is_enabled(&self, unit: &str) -> Result<bool, SettingsError> {
        self.user_systemctl_status(&["is-enabled", "--quiet", unit])
            .map_err(|error| SettingsError::Activation {
                message: error.to_string(),
            })
    }
    fn system_unit_is_enabled(&self, unit: &str) -> Result<bool, SettingsError> {
        self.systemctl_status(&["is-enabled", "--quiet", unit])
            .map_err(|error| SettingsError::Activation {
                message: error.to_string(),
            })
    }
    fn reload_user_units(&self) -> Result<(), SettingsError> {
        self.run_user_systemctl(&["daemon-reload"])
    }
    fn repair_system_services(
        &self,
        config: &Path,
        interactive: bool,
    ) -> Result<(), SettingsError> {
        let mut command = if interactive {
            let mut command = crate::setup::lock::command_with_lock(
                "/usr/bin/pkexec",
                self.command_lock.as_ref(),
            );
            command.arg("--disable-internal-agent");
            command
        } else {
            let mut command =
                crate::setup::lock::command_with_lock("/usr/bin/sudo", self.command_lock.as_ref());
            command.arg("-n");
            command
        };
        let output = command
            .arg("/usr/lib/lg-buddy/setup-services")
            .arg(config)
            .output()
            .map_err(|error| SettingsError::Activation {
                message: error.to_string(),
            })?;
        if output.status.success() {
            Ok(())
        } else if output.status.code() == Some(126) {
            Err(SettingsError::ActivationCancelled)
        } else {
            Err(SettingsError::Activation {
                message: format_command_failure(
                    output.status.code(),
                    &output.stdout,
                    &output.stderr,
                ),
            })
        }
    }

    fn system_lifecycle_is_active(&self) -> Result<bool, SettingsError> {
        Ok(self
            .systemctl_status(&["is-active", "--quiet", "LG_Buddy_lifecycle.service"])
            .unwrap_or(false))
    }

    fn start_system_lifecycle(&self) -> Result<(), SettingsError> {
        // pkexec resolves relative commands through the caller's PATH before
        // sanitizing the environment. Keep privileged selection independent of
        // both PATH and the test/diagnostic systemctl override.
        let systemctl = ["/usr/bin/systemctl", "/run/current-system/sw/bin/systemctl"]
            .into_iter()
            .find(|path| Path::new(path).is_file())
            .ok_or_else(|| SettingsError::Activation {
                message: "systemctl was not found in a trusted system location".to_string(),
            })?;
        let output = crate::setup::lock::command_with_lock("pkexec", self.command_lock.as_ref())
            .arg("--disable-internal-agent")
            .arg(systemctl)
            .arg("start")
            .arg("LG_Buddy_lifecycle.service")
            .output()
            .map_err(|error| SettingsError::Activation {
                message: format!("could not request system lifecycle activation: {error}"),
            })?;

        if output.status.success() {
            Ok(())
        } else if output.status.code() == Some(126) {
            Err(SettingsError::ActivationCancelled)
        } else {
            Err(SettingsError::Activation {
                message: format_command_failure(
                    output.status.code(),
                    &output.stdout,
                    &output.stderr,
                ),
            })
        }
    }
}

fn configured_service_path(
    connection: Result<dbus::blocking::Connection, dbus::Error>,
    service: &str,
) -> Result<PathBuf, SettingsError> {
    // Read typed systemd properties so paths containing spaces, quotes or
    // shell metacharacters do not need to be parsed from systemctl output.
    let inspect = || -> Result<PathBuf, Box<dyn std::error::Error>> {
        let connection = connection?;
        let manager = connection.with_proxy(
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            Duration::from_secs(2),
        );
        let (path,): (dbus::Path<'static>,) =
            manager.method_call("org.freedesktop.systemd1.Manager", "LoadUnit", (service,))?;
        let unit = connection.with_proxy("org.freedesktop.systemd1", path, Duration::from_secs(2));
        let environment: Vec<String> =
            unit.get("org.freedesktop.systemd1.Service", "Environment")?;
        let files: Vec<(String, bool)> =
            unit.get("org.freedesktop.systemd1.Service", "EnvironmentFiles")?;
        let unset: Vec<String> =
            unit.get("org.freedesktop.systemd1.Service", "UnsetEnvironment")?;
        if !files.is_empty() {
            return Err("the service uses environment files; its configuration override could not be verified".into());
        }
        let assignment = environment
            .iter()
            .rev()
            .find(|value| value.starts_with("LG_BUDDY_CONFIG="))
            .filter(|value| {
                !unset
                    .iter()
                    .any(|removed| removed == "LG_BUDDY_CONFIG" || removed == *value)
            })
            .ok_or("the service does not declare LG_BUDDY_CONFIG")?;
        let path = PathBuf::from(assignment.strip_prefix("LG_BUDDY_CONFIG=").unwrap());
        if !path.is_absolute() {
            return Err("the service's LG_BUDDY_CONFIG is not an absolute path".into());
        }
        Ok(path)
    };
    inspect().map_err(|error| SettingsError::Activation {
        message: format!("could not verify {service}'s configuration: {error}"),
    })
}

fn user_manager_connection() -> Result<dbus::blocking::Connection, dbus::Error> {
    if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR") {
        // Like systemctl --user, address the user manager independently of the
        // desktop's session bus (which may belong to dbus-run-session).
        let socket = PathBuf::from(runtime_dir).join("systemd/private");
        let mut address = String::from("unix:path=");
        for byte in socket.as_os_str().as_encoded_bytes() {
            // D-Bus addresses use percent escapes, including for delimiters
            // such as commas and semicolons that can occur in a pathname.
            write!(address, "%{byte:02X}").expect("writing to a String cannot fail");
        }
        // This is a peer connection, not a bus: do not send the bus-only Hello
        // request that Connection::new_address uses to register a client.
        return dbus::channel::Channel::open_private(&address).map(Into::into);
    }

    match env::var("DBUS_SESSION_BUS_ADDRESS") {
        Ok(address) => dbus::blocking::Connection::new_address(&address),
        Err(_) => dbus::blocking::Connection::new_session(),
    }
}

fn env_truthy(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.as_str(),
                "1" | "true" | "TRUE" | "True" | "yes" | "YES" | "Yes"
            )
        })
        .unwrap_or(false)
}

fn format_command_failure(status_code: Option<i32>, stdout: &[u8], stderr: &[u8]) -> String {
    let status = status_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "signal".to_string());
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();

    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => format!("systemctl exited with status {status}"),
        (false, true) => format!("systemctl exited with status {status}: {stdout}"),
        (true, false) => format!("systemctl exited with status {status}: {stderr}"),
        (false, false) => format!("systemctl exited with status {status}: {stderr}; {stdout}"),
    }
}
