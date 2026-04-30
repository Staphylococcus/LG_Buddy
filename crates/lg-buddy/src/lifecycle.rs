use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::Duration;

use crate::config::{Config, ScreenRestorePolicy};
use crate::events::{RuntimeEvent, RuntimeEventKind};
use crate::policy::{
    ActionKind, DecisionReason, DecisionReasonCode, Diagnostic, PolicyOutcome, StateMarker,
    StateTransition, TransitionReason, TransitionReasonCode,
};
use crate::state::{ScreenOwnershipMarker, SystemSleepAttemptState};
use crate::tv::{CurrentInput, TvClient, TvDevice};
use crate::wol::WakeOnLanSender;
use crate::{RunError, StartupMode};

const STARTUP_INITIAL_WAKE_DELAY: Duration = Duration::from_secs(6);
pub(crate) const STARTUP_WAKE_ATTEMPTS: u32 = 6;
const SYSTEM_PRE_SLEEP_GET_INPUT_RETRIES: u32 = 0;
const SYSTEM_PRE_SLEEP_POWER_OFF_ATTEMPTS: u32 = 1;
const SYSTEM_SLEEP_GET_INPUT_RETRIES: u32 = 3;
const SYSTEM_SLEEP_POWER_OFF_RETRIES: u32 = 4;

pub(crate) trait Sleeper {
    fn sleep(&self, duration: Duration);
}

pub(crate) struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

pub(crate) trait SleepRequestDetector {
    fn is_sleep_requested(&self) -> io::Result<bool>;
}

pub(crate) trait NetworkWaiter {
    fn wait_for_network(&self) -> io::Result<()>;
}

pub(crate) trait RebootDetector {
    fn is_reboot_pending(&self) -> io::Result<bool>;
}

pub(crate) struct JournalctlSleepDetector {
    command_path: PathBuf,
}

pub(crate) struct NmOnlineNetworkWaiter {
    command_path: PathBuf,
}

impl Default for JournalctlSleepDetector {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Default for NmOnlineNetworkWaiter {
    fn default() -> Self {
        Self::from_env()
    }
}

impl JournalctlSleepDetector {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_JOURNALCTL")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("journalctl")),
        }
    }
}

impl NmOnlineNetworkWaiter {
    fn from_env() -> Self {
        Self {
            command_path: env::var_os("LG_BUDDY_NM_ONLINE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("nm-online")),
        }
    }
}

impl SleepRequestDetector for JournalctlSleepDetector {
    fn is_sleep_requested(&self) -> io::Result<bool> {
        let output = ProcessCommand::new(&self.command_path)
            .args(["-u", "NetworkManager", "-n", "30", "--no-pager"])
            .output()?;

        if !output.status.success() {
            return Ok(false);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains("manager: sleep: sleep requested"))
    }
}

impl NetworkWaiter for NmOnlineNetworkWaiter {
    fn wait_for_network(&self) -> io::Result<()> {
        let _ = ProcessCommand::new(&self.command_path)
            .args(["-q", "-t", "60"])
            .output()?;

        Ok(())
    }
}

pub(crate) struct StartupDeps<'a, C, S, Sl, N> {
    pub(crate) tv_client: &'a C,
    pub(crate) wol_sender: &'a S,
    pub(crate) sleeper: &'a Sl,
    pub(crate) network_waiter: &'a N,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LifecyclePolicyDecision<N> {
    next: N,
    outcome: PolicyOutcome,
}

impl<N> LifecyclePolicyDecision<N> {
    fn new(next: N) -> Self {
        Self {
            next,
            outcome: PolicyOutcome::new(),
        }
    }

    fn with_outcome(next: N, outcome: PolicyOutcome) -> Self {
        Self { next, outcome }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupRoute {
    ColdBoot,
    SystemResume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreNext {
    Restore,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownNext {
    QueryInput,
    PowerOff,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RebootObservation {
    Pending,
    NotPending,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TvInputObservation {
    Current(CurrentInput),
    QueryFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TvEffectObservation {
    Succeeded,
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemSleepNext {
    PowerOff,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemSleepPowerOffContext {
    KnownInput,
    FallbackAfterInputQueryFailure { marker_was_set: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SystemSleepAttemptNext {
    Continue,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NetworkTeardownNext {
    AttemptPreSleep,
    ClearAttempt,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NetworkTeardownPolicyInput {
    policy_enabled: bool,
    machine_sleep_pending: Option<bool>,
    phase_read_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    Startup { mode: StartupMode },
    ShutdownRequested,
    NetworkTeardownImminent { machine_sleep_pending: Option<bool> },
    MachinePreparingForSleep,
    MachineResumed,
}

impl LifecycleEvent {
    pub fn from_runtime_event(event: RuntimeEvent) -> Option<Self> {
        match event.kind {
            RuntimeEventKind::MachineStartup { mode } => Some(Self::Startup { mode }),
            RuntimeEventKind::MachineShutdownRequested => Some(Self::ShutdownRequested),
            RuntimeEventKind::NetworkTeardownImminent {
                machine_sleep_pending,
            } => Some(Self::NetworkTeardownImminent {
                machine_sleep_pending,
            }),
            RuntimeEventKind::MachinePreparingForSleep => Some(Self::MachinePreparingForSleep),
            RuntimeEventKind::MachineResumed => Some(Self::MachineResumed),
            RuntimeEventKind::SessionIdle
            | RuntimeEventKind::SessionActive
            | RuntimeEventKind::SessionLocked
            | RuntimeEventKind::SessionUnlocked
            | RuntimeEventKind::ScreenWakeRequested
            | RuntimeEventKind::UserActivityObserved
            | RuntimeEventKind::ScreenBlankRequested
            | RuntimeEventKind::ScreenRestoreRequested
            | RuntimeEventKind::BrightnessRequested => None,
        }
    }
}

fn decide_startup_route(mode: StartupMode, marker_exists: bool) -> StartupRoute {
    if matches!(mode, StartupMode::Wake) || matches!(mode, StartupMode::Auto) && marker_exists {
        StartupRoute::SystemResume
    } else {
        StartupRoute::ColdBoot
    }
}

fn decide_startup_cold_boot() -> PolicyOutcome {
    PolicyOutcome::new()
        .with_state_transition(clear_system_marker(TransitionReasonCode::StartupBoot))
        .with_action(
            ActionKind::TvStartupRestore,
            DecisionReason::new(DecisionReasonCode::ManualRequest),
        )
        .with_action(
            ActionKind::WakeOnLan,
            DecisionReason::new(DecisionReasonCode::ManualRequest),
        )
}

fn select_lifecycle_wake_packet(reason: DecisionReasonCode) -> PolicyOutcome {
    PolicyOutcome::new().with_action(ActionKind::WakeOnLan, DecisionReason::new(reason))
}

fn select_lifecycle_input_restore(reason: DecisionReasonCode) -> PolicyOutcome {
    PolicyOutcome::new().with_action(ActionKind::TvInputRestore, DecisionReason::new(reason))
}

fn decide_restore_after_system_sleep_start(
    policy: ScreenRestorePolicy,
    marker_exists: bool,
) -> LifecyclePolicyDecision<RestoreNext> {
    if !restore_is_allowed(policy, marker_exists) {
        return LifecyclePolicyDecision::with_outcome(
            RestoreNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(DecisionReasonCode::MarkerMissing)),
        );
    }

    let mut outcome = PolicyOutcome::new()
        .with_state_transition(clear_system_marker(TransitionReasonCode::RestoreCompleted))
        .with_action(
            ActionKind::TvSystemResumeRestore,
            DecisionReason::new(DecisionReasonCode::RuntimeEvent),
        )
        .with_action(
            ActionKind::WakeOnLan,
            DecisionReason::new(DecisionReasonCode::RuntimeEvent),
        );

    if !marker_exists {
        outcome = outcome.with_diagnostic(Diagnostic::info(
            "aggressive markerless system resume restore",
        ));
    }

    LifecyclePolicyDecision::with_outcome(RestoreNext::Restore, outcome)
}

fn decide_restore_input_succeeded() -> PolicyOutcome {
    PolicyOutcome::new()
}

fn decide_restore_input_exhausted(message: &'static str) -> PolicyOutcome {
    PolicyOutcome::new().with_diagnostic(Diagnostic::warning(message))
}

fn decide_shutdown_after_reboot(
    observation: RebootObservation,
) -> LifecyclePolicyDecision<ShutdownNext> {
    match observation {
        RebootObservation::Pending => LifecyclePolicyDecision::with_outcome(
            ShutdownNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(DecisionReasonCode::NotApplicable)),
        ),
        RebootObservation::NotPending => LifecyclePolicyDecision::new(ShutdownNext::QueryInput),
        RebootObservation::Unknown(detail) => LifecyclePolicyDecision::with_outcome(
            ShutdownNext::QueryInput,
            PolicyOutcome::new().with_diagnostic(Diagnostic::warning(format!(
                "could not determine reboot state: {detail}"
            ))),
        ),
    }
}

fn decide_shutdown_after_input(
    configured_input: crate::config::HdmiInput,
    observation: TvInputObservation,
) -> LifecyclePolicyDecision<ShutdownNext> {
    match observation {
        TvInputObservation::Current(current_input) if current_input.is_hdmi(configured_input) => {
            LifecyclePolicyDecision::with_outcome(
                ShutdownNext::PowerOff,
                PolicyOutcome::new().with_action(
                    ActionKind::TvShutdownPowerOff,
                    DecisionReason::new(DecisionReasonCode::RuntimeEvent),
                ),
            )
        }
        TvInputObservation::Current(current_input) => LifecyclePolicyDecision::with_outcome(
            ShutdownNext::Stop,
            PolicyOutcome::new().with_no_action(DecisionReason::with_detail(
                DecisionReasonCode::InputMismatch,
                format!(
                    "TV is on {current_input}, not {}",
                    configured_input.as_str()
                ),
            )),
        ),
        TvInputObservation::QueryFailed(_) => LifecyclePolicyDecision::with_outcome(
            ShutdownNext::PowerOff,
            PolicyOutcome::new()
                .with_diagnostic(Diagnostic::warning("shutdown input query failed"))
                .with_action(
                    ActionKind::TvShutdownPowerOff,
                    DecisionReason::new(DecisionReasonCode::TransportFailure),
                ),
        ),
    }
}

fn decide_shutdown_power_off_result(observation: TvEffectObservation) -> PolicyOutcome {
    match observation {
        TvEffectObservation::Succeeded => PolicyOutcome::new(),
        TvEffectObservation::Failed(detail) => PolicyOutcome::new().with_diagnostic(
            Diagnostic::warning(format!("shutdown power_off failed: {detail}")),
        ),
    }
}

fn decide_system_sleep_after_input(
    configured_input: crate::config::HdmiInput,
    observation: TvInputObservation,
) -> LifecyclePolicyDecision<SystemSleepNext> {
    match observation {
        TvInputObservation::Current(current_input) if current_input.is_hdmi(configured_input) => {
            LifecyclePolicyDecision::with_outcome(
                SystemSleepNext::PowerOff,
                PolicyOutcome::new().with_action(
                    ActionKind::TvSystemSleepPowerOff,
                    DecisionReason::new(DecisionReasonCode::RuntimeEvent),
                ),
            )
        }
        TvInputObservation::Current(current_input) => LifecyclePolicyDecision::with_outcome(
            SystemSleepNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::with_detail(
                    DecisionReasonCode::InputMismatch,
                    format!(
                        "TV is on {current_input}, not {}",
                        configured_input.as_str()
                    ),
                ))
                .with_state_transition(clear_system_marker(TransitionReasonCode::InputMismatch)),
        ),
        TvInputObservation::QueryFailed(_) => LifecyclePolicyDecision::with_outcome(
            SystemSleepNext::PowerOff,
            PolicyOutcome::new()
                .with_diagnostic(Diagnostic::warning(
                    "input query failed before system sleep",
                ))
                .with_action(
                    ActionKind::TvSystemSleepPowerOff,
                    DecisionReason::new(DecisionReasonCode::TransportFailure),
                ),
        ),
    }
}

fn decide_system_sleep_power_off_result(
    observation: TvEffectObservation,
    context: SystemSleepPowerOffContext,
) -> PolicyOutcome {
    match (observation, context) {
        (TvEffectObservation::Succeeded, _) => PolicyOutcome::new()
            .with_state_transition(create_system_marker(TransitionReasonCode::ActionSelected)),
        (TvEffectObservation::Failed(_), SystemSleepPowerOffContext::KnownInput) => {
            PolicyOutcome::new().with_diagnostic(Diagnostic::warning(
                "power_off failed on known input; state not set",
            ))
        }
        (
            TvEffectObservation::Failed(_),
            SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
                marker_was_set: true,
            },
        ) => PolicyOutcome::new()
            .with_state_transition(StateTransition::preserve_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::Other),
            ))
            .with_diagnostic(Diagnostic::warning(
                "fallback power_off failed; preserving existing system marker",
            )),
        (
            TvEffectObservation::Failed(_),
            SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
                marker_was_set: false,
            },
        ) => PolicyOutcome::new()
            .with_state_transition(clear_system_marker(TransitionReasonCode::TransportFailure))
            .with_diagnostic(Diagnostic::warning(
                "fallback power_off failed after retries; state left unset",
            )),
    }
}

fn decide_system_sleep_attempt_start(
    lock_acquired: bool,
    already_attempted: bool,
) -> LifecyclePolicyDecision<SystemSleepAttemptNext> {
    if !lock_acquired || already_attempted {
        return LifecyclePolicyDecision::with_outcome(
            SystemSleepAttemptNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(
                    DecisionReasonCode::DuplicateSystemSleepAttempt,
                ))
                .with_state_transition(StateTransition::preserve_marker(
                    StateMarker::SystemSleepAttempt,
                    TransitionReason::new(TransitionReasonCode::DuplicateSystemSleepAttempt),
                )),
        );
    }

    LifecyclePolicyDecision::with_outcome(
        SystemSleepAttemptNext::Continue,
        PolicyOutcome::new().with_state_transition(StateTransition::create_marker(
            StateMarker::SystemSleepAttempt,
            TransitionReason::new(TransitionReasonCode::ActionSelected),
        )),
    )
}

fn decide_network_teardown(
    input: NetworkTeardownPolicyInput,
) -> LifecyclePolicyDecision<NetworkTeardownNext> {
    if !input.policy_enabled {
        return LifecyclePolicyDecision::with_outcome(
            NetworkTeardownNext::Stop,
            PolicyOutcome::new()
                .with_no_action(DecisionReason::new(DecisionReasonCode::ConfigDisabled)),
        );
    }

    match input.machine_sleep_pending {
        Some(true) => LifecyclePolicyDecision::new(NetworkTeardownNext::AttemptPreSleep),
        Some(false) => LifecyclePolicyDecision::with_outcome(
            NetworkTeardownNext::ClearAttempt,
            PolicyOutcome::new()
                .with_state_transition(clear_system_sleep_attempt_marker(
                    TransitionReasonCode::RuntimePhaseIneligible,
                ))
                .with_no_action(DecisionReason::new(
                    DecisionReasonCode::RuntimePhaseIneligible,
                )),
        ),
        None => {
            let detail = input
                .phase_read_error
                .unwrap_or_else(|| "sleep phase is unknown".to_string());
            LifecyclePolicyDecision::with_outcome(
                NetworkTeardownNext::Stop,
                PolicyOutcome::new()
                    .with_no_action(DecisionReason::with_detail(
                        DecisionReasonCode::RuntimePhaseUnknown,
                        detail.clone(),
                    ))
                    .with_diagnostic(Diagnostic::warning(format!(
                        "could not read logind PreparingForSleep; failing open: {detail}"
                    ))),
            )
        }
    }
}

pub(crate) fn run_startup_with<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    N: NetworkWaiter,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    deps: StartupDeps<'_, C, S, Sl, N>,
    mode: StartupMode,
) -> Result<(), RunError> {
    run_startup_with_outcome(writer, config, marker, deps, mode).map(|_| ())
}

pub(crate) fn run_startup_with_outcome<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    N: NetworkWaiter,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    deps: StartupDeps<'_, C, S, Sl, N>,
    mode: StartupMode,
) -> Result<PolicyOutcome, RunError> {
    let marker_exists = marker.exists();

    match decide_startup_route(mode, marker_exists) {
        StartupRoute::SystemResume => {
            return restore_after_system_sleep_with_outcome(
                writer,
                config,
                marker,
                deps.tv_client,
                deps.wol_sender,
                deps.sleeper,
                deps.network_waiter,
            );
        }
        StartupRoute::ColdBoot => {}
    }

    let mut outcome = PolicyOutcome::new();
    let tv = TvDevice::new(deps.tv_client, config.tv_ip);
    if let Err(err) = deps.network_waiter.wait_for_network() {
        outcome
            .diagnostics
            .push(Diagnostic::warning(format!("network wait failed: {err}")));
    }

    match mode {
        StartupMode::Boot => {
            writeln!(
                writer,
                "LG Buddy Startup: Cold boot: Turning TV on and switching to {}.",
                config.input.as_str()
            )?;
        }
        StartupMode::Auto => {
            writeln!(
                writer,
                "LG Buddy Startup: Cold boot: Turning TV on and switching to {}.",
                config.input.as_str()
            )?;
        }
        StartupMode::Wake => unreachable!("wake mode should be handled by lifecycle restore"),
    }

    let start_outcome = decide_startup_cold_boot();
    apply_system_screen_marker_transitions(marker, &start_outcome)?;
    outcome.merge(start_outcome);
    send_wake_packet(
        writer,
        "LG Buddy Startup",
        &tv,
        deps.wol_sender,
        &config.tv_mac,
    )?;
    deps.sleeper.sleep(startup_initial_wake_delay());

    for attempt in 1..=STARTUP_WAKE_ATTEMPTS {
        outcome.merge(select_lifecycle_input_restore(
            DecisionReasonCode::ManualRequest,
        ));
        if tv.input().set(config.input).is_ok() {
            outcome.merge(decide_restore_input_succeeded());
            writeln!(
                writer,
                "LG Buddy Startup: TV turned on and set to {}.",
                config.input.as_str()
            )?;
            return Ok(outcome);
        }

        let retry_delay = startup_retry_delay(attempt);
        writeln!(
            writer,
            "LG Buddy Startup: Attempt {attempt} failed, retrying in {}s...",
            retry_delay.as_secs()
        )?;
        outcome.merge(select_lifecycle_wake_packet(
            DecisionReasonCode::ManualRequest,
        ));
        send_wake_packet(
            writer,
            "LG Buddy Startup",
            &tv,
            deps.wol_sender,
            &config.tv_mac,
        )?;
        deps.sleeper.sleep(retry_delay);
    }

    writeln!(
        writer,
        "LG Buddy Startup: set_input failed after {STARTUP_WAKE_ATTEMPTS} attempts"
    )?;
    outcome.merge(decide_restore_input_exhausted(
        "startup input restore exhausted retries",
    ));
    Ok(outcome)
}

pub(crate) fn run_shutdown_with<W: Write, C: TvClient, R: RebootDetector>(
    writer: &mut W,
    config: &Config,
    tv_client: &C,
    reboot_detector: &R,
) -> Result<(), RunError> {
    run_shutdown_with_outcome(writer, config, tv_client, reboot_detector).map(|_| ())
}

pub(crate) fn run_shutdown_with_outcome<W: Write, C: TvClient, R: RebootDetector>(
    writer: &mut W,
    config: &Config,
    tv_client: &C,
    reboot_detector: &R,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    let reboot_observation = match reboot_detector.is_reboot_pending() {
        Ok(true) => RebootObservation::Pending,
        Ok(false) => RebootObservation::NotPending,
        Err(err) => RebootObservation::Unknown(err.to_string()),
    };
    let reboot_decision = decide_shutdown_after_reboot(reboot_observation.clone());
    match reboot_observation {
        RebootObservation::Pending => {
            writeln!(writer, "LG Buddy Shutdown: Reboot; ignoring")?;
            outcome.merge(reboot_decision.outcome);
            return Ok(outcome);
        }
        RebootObservation::NotPending => {}
        RebootObservation::Unknown(err) => {
            writeln!(
                writer,
                "LG Buddy Shutdown: Could not determine reboot state. Continuing shutdown. {err}"
            )?;
        }
    }
    outcome.merge(reboot_decision.outcome);

    let tv = TvDevice::new(tv_client, config.tv_ip);

    match tv.input().current() {
        Ok(current_input) => {
            let decision = decide_shutdown_after_input(
                config.input,
                TvInputObservation::Current(current_input.clone()),
            );
            match decision.next {
                ShutdownNext::PowerOff => {
                    writeln!(
                        writer,
                        "LG Buddy Shutdown: TV is on {}. Turning off for shutdown.",
                        config.input.as_str()
                    )?;
                    outcome.merge(decision.outcome);
                    let power_off = match tv.power().off() {
                        Ok(_) => TvEffectObservation::Succeeded,
                        Err(err) => TvEffectObservation::Failed(err.to_string()),
                    };
                    log_shutdown_power_off_result(writer, power_off.clone())?;
                    outcome.merge(decide_shutdown_power_off_result(power_off));
                }
                ShutdownNext::Stop => {
                    writeln!(
                        writer,
                        "LG Buddy Shutdown: TV is on {current_input} (not {}). Skipping.",
                        config.input.as_str()
                    )?;
                    outcome.merge(decision.outcome);
                }
                ShutdownNext::QueryInput => unreachable!("input decision cannot request input"),
            }
        }
        Err(err) => {
            let decision = decide_shutdown_after_input(
                config.input,
                TvInputObservation::QueryFailed(err.to_string()),
            );
            writeln!(
                writer,
                "LG Buddy Shutdown: Could not query TV input. Proceeding with power_off."
            )?;
            outcome.merge(decision.outcome);
            let power_off = match tv.power().off() {
                Ok(_) => TvEffectObservation::Succeeded,
                Err(err) => TvEffectObservation::Failed(err.to_string()),
            };
            log_shutdown_power_off_result(writer, power_off.clone())?;
            outcome.merge(decide_shutdown_power_off_result(power_off));
        }
    }

    Ok(outcome)
}

pub(crate) fn attempt_system_sleep_power_off_with<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    sleeper: &Sl,
) -> Result<(), RunError> {
    attempt_system_sleep_power_off_with_outcome(writer, config, marker, tv_client, sleeper)
        .map(|_| ())
}

pub(crate) fn attempt_system_sleep_power_off_with_outcome<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    sleeper: &Sl,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    let tv = TvDevice::new(tv_client, config.tv_ip);
    let state_was_set = marker.exists();

    match query_current_input_with_retries(&tv, sleeper, SYSTEM_PRE_SLEEP_GET_INPUT_RETRIES) {
        Ok(current_input) => {
            let decision = decide_system_sleep_after_input(
                config.input,
                TvInputObservation::Current(current_input.clone()),
            );
            apply_system_screen_marker_transitions(marker, &decision.outcome)?;
            match decision.next {
                SystemSleepNext::PowerOff => {
                    writeln!(
                        writer,
                        "LG Buddy Sleep Pre: TV is on {}. Turning off for sleep.",
                        config.input.as_str()
                    )?;
                    outcome.merge(decision.outcome);
                    let power_off = match tv.power().off() {
                        Ok(_) => TvEffectObservation::Succeeded,
                        Err(err) => TvEffectObservation::Failed(err.to_string()),
                    };
                    let result_outcome = decide_system_sleep_power_off_result(
                        power_off.clone(),
                        SystemSleepPowerOffContext::KnownInput,
                    );
                    apply_system_screen_marker_transitions(marker, &result_outcome)?;
                    render_system_sleep_power_off_result(
                        writer,
                        power_off,
                        SystemSleepPowerOffContext::KnownInput,
                    )?;
                    outcome.merge(result_outcome);
                }
                SystemSleepNext::Stop => {
                    writeln!(
                        writer,
                        "LG Buddy Sleep Pre: TV is on {current_input} (not {}). Skipping.",
                        config.input.as_str()
                    )?;
                    outcome.merge(decision.outcome);
                }
            }
        }
        Err(err) => {
            let decision = decide_system_sleep_after_input(
                config.input,
                TvInputObservation::QueryFailed(err.to_string()),
            );
            writeln!(
                writer,
                "LG Buddy Sleep Pre: Could not query TV input. Attempting power_off fallback."
            )?;
            outcome.merge(decision.outcome);

            let power_off = if retry_power_off(&tv, sleeper, SYSTEM_PRE_SLEEP_POWER_OFF_ATTEMPTS) {
                TvEffectObservation::Succeeded
            } else {
                TvEffectObservation::Failed("power_off failed".to_string())
            };
            let context = SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
                marker_was_set: state_was_set,
            };
            let result_outcome = decide_system_sleep_power_off_result(power_off.clone(), context);
            apply_system_screen_marker_transitions(marker, &result_outcome)?;
            render_system_sleep_power_off_result(writer, power_off, context)?;
            outcome.merge(result_outcome);
        }
    }

    Ok(outcome)
}

pub(crate) fn attempt_system_sleep_power_off_once_with_outcome<
    W: Write,
    C: TvClient,
    Sl: Sleeper,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    attempt_state: &SystemSleepAttemptState,
    tv_client: &C,
    sleeper: &Sl,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    let guard = attempt_state.try_lock()?;
    let lock_acquired = guard.is_some();
    let decision = decide_system_sleep_attempt_start(lock_acquired, false);
    if decision.next == SystemSleepAttemptNext::Stop {
        writeln!(
            writer,
            "LG Buddy Sleep Pre: another system sleep attempt is already running; skipping duplicate pre-sleep handling."
        )?;
        outcome.merge(decision.outcome);
        return Ok(outcome);
    };

    let _guard = guard.expect("lock should be present when acquired");
    let already_attempted = attempt_state.exists();
    let decision = decide_system_sleep_attempt_start(true, already_attempted);
    if decision.next == SystemSleepAttemptNext::Stop {
        writeln!(
            writer,
            "LG Buddy Sleep Pre: system sleep was already handled for this cycle; skipping duplicate pre-sleep handling."
        )?;
        outcome.merge(decision.outcome);
        return Ok(outcome);
    }

    apply_system_sleep_attempt_transitions(attempt_state, &decision.outcome)?;
    outcome.merge(decision.outcome);
    outcome.merge(attempt_system_sleep_power_off_with_outcome(
        writer, config, marker, tv_client, sleeper,
    )?);
    Ok(outcome)
}

pub(crate) fn handle_network_teardown_with<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    attempt_state: &SystemSleepAttemptState,
    tv_client: &C,
    sleeper: &Sl,
    event: RuntimeEvent,
    phase_read_error: Option<&str>,
) -> Result<(), RunError> {
    handle_network_teardown_with_outcome(
        writer,
        config,
        marker,
        attempt_state,
        tv_client,
        sleeper,
        event,
        phase_read_error,
    )
    .map(|_| ())
}

pub(crate) fn handle_network_teardown_with_outcome<W: Write, C: TvClient, Sl: Sleeper>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    attempt_state: &SystemSleepAttemptState,
    tv_client: &C,
    sleeper: &Sl,
    event: RuntimeEvent,
    phase_read_error: Option<&str>,
) -> Result<PolicyOutcome, RunError> {
    let Some(LifecycleEvent::NetworkTeardownImminent {
        machine_sleep_pending,
    }) = LifecycleEvent::from_runtime_event(event)
    else {
        return Err(RunError::Policy(
            "network teardown handler received a non-network lifecycle event".to_string(),
        ));
    };

    let mut outcome = PolicyOutcome::new();
    let decision = decide_network_teardown(NetworkTeardownPolicyInput {
        policy_enabled: config.system_sleep_wake_policy.is_enabled(),
        machine_sleep_pending,
        phase_read_error: phase_read_error.map(str::to_string),
    });

    match decision.next {
        NetworkTeardownNext::AttemptPreSleep => {
            writeln!(
                writer,
                "LG Buddy NetworkManager: logind is preparing for sleep; running pre-sleep TV handling before network teardown."
            )?;
            outcome.merge(decision.outcome);
            outcome.merge(attempt_system_sleep_power_off_once_with_outcome(
                writer,
                config,
                marker,
                attempt_state,
                tv_client,
                sleeper,
            )?);
        }
        NetworkTeardownNext::ClearAttempt => {
            outcome.merge(decision.outcome);
            if let Err(err) = attempt_state.clear() {
                outcome.diagnostics.push(Diagnostic::warning(format!(
                    "could not clear stale system sleep attempt marker while not sleeping: {err}"
                )));
                writeln!(
                    writer,
                    "LG Buddy NetworkManager: could not clear stale system sleep attempt marker while not sleeping. {err}"
                )?;
            }
            writeln!(
                writer,
                "LG Buddy NetworkManager: logind is not preparing for sleep; leaving network disconnect alone."
            )?;
        }
        NetworkTeardownNext::Stop => {
            render_network_teardown_stop(writer, &decision.outcome)?;
            outcome.merge(decision.outcome);
        }
    }

    Ok(outcome)
}

pub(crate) fn attempt_legacy_network_manager_sleep_with<
    W: Write,
    C: TvClient,
    D: SleepRequestDetector,
    Sl: Sleeper,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    detector: &D,
    sleeper: &Sl,
) -> Result<(), RunError> {
    match detector.is_sleep_requested() {
        Ok(false) => return Ok(()),
        Ok(true) => {}
        Err(err) => {
            writeln!(
                writer,
                "LG Buddy Sleep: Could not determine NetworkManager sleep state. Skipping. {err}"
            )?;
            return Ok(());
        }
    }

    let tv = TvDevice::new(tv_client, config.tv_ip);

    match query_current_input_with_retries(&tv, sleeper, SYSTEM_SLEEP_GET_INPUT_RETRIES) {
        Ok(current_input) if !current_input.is_hdmi(config.input) => {
            marker.clear()?;
            return Ok(());
        }
        Ok(_) | Err(_) => {}
    }

    let state_was_set = marker.exists();

    if retry_power_off(&tv, sleeper, SYSTEM_SLEEP_POWER_OFF_RETRIES) {
        marker.create()?;
    } else if !state_was_set {
        marker.clear()?;
    }

    Ok(())
}

pub(crate) fn restore_after_system_sleep_with<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    N: NetworkWaiter,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    wol_sender: &S,
    sleeper: &Sl,
    network_waiter: &N,
) -> Result<(), RunError> {
    restore_after_system_sleep_with_outcome(
        writer,
        config,
        marker,
        tv_client,
        wol_sender,
        sleeper,
        network_waiter,
    )
    .map(|_| ())
}

pub(crate) fn restore_after_system_sleep_with_outcome<
    W: Write,
    C: TvClient,
    S: WakeOnLanSender,
    Sl: Sleeper,
    N: NetworkWaiter,
>(
    writer: &mut W,
    config: &Config,
    marker: &ScreenOwnershipMarker,
    tv_client: &C,
    wol_sender: &S,
    sleeper: &Sl,
    network_waiter: &N,
) -> Result<PolicyOutcome, RunError> {
    let mut outcome = PolicyOutcome::new();
    let tv = TvDevice::new(tv_client, config.tv_ip);
    let marker_exists = marker.exists();
    let _ = network_waiter.wait_for_network();

    let start_decision =
        decide_restore_after_system_sleep_start(config.screen_restore_policy, marker_exists);
    render_restore_after_system_sleep_start(writer, marker_exists, &start_decision)?;
    if start_decision.next == RestoreNext::Stop {
        outcome.merge(start_decision.outcome);
        return Ok(outcome);
    }

    apply_system_screen_marker_transitions(marker, &start_decision.outcome)?;
    outcome.merge(start_decision.outcome);
    send_wake_packet(writer, "LG Buddy Startup", &tv, wol_sender, &config.tv_mac)?;
    sleeper.sleep(startup_initial_wake_delay());

    for attempt in 1..=STARTUP_WAKE_ATTEMPTS {
        outcome.merge(select_lifecycle_input_restore(
            DecisionReasonCode::RuntimeEvent,
        ));
        if tv.input().set(config.input).is_ok() {
            outcome.merge(decide_restore_input_succeeded());
            writeln!(
                writer,
                "LG Buddy Startup: TV turned on and set to {}.",
                config.input.as_str()
            )?;
            return Ok(outcome);
        }

        let retry_delay = startup_retry_delay(attempt);
        writeln!(
            writer,
            "LG Buddy Startup: Attempt {attempt} failed, retrying in {}s...",
            retry_delay.as_secs()
        )?;
        outcome.merge(select_lifecycle_wake_packet(
            DecisionReasonCode::RuntimeEvent,
        ));
        send_wake_packet(writer, "LG Buddy Startup", &tv, wol_sender, &config.tv_mac)?;
        sleeper.sleep(retry_delay);
    }

    writeln!(
        writer,
        "LG Buddy Startup: set_input failed after {STARTUP_WAKE_ATTEMPTS} attempts"
    )?;
    outcome.merge(decide_restore_input_exhausted(
        "system resume input restore exhausted retries",
    ));
    Ok(outcome)
}

pub(crate) fn restore_is_allowed(policy: ScreenRestorePolicy, marker_exists: bool) -> bool {
    marker_exists || policy == ScreenRestorePolicy::Aggressive
}

pub(crate) fn log_markerless_restore_notice<W: Write>(
    writer: &mut W,
    prefix: &str,
) -> io::Result<()> {
    writeln!(
        writer,
        "{prefix}: State file not found. Aggressive restore policy is enabled, so LG Buddy will attempt wake anyway."
    )
}

pub(crate) fn send_wake_packet<W: Write, C: TvClient, S: WakeOnLanSender>(
    writer: &mut W,
    prefix: &str,
    tv: &TvDevice<'_, C>,
    wol_sender: &S,
    tv_mac: &crate::config::MacAddress,
) -> Result<(), RunError> {
    if let Err(err) = tv.power().wake(wol_sender, tv_mac) {
        writeln!(
            writer,
            "{prefix}: Wake-on-LAN send failed. Continuing anyway. {err}"
        )?;
    }

    Ok(())
}

fn log_shutdown_power_off_result<W: Write>(
    writer: &mut W,
    observation: TvEffectObservation,
) -> Result<(), RunError> {
    if let TvEffectObservation::Failed(detail) = observation {
        writeln!(
            writer,
            "LG Buddy Shutdown: power_off failed, continuing shutdown. {detail}"
        )?;
    }

    Ok(())
}

fn render_system_sleep_power_off_result<W: Write>(
    writer: &mut W,
    observation: TvEffectObservation,
    context: SystemSleepPowerOffContext,
) -> Result<(), RunError> {
    let TvEffectObservation::Failed(_) = observation else {
        return Ok(());
    };

    match context {
        SystemSleepPowerOffContext::KnownInput => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: power_off failed on known input. State not set."
            )?;
        }
        SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
            marker_was_set: true,
        } => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: Fallback power_off failed, but state already set by another hook. Keeping state."
            )?;
        }
        SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
            marker_was_set: false,
        } => {
            writeln!(
                writer,
                "LG Buddy Sleep Pre: Fallback power_off failed after retries. Leaving state unset."
            )?;
        }
    }

    Ok(())
}

fn render_network_teardown_stop<W: Write>(
    writer: &mut W,
    outcome: &PolicyOutcome,
) -> Result<(), RunError> {
    if outcome
        .no_actions
        .iter()
        .any(|decision| decision.reason.code == DecisionReasonCode::ConfigDisabled)
    {
        writeln!(
            writer,
            "LG Buddy NetworkManager: system sleep/wake handling is disabled by config; skipping pre-down handling."
        )?;
        return Ok(());
    }

    if let Some(decision) = outcome
        .no_actions
        .iter()
        .find(|decision| decision.reason.code == DecisionReasonCode::RuntimePhaseUnknown)
    {
        let detail = decision
            .reason
            .detail
            .as_deref()
            .unwrap_or("sleep phase is unknown");
        writeln!(
            writer,
            "LG Buddy NetworkManager: could not read logind PreparingForSleep; failing open. {detail}"
        )?;
    }

    Ok(())
}

fn render_restore_after_system_sleep_start<W: Write>(
    writer: &mut W,
    marker_exists: bool,
    decision: &LifecyclePolicyDecision<RestoreNext>,
) -> Result<(), RunError> {
    if decision.next == RestoreNext::Stop {
        writeln!(
            writer,
            "LG Buddy Startup: Wake from sleep: TV was not on our input. Skipping."
        )?;
    } else if marker_exists {
        writeln!(
            writer,
            "LG Buddy Startup: Wake from sleep: LG Buddy turned TV off. Restoring."
        )?;
    } else {
        log_markerless_restore_notice(writer, "LG Buddy Startup")?;
        writeln!(
            writer,
            "LG Buddy Startup: Wake from sleep: Restoring display state."
        )?;
    }

    Ok(())
}

fn apply_system_screen_marker_transitions(
    marker: &ScreenOwnershipMarker,
    outcome: &PolicyOutcome,
) -> Result<(), RunError> {
    for transition in &outcome.state_transitions {
        match transition {
            StateTransition::CreateMarker {
                marker: StateMarker::SystemScreenOwnership,
                ..
            } => marker.create()?,
            StateTransition::ClearMarker {
                marker: StateMarker::SystemScreenOwnership,
                ..
            } => marker.clear()?,
            StateTransition::PreserveMarker {
                marker: StateMarker::SystemScreenOwnership,
                ..
            } => {}
            StateTransition::CreateMarker { .. }
            | StateTransition::ClearMarker { .. }
            | StateTransition::PreserveMarker { .. } => {
                return Err(RunError::Policy(
                    "lifecycle screen marker applier received a non-system-screen transition"
                        .to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn apply_system_sleep_attempt_transitions(
    attempt_state: &SystemSleepAttemptState,
    outcome: &PolicyOutcome,
) -> Result<(), RunError> {
    for transition in &outcome.state_transitions {
        match transition {
            StateTransition::CreateMarker {
                marker: StateMarker::SystemSleepAttempt,
                ..
            } => attempt_state.mark_attempted()?,
            StateTransition::ClearMarker {
                marker: StateMarker::SystemSleepAttempt,
                ..
            } => attempt_state.clear()?,
            StateTransition::PreserveMarker {
                marker: StateMarker::SystemSleepAttempt,
                ..
            } => {}
            StateTransition::CreateMarker { .. }
            | StateTransition::ClearMarker { .. }
            | StateTransition::PreserveMarker { .. } => {
                return Err(RunError::Policy(
                    "lifecycle sleep-attempt applier received a non-attempt transition".to_string(),
                ));
            }
        }
    }

    Ok(())
}

fn create_system_marker(reason: TransitionReasonCode) -> StateTransition {
    StateTransition::create_marker(
        StateMarker::SystemScreenOwnership,
        TransitionReason::new(reason),
    )
}

fn clear_system_marker(reason: TransitionReasonCode) -> StateTransition {
    StateTransition::clear_marker(
        StateMarker::SystemScreenOwnership,
        TransitionReason::new(reason),
    )
}

fn clear_system_sleep_attempt_marker(reason: TransitionReasonCode) -> StateTransition {
    StateTransition::clear_marker(
        StateMarker::SystemSleepAttempt,
        TransitionReason::new(reason),
    )
}

fn query_current_input_with_retries<C: TvClient, Sl: Sleeper>(
    tv: &TvDevice<'_, C>,
    sleeper: &Sl,
    retries: u32,
) -> Result<CurrentInput, crate::tv::TvError> {
    let mut last_err = None;

    for attempt in 0..=retries {
        match tv.input().current() {
            Ok(current_input) => return Ok(current_input),
            Err(err) => {
                last_err = Some(err);
                if attempt < retries {
                    sleeper.sleep(system_sleep_retry_delay());
                }
            }
        }
    }

    Err(last_err.expect("retry loop should capture a tv error"))
}

fn retry_power_off<C: TvClient, Sl: Sleeper>(
    tv: &TvDevice<'_, C>,
    sleeper: &Sl,
    attempts: u32,
) -> bool {
    for attempt in 1..=attempts {
        if tv.power().off().is_ok() {
            return true;
        }

        if attempt < attempts {
            sleeper.sleep(system_sleep_retry_delay());
        }
    }

    false
}

fn duration_override_secs(env_key: &str, default: Duration) -> Duration {
    env::var(env_key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(default)
}

pub(crate) fn startup_initial_wake_delay() -> Duration {
    duration_override_secs(
        "LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS",
        STARTUP_INITIAL_WAKE_DELAY,
    )
}

pub(crate) fn startup_retry_delay(attempt: u32) -> Duration {
    duration_override_secs(
        "LG_BUDDY_STARTUP_RETRY_DELAY_SECS",
        Duration::from_secs(u64::from((attempt * 2).min(30))),
    )
}

fn system_sleep_retry_delay() -> Duration {
    duration_override_secs("LG_BUDDY_SLEEP_RETRY_DELAY_SECS", Duration::from_secs(1))
}

#[cfg(test)]
mod tests {
    mod support {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/support/mod.rs"));
    }

    use super::{
        attempt_system_sleep_power_off_once_with_outcome,
        attempt_system_sleep_power_off_with_outcome, decide_network_teardown,
        decide_restore_after_system_sleep_start, decide_shutdown_after_input,
        decide_shutdown_after_reboot, decide_startup_route, decide_system_sleep_after_input,
        decide_system_sleep_attempt_start, decide_system_sleep_power_off_result,
        handle_network_teardown_with_outcome, restore_after_system_sleep_with_outcome,
        run_shutdown_with_outcome, run_startup_with_outcome, LifecycleEvent, NetworkTeardownNext,
        NetworkTeardownPolicyInput, NetworkWaiter, RebootDetector, RebootObservation, RestoreNext,
        ShutdownNext, Sleeper, StartupDeps, StartupRoute, SystemSleepAttemptNext, SystemSleepNext,
        SystemSleepPowerOffContext, TvEffectObservation, TvInputObservation,
    };
    use crate::config::{
        Config, HdmiInput, MacAddress, ScreenBackend, ScreenRestorePolicy, SystemSleepWakePolicy,
    };
    use crate::events::{EventSource, RuntimeEvent, RuntimeEventKind};
    use crate::policy::{
        ActionKind, DecisionReasonCode, StateMarker, StateTransition, TransitionReason,
        TransitionReasonCode,
    };
    use crate::state::{ScreenOwnershipMarker, SystemSleepAttemptState};
    use crate::tv::{BscpylgtvCommandClient, CurrentInput};
    use crate::wol::{WakeOnLanError, WakeOnLanSender};
    use crate::StartupMode;
    use std::cell::RefCell;
    use std::fs;
    use std::io;
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};
    use support::MockBscpylgtv;

    #[test]
    fn pure_startup_policy_routes_from_mode_and_marker_observation() {
        assert_eq!(
            decide_startup_route(StartupMode::Boot, true),
            StartupRoute::ColdBoot
        );
        assert_eq!(
            decide_startup_route(StartupMode::Wake, false),
            StartupRoute::SystemResume
        );
        assert_eq!(
            decide_startup_route(StartupMode::Auto, true),
            StartupRoute::SystemResume
        );
        assert_eq!(
            decide_startup_route(StartupMode::Auto, false),
            StartupRoute::ColdBoot
        );
    }

    #[test]
    fn pure_network_teardown_policy_selects_sleep_gate_behavior() {
        let pending = decide_network_teardown(NetworkTeardownPolicyInput {
            policy_enabled: true,
            machine_sleep_pending: Some(true),
            phase_read_error: None,
        });

        assert_eq!(pending.next, NetworkTeardownNext::AttemptPreSleep);
        assert!(pending.outcome.actions.is_empty());
        assert!(pending.outcome.no_actions.is_empty());

        let ordinary_disconnect = decide_network_teardown(NetworkTeardownPolicyInput {
            policy_enabled: true,
            machine_sleep_pending: Some(false),
            phase_read_error: None,
        });

        assert_eq!(ordinary_disconnect.next, NetworkTeardownNext::ClearAttempt);
        assert_eq!(
            ordinary_disconnect.outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
        assert_eq!(
            ordinary_disconnect.outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::RuntimePhaseIneligible),
            )]
        );

        let disabled = decide_network_teardown(NetworkTeardownPolicyInput {
            policy_enabled: false,
            machine_sleep_pending: Some(true),
            phase_read_error: None,
        });

        assert_eq!(disabled.next, NetworkTeardownNext::Stop);
        assert_eq!(
            disabled.outcome.no_actions[0].reason.code,
            DecisionReasonCode::ConfigDisabled
        );

        let unknown_phase = decide_network_teardown(NetworkTeardownPolicyInput {
            policy_enabled: true,
            machine_sleep_pending: None,
            phase_read_error: Some("dbus unavailable".to_string()),
        });

        assert_eq!(unknown_phase.next, NetworkTeardownNext::Stop);
        assert_eq!(
            unknown_phase.outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseUnknown
        );
        assert_eq!(unknown_phase.outcome.diagnostics.len(), 1);
    }

    #[test]
    fn pure_system_sleep_policy_selects_actions_and_marker_transitions_from_observations() {
        let matching_input = decide_system_sleep_after_input(
            HdmiInput::Hdmi3,
            TvInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi3)),
        );

        assert_eq!(matching_input.next, SystemSleepNext::PowerOff);
        assert_eq!(
            matching_input.outcome.actions[0].kind,
            ActionKind::TvSystemSleepPowerOff
        );

        let different_input = decide_system_sleep_after_input(
            HdmiInput::Hdmi3,
            TvInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi1)),
        );

        assert_eq!(different_input.next, SystemSleepNext::Stop);
        assert_eq!(
            different_input.outcome.no_actions[0].reason.code,
            DecisionReasonCode::InputMismatch
        );
        assert_eq!(
            different_input.outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::InputMismatch),
            )]
        );

        let power_off_success = decide_system_sleep_power_off_result(
            TvEffectObservation::Succeeded,
            SystemSleepPowerOffContext::KnownInput,
        );

        assert_eq!(
            power_off_success.state_transitions,
            vec![StateTransition::create_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            )]
        );

        let query_failure = decide_system_sleep_after_input(
            HdmiInput::Hdmi3,
            TvInputObservation::QueryFailed("timeout".to_string()),
        );

        assert_eq!(query_failure.next, SystemSleepNext::PowerOff);
        assert_eq!(
            query_failure.outcome.actions[0].reason.code,
            DecisionReasonCode::TransportFailure
        );
        assert_eq!(query_failure.outcome.diagnostics.len(), 1);

        let fallback_failure_preserves_marker = decide_system_sleep_power_off_result(
            TvEffectObservation::Failed("timeout".to_string()),
            SystemSleepPowerOffContext::FallbackAfterInputQueryFailure {
                marker_was_set: true,
            },
        );

        assert_eq!(
            fallback_failure_preserves_marker.state_transitions,
            vec![StateTransition::preserve_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::Other),
            )]
        );
    }

    #[test]
    fn pure_system_sleep_attempt_policy_dedupes_before_effects() {
        let blocked_by_lock = decide_system_sleep_attempt_start(false, false);

        assert_eq!(blocked_by_lock.next, SystemSleepAttemptNext::Stop);
        assert_eq!(
            blocked_by_lock.outcome.no_actions[0].reason.code,
            DecisionReasonCode::DuplicateSystemSleepAttempt
        );
        assert_eq!(
            blocked_by_lock.outcome.state_transitions,
            vec![StateTransition::preserve_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::DuplicateSystemSleepAttempt),
            )]
        );

        let already_attempted = decide_system_sleep_attempt_start(true, true);

        assert_eq!(already_attempted.next, SystemSleepAttemptNext::Stop);
        assert_eq!(
            already_attempted.outcome.no_actions[0].reason.code,
            DecisionReasonCode::DuplicateSystemSleepAttempt
        );

        let first_attempt = decide_system_sleep_attempt_start(true, false);

        assert_eq!(first_attempt.next, SystemSleepAttemptNext::Continue);
        assert_eq!(
            first_attempt.outcome.state_transitions,
            vec![StateTransition::create_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            )]
        );
    }

    #[test]
    fn pure_restore_policy_uses_marker_state_and_restore_mode_as_inputs() {
        let conservative_missing =
            decide_restore_after_system_sleep_start(ScreenRestorePolicy::MarkerOnly, false);

        assert_eq!(conservative_missing.next, RestoreNext::Stop);
        assert_eq!(
            conservative_missing.outcome.no_actions[0].reason.code,
            DecisionReasonCode::MarkerMissing
        );

        let aggressive_missing =
            decide_restore_after_system_sleep_start(ScreenRestorePolicy::Aggressive, false);

        assert_eq!(aggressive_missing.next, RestoreNext::Restore);
        assert_eq!(
            aggressive_missing.outcome.actions[0].kind,
            ActionKind::TvSystemResumeRestore
        );
        assert_eq!(
            aggressive_missing.outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::RestoreCompleted),
            )]
        );
    }

    #[test]
    fn pure_shutdown_policy_uses_reboot_and_input_observations() {
        let reboot = decide_shutdown_after_reboot(RebootObservation::Pending);

        assert_eq!(reboot.next, ShutdownNext::Stop);
        assert_eq!(
            reboot.outcome.no_actions[0].reason.code,
            DecisionReasonCode::NotApplicable
        );

        let matching_input = decide_shutdown_after_input(
            HdmiInput::Hdmi1,
            TvInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi1)),
        );

        assert_eq!(matching_input.next, ShutdownNext::PowerOff);
        assert_eq!(
            matching_input.outcome.actions[0].kind,
            ActionKind::TvShutdownPowerOff
        );

        let unknown_reboot_state =
            decide_shutdown_after_reboot(RebootObservation::Unknown("journal unavailable".into()));

        assert_eq!(unknown_reboot_state.next, ShutdownNext::QueryInput);
        assert_eq!(unknown_reboot_state.outcome.diagnostics.len(), 1);

        let mismatched_input = decide_shutdown_after_input(
            HdmiInput::Hdmi1,
            TvInputObservation::Current(CurrentInput::Hdmi(HdmiInput::Hdmi4)),
        );

        assert_eq!(mismatched_input.next, ShutdownNext::Stop);
        assert_eq!(
            mismatched_input.outcome.no_actions[0].reason.code,
            DecisionReasonCode::InputMismatch
        );

        let query_failure = decide_shutdown_after_input(
            HdmiInput::Hdmi1,
            TvInputObservation::QueryFailed("timeout".to_string()),
        );

        assert_eq!(query_failure.next, ShutdownNext::PowerOff);
        assert_eq!(
            query_failure.outcome.actions[0].reason.code,
            DecisionReasonCode::TransportFailure
        );
        assert_eq!(query_failure.outcome.diagnostics.len(), 1);
    }

    #[test]
    fn lifecycle_events_map_from_canonical_runtime_events() {
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::new(
                EventSource::CliApi,
                RuntimeEventKind::MachineStartup {
                    mode: StartupMode::Boot,
                },
            )),
            Some(LifecycleEvent::Startup {
                mode: StartupMode::Boot,
            })
        );
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::network_teardown_imminent(Some(true))),
            Some(LifecycleEvent::NetworkTeardownImminent {
                machine_sleep_pending: Some(true),
            })
        );
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::from_logind_prepare_for_sleep(true)),
            Some(LifecycleEvent::MachinePreparingForSleep)
        );
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::from_logind_prepare_for_sleep(false)),
            Some(LifecycleEvent::MachineResumed)
        );
        assert_eq!(
            LifecycleEvent::from_runtime_event(RuntimeEvent::new(
                EventSource::DesktopSession,
                RuntimeEventKind::SessionIdle,
            )),
            None
        );
    }

    #[test]
    fn system_sleep_power_off_outcome_records_action_and_system_marker_creation() {
        let temp_dir = TestDir::new("lifecycle-sleep-outcome");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-sleep-outcome-tv");
        mock.set_input("HDMI_3");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = attempt_system_sleep_power_off_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi3),
            &marker,
            &client,
            &sleeper,
        )
        .expect("system sleep power off should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvSystemSleepPowerOff]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::create_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::ActionSelected),
            )]
        );
        assert!(outcome.no_actions.is_empty());
        assert!(marker.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    #[test]
    fn duplicate_system_sleep_attempt_records_no_action_and_preserves_attempt_marker() {
        let temp_dir = TestDir::new("lifecycle-duplicate-sleep-outcome");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        attempt_state
            .mark_attempted()
            .expect("create existing attempt marker");
        let mock = MockBscpylgtv::new("lifecycle-duplicate-sleep-outcome-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = attempt_system_sleep_power_off_once_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
        )
        .expect("duplicate system sleep attempt should skip");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::DuplicateSystemSleepAttempt
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::preserve_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::DuplicateSystemSleepAttempt),
            )]
        );
        assert_call_commands(&mock, &[]);
    }

    #[test]
    fn network_teardown_pending_sleep_records_attempt_and_sleep_power_off() {
        let temp_dir = TestDir::new("lifecycle-nm-pending-sleep");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-nm-pending-sleep-tv");
        mock.set_input("HDMI_2");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            RuntimeEvent::network_teardown_imminent(Some(true)),
            None,
        )
        .expect("pending sleep teardown should power off once");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvSystemSleepPowerOff]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![
                StateTransition::create_marker(
                    StateMarker::SystemSleepAttempt,
                    TransitionReason::new(TransitionReasonCode::ActionSelected),
                ),
                StateTransition::create_marker(
                    StateMarker::SystemScreenOwnership,
                    TransitionReason::new(TransitionReasonCode::ActionSelected),
                ),
            ]
        );
        assert!(marker.exists());
        assert!(attempt_state.exists());
        assert_call_commands(&mock, &["get_input", "power_off"]);
        assert!(rendered(&output).contains("logind is preparing for sleep"));
    }

    #[test]
    fn network_teardown_ordinary_disconnect_records_no_action_and_clears_attempt() {
        let temp_dir = TestDir::new("lifecycle-nm-not-sleeping");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        attempt_state
            .mark_attempted()
            .expect("create stale attempt marker");
        let mock = MockBscpylgtv::new("lifecycle-nm-not-sleeping-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            RuntimeEvent::network_teardown_imminent(Some(false)),
            None,
        )
        .expect("ordinary network disconnect should fail open");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseIneligible
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemSleepAttempt,
                TransitionReason::new(TransitionReasonCode::RuntimePhaseIneligible),
            )]
        );
        assert!(!attempt_state.exists());
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("not preparing for sleep"));
    }

    #[test]
    fn network_teardown_unknown_phase_records_fail_open_no_action() {
        let temp_dir = TestDir::new("lifecycle-nm-unknown-phase");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-nm-unknown-phase-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            RuntimeEvent::network_teardown_imminent(None),
            Some("system bus unavailable"),
        )
        .expect("unknown sleep phase should fail open");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::RuntimePhaseUnknown
        );
        assert_eq!(outcome.diagnostics.len(), 1);
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("failing open"));
    }

    #[test]
    fn network_teardown_disabled_policy_records_no_action_without_state_change() {
        let temp_dir = TestDir::new("lifecycle-nm-disabled");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        let attempt_state = SystemSleepAttemptState::new(temp_dir.path().to_path_buf());
        let mock = MockBscpylgtv::new("lifecycle-nm-disabled-tv");
        let client = client_for_mock(&mock);
        let sleeper = RecordingSleeper::default();

        let mut config = sample_config(HdmiInput::Hdmi2);
        config.system_sleep_wake_policy = SystemSleepWakePolicy::Disabled;
        let mut output = Vec::new();
        let outcome = handle_network_teardown_with_outcome(
            &mut output,
            &config,
            &marker,
            &attempt_state,
            &client,
            &sleeper,
            RuntimeEvent::network_teardown_imminent(Some(true)),
            None,
        )
        .expect("disabled policy should skip");

        assert!(outcome.actions.is_empty());
        assert_eq!(outcome.no_actions.len(), 1);
        assert_eq!(
            outcome.no_actions[0].reason.code,
            DecisionReasonCode::ConfigDisabled
        );
        assert!(outcome.state_transitions.is_empty());
        assert_call_commands(&mock, &[]);
        assert!(rendered(&output).contains("disabled by config"));
    }

    #[test]
    fn system_resume_outcome_records_restore_actions_and_system_marker_clear() {
        let temp_dir = TestDir::new("lifecycle-resume-outcome");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create system marker");
        let mock = MockBscpylgtv::new("lifecycle-resume-outcome-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();

        let mut output = Vec::new();
        let outcome = restore_after_system_sleep_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi4),
            &marker,
            &client,
            &wol,
            &sleeper,
            &network,
        )
        .expect("system resume restore should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![
                ActionKind::TvSystemResumeRestore,
                ActionKind::WakeOnLan,
                ActionKind::TvInputRestore,
            ]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::RestoreCompleted),
            )]
        );
        assert!(!marker.exists());
        assert_eq!(wol.calls().len(), 1);
        assert_eq!(sleeper.durations(), vec![Duration::from_secs(6)]);
        assert_eq!(network.calls(), 1);
        assert_call_commands(&mock, &["set_input"]);
    }

    #[test]
    fn startup_boot_outcome_records_restore_actions_and_system_marker_clear() {
        let temp_dir = TestDir::new("lifecycle-startup-outcome");
        let marker = ScreenOwnershipMarker::new(temp_dir.path().to_path_buf());
        marker.create().expect("create stale system marker");
        let mock = MockBscpylgtv::new("lifecycle-startup-outcome-tv");
        let client = client_for_mock(&mock);
        let wol = RecordingWakeOnLanSender::default();
        let sleeper = RecordingSleeper::default();
        let network = FakeNetworkWaiter::clear();
        let deps = StartupDeps {
            tv_client: &client,
            wol_sender: &wol,
            sleeper: &sleeper,
            network_waiter: &network,
        };

        let mut output = Vec::new();
        let outcome = run_startup_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi2),
            &marker,
            deps,
            StartupMode::Boot,
        )
        .expect("startup boot should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![
                ActionKind::TvStartupRestore,
                ActionKind::WakeOnLan,
                ActionKind::TvInputRestore,
            ]
        );
        assert_eq!(
            outcome.state_transitions,
            vec![StateTransition::clear_marker(
                StateMarker::SystemScreenOwnership,
                TransitionReason::new(TransitionReasonCode::StartupBoot),
            )]
        );
        assert!(!marker.exists());
        assert_eq!(network.calls(), 1);
        assert_call_commands(&mock, &["set_input"]);
    }

    #[test]
    fn shutdown_outcome_records_power_off_action_for_configured_input() {
        let mock = MockBscpylgtv::new("lifecycle-shutdown-outcome-tv");
        mock.set_input("HDMI_1");
        let client = client_for_mock(&mock);
        let reboot = FakeRebootDetector::clear();

        let mut output = Vec::new();
        let outcome = run_shutdown_with_outcome(
            &mut output,
            &sample_config(HdmiInput::Hdmi1),
            &client,
            &reboot,
        )
        .expect("shutdown should succeed");

        assert_eq!(
            outcome
                .actions
                .iter()
                .map(|action| action.kind)
                .collect::<Vec<_>>(),
            vec![ActionKind::TvShutdownPowerOff]
        );
        assert!(outcome.no_actions.is_empty());
        assert!(outcome.state_transitions.is_empty());
        assert_call_commands(&mock, &["get_input", "power_off"]);
    }

    fn sample_config(input: HdmiInput) -> Config {
        Config {
            tv_ip: "192.0.2.42".parse::<Ipv4Addr>().expect("parse ipv4"),
            tv_mac: "aa:bb:cc:dd:ee:ff"
                .parse::<MacAddress>()
                .expect("parse mac"),
            input,
            screen_backend: ScreenBackend::Auto,
            screen_idle_timeout: 300,
            screen_restore_policy: ScreenRestorePolicy::MarkerOnly,
            system_sleep_wake_policy: SystemSleepWakePolicy::Enabled,
        }
    }

    fn client_for_mock(mock: &MockBscpylgtv) -> BscpylgtvCommandClient {
        BscpylgtvCommandClient::with_args(mock.command_path(), mock.command_args())
    }

    fn assert_call_commands(mock: &MockBscpylgtv, expected: &[&str]) {
        let actual = mock
            .calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>();
        let expected = expected
            .iter()
            .map(|command| command.to_string())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
    }

    fn rendered(output: &[u8]) -> String {
        String::from_utf8(output.to_vec()).expect("utf8 output")
    }

    #[derive(Default)]
    struct RecordingWakeOnLanSender {
        calls: RefCell<Vec<MacAddress>>,
    }

    impl RecordingWakeOnLanSender {
        fn calls(&self) -> Vec<MacAddress> {
            self.calls.borrow().clone()
        }
    }

    impl WakeOnLanSender for RecordingWakeOnLanSender {
        fn send_magic_packet(&self, mac: &MacAddress) -> Result<(), WakeOnLanError> {
            self.calls.borrow_mut().push(mac.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingSleeper {
        durations: RefCell<Vec<Duration>>,
    }

    impl RecordingSleeper {
        fn durations(&self) -> Vec<Duration> {
            self.durations.borrow().clone()
        }
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.durations.borrow_mut().push(duration);
        }
    }

    struct FakeNetworkWaiter {
        calls: RefCell<u32>,
    }

    impl FakeNetworkWaiter {
        fn clear() -> Self {
            Self {
                calls: RefCell::new(0),
            }
        }

        fn calls(&self) -> u32 {
            *self.calls.borrow()
        }
    }

    impl NetworkWaiter for FakeNetworkWaiter {
        fn wait_for_network(&self) -> io::Result<()> {
            *self.calls.borrow_mut() += 1;
            Ok(())
        }
    }

    struct FakeRebootDetector {
        pending: io::Result<bool>,
    }

    impl FakeRebootDetector {
        fn clear() -> Self {
            Self { pending: Ok(false) }
        }
    }

    impl RebootDetector for FakeRebootDetector {
        fn is_reboot_pending(&self) -> io::Result<bool> {
            match &self.pending {
                Ok(value) => Ok(*value),
                Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
            }
        }
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);

            let unique = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let timestamp = std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time after unix epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "lg-buddy-{label}-{}-{timestamp}-{unique}",
                process::id()
            ));

            fs::create_dir_all(&path).expect("create test temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
