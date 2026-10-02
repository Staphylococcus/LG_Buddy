//! Explicit GNOME probe process adapter.
//!
//! This slice composes the caller's deadline and cancellation around a
//! dedicated child process and uses the child's exit status as the readiness
//! protocol: the child performs the raw bus acquisition and the accepted
//! GNOME readiness check, then reports it as exit code 0 (ready), 1
//! (unavailable), 2 (cancelled), or 3 (not ready). It does not authorize
//! runtime/publication, select GNOME, or wire GUI/backend/migration. A clean
//! child exit attempts the explicit `RemoveWatch`; a forced termination runs
//! neither Rust `Drop` nor `RemoveWatch`, so this fixture does not prove
//! Mutter watch cleanup.

use std::error;
use std::fmt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::command::{run_status_bounded_cancellable, CommandResult};

/// Fixed-message probe errors. Display/Debug text never embeds a raw
/// address, executable path, process output, or transport text.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GnomeProbeError {
    /// A stop was observed, or the runner reported cancellation.
    Cancelled,
    /// The caller's deadline expired while the probe child was running.
    Timeout,
    /// The probe context or executable could not be used.
    Unavailable,
    /// The probe connected, but GNOME readiness is not yet satisfied.
    NotReady,
    /// The probe terminated with an unrecognized status.
    Failed,
}

impl fmt::Display for GnomeProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Cancelled => "GNOME probe cancelled before completion",
            Self::Timeout => "GNOME probe timed out",
            Self::Unavailable => "GNOME probe context or executable unavailable",
            Self::NotReady => "GNOME is not ready",
            Self::Failed => "GNOME probe failed with an unrecognized status",
        };
        f.write_str(message)
    }
}

impl error::Error for GnomeProbeError {}

/// Map a runner result to the probe's typed outcome. A cancellation (the
/// runner's own flag, or a stop observed at this result checkpoint) wins
/// first, then the timeout, then the unavailable flag; after that the exit
/// status is classified by the exit-status readiness protocol.
fn map_probe_result(result: &CommandResult, stop: &AtomicBool) -> Result<(), GnomeProbeError> {
    if result.cancelled || stop.load(Ordering::SeqCst) {
        return Err(GnomeProbeError::Cancelled);
    }
    if result.timed_out {
        return Err(GnomeProbeError::Timeout);
    }
    if result.unavailable {
        return Err(GnomeProbeError::Unavailable);
    }
    match result.status.and_then(|status| status.code()) {
        Some(0) => Ok(()),
        Some(1) => Err(GnomeProbeError::Unavailable),
        Some(2) => Err(GnomeProbeError::Cancelled),
        Some(3) => Err(GnomeProbeError::NotReady),
        // Any other exit code, a signal termination, or a missing status.
        _ => Err(GnomeProbeError::Failed),
    }
}

/// Check GNOME readiness through an explicit probe process.
///
/// The adapter uses only its explicit context: it never reads or mutates
/// environment, chooses a bus or display, searches `PATH`, resolves or
/// installs the executable, or tries alternate binaries. A non-absolute
/// executable, or a missing/empty/unsupported/malformed bus address
/// (validated by `crate::is_valid_probe_address`), is rejected without
/// spawning. Ordinary child environment inheritance is allowed, but it does
/// not choose context: the child receives the bus address only through the
/// `--bus` argument, passed through unchanged. The cancellable runner owns
/// process-group termination and reaping before it returns.
pub(crate) fn check_gnome_readiness_with_probe(
    executable: &Path,
    bus_address: Option<&str>,
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<(), GnomeProbeError> {
    // A stop observed on entry wins and launches nothing.
    if stop.load(Ordering::SeqCst) {
        return Err(GnomeProbeError::Cancelled);
    }
    let Some(address) = bus_address else {
        // A stop observed before returning even a validation failure wins.
        if stop.load(Ordering::SeqCst) {
            return Err(GnomeProbeError::Cancelled);
        }
        return Err(GnomeProbeError::Unavailable);
    };
    if !executable.is_absolute() || !crate::is_valid_probe_address(address) {
        if stop.load(Ordering::SeqCst) {
            return Err(GnomeProbeError::Cancelled);
        }
        return Err(GnomeProbeError::Unavailable);
    }
    let mut command = Command::new(executable);
    command.args(["gnome-readiness-probe", "--bus", address]);
    let result = run_status_bounded_cancellable(command, timeout, stop);
    map_probe_result(&result, stop)
}

#[cfg(test)]
mod tests {
    use super::{check_gnome_readiness_with_probe, map_probe_result, GnomeProbeError};
    use crate::command::{run_status_bounded, CommandResult, COMMAND_TIMEOUT};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitStatus};
    use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    /// Unique test-owned fixture directory per (process, counter) so
    /// parallel test cases never collide; removed on drop even when a test
    /// assertion unwinds first.
    static FIXTURE_COUNTER: AtomicI64 = AtomicI64::new(0);

    struct ProbeFixture {
        dir: PathBuf,
    }

    fn probe_fixture(name: &str) -> ProbeFixture {
        let n = FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!(
            "/tmp/lg-buddy-probe-test-{}-{}-{name}",
            std::process::id(),
            n
        ));
        std::fs::create_dir(&dir).expect("create probe fixture directory");
        ProbeFixture { dir }
    }

    impl Drop for ProbeFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Install an executable script inside the fixture and return its path.
    fn install_script(fixture: &ProbeFixture, name: &str, script: &str) -> PathBuf {
        let path = fixture.dir.join(name);
        // Keep the writable script descriptor out of the multithreaded test
        // process. Concurrently spawned children could inherit it before exec
        // closes CLOEXEC descriptors, temporarily making the script ETXTBSY.
        // Reap the writer before executing the finished fixture.
        let mut writer = Command::new("/bin/sh");
        writer
            .args([
                "-c",
                "printf '%s' \"$1\" > \"$2\"",
                "fixture-writer",
                script,
            ])
            .arg(&path);
        assert!(
            run_status_bounded(writer, COMMAND_TIMEOUT).succeeded(),
            "write probe fixture script"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make probe fixture script executable");
        path
    }

    /// A probe child that records its argv and PID, then exits with `code`.
    fn exit_child(fixture: &ProbeFixture, code: u8) -> PathBuf {
        install_script(
            fixture,
            "probe.sh",
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv}\necho \"$$\" > {pid}\nexit {code}\n",
                argv = fixture.dir.join("argv").display(),
                pid = fixture.dir.join("pid").display(),
            ),
        )
    }

    const SUCCESS_ADDRESS: &str = "unix:path=/tmp/lg-buddy-probe-success.sock%2C%3B%FF";

    fn argv_lines(fixture: &ProbeFixture) -> Vec<String> {
        std::fs::read_to_string(fixture.dir.join("argv"))
            .expect("fixture records its argv")
            .lines()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn successful_probe_records_exact_argv_and_succeeds() {
        let fixture = probe_fixture("success");
        let executable = exit_child(&fixture, 0);
        let stop = AtomicBool::new(false);

        let result = check_gnome_readiness_with_probe(
            &executable,
            Some(SUCCESS_ADDRESS),
            COMMAND_TIMEOUT,
            &stop,
        );
        assert_eq!(result, Ok(()));

        let argv = argv_lines(&fixture);
        assert_eq!(argv.len(), 3);
        assert_eq!(argv[0], "gnome-readiness-probe");
        assert_eq!(argv[1], "--bus");
        // The percent-escaped address reaches the child byte-for-byte.
        assert_eq!(argv[2], SUCCESS_ADDRESS);
        // A recorded PID proves a real process ran, which a no-op adapter
        // cannot produce.
        let pid =
            std::fs::read_to_string(fixture.dir.join("pid")).expect("fixture records its PID");
        assert!(pid.trim().parse::<u32>().is_ok(), "PID record: {pid:?}");
    }

    #[test]
    fn successful_probe_forwards_an_abstract_address_unchanged() {
        let fixture = probe_fixture("success-abstract");
        let executable = exit_child(&fixture, 0);
        let address = "unix:abstract=%41%42";
        let stop = AtomicBool::new(false);

        let result =
            check_gnome_readiness_with_probe(&executable, Some(address), COMMAND_TIMEOUT, &stop);
        assert_eq!(result, Ok(()));
        let argv = argv_lines(&fixture);
        assert_eq!(argv.len(), 3);
        assert_eq!(argv[2], address);
    }

    #[test]
    fn exit_codes_map_to_their_typed_errors() {
        let cases = [
            (1u8, GnomeProbeError::Unavailable),
            (2u8, GnomeProbeError::Cancelled),
            (3u8, GnomeProbeError::NotReady),
            (7u8, GnomeProbeError::Failed),
        ];
        for (code, expected) in cases {
            let fixture = probe_fixture(&format!("exit-{code}"));
            let executable = exit_child(&fixture, code);
            let stop = AtomicBool::new(false);
            let result = check_gnome_readiness_with_probe(
                &executable,
                Some("unix:abstract=exit-code-probe"),
                COMMAND_TIMEOUT,
                &stop,
            );
            assert_eq!(result, Err(expected), "exit code {code}");
            assert!(
                fixture.dir.join("pid").exists(),
                "exit code {code} fixture must have run"
            );
        }
    }

    #[test]
    fn signal_terminated_probe_is_failed() {
        let fixture = probe_fixture("signal");
        let executable = install_script(
            &fixture,
            "probe.sh",
            &format!(
                "#!/bin/sh\nprintf '%s\\n%s\\n%s\\n' \"$1\" \"$2\" \"$3\" > {argv}\nkill -TERM $$\n",
                argv = fixture.dir.join("argv").display()
            ),
        );
        let stop = AtomicBool::new(false);
        let result = check_gnome_readiness_with_probe(
            &executable,
            Some("unix:abstract=signal-probe"),
            COMMAND_TIMEOUT,
            &stop,
        );
        assert_eq!(result, Err(GnomeProbeError::Failed));
        assert!(fixture.dir.join("argv").exists(), "fixture must have run");
    }

    #[test]
    fn missing_absolute_executable_is_unavailable_without_spawn() {
        let fixture = probe_fixture("missing-exec");
        let executable = fixture.dir.join("never-installed");
        let stop = AtomicBool::new(false);
        let result = check_gnome_readiness_with_probe(
            &executable,
            Some("unix:abstract=missing-exec"),
            COMMAND_TIMEOUT,
            &stop,
        );
        assert_eq!(result, Err(GnomeProbeError::Unavailable));
    }

    #[test]
    fn rejected_context_is_unavailable_without_spawning() {
        let fixture = probe_fixture("rejected-context");
        let marker = fixture.dir.join("ran");
        let executable = install_script(
            &fixture,
            "probe.sh",
            &format!("#!/bin/sh\ntouch {}\nexit 0\n", marker.display()),
        );
        // Resolve to the same real executable through a relative path without
        // changing the process working directory. A missing binary would make
        // this test pass even if the adapter forgot its absolute-path guard.
        let cwd = std::env::current_dir().unwrap();
        let mut relative_executable = PathBuf::new();
        for _ in cwd.ancestors().skip(1) {
            relative_executable.push("..");
        }
        relative_executable.push(executable.strip_prefix("/").unwrap());
        assert!(relative_executable.is_file());
        assert!(!relative_executable.is_absolute());

        for (executable, address) in [
            (
                relative_executable.as_path(),
                Some("unix:abstract=relative-exec"),
            ),
            (executable.as_path(), None),
            (executable.as_path(), Some("")),
            (executable.as_path(), Some("autolaunch:")),
            (executable.as_path(), Some("unixexec:/usr/bin/fake-busd")),
            (
                executable.as_path(),
                Some("unix:path=/tmp/a.sock;unix:path=/tmp/b.sock"),
            ),
        ] {
            let stop = AtomicBool::new(false);
            let result =
                check_gnome_readiness_with_probe(executable, address, COMMAND_TIMEOUT, &stop);
            assert_eq!(
                result,
                Err(GnomeProbeError::Unavailable),
                "{executable:?} {address:?}"
            );
            assert!(!marker.exists(), "a rejected context must spawn nothing");
        }
    }

    #[test]
    fn pre_cancel_launches_nothing() {
        let fixture = probe_fixture("pre-cancel");
        let marker = fixture.dir.join("ran");
        let executable = install_script(
            &fixture,
            "probe.sh",
            &format!(
                "#!/bin/sh\ntouch {marker}\nexit 0\n",
                marker = marker.display()
            ),
        );
        let stop = AtomicBool::new(true);
        let result = check_gnome_readiness_with_probe(
            &executable,
            Some("unix:abstract=pre-cancel"),
            COMMAND_TIMEOUT,
            &stop,
        );
        assert_eq!(result, Err(GnomeProbeError::Cancelled));
        assert!(
            !marker.exists(),
            "a pre-cancelled probe must launch nothing"
        );
    }

    fn synthetic(
        status: Option<ExitStatus>,
        timed_out: bool,
        unavailable: bool,
        cancelled: bool,
    ) -> CommandResult {
        CommandResult {
            status,
            timed_out,
            unavailable,
            cancelled,
            ..CommandResult::default()
        }
    }

    #[test]
    fn result_mapping_covers_precedence_and_missing_status() {
        let quiet = AtomicBool::new(false);
        let cancelled_flag = AtomicBool::new(true);
        let cases = [
            (
                synthetic(Some(ExitStatus::from_raw(0)), false, false, false),
                &quiet,
                Ok(()),
            ),
            (
                synthetic(Some(ExitStatus::from_raw(1 << 8)), false, false, false),
                &quiet,
                Err(GnomeProbeError::Unavailable),
            ),
            (
                synthetic(Some(ExitStatus::from_raw(2 << 8)), false, false, false),
                &quiet,
                Err(GnomeProbeError::Cancelled),
            ),
            (
                synthetic(Some(ExitStatus::from_raw(3 << 8)), false, false, false),
                &quiet,
                Err(GnomeProbeError::NotReady),
            ),
            (
                synthetic(Some(ExitStatus::from_raw(7 << 8)), false, false, false),
                &quiet,
                Err(GnomeProbeError::Failed),
            ),
            // A signal termination has no exit code.
            (
                synthetic(
                    Some(ExitStatus::from_raw(libc::SIGTERM)),
                    false,
                    false,
                    false,
                ),
                &quiet,
                Err(GnomeProbeError::Failed),
            ),
            // A missing status without any overriding flag is a failed probe.
            (
                synthetic(None, false, false, false),
                &quiet,
                Err(GnomeProbeError::Failed),
            ),
            // An ordinary exit 0 must never be accepted after an observed
            // cancellation.
            (
                synthetic(Some(ExitStatus::from_raw(0)), false, false, true),
                &quiet,
                Err(GnomeProbeError::Cancelled),
            ),
            (
                synthetic(Some(ExitStatus::from_raw(0)), false, false, false),
                &cancelled_flag,
                Err(GnomeProbeError::Cancelled),
            ),
            // Cancellation also wins over a competing timeout flag.
            (
                synthetic(Some(ExitStatus::from_raw(libc::SIGKILL)), true, false, true),
                &quiet,
                Err(GnomeProbeError::Cancelled),
            ),
            (
                synthetic(
                    Some(ExitStatus::from_raw(libc::SIGKILL)),
                    true,
                    false,
                    false,
                ),
                &quiet,
                Err(GnomeProbeError::Timeout),
            ),
            (
                synthetic(None, false, true, false),
                &quiet,
                Err(GnomeProbeError::Unavailable),
            ),
        ];
        for (result, stop, expected) in cases {
            assert_eq!(map_probe_result(&result, stop), expected);
        }
    }

    /// A probe child that signals ready (marker plus PID record) and then
    /// waits on a 10 s descendant as a safety bound; the runner's deadline
    /// or cancellation must stop the group before that bound.
    fn blocking_child(fixture: &ProbeFixture, ready: &Path, pidfile: &Path) -> PathBuf {
        install_script(
            fixture,
            "probe.sh",
            &format!(
                "#!/bin/sh\nsleep 10 &\np=$!\nprintf '%s\\n%s\\n' \"$$\" \"$p\" > {pidfile}\ntouch {ready}\nwait $p\n",
                pidfile = pidfile.display(),
                ready = ready.display(),
            ),
        )
    }

    /// True once a descendant PID is no longer running (its `/proc` entry is
    /// gone, or it is a zombie awaiting init).
    fn descendant_stopped(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.split_whitespace().next() == Some("Z")),
            Err(_) => true,
        }
    }

    /// The direct child must already be reaped (a WNOHANG waitpid on it
    /// yields ECHILD) and the descendant must be stopped by the group
    /// termination within a bounded deadline.
    fn assert_child_reaped_and_descendant_stopped(pidfile: &Path) {
        let pids: Vec<u32> = std::fs::read_to_string(pidfile)
            .expect("fixture publishes its PIDs")
            .lines()
            .map(|line| line.parse().expect("each line is a PID"))
            .collect();
        let mut code: libc::c_int = 0;
        let waited = unsafe { libc::waitpid(pids[0] as i32, &mut code, libc::WNOHANG) };
        assert_eq!(waited, -1, "the direct child must already be reaped");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while !descendant_stopped(pids[1]) {
            assert!(
                Instant::now() < deadline,
                "descendant survived the group termination"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn blocking_probe_hits_the_caller_timeout_and_reaps_the_child() {
        let fixture = probe_fixture("timeout");
        let ready = fixture.dir.join("ready");
        let pidfile = fixture.dir.join("pids");
        let executable = blocking_child(&fixture, &ready, &pidfile);
        let stop = AtomicBool::new(false);
        let started = Instant::now();

        let result = check_gnome_readiness_with_probe(
            &executable,
            Some("unix:abstract=timeout-probe"),
            Duration::from_millis(100),
            &stop,
        );

        assert_eq!(result, Err(GnomeProbeError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_child_reaped_and_descendant_stopped(&pidfile);
    }

    #[test]
    fn cancellation_after_ready_stops_the_probe_promptly() {
        let fixture = probe_fixture("cancel");
        let ready = fixture.dir.join("ready");
        let pidfile = fixture.dir.join("pids");
        let executable = blocking_child(&fixture, &ready, &pidfile);
        let stop = Arc::new(AtomicBool::new(false));
        let stopper = thread::spawn({
            let ready = ready.clone();
            let stop = stop.clone();
            move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !ready.exists() {
                    assert!(Instant::now() < deadline, "fixture never signalled ready");
                    thread::sleep(Duration::from_millis(5));
                }
                stop.store(true, Ordering::SeqCst);
            }
        });
        let started = Instant::now();

        let result = check_gnome_readiness_with_probe(
            &executable,
            Some("unix:abstract=cancel-probe"),
            Duration::from_secs(5),
            &stop,
        );

        stopper.join().expect("stopper must not panic");
        assert_eq!(result, Err(GnomeProbeError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_child_reaped_and_descendant_stopped(&pidfile);
    }

    #[test]
    fn error_text_exposes_no_context() {
        let samples = [
            "unix:path=/tmp/lg-buddy-secret.sock",
            "/tmp/lg-buddy-probe.sh",
        ];
        for error in [
            GnomeProbeError::Cancelled,
            GnomeProbeError::Timeout,
            GnomeProbeError::Unavailable,
            GnomeProbeError::NotReady,
            GnomeProbeError::Failed,
        ] {
            let display = error.to_string();
            let debug = format!("{error:?}");
            for sample in &samples {
                assert!(!display.contains(sample), "{display}");
                assert!(!debug.contains(sample), "{debug}");
            }
        }
    }
}
