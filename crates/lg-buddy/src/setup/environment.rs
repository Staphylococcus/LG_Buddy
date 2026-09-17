//! Concrete adapters own domain input routing. The flow sees only shared
//! responses; configuration, authorization mode and paths are resolved once.
use super::{
    flow::{AuthorizationMode, SetupStep, SetupSteps, StepAnswer},
    StepCancellation, StepFailure, StepResponse,
};
use crate::{
    presentation::brightness::UserFacingError,
    settings::{ConfigPathResolver, ServiceController, SystemdUserServiceController},
};
use std::{
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};

pub(super) struct SetupContext {
    pub config: PathBuf,
    pub user_units: PathBuf,
    pub system_root: PathBuf,
    pub kwin_helper: PathBuf,
    pub lock_path: PathBuf,
    pub authorization: AuthorizationMode,
}

impl SetupContext {
    pub(super) fn from_env(authorization: AuthorizationMode) -> Result<Self, StepFailure> {
        let uid = unsafe { libc::geteuid() };
        if uid == 0 {
            return Err(context_failure("Run setup as the desktop user, not root."));
        }
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| context_failure("HOME must be an absolute path."))?;
        let config = ConfigPathResolver::resolve_from_env().map_err(context_failure)?;
        let config = std::path::absolute(config).map_err(context_failure)?;
        Ok(Self {
            config,
            user_units: user_units_directory(&home, env::var_os("XDG_CONFIG_HOME")),
            system_root: PathBuf::from("/"),
            kwin_helper: PathBuf::from("/usr/lib/lg-buddy/kwin/setup.sh"),
            // A different config or caller-provided runtime override cannot
            // bypass another GUI/CLI flow for this user.
            lock_path: PathBuf::from(format!("/run/user/{uid}/lg-buddy-onboarding.lock")),
            authorization,
        })
    }
}

fn user_units_directory(home: &Path, xdg_config_home: Option<OsString>) -> PathBuf {
    xdg_config_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".config"))
        .join("systemd/user")
}

pub(super) struct NativeSteps<C = SystemdUserServiceController> {
    pub context: SetupContext,
    pub controller: C,
}

impl NativeSteps {
    pub(super) fn new(context: SetupContext) -> Self {
        Self {
            context,
            controller: SystemdUserServiceController::from_env(),
        }
    }
}

impl<C: ServiceController> NativeSteps<C> {
    fn services(&self) -> super::provision::ServiceInstallation<'_, C> {
        super::provision::ServiceInstallation {
            config: &self.context.config,
            user_units: &self.context.user_units,
            system_root: &self.context.system_root,
            controller: &self.controller,
            authorization: self.context.authorization,
        }
    }
    fn plasma(&self) -> super::kwin::KWinSetup<'_> {
        super::kwin::KWinSetup {
            helper: &self.context.kwin_helper,
            authorization: self.context.authorization,
            command_lock: None,
        }
    }
}

impl<C: ServiceController + Send> SetupSteps for NativeSteps<C> {
    fn inspect(&self, step: SetupStep) -> StepResponse {
        match step {
            SetupStep::Pairing => super::pairing::inspect(&self.context.config),
            SetupStep::Services => self.services().inspect(),
            SetupStep::Plasma => self.plasma().inspect(),
        }
    }
    fn execute(
        &self,
        step: SetupStep,
        answer: StepAnswer,
        cancellation: &StepCancellation,
        lease: &super::lock::FlowLock,
        progress: &mut dyn FnMut(StepResponse),
    ) -> StepResponse {
        match (step, answer) {
            (SetupStep::Pairing, StepAnswer::Pairing(request)) => {
                super::pairing::execute(&self.context.config, Some(request), cancellation, progress)
            }
            (SetupStep::Services, StepAnswer::Continue) => {
                self.controller
                    .with_command_lock(lease.file(), |controller| {
                        super::provision::ServiceInstallation {
                            config: &self.context.config,
                            user_units: &self.context.user_units,
                            system_root: &self.context.system_root,
                            controller,
                            authorization: self.context.authorization,
                        }
                        .execute(cancellation, progress)
                    })
            }
            (SetupStep::Plasma, StepAnswer::Continue) => {
                let mut step = self.plasma();
                step.command_lock = Some(lease.file());
                step.execute(false, cancellation, progress)
            }
            (SetupStep::Plasma, StepAnswer::InstallBuildDependencies) => {
                let mut step = self.plasma();
                step.command_lock = Some(lease.file());
                step.execute(true, cancellation, progress)
            }
            _ => StepResponse::Failed(StepFailure {
                presentation: UserFacingError::new(
                    "Setup input required",
                    "Respond to the current setup request before continuing.",
                ),
                diagnostic: "answer does not match the setup step".into(),
                retryable: true,
            }),
        }
    }
}

fn context_failure(error: impl ToString) -> StepFailure {
    StepFailure {
        presentation: UserFacingError::new(
            "Setup unavailable",
            "The user configuration and session could not be resolved.",
        ),
        diagnostic: error.to_string(),
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn user_units_follow_the_xdg_configuration_directory() {
        let home = Path::new("/home/user");
        for value in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative")),
        ] {
            assert_eq!(
                user_units_directory(home, value),
                home.join(".config/systemd/user")
            );
        }
        assert_eq!(
            user_units_directory(home, Some("/custom/config".into())),
            PathBuf::from("/custom/config/systemd/user")
        );
    }

    #[test]
    fn context_child() {
        let Some(expected) = env::var_os("LG_BUDDY_CONTEXT_TEST_UNITS") else {
            return;
        };
        let context = SetupContext::from_env(AuthorizationMode::Noninteractive).unwrap();
        assert_eq!(context.user_units, PathBuf::from(expected));
        let uid = unsafe { libc::geteuid() };
        assert_eq!(
            context.lock_path,
            PathBuf::from(format!("/run/user/{uid}/lg-buddy-onboarding.lock"))
        );
    }

    #[test]
    fn production_context_honors_xdg_without_changing_the_flow_lock() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let status = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "setup::environment::tests::context_child",
                "--nocapture",
            ])
            .env("XDG_CONFIG_HOME", "/custom/lg-buddy-test-config")
            .env("XDG_RUNTIME_DIR", "/custom/lg-buddy-test-runtime")
            .env(
                "LG_BUDDY_CONTEXT_TEST_UNITS",
                "/custom/lg-buddy-test-config/systemd/user",
            )
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
}
