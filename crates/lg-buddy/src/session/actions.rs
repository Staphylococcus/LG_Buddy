//! Runtime ownership and environment setup for screen and lifecycle actions.
//! Policy handlers borrow the owner's client; one-shot commands use a fresh owner.

use std::io::Write;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{load_config, resolve_config_path_from_env, Config, MacAddress, TvPlatform};
use crate::events::RuntimeEvent;
use crate::lifecycle::{self, NmOnlineNetworkWaiter};
use crate::runtime_phase::LogindRuntimePhaseProvider;
use crate::screen::{self, ScreenOnDeps, SystemMarkerLifecycleStatusProvider};
use crate::state::{
    ScreenOwnershipMarker, StateScope, SystemSleepAttemptState, SystemSleepCycleState,
};
use crate::tv::{build_tv_client, SelectedTvClient, TvClientBuildOptions};
use crate::wol::UdpWakeOnLanSender;
use crate::RunError;

#[cfg(test)]
mod tests;

pub(crate) const SYSTEM_PRE_SLEEP_TV_COMMAND_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(PartialEq, Eq)]
struct TvClientBinding {
    config_path: PathBuf,
    tv_ip: Ipv4Addr,
    tv_mac: MacAddress,
    platform: TvPlatform,
    options: TvClientBuildOptions,
}

/// A monitor retains this owner across events. The adapter continues to own
/// authentication, connection invalidation, and the no-ambiguous-write-replay rule.
#[derive(Default)]
pub struct RuntimeActionExecutor {
    tv_client: Option<(TvClientBinding, SelectedTvClient)>,
}

impl RuntimeActionExecutor {
    fn load_config(&mut self) -> Result<(PathBuf, Config), RunError> {
        let result = (|| {
            let path = resolve_config_path_from_env().map_err(RunError::ConfigPath)?;
            let config = load_config(&path).map_err(RunError::Config)?;
            Ok((path, config))
        })();
        // An unpaired or unreadable profile must not leave an old TV connection
        // available for reuse if configuration later becomes valid again.
        if result.is_err() {
            self.tv_client = None;
        }
        result
    }

    fn tv_client(
        &mut self,
        config_path: &Path,
        config: &Config,
        options: TvClientBuildOptions,
    ) -> Result<&SelectedTvClient, RunError> {
        let binding = TvClientBinding {
            config_path: config_path.to_owned(),
            tv_ip: config.tv_ip,
            tv_mac: config.tv_mac,
            platform: config.tv_platform,
            options,
        };
        // Legacy commands retain their per-action environment/auth resolution.
        // Native clients can be reused only for the same target and policy:
        // a foreground timeout or pairing prompt must never leak into suspend.
        if config.tv_platform == TvPlatform::Bscpylgtv
            || self
                .tv_client
                .as_ref()
                .is_none_or(|(current, _)| *current != binding)
        {
            self.tv_client = None;
            let client = build_tv_client(config_path, config.tv_ip, config.tv_platform, options)?;
            self.tv_client = Some((binding, client));
        }
        Ok(&self.tv_client.as_ref().expect("TV client initialized").1)
    }

    pub(crate) fn run_screen_off<W: Write>(
        &mut self,
        writer: &mut W,
        event: RuntimeEvent,
    ) -> Result<bool, RunError> {
        let (config_path, config) = self.load_config()?;
        let marker =
            ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
        let tv_client =
            self.tv_client(&config_path, &config, TvClientBuildOptions::production())?;
        let mut phase_provider = LogindRuntimePhaseProvider::from_system_bus();
        let lifecycle_status =
            SystemMarkerLifecycleStatusProvider::from_env().map_err(RunError::StateDir)?;

        screen::run_screen_off_with_result_for_event(
            writer,
            &config,
            &marker,
            tv_client,
            event,
            &mut phase_provider,
            &lifecycle_status,
        )
        .map(|result| result.blank_succeeded)
    }

    pub(crate) fn run_timed_power_off<W: Write>(
        &mut self,
        writer: &mut W,
        event: RuntimeEvent,
    ) -> Result<(), RunError> {
        let (config_path, config) = self.load_config()?;
        let marker =
            ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
        if !config.screen_idle_blank.is_enabled() {
            writeln!(
                writer,
                "LG Buddy Timed Power Off: Screen idle blanking is disabled; skipping power-off."
            )?;
            return Ok(());
        }
        if !marker.exists() {
            writeln!(
                writer,
                "LG Buddy Timed Power Off: Screen ownership marker is absent; skipping power-off."
            )?;
            return Ok(());
        }
        let tv_client =
            self.tv_client(&config_path, &config, TvClientBuildOptions::production())?;
        let mut phase_provider = LogindRuntimePhaseProvider::from_system_bus();
        let lifecycle_status =
            SystemMarkerLifecycleStatusProvider::from_env().map_err(RunError::StateDir)?;

        screen::run_timed_power_off_with_event(
            writer,
            &config,
            &marker,
            tv_client,
            event,
            &mut phase_provider,
            &lifecycle_status,
        )
        .map(|_| ())
    }

    pub(crate) fn run_screen_on<W: Write>(
        &mut self,
        writer: &mut W,
        event: RuntimeEvent,
    ) -> Result<(), RunError> {
        let (config_path, config) = self.load_config()?;
        let marker =
            ScreenOwnershipMarker::from_env(StateScope::Session).map_err(RunError::StateDir)?;
        let tv_client =
            self.tv_client(&config_path, &config, TvClientBuildOptions::production())?;
        let wol_sender = UdpWakeOnLanSender::default();
        let sleeper = screen::ThreadSleeper;
        let mut phase_provider = LogindRuntimePhaseProvider::from_system_bus();
        let lifecycle_status =
            SystemMarkerLifecycleStatusProvider::from_env().map_err(RunError::StateDir)?;

        screen::run_screen_on_with_event(
            writer,
            &config,
            &marker,
            ScreenOnDeps {
                tv_client,
                wol_sender: &wol_sender,
                sleeper: &sleeper,
                phase_provider: &mut phase_provider,
                lifecycle_status: &lifecycle_status,
            },
            event,
        )
    }

    pub(crate) fn run_sleep_pre<W: Write>(
        &mut self,
        writer: &mut W,
        event: RuntimeEvent,
    ) -> Result<(), RunError> {
        let (config_path, config) = self.load_config()?;
        let marker =
            ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
        let cycle_state =
            SystemSleepCycleState::from_env(StateScope::System).map_err(RunError::StateDir)?;
        let tv_client = self.tv_client(
            &config_path,
            &config,
            TvClientBuildOptions::production()
                .stored_token_only()
                .with_command_timeout(SYSTEM_PRE_SLEEP_TV_COMMAND_TIMEOUT),
        )?;
        let sleeper = lifecycle::ThreadSleeper;

        lifecycle::handle_system_suspend_with(
            writer,
            &config,
            &marker,
            &cycle_state,
            tv_client,
            &sleeper,
            event,
        )
    }

    pub(crate) fn run_system_resume<W: Write>(&mut self, writer: &mut W) -> Result<(), RunError> {
        let (config_path, config) = self.load_config()?;
        let marker =
            ScreenOwnershipMarker::from_env(StateScope::System).map_err(RunError::StateDir)?;
        let attempt_state =
            SystemSleepAttemptState::from_env(StateScope::System).map_err(RunError::StateDir)?;
        let tv_client = self.tv_client(
            &config_path,
            &config,
            TvClientBuildOptions::production().stored_token_only(),
        )?;
        let wol_sender = UdpWakeOnLanSender::default();
        let sleeper = lifecycle::ThreadSleeper;
        let network_waiter = NmOnlineNetworkWaiter::default();

        let result = (|| -> Result<(), RunError> {
            match tv_client.can_authenticate_unattended() {
                Ok(true) => lifecycle::restore_after_system_sleep_with(
                    writer,
                    &config,
                    &marker,
                    tv_client,
                    &wol_sender,
                    &sleeper,
                    &network_waiter,
                ),
                Ok(false) => {
                    marker.clear()?;
                    writeln!(
                        writer,
                        "LG Buddy System Resume: No stored native TV credential; skipping unattended TV control."
                    )?;
                    Ok(())
                }
                Err(err) => {
                    marker.clear()?;
                    Err(err.into())
                }
            }
        })();

        let mut cleanup_error = None;

        if let Err(err) = attempt_state.clear() {
            writeln!(
                writer,
                "LG Buddy System Resume: could not clear system sleep attempt marker after resume. {err}"
            )?;
            cleanup_error = Some(err);
        }

        if let Err(err) = attempt_state.clear_outcome() {
            writeln!(
                writer,
                "LG Buddy System Resume: could not clear system sleep cycle state after resume. {err}"
            )?;
            if cleanup_error.is_none() {
                cleanup_error = Some(err);
            }
        }

        if result.is_ok() {
            if let Some(err) = cleanup_error {
                return Err(RunError::Io(err));
            }
        }

        result
    }
}
