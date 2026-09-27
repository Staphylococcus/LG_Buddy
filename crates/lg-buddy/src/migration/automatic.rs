//! Local, idempotent configuration conversion. No credential or runtime setup.
use super::{inspect_config, MigrationInspection, MonitoringChoice, ScreenChoiceRequired};
use crate::config::{
    parse_config_entries, parse_current_config, stale_config_reasons, CurrentConfig,
    StaleConfigReason,
};
use crate::pairing_store::{
    config::{config_snapshot, owner_for_write, ConfigWriteError, Point as WritePoint},
    PairingLock, PairingStoreError,
};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum MigrationOutcome {
    /// No saved TV or explicit legacy backend; ordinary first-time setup applies.
    Unconfigured,
    /// Already current; no config rewrite. Initially current input does not
    /// need a mutation lock or write access.
    Current(CurrentConfig),
    /// Config was atomically replaced and reloaded; this says nothing about
    /// native credentials, TV availability, or desktop capability.
    Converted {
        current: CurrentConfig,
        durability_warning: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomaticMigrationError {
    InvalidConfiguration,
    Storage,
    Busy,
    ConfigurationChanged,
    CommitIndeterminate,
    /// Publication succeeded, but the config could not be reloaded unchanged.
    CommittedReloadFailed,
}

impl fmt::Display for AutomaticMigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidConfiguration => "the saved configuration is incomplete or invalid",
            Self::Storage => "configuration conversion requires a readable file and safe write access as its non-root owner",
            Self::Busy => "another configuration writer is still active; retry startup",
            Self::ConfigurationChanged => "configuration changed during conversion; retry with the current file",
            Self::CommitIndeterminate => "configuration publication could not be confirmed; inspect the current file before retrying",
            Self::CommittedReloadFailed => "configuration was converted but could not be reloaded unchanged; retry startup",
        })
    }
}
impl std::error::Error for AutomaticMigrationError {}
impl From<ConfigWriteError> for AutomaticMigrationError {
    fn from(error: ConfigWriteError) -> Self {
        match error {
            ConfigWriteError::Changed => Self::ConfigurationChanged,
            ConfigWriteError::Storage => Self::Storage,
            ConfigWriteError::Indeterminate => Self::CommitIndeterminate,
        }
    }
}

/// Convert supported legacy settings before the caller starts config-dependent
/// work. This is an explicit mutation boundary; read-only loaders never call it.
/// The existing setup/runtime paths still own pairing and capability checks.
pub fn migrate_config(path: &Path) -> Result<MigrationOutcome, AutomaticMigrationError> {
    migrate_with(path, &mut |_| Ok(()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Point {
    BeforeLock,
    BeforePublish,
    Reload,
    Write(WritePoint),
}

enum Inspection {
    Ready(MigrationOutcome),
    Convert(String),
}

fn inspect(path: &Path, contents: Option<&str>) -> Result<Inspection, AutomaticMigrationError> {
    let Some(contents) = contents else {
        return Ok(Inspection::Ready(MigrationOutcome::Unconfigured));
    };
    let entries = parse_config_entries(contents);
    if !crate::tvs::has_tv_profile_fields(|key| entries.contains_key(key))
        && !stale_config_reasons(contents).contains(&StaleConfigReason::SwayidleBackend)
    {
        return Ok(Inspection::Ready(MigrationOutcome::Unconfigured));
    }
    match inspect_config(path, contents)
        .map_err(|_| AutomaticMigrationError::InvalidConfiguration)?
    {
        MigrationInspection::Current => Ok(Inspection::Ready(MigrationOutcome::Current(
            CurrentConfig {
                path: path.into(),
                config: parse_current_config(contents)
                    .map_err(|_| AutomaticMigrationError::InvalidConfiguration)?,
            },
        ))),
        MigrationInspection::Required(plan) => {
            // ponytail: the saved preference determines the conversion; the
            // legacy planner's choice is internal, never a new user decision.
            let choice = match plan.screen_choice_required() {
                ScreenChoiceRequired::Required => Some(MonitoringChoice::Native),
                ScreenChoiceRequired::FixedDisabled | ScreenChoiceRequired::NotApplicable => None,
            };
            let candidate = plan
                .select(choice)
                .map_err(|_| AutomaticMigrationError::InvalidConfiguration)?;
            Ok(Inspection::Convert(candidate.rendered().to_owned()))
        }
    }
}

fn read_config(path: &Path) -> Result<Option<String>, AutomaticMigrationError> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A dangling managed link is not a fresh installation.
            return if fs::symlink_metadata(path).is_ok() {
                Err(AutomaticMigrationError::Storage)
            } else {
                Ok(None)
            };
        }
        Err(_) => return Err(AutomaticMigrationError::Storage),
    };
    if !file.metadata().is_ok_and(|m| m.is_file()) {
        return Err(AutomaticMigrationError::Storage);
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|_| AutomaticMigrationError::Storage)?;
    Ok(Some(contents))
}

fn lock_config(path: &Path) -> Result<PairingLock, AutomaticMigrationError> {
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match PairingLock::for_config(path) {
            Ok(lock) => return Ok(lock),
            Err(PairingStoreError::PairingInProgress { .. }) => {
                if Instant::now() >= deadline {
                    return Err(AutomaticMigrationError::Busy);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err(AutomaticMigrationError::Storage),
        }
    }
}

fn migrate_with(
    path: &Path,
    hook: &mut dyn FnMut(Point) -> io::Result<()>,
) -> Result<MigrationOutcome, AutomaticMigrationError> {
    // Current and first-run input never acquire a mutation lock, create a
    // directory, or request write access (including managed/read-only config).
    if let Inspection::Ready(outcome) = inspect(path, read_config(path)?.as_deref())? {
        return Ok(outcome);
    }
    let owner = owner_for_write(path)?;
    hook(Point::BeforeLock).map_err(|_| AutomaticMigrationError::Storage)?;
    let guard = lock_config(path)?;
    if fs::canonicalize(path).ok().as_deref() != Some(guard.target()) {
        return Err(AutomaticMigrationError::ConfigurationChanged);
    }
    // A concurrent startup may already have converted the same file.
    if let Inspection::Ready(outcome) = inspect(path, read_config(path)?.as_deref())? {
        return Ok(outcome);
    }
    let snapshot = config_snapshot(path, &owner)?;
    let contents = std::str::from_utf8(&snapshot.bytes)
        .map_err(|_| AutomaticMigrationError::InvalidConfiguration)?;
    let rendered = match inspect(path, Some(contents))? {
        Inspection::Ready(outcome) => return Ok(outcome),
        Inspection::Convert(rendered) => rendered,
    };
    let stage = snapshot.stage(path, rendered.as_bytes(), &mut |point| {
        hook(Point::Write(point))
    })?;
    hook(Point::BeforePublish).map_err(|_| AutomaticMigrationError::Storage)?;
    if fs::canonicalize(path).ok().as_deref() != Some(guard.target()) {
        return Err(AutomaticMigrationError::ConfigurationChanged);
    }
    snapshot.check(path, &owner)?;
    let durability_warning =
        stage.publish(path, &snapshot.bytes, rendered.as_bytes(), &mut |point| {
            hook(Point::Write(point))
        })?;
    hook(Point::Reload).map_err(|_| AutomaticMigrationError::CommittedReloadFailed)?;
    // Do not continue startup on an intervening external edit after publication.
    let reloaded = read_config(path)
        .map_err(|_| AutomaticMigrationError::CommittedReloadFailed)?
        .ok_or(AutomaticMigrationError::CommittedReloadFailed)?;
    if reloaded != rendered {
        return Err(AutomaticMigrationError::CommittedReloadFailed);
    }
    let current = CurrentConfig {
        path: path.into(),
        config: parse_current_config(&reloaded)
            .map_err(|_| AutomaticMigrationError::CommittedReloadFailed)?,
    };
    Ok(MigrationOutcome::Converted {
        current,
        durability_warning,
    })
}

#[cfg(test)]
mod tests;
