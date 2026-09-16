//! Native service installation/repair. Context is supplied by the flow.
use super::{StepCancellation, StepFailure, StepResponse};
use crate::presentation::brightness::UserFacingError;
use crate::settings::{
    ServiceController, SettingValue, SettingsError, SettingsStore, UserServiceState,
};
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const SCREEN: &str = "LG_Buddy_screen.service";
const TIMER: &str = "LG_Buddy_update_check.timer";
const LIFECYCLE: &str = "LG_Buddy_lifecycle.service";
const STARTUP: &str = "LG_Buddy.service";
const NM_HOOK: &str =
    "#!/bin/sh\nset -eu\n[ \"${2:-}\" = pre-down ] || exit 0\nexec /usr/bin/lg-buddy nm-pre-down\n";

pub(crate) struct ServiceInstallation<'a, C> {
    pub config: &'a Path,
    pub user_units: &'a Path,
    pub system_root: &'a Path,
    pub interactive_authorization: bool,
    pub controller: &'a C,
}

struct FileSpec {
    path: PathBuf,
    contents: String,
    mode: u32,
    owner: u32,
}
fn file(path: PathBuf, contents: impl Into<String>, mode: u32) -> FileSpec {
    FileSpec {
        path,
        contents: contents.into(),
        mode,
        owner: unsafe { libc::geteuid() },
    }
}

impl<C: ServiceController> ServiceInstallation<'_, C> {
    pub(crate) fn inspect(&self) -> StepResponse {
        if self.system_root.join("etc/NIXOS").exists()
            || self.system_root.join("run/ostree-booted").exists()
        {
            return StepResponse::Blocked(failure(
                "This installation does not support automatic service setup.",
                "declaratively managed or immutable system",
                false,
            ));
        }
        match self.ready() {
            Ok(true) => StepResponse::Complete,
            Ok(false) if self.controller.systemd_actions_disabled() => {
                StepResponse::Blocked(failure(
                    "Service changes are disabled.",
                    "systemd mutations disabled",
                    false,
                ))
            }
            Ok(false) => match self.system_ready() {
                Ok(system_ready) => StepResponse::ActionRequired {
                    explanation: "Install or repair LG Buddy's background services.",
                    requires_authorization: !system_ready,
                },
                Err(error) => StepResponse::Failed(failure(
                    "Service setup could not be checked.",
                    error,
                    true,
                )),
            },
            Err(error) => {
                StepResponse::Failed(failure("Service setup could not be checked.", error, true))
            }
        }
    }

    pub(crate) fn execute(
        &self,
        cancellation: &StepCancellation,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse {
        if !cancellation.begin() {
            return if cancellation.is_cancelled() {
                StepResponse::Cancelled
            } else {
                StepResponse::Blocked(failure(
                    "This attempt has already started.",
                    "duplicate service setup attempt",
                    false,
                ))
            };
        }
        progress(StepResponse::Running {
            message: "Setting up LG Buddy background services…",
            cancelable: false,
        });
        let before = self.inspect();
        let response = if matches!(before, StepResponse::ActionRequired { .. }) {
            match self.repair() {
                Ok(()) => match self.inspect() {
                    StepResponse::Complete => StepResponse::Complete,
                    StepResponse::Failed(error) | StepResponse::Blocked(error) => {
                        StepResponse::Failed(error)
                    }
                    _ => StepResponse::Failed(failure(
                        "Service setup did not become ready.",
                        "verification after service repair failed",
                        true,
                    )),
                },
                Err(SettingsError::ActivationCancelled) => StepResponse::Cancelled,
                Err(error) => StepResponse::Failed(failure(
                    "Service setup could not be completed.",
                    error,
                    true,
                )),
            }
        } else {
            before
        };
        cancellation.finish();
        response
    }

    fn desired_timer(&self) -> Result<bool, SettingsError> {
        let store = SettingsStore::load(self.config)?;
        Ok(store
            .effective_by_name("updates.auto_check")?
            .required_value()?
            == SettingValue::Enum("enabled"))
    }
    fn override_contents(&self) -> Result<String, SettingsError> {
        let config = fs::canonicalize(self.config).map_err(io_error)?;
        let config = config
            .to_str()
            .ok_or_else(|| io_error("configuration path is not UTF-8"))?;
        if config.contains(['\n', '\r']) {
            return Err(io_error("configuration path contains a line break"));
        }
        let escaped = config
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%");
        Ok(format!(
            "[Service]\nEnvironment=\"LG_BUDDY_CONFIG={escaped}\"\n"
        ))
    }
    fn user_files(&self) -> Result<Vec<FileSpec>, SettingsError> {
        let override_contents = self.override_contents()?;
        Ok(vec![
            file(
                self.user_units.join(SCREEN),
                include_str!("../../../../systemd/LG_Buddy_screen.service"),
                0o644,
            ),
            file(
                self.user_units.join(TIMER),
                include_str!("../../../../systemd/LG_Buddy_update_check.timer"),
                0o644,
            ),
            file(
                self.user_units.join("LG_Buddy_update_check.service"),
                include_str!("../../../../systemd/LG_Buddy_update_check.service"),
                0o644,
            ),
            file(
                self.user_units
                    .join("LG_Buddy_screen.service.d/config.conf"),
                override_contents.clone(),
                0o644,
            ),
            file(
                self.user_units
                    .join("LG_Buddy_update_check.service.d/config.conf"),
                override_contents,
                0o644,
            ),
        ])
    }
    fn system_files(&self) -> Result<Vec<FileSpec>, SettingsError> {
        let override_contents = self.override_contents()?;
        let mut files = vec![
            file(
                self.system_root.join("etc/systemd/system/LG_Buddy.service"),
                include_str!("../../../../systemd/LG_Buddy.service"),
                0o644,
            ),
            file(
                self.system_root
                    .join("etc/systemd/system/LG_Buddy_lifecycle.service"),
                include_str!("../../../../systemd/LG_Buddy_lifecycle.service"),
                0o644,
            ),
            file(
                self.system_root
                    .join("etc/systemd/system/LG_Buddy.service.d/config.conf"),
                override_contents.clone(),
                0o644,
            ),
            file(
                self.system_root
                    .join("etc/systemd/system/LG_Buddy_lifecycle.service.d/config.conf"),
                override_contents,
                0o644,
            ),
            file(
                self.system_root.join("etc/tmpfiles.d/lg_buddy.conf"),
                include_str!("../../../../systemd/lg_buddy.conf"),
                0o644,
            ),
            file(
                self.system_root
                    .join("etc/NetworkManager/dispatcher.d/pre-down.d/LG_Buddy_lifecycle"),
                NM_HOOK,
                0o755,
            ),
            file(
                self.system_root.join("usr/lib/lg-buddy/config-path"),
                format!(
                    "{}\n",
                    fs::canonicalize(self.config).map_err(io_error)?.display()
                ),
                0o644,
            ),
        ];
        if self.system_root == Path::new("/") {
            for spec in &mut files {
                spec.owner = 0;
            }
        }
        Ok(files)
    }
    fn system_ready(&self) -> Result<bool, SettingsError> {
        Ok(files_match(&self.system_files()?)?
            && self.controller.system_unit_is_enabled(STARTUP)?
            && self.controller.system_unit_is_enabled(LIFECYCLE)?
            && self.controller.system_lifecycle_is_active()?
            && self.binding_matches(self.controller.system_service_config_path(STARTUP))?
            && self.binding_matches(self.controller.system_service_config_path(LIFECYCLE))?)
    }
    fn ready(&self) -> Result<bool, SettingsError> {
        let timer = self.desired_timer()?;
        if !files_match(&self.user_files()?)? || !self.system_ready()? {
            return Ok(false);
        }
        if !self.binding_matches(self.controller.user_service_config_path(SCREEN))?
            || !self.binding_matches(
                self.controller
                    .user_service_config_path("LG_Buddy_update_check.service"),
            )?
        {
            return Ok(false);
        }
        Ok(self.controller.user_unit_is_enabled(SCREEN)?
            && self.controller.user_service_is_active(SCREEN)?
            && self.controller.user_unit_is_enabled(TIMER)? == timer
            && self.controller.user_service_is_active(TIMER)? == timer)
    }
    fn binding_matches(
        &self,
        declared: Result<PathBuf, SettingsError>,
    ) -> Result<bool, SettingsError> {
        let expected = fs::canonicalize(self.config).map_err(io_error)?;
        // A missing or uninspectable binding is never proof of completion.
        Ok(declared
            .ok()
            .and_then(|path| fs::canonicalize(path).ok())
            .as_ref()
            == Some(&expected))
    }
    fn repair(&self) -> Result<(), SettingsError> {
        let timer = self.desired_timer()?;
        if !self.system_ready()? {
            self.controller.repair_system_services(
                &fs::canonicalize(self.config).map_err(io_error)?,
                self.interactive_authorization,
            )?;
        }
        let stale_binding =
            !self.binding_matches(self.controller.user_service_config_path(SCREEN))?;
        let files = self.user_files()?;
        // Stop before changing files or reloading: manager properties describe
        // the next invocation, not the environment of an existing process.
        // An interrupted repair then remains visibly incomplete (inactive).
        if (stale_binding || !files_match(&files)?)
            && self.controller.user_service_state(SCREEN)? != UserServiceState::Missing
        {
            self.controller.stop_user_service(SCREEN)?;
        }
        for spec in files {
            write_file(&spec)?;
        }
        // Reload on every incomplete attempt: previous execution may have stopped after writing files.
        self.controller.reload_user_units()?;
        if !self.controller.user_unit_is_enabled(SCREEN)?
            || !self.controller.user_service_is_active(SCREEN)?
        {
            self.controller.enable_start_user_unit(SCREEN)?;
        }
        if !self.controller.user_service_is_active(SCREEN)? {
            self.controller.restart_user_service(SCREEN)?;
        }
        if timer {
            if !self.controller.user_unit_is_enabled(TIMER)?
                || !self.controller.user_service_is_active(TIMER)?
            {
                self.controller.enable_start_user_unit(TIMER)?;
            }
            if !self.controller.user_service_is_active(TIMER)? {
                self.controller.restart_user_service(TIMER)?;
            }
        } else if self.controller.user_unit_is_enabled(TIMER)?
            || self.controller.user_service_is_active(TIMER)?
        {
            self.controller.disable_stop_user_unit(TIMER)?;
        }
        Ok(())
    }
}

fn failure(message: &str, diagnostic: impl ToString, retryable: bool) -> StepFailure {
    StepFailure {
        presentation: UserFacingError::new("Service setup incomplete", message),
        diagnostic: diagnostic.to_string(),
        retryable,
    }
}
fn io_error(error: impl ToString) -> SettingsError {
    SettingsError::Activation {
        message: error.to_string(),
    }
}
fn matches_file(spec: &FileSpec) -> Result<bool, SettingsError> {
    match fs::symlink_metadata(&spec.path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(error)),
        Ok(metadata) => Ok(metadata.is_file()
            && metadata.uid() == spec.owner
            && metadata.permissions().mode() & 0o7777 == spec.mode
            && fs::read(&spec.path).map_err(io_error)? == spec.contents.as_bytes()),
    }
}
fn files_match(files: &[FileSpec]) -> Result<bool, SettingsError> {
    for spec in files {
        if !matches_file(spec)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn write_file(spec: &FileSpec) -> Result<bool, SettingsError> {
    use std::os::unix::fs::OpenOptionsExt;
    if matches_file(spec)? {
        return Ok(false);
    }
    if fs::symlink_metadata(&spec.path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io_error("refusing to replace a service symlink"));
    }
    fs::create_dir_all(spec.path.parent().unwrap()).map_err(io_error)?;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let (temporary, mut output) = loop {
        let path = spec.path.with_extension(format!(
            "setup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(spec.mode)
            .open(&path)
        {
            Ok(output) => break (path, output),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(error)),
        }
    };
    let result = (|| {
        output.write_all(spec.contents.as_bytes())?;
        output.set_permissions(fs::Permissions::from_mode(spec.mode))?;
        output.sync_all()?;
        fs::rename(&temporary, &spec.path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(io_error)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
