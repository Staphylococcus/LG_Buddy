use std::error::Error;
use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::HdmiInput;

pub const DEFAULT_BSCPYLGTV_COMMAND_PATH: &str = "/usr/bin/LG_Buddy_PIP/bin/bscpylgtvcommand";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    stdout: String,
    stderr: String,
}

impl CommandOutput {
    pub fn new(stdout: String, stderr: String) -> Self {
        Self { stdout, stderr }
    }

    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    pub fn stderr(&self) -> &str {
        &self.stderr
    }

    pub fn combined_output(&self) -> String {
        match (self.stdout.is_empty(), self.stderr.is_empty()) {
            (false, false) => format!("{}{}", self.stdout, self.stderr),
            (false, true) => self.stdout.clone(),
            (true, false) => self.stderr.clone(),
            (true, true) => String::new(),
        }
    }
}

#[derive(Debug)]
pub enum TvError {
    Io {
        command: &'static str,
        source: io::Error,
    },
    CommandFailed {
        command: &'static str,
        status: Option<i32>,
        output: CommandOutput,
    },
    InvalidOutput {
        command: &'static str,
        output: CommandOutput,
        message: &'static str,
    },
}

impl fmt::Display for TvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { command, source } => {
                write!(f, "failed to run `{command}`: {source}")
            }
            Self::CommandFailed {
                command,
                status,
                output,
            } => {
                write!(
                    f,
                    "`{command}` failed with status {}",
                    status
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "terminated by signal".to_string())
                )?;

                let combined = output.combined_output();
                if !combined.trim().is_empty() {
                    write!(f, ": {}", combined.trim_end())?;
                }

                Ok(())
            }
            Self::InvalidOutput {
                command,
                output,
                message,
            } => {
                write!(f, "invalid output from `{command}`: {message}")?;
                let combined = output.combined_output();
                if !combined.trim().is_empty() {
                    write!(f, ": {}", combined.trim_end())?;
                }
                Ok(())
            }
        }
    }
}

impl Error for TvError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::CommandFailed { .. } | Self::InvalidOutput { .. } => None,
        }
    }
}

pub trait TvClient {
    fn get_input(&self, tv_ip: Ipv4Addr) -> Result<String, TvError>;
    fn set_input(&self, tv_ip: Ipv4Addr, input: HdmiInput) -> Result<CommandOutput, TvError>;
    fn power_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
    fn turn_screen_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
    fn turn_screen_on(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BscpylgtvCommandClient {
    command_path: PathBuf,
    command_args: Vec<String>,
}

impl Default for BscpylgtvCommandClient {
    fn default() -> Self {
        Self::new(DEFAULT_BSCPYLGTV_COMMAND_PATH)
    }
}

impl BscpylgtvCommandClient {
    pub fn new(command_path: impl Into<PathBuf>) -> Self {
        Self {
            command_path: command_path.into(),
            command_args: Vec::new(),
        }
    }

    pub fn with_args<I, S>(command_path: impl Into<PathBuf>, command_args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            command_path: command_path.into(),
            command_args: command_args.into_iter().map(Into::into).collect(),
        }
    }

    pub fn command_path(&self) -> &Path {
        &self.command_path
    }

    pub fn command_args(&self) -> &[String] {
        &self.command_args
    }

    fn run_command(
        &self,
        tv_ip: Ipv4Addr,
        operation: &'static str,
        extra_args: &[&str],
    ) -> Result<CommandOutput, TvError> {
        let output = Command::new(&self.command_path)
            .args(&self.command_args)
            .arg(tv_ip.to_string())
            .arg(operation)
            .args(extra_args)
            .output()
            .map_err(|source| TvError::Io {
                command: operation,
                source,
            })?;

        let rendered = CommandOutput::new(
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        );

        if output.status.success() {
            Ok(rendered)
        } else {
            Err(TvError::CommandFailed {
                command: operation,
                status: output.status.code(),
                output: rendered,
            })
        }
    }
}

impl TvClient for BscpylgtvCommandClient {
    fn get_input(&self, tv_ip: Ipv4Addr) -> Result<String, TvError> {
        let output = self.run_command(tv_ip, "get_input", &[])?;
        last_non_empty_line(output.stdout()).ok_or(TvError::InvalidOutput {
            command: "get_input",
            output,
            message: "expected a non-empty line in stdout",
        })
    }

    fn set_input(&self, tv_ip: Ipv4Addr, input: HdmiInput) -> Result<CommandOutput, TvError> {
        self.run_command(tv_ip, "set_input", &[input.as_str()])
    }

    fn power_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        self.run_command(tv_ip, "power_off", &[])
    }

    fn turn_screen_off(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        self.run_command(tv_ip, "turn_screen_off", &[])
    }

    fn turn_screen_on(&self, tv_ip: Ipv4Addr) -> Result<CommandOutput, TvError> {
        self.run_command(tv_ip, "turn_screen_on", &[])
    }
}

fn last_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::{
        BscpylgtvCommandClient, CommandOutput, TvClient, TvError, DEFAULT_BSCPYLGTV_COMMAND_PATH,
    };
    use crate::config::HdmiInput;
    use std::fs;
    use std::net::Ipv4Addr;
    use std::path::{Path, PathBuf};
    use std::process;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn default_client_uses_expected_command_path() {
        let client = BscpylgtvCommandClient::default();
        assert_eq!(
            client.command_path(),
            Path::new(DEFAULT_BSCPYLGTV_COMMAND_PATH)
        );
        assert!(client.command_args().is_empty());
    }

    #[test]
    fn combined_output_preserves_stdout_and_stderr() {
        let output = CommandOutput::new("hello\n".to_string(), "world\n".to_string());
        assert_eq!(output.combined_output(), "hello\nworld\n");
    }

    #[test]
    fn get_input_uses_last_non_empty_stdout_line() {
        let temp_dir = TestDir::new("tv-get-input");
        let log_path = temp_dir.path().join("invocation.log");
        let script_path = temp_dir.path().join("stub.sh");
        write_stub(
            &script_path,
            &log_path,
            r#"
printf '\n'
printf 'ignored\n'
printf 'com.webos.app.hdmi2\n'
"#,
        );

        let client = client_for_script(&script_path);
        let input = client
            .get_input(ip("192.168.1.42"))
            .expect("get_input should succeed");

        assert_eq!(input, "com.webos.app.hdmi2");
        assert_eq!(
            fs::read_to_string(&log_path).expect("read invocation log"),
            "192.168.1.42\nget_input\n"
        );
    }

    #[test]
    fn get_input_rejects_empty_output() {
        let temp_dir = TestDir::new("tv-get-input-empty");
        let log_path = temp_dir.path().join("invocation.log");
        let script_path = temp_dir.path().join("stub.sh");
        write_stub(&script_path, &log_path, "");

        let client = client_for_script(&script_path);
        let err = client
            .get_input(ip("192.168.1.42"))
            .expect_err("empty output should fail");

        match err {
            TvError::InvalidOutput {
                command, message, ..
            } => {
                assert_eq!(command, "get_input");
                assert_eq!(message, "expected a non-empty line in stdout");
            }
            other => panic!("expected invalid output error, got {other:?}"),
        }
    }

    #[test]
    fn set_input_passes_expected_arguments() {
        let temp_dir = TestDir::new("tv-set-input");
        let log_path = temp_dir.path().join("invocation.log");
        let script_path = temp_dir.path().join("stub.sh");
        write_stub(&script_path, &log_path, "printf 'ok\\n'\n");

        let client = client_for_script(&script_path);
        client
            .set_input(ip("10.0.0.5"), HdmiInput::Hdmi3)
            .expect("set_input should succeed");

        assert_eq!(
            fs::read_to_string(&log_path).expect("read invocation log"),
            "10.0.0.5\nset_input\nHDMI_3\n"
        );
    }

    #[test]
    fn command_failures_preserve_status_and_output() {
        let temp_dir = TestDir::new("tv-command-failure");
        let log_path = temp_dir.path().join("invocation.log");
        let script_path = temp_dir.path().join("stub.sh");
        write_stub(
            &script_path,
            &log_path,
            "printf 'failure stdout\\n'\nprintf 'failure stderr\\n' >&2\nexit 7\n",
        );

        let client = client_for_script(&script_path);
        let err = client
            .turn_screen_on(ip("10.0.0.8"))
            .expect_err("turn_screen_on should fail");

        match err {
            TvError::CommandFailed {
                command,
                status,
                output,
            } => {
                assert_eq!(command, "turn_screen_on");
                assert_eq!(status, Some(7));
                assert_eq!(output.stdout(), "failure stdout\n");
                assert_eq!(output.stderr(), "failure stderr\n");
            }
            other => panic!("expected command failure, got {other:?}"),
        }
    }

    fn write_stub(script_path: &Path, log_path: &Path, body: &str) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n{}",
            shell_quote(log_path),
            body
        );
        fs::write(script_path, script).expect("write stub script");
    }

    fn shell_quote(path: &Path) -> String {
        let rendered = path.to_string_lossy().replace('\'', "'\"'\"'");
        format!("'{rendered}'")
    }

    fn ip(value: &str) -> Ipv4Addr {
        value.parse().expect("parse IPv4 address")
    }

    fn client_for_script(script_path: &Path) -> BscpylgtvCommandClient {
        BscpylgtvCommandClient::with_args("/bin/sh", [script_path.to_string_lossy().into_owned()])
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(label: &str) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);

            let unique = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
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
