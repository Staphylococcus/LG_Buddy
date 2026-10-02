//! Bounded capture for read-only subprocesses, plus a cancellable status-only
//! runner. Mutating commands use their own lifecycle.

use std::env;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

pub(crate) const MAX_COMMAND_BYTES: usize = 16 * 1024;
pub(crate) const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(crate) struct CommandResult {
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
    pub unavailable: bool,
    pub truncated: bool,
    pub cancelled: bool,
}

impl CommandResult {
    /// Classify a single observed stop consistently, including a stop racing
    /// with spawn failure, timeout cleanup, or successful child reaping.
    fn with_observed_stop(mut self, stopped: bool) -> Self {
        self.cancelled |= stopped;
        if self.cancelled {
            self.timed_out = false;
            self.unavailable = false;
        }
        self
    }

    pub fn succeeded(&self) -> bool {
        !self.cancelled
            && !self.timed_out
            && !self.unavailable
            && self.status.is_some_and(|status| status.success())
    }

    pub fn stopped_at_output_limit(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            !self.cancelled
                && self.truncated
                && self.status.and_then(|status| status.signal()) == Some(libc::SIGPIPE)
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

pub(crate) fn command_path(override_name: &str, fallback: &str) -> PathBuf {
    env::var_os(override_name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

pub(crate) fn run_bounded(program: &Path, args: &[&str], timeout: Duration) -> CommandResult {
    let mut command = Command::new(program);
    command.args(args);
    run_bounded_command(command, timeout)
}

pub(crate) fn run_bounded_command(command: Command, timeout: Duration) -> CommandResult {
    run_command(command, timeout, true, None)
}

pub(crate) fn run_status_bounded(command: Command, timeout: Duration) -> CommandResult {
    run_command(command, timeout, false, None)
}

/// Status-only runner with caller cancellation. stdout/stderr are discarded,
/// so there are no capture readers to cancel. `stop` is monotonic
/// false-to-true; an observed stop at a checkpoint wins over timeout,
/// spawn failure, or a terminal status. On cancellation or timeout while the
/// child is still owned, the runner kills its process group and reaps the
/// direct child. It never reaps descendants or signals an already-reaped PID.
pub(crate) fn run_status_bounded_cancellable(
    command: Command,
    timeout: Duration,
    stop: &AtomicBool,
) -> CommandResult {
    run_command(command, timeout, false, Some(stop))
}

fn run_command(
    mut command: Command,
    timeout: Duration,
    capture_output: bool,
    stop: Option<&AtomicBool>,
) -> CommandResult {
    use std::os::unix::process::CommandExt;
    let stop_requested = || stop.is_some_and(|flag| flag.load(Ordering::SeqCst));
    // A pre-spawn stop means zero processes were launched.
    if stop_requested() {
        return CommandResult {
            cancelled: true,
            ..CommandResult::default()
        };
    }
    let mut child = match command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(if capture_output {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(if capture_output {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            return CommandResult {
                unavailable: true,
                ..CommandResult::default()
            }
            .with_observed_stop(stop_requested());
        }
    };

    let deadline = Instant::now() + timeout;
    let stdout_reader = child
        .stdout
        .take()
        .map(|stdout| thread::spawn(move || read_bounded_pipe(stdout, deadline, true)));
    let stderr_reader = child
        .stderr
        .take()
        .map(|stderr| thread::spawn(move || read_bounded_pipe(stderr, deadline, false)));
    let mut timed_out = false;
    let mut cancelled = false;
    let status = loop {
        // A stop observed after spawn but before any wait still kills and
        // reaps the child, so it is reported as a cancellation, not a
        // normal exit.
        if stop_requested() {
            cancelled = true;
            break terminate_owned_child(&mut child);
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                break terminate_owned_child(&mut child);
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                break terminate_owned_child(&mut child);
            }
        }
    };
    // Both readers use the same absolute deadline as the child. In
    // particular, a descendant that inherited either pipe cannot make a join
    // wait beyond the collection timeout.
    let mut stdout = stdout_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    let truncated = stdout.len() > MAX_COMMAND_BYTES;
    stdout.truncate(MAX_COMMAND_BYTES);
    let mut stderr = stderr_reader
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    stderr.truncate(MAX_COMMAND_BYTES);
    CommandResult {
        status,
        stdout,
        stderr,
        timed_out,
        unavailable: false,
        truncated,
        cancelled,
    }
    // Observe stop once at the return boundary. The child is already reaped,
    // so cancellation changes the result without signalling it again.
    .with_observed_stop(stop_requested())
}

/// Kill the owned (not yet reaped) child's dedicated process group, fall
/// back to killing the direct child, then reap the direct child. Returns
/// the exit status when the reap succeeds; a failed wait is `None` and the
/// waiter is never detached.
#[cfg(unix)]
fn terminate_owned_child(child: &mut Child) -> Option<ExitStatus> {
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    child.wait().ok()
}

fn read_bounded_pipe(
    mut pipe: impl Read + AsRawFd,
    deadline: Instant,
    close_at_limit: bool,
) -> Vec<u8> {
    #[cfg(unix)]
    {
        let fd = pipe.as_raw_fd();
        // A nonblocking descriptor lets this reader honor the deadline even
        // after the direct child exits while a descendant retains the pipe.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Vec::new();
        }

        let mut output = Vec::new();
        let mut buffer = [0_u8; 1024];
        // Close stdout at the limit to retain the journal truncation behavior.
        // Drain excess stderr so noisy diagnostics cannot change the exit code.
        while Instant::now() < deadline && (!close_at_limit || output.len() < MAX_COMMAND_BYTES + 1)
        {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let remaining = MAX_COMMAND_BYTES + 1 - output.len();
                    output.extend_from_slice(&buffer[..read.min(remaining)]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
        output
    }

    #[cfg(not(unix))]
    {
        // LG Buddy's supported hosts are Unix. Keep a capped fallback for
        // other targets; the child is still terminated by the parent deadline.
        let mut limited = pipe.take((MAX_COMMAND_BYTES + 1) as u64);
        let mut output = Vec::new();
        let _ = limited.read_to_end(&mut output);
        output
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
    use std::sync::Arc;

    /// Unique temporary marker path, derived from the test process PID plus a
    /// monotonic counter so parallel test cases never collide.
    static MARKER_COUNTER: AtomicI64 = AtomicI64::new(0);
    fn unique_marker(label: &str) -> PathBuf {
        let n = MARKER_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        PathBuf::from(format!(
            "/tmp/lg-buddy-cmd-test-{}-{}-{}",
            std::process::id(),
            n,
            label
        ))
    }

    /// True once a descendant PID is no longer running (its `/proc` entry is
    /// gone, or it is a zombie awaiting init). A descendant is a grandchild of
    /// this test process, so it is reaped by init, not by us.
    fn descendant_stopped(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat
                .rsplit_once(") ")
                .is_some_and(|(_, rest)| rest.split_whitespace().next() == Some("Z")),
            Err(_) => true,
        }
    }

    #[test]
    fn output_limit_retains_bytes_and_the_termination_reason() {
        let result = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "exec head -c 1048576 /dev/zero"],
            COMMAND_TIMEOUT,
        );
        assert_eq!(result.stdout.len(), MAX_COMMAND_BYTES);
        assert!(result.truncated);
        assert!(!result.timed_out);
        assert!(result.stopped_at_output_limit(), "{result:?}");
    }

    #[test]
    fn only_sigpipe_after_truncation_is_an_output_limit_exit() {
        for (truncated, status, expected) in [
            (true, libc::SIGPIPE, true),
            (false, libc::SIGPIPE, false),
            (true, 1 << 8, false),
            (true, libc::SIGTERM, false),
        ] {
            let result = CommandResult {
                truncated,
                status: Some(ExitStatus::from_raw(status)),
                ..CommandResult::default()
            };
            assert_eq!(result.stopped_at_output_limit(), expected);
        }
    }

    #[test]
    fn successful_output_and_command_failure_are_distinct() {
        let result = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "printf 'state\\n'"],
            COMMAND_TIMEOUT,
        );
        assert!(result.succeeded());
        assert_eq!(result.stdout, b"state\n");
        assert!(!result.truncated);
        let failed = run_bounded(Path::new("/bin/sh"), &["-c", "exit 7"], COMMAND_TIMEOUT);
        assert_eq!(failed.status.unwrap().code(), Some(7));
        assert!(!failed.succeeded());
        let missing = run_bounded(Path::new("/dev/null/missing-command"), &[], COMMAND_TIMEOUT);
        assert!(missing.unavailable);
    }

    #[test]
    fn status_only_queries_discard_output_without_terminating_the_command() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2; exit 7",
        ]);
        let result = run_status_bounded(command, COMMAND_TIMEOUT);
        assert_eq!(result.status.unwrap().code(), Some(7));
        assert!(result.stdout.is_empty());
        assert!(result.stderr.is_empty());
        assert!(!result.truncated);
    }

    #[test]
    fn stderr_is_capped_and_drained_without_changing_the_exit_status() {
        let result = run_bounded(
            Path::new("/bin/sh"),
            &[
                "-c",
                "printf 'state\\n'; head -c 1048576 /dev/zero >&2; exit 7",
            ],
            COMMAND_TIMEOUT,
        );
        assert_eq!(result.status.unwrap().code(), Some(7));
        assert_eq!(result.stdout, b"state\n");
        assert_eq!(result.stderr, vec![0; MAX_COMMAND_BYTES]);
        assert!(!result.truncated);
        assert!(!result.timed_out);
        assert!(!result.stopped_at_output_limit());
    }

    #[test]
    fn deadline_stops_the_child() {
        let started = Instant::now();
        let result = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "printf 'before timeout' >&2; exec sleep 30"],
            Duration::from_millis(100),
        );
        assert!(result.timed_out);
        assert_eq!(result.stderr, b"before timeout");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn deadline_stops_descendants_of_a_waiting_helper() {
        let result = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "sleep 30 & echo $!; wait"],
            Duration::from_millis(100),
        );
        assert!(result.timed_out);
        let pid: u32 = String::from_utf8(result.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            if stat.rsplit_once(") ").unwrap().1.split_whitespace().next() == Some("Z") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "helper descendant is still running: {stat}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn deadline_also_bounds_a_descendant_holding_both_output_pipes() {
        let started = Instant::now();
        let result = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "printf 'out'; printf 'err' >&2; (sleep 1) & exit 0"],
            Duration::from_millis(100),
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(result.succeeded());
        assert_eq!(result.stdout, b"out");
        assert_eq!(result.stderr, b"err");
    }

    /// Removes a test marker even when a test assertion unwinds first.
    struct MarkerGuard(PathBuf);
    impl Drop for MarkerGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// The helper spawns a descendant, records its own PID and the
    /// descendant's PID, signals ready, and waits on the descendant, so a
    /// group-level termination must stop the descendant too. The descendant's
    /// natural lifetime (10 s) outlives the runner's 5 s timeout, so a
    /// stubbed-out cancellation still terminates via the timeout cleanup and
    /// reaps the group: no unbounded orphan.
    fn running_cancel_fixture(stop_delay: Duration) -> (CommandResult, u32, u32, Instant) {
        let ready = unique_marker("cancel-ready");
        let pidfile = unique_marker("cancel-pids");
        let _ready_guard = MarkerGuard(ready.clone());
        let _pidfile_guard = MarkerGuard(pidfile.clone());
        let stop = Arc::new(AtomicBool::new(false));
        let stopper = thread::spawn({
            let ready = ready.clone();
            let stop = stop.clone();
            move || {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !ready.exists() {
                    assert!(Instant::now() < deadline, "helper never signalled ready");
                    thread::sleep(Duration::from_millis(5));
                }
                thread::sleep(stop_delay);
                stop.store(true, Ordering::SeqCst);
            }
        });
        let started = Instant::now();
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!(
                "sleep 10 & p=$!; printf '%s\\n%s\\n' \"$$\" \"$p\" > {pid}; touch {ready}; wait $p",
                pid = pidfile.display(),
                ready = ready.display(),
            ),
        ]);
        let result = run_status_bounded_cancellable(command, Duration::from_secs(5), &stop);
        stopper.join().expect("stopper must not panic");
        let pids: Vec<u32> = std::fs::read_to_string(&pidfile)
            .expect("helper must publish its PIDs")
            .lines()
            .map(|line| line.parse().expect("each line is a PID"))
            .collect();
        (result, pids[0], pids[1], started)
    }

    /// Cancellation invariants for the running-helper fixture: prompt return,
    /// killed status, the direct child already reaped (a WNOHANG waitpid on
    /// it yields ECHILD; a fake immediate cancelled return that skipped
    /// kill+reap leaves a live or zombie child and fails this check), and the
    /// descendant stopped. The descendant is a grandchild of this test
    /// process, so init reaps it; group termination is what stopped it.
    fn assert_group_cancelled(
        result: &CommandResult,
        child_pid: u32,
        desc_pid: u32,
        started: Instant,
    ) {
        assert!(result.cancelled, "{result:?}");
        assert!(!result.timed_out && !result.unavailable, "{result:?}");
        assert!(!result.succeeded());
        let status = result
            .status
            .expect("the owned child must be reaped before returning");
        assert_eq!(status.signal(), Some(libc::SIGKILL), "{result:?}");
        let mut code: libc::c_int = 0;
        let waited = unsafe { libc::waitpid(child_pid as i32, &mut code, libc::WNOHANG) };
        assert_eq!(waited, -1, "direct child must already be reaped");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while !descendant_stopped(desc_pid) {
            assert!(
                Instant::now() < deadline,
                "descendant survived the group termination"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn pre_cancel_before_spawn_launches_nothing() {
        let marker = unique_marker("precancel");
        let _marker_guard = MarkerGuard(marker.clone());
        let stop = AtomicBool::new(true);
        let mut command = Command::new("/bin/sh");
        command.args(["-c", &format!("touch {}", marker.display())]);
        let result = run_status_bounded_cancellable(command, COMMAND_TIMEOUT, &stop);
        assert!(result.cancelled, "{result:?}");
        assert!(result.status.is_none());
        assert!(!result.succeeded());
        assert!(!result.timed_out && !result.unavailable);
        assert!(
            !marker.exists(),
            "a pre-cancelled command must launch nothing"
        );
    }

    #[test]
    fn pre_cancel_wins_over_a_missing_command() {
        let stop = AtomicBool::new(true);
        let command = Command::new("/dev/null/missing-command");
        let result = run_status_bounded_cancellable(command, COMMAND_TIMEOUT, &stop);
        assert!(result.cancelled, "{result:?}");
        assert!(!result.unavailable, "{result:?}");
        assert!(result.status.is_none());
    }

    #[test]
    fn cancel_after_spawn_kills_the_group_and_reaps_the_child() {
        let (result, child_pid, desc_pid, started) = running_cancel_fixture(Duration::ZERO);
        assert_group_cancelled(&result, child_pid, desc_pid, started);
    }

    #[test]
    fn cancel_during_polling_kills_the_group_and_reaps_the_child() {
        let (result, child_pid, desc_pid, started) =
            running_cancel_fixture(Duration::from_millis(100));
        assert_group_cancelled(&result, child_pid, desc_pid, started);
    }

    #[test]
    fn stop_observed_at_return_overrides_success_timeout_and_spawn_failure() {
        // Exercise the actual return classifier deterministically: arranging
        // a stop with sleeps cannot establish that it raced with child reaping.
        for (timed_out, unavailable, raw_status) in [
            (false, false, Some(0)),
            (true, false, Some(libc::SIGKILL)),
            (false, true, None),
        ] {
            let result = CommandResult {
                status: raw_status.map(ExitStatus::from_raw),
                timed_out,
                unavailable,
                ..CommandResult::default()
            }
            .with_observed_stop(true);
            assert!(result.cancelled, "{result:?}");
            assert!(!result.timed_out && !result.unavailable, "{result:?}");
            assert!(!result.succeeded());
            assert_eq!(result.status.map(|status| status.into_raw()), raw_status);
        }
    }

    #[test]
    fn stop_false_keeps_success_failure_unavailable_and_timeout_distinct_from_cancel() {
        let stop = AtomicBool::new(false);
        let mut ok_command = Command::new("/bin/sh");
        ok_command.args(["-c", "exit 0"]);
        let ok = run_status_bounded_cancellable(ok_command, COMMAND_TIMEOUT, &stop);
        assert!(ok.succeeded(), "{ok:?}");
        assert!(!ok.cancelled);
        let mut failed_command = Command::new("/bin/sh");
        failed_command.args(["-c", "exit 7"]);
        let failed = run_status_bounded_cancellable(failed_command, COMMAND_TIMEOUT, &stop);
        assert_eq!(failed.status.unwrap().code(), Some(7));
        assert!(!failed.succeeded());
        assert!(!failed.cancelled);
        let missing_command = Command::new("/dev/null/missing-command");
        let missing = run_status_bounded_cancellable(missing_command, COMMAND_TIMEOUT, &stop);
        assert!(missing.unavailable, "{missing:?}");
        assert!(!missing.cancelled);
        let started = Instant::now();
        let mut late_command = Command::new("/bin/sh");
        late_command.args(["-c", "exec sleep 30"]);
        let late = run_status_bounded_cancellable(late_command, Duration::from_millis(100), &stop);
        assert!(late.timed_out, "{late:?}");
        assert!(!late.cancelled);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn exit_zero_is_never_successful_when_cancelled_timed_out_or_unavailable() {
        let cases = [
            CommandResult {
                status: Some(ExitStatus::from_raw(0)),
                ..CommandResult::default()
            },
            CommandResult {
                status: Some(ExitStatus::from_raw(0)),
                cancelled: true,
                ..CommandResult::default()
            },
            CommandResult {
                status: Some(ExitStatus::from_raw(0)),
                timed_out: true,
                ..CommandResult::default()
            },
            CommandResult {
                status: Some(ExitStatus::from_raw(0)),
                unavailable: true,
                ..CommandResult::default()
            },
        ];
        for (index, result) in cases.iter().enumerate() {
            if index == 0 {
                assert!(result.succeeded(), "{result:?}");
                continue;
            }
            assert!(!result.succeeded(), "{result:?}");
            assert!(!result.stopped_at_output_limit(), "{result:?}");
        }
        let mut truncated = CommandResult {
            status: Some(ExitStatus::from_raw(libc::SIGPIPE)),
            truncated: true,
            ..CommandResult::default()
        };
        assert_eq!(truncated.status.unwrap().signal(), Some(libc::SIGPIPE));
        assert!(truncated.stopped_at_output_limit());
        truncated.cancelled = true;
        assert!(!truncated.stopped_at_output_limit());
    }
}
