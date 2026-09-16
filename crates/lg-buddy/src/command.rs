//! Bounded capture for read-only subprocesses. Mutating commands use their own lifecycle.

use std::env;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
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
}

impl CommandResult {
    pub fn succeeded(&self) -> bool {
        self.status.is_some_and(|status| status.success())
    }

    pub fn stopped_at_output_limit(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            self.truncated && self.status.and_then(|status| status.signal()) == Some(libc::SIGPIPE)
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
    run_command(command, timeout, true)
}

pub(crate) fn run_status_bounded(command: Command, timeout: Duration) -> CommandResult {
    run_command(command, timeout, false)
}

fn run_command(mut command: Command, timeout: Duration, capture_output: bool) -> CommandResult {
    use std::os::unix::process::CommandExt;
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
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
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
    }
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
}
