//! Bounded subprocess capture shared by service and journal collectors.

use std::env;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub(super) const MAX_COMMAND_BYTES: usize = 16 * 1024;
pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(super) struct CommandResult {
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
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

pub(super) fn command_path(override_name: &str, fallback: &str) -> PathBuf {
    env::var_os(override_name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

pub(super) fn run_bounded(program: &Path, args: &[&str], timeout: Duration) -> CommandResult {
    let mut child = match Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
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
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = thread::spawn(move || read_bounded_stdout(stdout, deadline));
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
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
    // The reader uses the same absolute deadline as the child. In
    // particular, a descendant that inherited stdout cannot make this join
    // wait beyond the collection timeout.
    let mut stdout = reader.join().unwrap_or_default();
    let truncated = stdout.len() > MAX_COMMAND_BYTES;
    stdout.truncate(MAX_COMMAND_BYTES);
    CommandResult {
        status,
        stdout,
        timed_out,
        unavailable: false,
        truncated,
    }
}

fn read_bounded_stdout(mut stdout: ChildStdout, deadline: Instant) -> Vec<u8> {
    #[cfg(unix)]
    {
        let fd = stdout.as_raw_fd();
        // A nonblocking descriptor lets this reader honor the deadline even
        // after the direct child exits while a descendant retains the pipe.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return Vec::new();
        }

        let mut output = Vec::new();
        let mut buffer = [0_u8; 1024];
        while output.len() < MAX_COMMAND_BYTES + 1 {
            match stdout.read(&mut buffer) {
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
        let mut limited = stdout.take((MAX_COMMAND_BYTES + 1) as u64);
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
    fn deadline_stops_the_child() {
        let started = Instant::now();
        let result = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "exec sleep 30"],
            Duration::from_millis(100),
        );
        assert!(result.timed_out);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn deadline_also_bounds_a_descendant_holding_stdout() {
        let started = Instant::now();
        let result = run_bounded(
            Path::new("/bin/sh"),
            &["-c", "(sleep 1) & exit 0"],
            Duration::from_millis(100),
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(result.succeeded());
    }
}
