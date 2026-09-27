//! Foreground native backend readiness.
//!
//! Composes the accepted strong GNOME probe
//! (`sources::desktop::gnome::probe::check_gnome_readiness_with_probe`) and
//! the accepted strong foreground Wayland checker
//! (`sources::desktop::wayland::check_foreground_wayland_readiness`) around
//! the existing `resolve_backend_with_probe` selection policy. Every value
//! comes from the explicit captured `NativeReadinessContext`; this module
//! never reads or mutates the process environment, never probes commands
//! (so it can never select swayidle), and performs no service, status,
//! action, config, token, runtime, or GUI wiring.

use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::backend::{
    resolve_backend_with_probe, BackendProbe, BackendResolution, ScreenBackend,
    WaylandProviderCapabilities,
};
use crate::sources::desktop::gnome::probe::{check_gnome_readiness_with_probe, GnomeProbeError};
use crate::sources::desktop::wayland::{check_foreground_wayland_readiness, WaylandProviderError};

/// Explicit captured foreground context for one native readiness check.
///
/// Deliberately does not derive `Debug`: the fields hold a probe
/// executable, a bus address, and display/runtime values that error and
/// log text must never expose.
pub struct NativeReadinessContext {
    pub probe_executable: PathBuf,
    pub bus_address: Option<String>,
    pub inherited_wayland_socket: Option<OsString>,
    pub wayland_display: Option<OsString>,
    pub runtime_dir: Option<OsString>,
    pub backend_override: Option<OsString>,
    pub probe_timeout: Duration,
}

/// Typed foreground native readiness errors with fixed, context-free
/// Display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeReadinessError {
    /// A stop was observed; the check was cancelled before completion.
    Cancelled,
    /// `LG_BUDDY_SCREEN_BACKEND` set any value other than exactly `auto`.
    ConflictingOverride,
    /// Neither the GNOME probe nor the native Wayland checker reported a
    /// ready source.
    Unavailable,
}

impl fmt::Display for NativeReadinessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => {
                write!(f, "native readiness check was cancelled before completion")
            }
            Self::ConflictingOverride => write!(
                f,
                "a conflicting LG_BUDDY_SCREEN_BACKEND value is set; remove it or set it to auto to use a native activity source, or set screen.idle_blank to disabled to keep LG Buddy without idle monitoring"
            ),
            Self::Unavailable => write!(
                f,
                "no native activity source is ready; retry native readiness when GNOME or a Wayland compositor is available, or set screen.idle_blank to disabled"
            ),
        }
    }
}

impl std::error::Error for NativeReadinessError {}

/// Run the foreground native readiness check for the captured context.
///
/// A stop observed anywhere wins immediately, before any check and before
/// any error or success is returned. A `backend_override` other than
/// exactly `auto` (including empty and non-UTF8 values) conflicts and is
/// rejected before any probe runs. Otherwise the strong GNOME probe runs
/// first; only if GNOME is not ready does the strong foreground Wayland
/// checker run, exactly once. A cancelled GNOME probe never falls back.
pub fn check_native_readiness(
    context: &NativeReadinessContext,
    stop: &AtomicBool,
) -> Result<BackendResolution, NativeReadinessError> {
    if stop.load(Ordering::SeqCst) {
        return Err(NativeReadinessError::Cancelled);
    }
    if let Some(override_value) = &context.backend_override {
        if override_value != OsStr::new("auto") {
            return if stop.load(Ordering::SeqCst) {
                Err(NativeReadinessError::Cancelled)
            } else {
                Err(NativeReadinessError::ConflictingOverride)
            };
        }
    }
    if stop.load(Ordering::SeqCst) {
        return Err(NativeReadinessError::Cancelled);
    }

    let production = ProductionCheckers {
        executable: &context.probe_executable,
        bus_address: context.bus_address.as_deref(),
        inherited_wayland_socket: context.inherited_wayland_socket.as_deref(),
        wayland_display: context.wayland_display.as_deref(),
        runtime_dir: context.runtime_dir.as_deref(),
        probe_timeout: context.probe_timeout,
    };
    check_native_readiness_with_checkers(&production, stop)
}

/// The tested selection seam: the same policy the production entrypoint
/// runs, with the two checkers injected. GNOME stronger readiness is
/// evaluated first and its outcome cached for the three `gnome_*`
/// predicates; the injected Wayland checker is only called if the Auto
/// selection reaches it. The injected checkers never touch commands, so
/// swayidle can never be selected.
fn check_native_readiness_with_checkers(
    checkers: &impl NativeCheckers,
    stop: &AtomicBool,
) -> Result<BackendResolution, NativeReadinessError> {
    let probe = CachingBackendProbe {
        checkers,
        stop,
        gnome_ready: Cell::new(None),
        cancelled: Cell::new(false),
    };
    match resolve_backend_with_probe(&probe, ScreenBackend::Auto) {
        Ok(resolution) => {
            // A stop observed before returning a selection wins over it.
            if probe.cancelled.get() || stop.load(Ordering::SeqCst) {
                return Err(NativeReadinessError::Cancelled);
            }
            Ok(resolution)
        }
        // No native activity source is ready.
        Err(_) => {
            if probe.cancelled.get() || stop.load(Ordering::SeqCst) {
                return Err(NativeReadinessError::Cancelled);
            }
            Err(NativeReadinessError::Unavailable)
        }
    }
}

/// The two strong checkers the selection policy may invoke, exactly once
/// each per check.
trait NativeCheckers {
    /// Strong GNOME readiness through the explicit probe process.
    fn gnome_ready(&self, stop: &AtomicBool) -> Result<(), GnomeProbeError>;
    /// Strong foreground Wayland readiness through the accepted checker.
    fn wayland_ready(
        &self,
        stop: &AtomicBool,
    ) -> Result<WaylandProviderCapabilities, WaylandProviderError>;
}

struct ProductionCheckers<'a> {
    executable: &'a PathBuf,
    bus_address: Option<&'a str>,
    inherited_wayland_socket: Option<&'a OsStr>,
    wayland_display: Option<&'a OsStr>,
    runtime_dir: Option<&'a OsStr>,
    probe_timeout: Duration,
}

impl NativeCheckers for ProductionCheckers<'_> {
    fn gnome_ready(&self, stop: &AtomicBool) -> Result<(), GnomeProbeError> {
        check_gnome_readiness_with_probe(
            self.executable,
            self.bus_address,
            self.probe_timeout,
            stop,
        )
    }

    fn wayland_ready(
        &self,
        stop: &AtomicBool,
    ) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
        check_foreground_wayland_readiness(
            self.inherited_wayland_socket,
            self.wayland_display,
            self.runtime_dir,
            stop,
        )
    }
}

/// Adapter that feeds the accepted `resolve_backend_with_probe` policy from
/// the injected strong checkers. It caches the GNOME result for the three
/// `gnome_*` predicates, so the policy's three calls collapse to one
/// checker invocation; the Wayland checker is only called if the Auto
/// selection reaches it. `has_command` is never invoked on the Auto path
/// and is refused outright, so no command is ever probed.
struct CachingBackendProbe<'a, C> {
    checkers: &'a C,
    stop: &'a AtomicBool,
    gnome_ready: Cell<Option<bool>>,
    cancelled: Cell<bool>,
}

impl<C> BackendProbe for CachingBackendProbe<'_, C>
where
    C: NativeCheckers,
{
    fn has_command(&self, _command: &str) -> bool {
        // The Auto policy never probes commands, and this foreground
        // readiness check must never select swayidle.
        panic!("has_command must never be invoked by native readiness");
    }

    fn gnome_shell_available(&self) -> bool {
        self.gnome_available()
    }

    fn gnome_screen_saver_available(&self) -> bool {
        self.gnome_available()
    }

    fn gnome_idle_monitor_available(&self) -> bool {
        self.gnome_available()
    }

    fn wayland_capabilities(&self) -> Result<WaylandProviderCapabilities, String> {
        // A cancelled check never falls back: the Wayland checker is only
        // called while no stop has been observed.
        if self.cancelled.get() || self.stop.load(Ordering::SeqCst) {
            return Err(NativeReadinessError::Cancelled.to_string());
        }
        self.checkers.wayland_ready(self.stop).map_err(|error| {
            if matches!(error, WaylandProviderError::Cancelled) {
                self.cancelled.set(true);
            }
            error.to_string()
        })
    }
}

impl<C> CachingBackendProbe<'_, C>
where
    C: NativeCheckers,
{
    /// The one strong GNOME check, cached so the policy's three `gnome_*`
    /// predicates collapse into a single checker invocation. A stop
    /// observed before the check wins without running the checker.
    fn gnome_available(&self) -> bool {
        if self.gnome_ready.get().is_none() {
            if self.stop.load(Ordering::SeqCst) {
                self.gnome_ready.set(Some(false));
            } else {
                let result = self.checkers.gnome_ready(self.stop);
                if matches!(result, Err(GnomeProbeError::Cancelled)) {
                    self.cancelled.set(true);
                }
                self.gnome_ready.set(Some(result.is_ok()));
            }
        }
        self.gnome_ready.get() == Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        check_native_readiness, check_native_readiness_with_checkers, NativeCheckers,
        NativeReadinessContext, NativeReadinessError,
    };
    use crate::backend::{BackendResolution, WaylandProviderCapabilities};
    use crate::sources::desktop::gnome::probe::GnomeProbeError;
    use crate::sources::desktop::wayland::WaylandProviderError;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// Headless injected checkers: count invocations and can be steered to
    /// cancel in the middle of a check. No bus, display, or process.
    struct TestCheckers {
        gnome_outcome: GnomeTestOutcome,
        wayland_result: Result<(), &'static str>,
        gnome_calls: AtomicU32,
        wayland_calls: AtomicU32,
        cancel_on_gnome: bool,
        cancel_on_wayland: bool,
    }

    /// The strong GNOME checker outcome; a plain `Copy` tag so the injected
    /// checker never needs `GnomeProbeError` to be clonable.
    #[derive(Clone, Copy)]
    enum GnomeTestOutcome {
        Ready,
        Unavailable,
        NotReady,
    }

    impl TestCheckers {
        fn gnome_available(wayland_available: bool) -> Self {
            Self {
                gnome_outcome: GnomeTestOutcome::Ready,
                wayland_result: if wayland_available {
                    Ok(())
                } else {
                    Err("no Wayland compositor is available")
                },
                gnome_calls: AtomicU32::new(0),
                wayland_calls: AtomicU32::new(0),
                cancel_on_gnome: false,
                cancel_on_wayland: false,
            }
        }

        fn check(&self, stop: &AtomicBool) -> Result<BackendResolution, NativeReadinessError> {
            check_native_readiness_with_checkers(self, stop)
        }
    }

    impl NativeCheckers for TestCheckers {
        fn gnome_ready(&self, stop: &AtomicBool) -> Result<(), GnomeProbeError> {
            self.gnome_calls.fetch_add(1, Ordering::SeqCst);
            if self.cancel_on_gnome {
                stop.store(true, Ordering::SeqCst);
            }
            match self.gnome_outcome {
                GnomeTestOutcome::Ready => Ok(()),
                GnomeTestOutcome::Unavailable => Err(GnomeProbeError::Unavailable),
                GnomeTestOutcome::NotReady => Err(GnomeProbeError::NotReady),
            }
        }

        fn wayland_ready(
            &self,
            stop: &AtomicBool,
        ) -> Result<crate::backend::WaylandProviderCapabilities, WaylandProviderError> {
            self.wayland_calls.fetch_add(1, Ordering::SeqCst);
            if self.cancel_on_wayland {
                stop.store(true, Ordering::SeqCst);
            }
            self.wayland_result
                .map(|()| crate::backend::WaylandProviderCapabilities {
                    idle_notifier_version: 2,
                    seat_count: 1,
                })
                .map_err(|reason| WaylandProviderError::Connection(reason.to_string()))
        }
    }

    fn context(override_value: Option<OsString>) -> NativeReadinessContext {
        NativeReadinessContext {
            probe_executable: PathBuf::from("/usr/libexec/lg-buddy-probe"),
            bus_address: Some("unix:abstract=lg-buddy-test".to_string()),
            inherited_wayland_socket: None,
            wayland_display: Some(OsString::from("wayland-1")),
            runtime_dir: Some(OsString::from("/run/user/1000")),
            backend_override: override_value,
            probe_timeout: Duration::from_secs(2),
        }
    }

    #[test]
    fn typed_cancellation_without_stop_flag_is_never_downgraded_or_falls_back() {
        struct TypedCancelled(bool);
        impl NativeCheckers for TypedCancelled {
            fn gnome_ready(&self, _: &AtomicBool) -> Result<(), GnomeProbeError> {
                if self.0 {
                    Err(GnomeProbeError::Cancelled)
                } else {
                    Err(GnomeProbeError::Unavailable)
                }
            }
            fn wayland_ready(
                &self,
                _: &AtomicBool,
            ) -> Result<WaylandProviderCapabilities, WaylandProviderError> {
                assert!(!self.0, "cancelled GNOME must not reach Wayland");
                Err(WaylandProviderError::Cancelled)
            }
        }
        for gnome_cancelled in [true, false] {
            let stop = AtomicBool::new(false);
            assert_eq!(
                check_native_readiness_with_checkers(&TypedCancelled(gnome_cancelled), &stop),
                Err(NativeReadinessError::Cancelled)
            );
            assert!(!stop.load(Ordering::Acquire));
        }
    }

    #[test]
    fn gnome_is_preferred_and_the_wayland_checker_is_never_called() {
        let checkers = TestCheckers::gnome_available(true);
        let stop = AtomicBool::new(false);

        let resolution = checkers.check(&stop).expect("GNOME is ready");

        assert_eq!(resolution.backend(), crate::config::ScreenBackend::Gnome);
        assert_eq!(resolution.fallback_reason(), None);
        assert_eq!(checkers.gnome_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checkers.wayland_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_gnome_failure_falls_back_to_wayland_exactly_once() {
        let checkers = TestCheckers {
            gnome_outcome: GnomeTestOutcome::Unavailable,
            ..TestCheckers::gnome_available(true)
        };
        let stop = AtomicBool::new(false);

        let resolution = checkers.check(&stop).expect("native Wayland is ready");

        assert_eq!(resolution.backend(), crate::config::ScreenBackend::Wayland);
        assert!(
            resolution.fallback_reason().is_some(),
            "a fallback selection carries the GNOME reason"
        );
        assert_eq!(checkers.gnome_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checkers.wayland_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn both_sources_failing_is_unavailable() {
        let checkers = TestCheckers {
            gnome_outcome: GnomeTestOutcome::Unavailable,
            wayland_result: Err("no Wayland compositor is available"),
            ..TestCheckers::gnome_available(false)
        };
        let stop = AtomicBool::new(false);

        let err = checkers
            .check(&stop)
            .expect_err("no native source is ready");

        assert_eq!(err, NativeReadinessError::Unavailable);
        assert!(err.to_string().contains("screen.idle_blank to disabled"));
    }

    #[test]
    fn a_conflicting_override_makes_zero_checker_calls() {
        for override_value in [
            OsString::from("gnome"),
            OsString::from("wayland"),
            OsString::from("swayidle"),
            OsString::new(),
            OsString::from("gnome "),
            OsString::from_vec(vec![0xff, 0xfe]),
        ] {
            let checkers = TestCheckers::gnome_available(true);
            let context = context(Some(override_value.clone()));
            let stop = AtomicBool::new(false);

            let err =
                check_native_readiness(&context, &stop).expect_err("a non-auto override conflicts");

            assert_eq!(
                err,
                NativeReadinessError::ConflictingOverride,
                "{override_value:?}"
            );
            assert!(err.to_string().contains("LG_BUDDY_SCREEN_BACKEND"));
            assert_eq!(checkers.gnome_calls.load(Ordering::SeqCst), 0);
            assert_eq!(checkers.wayland_calls.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn a_pre_cancelled_check_makes_zero_checker_calls() {
        let checkers = TestCheckers::gnome_available(true);
        let stop = AtomicBool::new(true);

        let err = checkers.check(&stop).expect_err("already cancelled");

        assert_eq!(err, NativeReadinessError::Cancelled);
        assert_eq!(checkers.gnome_calls.load(Ordering::SeqCst), 0);
        assert_eq!(checkers.wayland_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancellation_during_gnome_blocks_the_fallback_and_wins() {
        let checkers = TestCheckers {
            gnome_outcome: GnomeTestOutcome::NotReady,
            cancel_on_gnome: true,
            ..TestCheckers::gnome_available(true)
        };
        let stop = Arc::new(AtomicBool::new(false));

        let err = checkers.check(&stop).expect_err("the stop must win");

        assert_eq!(err, NativeReadinessError::Cancelled);
        assert_eq!(checkers.gnome_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            checkers.wayland_calls.load(Ordering::SeqCst),
            0,
            "a cancelled GNOME check never falls back"
        );
    }

    #[test]
    fn cancellation_during_wayland_wins_even_over_a_ready_result() {
        let checkers = TestCheckers {
            cancel_on_wayland: true,
            ..TestCheckers {
                gnome_outcome: GnomeTestOutcome::NotReady,
                ..TestCheckers::gnome_available(true)
            }
        };
        let stop = Arc::new(AtomicBool::new(false));

        let err = checkers.check(&stop).expect_err("the stop must win");

        assert_eq!(err, NativeReadinessError::Cancelled);
        assert_eq!(checkers.gnome_calls.load(Ordering::SeqCst), 1);
        assert_eq!(checkers.wayland_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn auto_or_an_absent_override_is_valid() {
        let stop = AtomicBool::new(false);

        // The production checkers run headless here (no probe executable,
        // no compositor), so both native sources fail; a valid override
        // must therefore reach them and fail with `Unavailable`, never
        // with `ConflictingOverride`.
        for override_value in [Some(OsString::from("auto")), None] {
            let context = context(override_value.clone());

            let err = check_native_readiness(&context, &stop)
                .expect_err("no native source is ready in a headless check");

            assert_eq!(err, NativeReadinessError::Unavailable, "{override_value:?}");
        }
    }
}
