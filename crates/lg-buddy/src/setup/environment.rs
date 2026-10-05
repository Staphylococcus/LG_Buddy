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
    sync::Arc,
};

pub(super) struct SetupContext {
    pub config: PathBuf,
    pub user_units: PathBuf,
    pub system_root: PathBuf,
    pub kwin_helper: PathBuf,
    pub lock_path: PathBuf,
    pub authorization: AuthorizationMode,
    pub authorization_session: Arc<super::authorization::AuthorizationSession>,
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
            kwin_helper: installed_kwin_helper(&env::current_exe().map_err(context_failure)?),
            // A different config or caller-provided runtime override cannot
            // bypass another GUI/CLI flow for this user.
            lock_path: PathBuf::from(format!("/run/user/{uid}/lg-buddy-onboarding.lock")),
            authorization,
            authorization_session: Arc::new(super::authorization::AuthorizationSession::new(
                authorization,
            )),
        })
    }
}

fn installed_kwin_helper(executable: &Path) -> PathBuf {
    if let Some(prefix) = executable.parent().and_then(Path::parent) {
        let helper = prefix.join("lib/lg-buddy/kwin/setup.sh");
        if helper.is_file() {
            return helper;
        }
    }
    PathBuf::from("/usr/lib/lg-buddy/kwin/setup.sh")
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

pub(super) fn restart_verifier() -> Result<(), StepFailure> {
    let context = SetupContext::from_env(AuthorizationMode::Noninteractive)?;
    restart_verifier_with(&context, &SystemdUserServiceController::from_env())
}

pub(super) fn restart_verifier_with(
    context: &SetupContext,
    controller: &impl ServiceController,
) -> Result<(), StepFailure> {
    use super::recovery::{
        RecoveryAction as Action, RecoveryCause as Cause, RepairBoundary as Boundary, SetupRecovery,
    };
    let failure = |cause, boundary, action, detail: &str, diagnostic: String| StepFailure {
        presentation: UserFacingError::new("Session service needs recovery", detail),
        recovery: SetupRecovery::new(cause, boundary, action),
        diagnostic,
        retryable: true,
    };
    if context.system_root.join("etc/NIXOS").exists()
        || context.system_root.join("run/ostree-booted").exists()
        || controller.systemd_actions_disabled()
    {
        return Err(failure(Cause::ManagedInstallation, Boundary::SystemConfiguration, Action::RepairExternally, "Restart LG Buddy's session service through your system configuration or image, then recheck setup.", "session service is externally managed".into()));
    }
    let binding = controller
        .user_service_config_path("LG_Buddy_screen.service")
        .map_err(|error| {
            failure(
                Cause::MissingIntegration,
                Boundary::LocalSetup,
                Action::Repair,
                "Repair LG Buddy's background services, then verify setup again.",
                error.to_string(),
            )
        })?;
    let normalized = |path: &Path| path.canonicalize().or_else(|_| std::path::absolute(path));
    if !matches!((normalized(&binding), normalized(&context.config)), (Ok(binding), Ok(config)) if binding == config)
    {
        return Err(failure(Cause::InvalidConfiguration, Boundary::SystemConfiguration, Action::RepairExternally, "The session service is bound to another configuration. Correct its binding externally, then recheck setup.", "refusing to restart a differently bound session service".into()));
    }
    let lease = super::lock::FlowLock::acquire(&context.lock_path)?;
    controller.with_command_lock(lease.file(), |controller| controller.restart_user_service("LG_Buddy_screen.service")).map_err(|error| failure(Cause::TemporaryFailure, Boundary::SessionService, Action::Retry, "The session service could not be restarted. Check Diagnostics, then retry verification.", error.to_string()))
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
            authorization_session: Some(&self.context.authorization_session),
        }
    }
}

impl<C: ServiceController + Send> SetupSteps for NativeSteps<C> {
    fn authorization_session(&self) -> Option<Arc<super::authorization::AuthorizationSession>> {
        Some(self.context.authorization_session.clone())
    }

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
            (SetupStep::Pairing, StepAnswer::CorrectTv { request, revision }) => {
                super::configuration::correct_tv(
                    &self.context.config,
                    request,
                    revision,
                    cancellation,
                )
            }
            (SetupStep::Services, StepAnswer::CorrectUpdatePreference { enabled, revision }) => {
                super::configuration::correct_updates(
                    &self.context.config,
                    enabled,
                    revision,
                    cancellation,
                )
            }
            (SetupStep::Pairing, StepAnswer::Pairing(request)) => {
                super::pairing::execute(&self.context.config, Some(request), cancellation, progress)
            }
            (SetupStep::Services, StepAnswer::Continue) => {
                self.controller.with_setup_authorization(
                    lease.file(),
                    self.context.authorization_session.clone(),
                    |controller| {
                        super::provision::ServiceInstallation {
                            config: &self.context.config,
                            user_units: &self.context.user_units,
                            system_root: &self.context.system_root,
                            controller,
                            authorization: self.context.authorization,
                        }
                        .execute(cancellation, progress)
                    },
                )
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
                recovery: super::recovery::SetupRecovery::new(
                    super::recovery::RecoveryCause::InputRequired,
                    super::recovery::RepairBoundary::UserInput,
                    super::recovery::RecoveryAction::ProvideInput,
                ),
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
        recovery: super::recovery::SetupRecovery::new(
            super::recovery::RecoveryCause::InvalidEnvironment,
            super::recovery::RepairBoundary::SystemConfiguration,
            super::recovery::RecoveryAction::RepairExternally,
        ),
        retryable: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kwin_helper_follows_the_installed_executable_prefix() {
        let prefix = env::temp_dir().join(format!("lg-buddy-kwin-prefix-{}", std::process::id()));
        let helper = prefix.join("lib/lg-buddy/kwin/setup.sh");
        assert_eq!(
            installed_kwin_helper(&prefix.join("bin/lg-buddy")),
            PathBuf::from("/usr/lib/lg-buddy/kwin/setup.sh")
        );
        std::fs::create_dir_all(helper.parent().unwrap()).unwrap();
        std::fs::write(&helper, "exit 2\n").unwrap();
        for executable in ["lg-buddy", "lg-buddy-gui", ".lg-buddy-gui-wrapped"] {
            assert_eq!(
                installed_kwin_helper(&prefix.join("bin").join(executable)),
                helper
            );
        }
        std::fs::remove_dir_all(prefix).unwrap();
    }

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
