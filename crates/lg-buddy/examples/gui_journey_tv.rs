//! Native TV fixture for the installed GUI journey.
//!
//! Start it with one control directory argument. The fixture publishes one
//! atomic `state.json` there and consumes atomically-written command files:
//! `stateful`, `pairing-rejected`, `stall`, `interrupted`, or `stop`.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use serde_json::json;

mod auth {
    pub use lg_buddy::auth::SystemUser;
}

mod platform_access_token {
    pub use lg_buddy::platform_access_token::{PlatformAccessToken, PlatformAccessTokenStore};
}

// Keep the process fixture on the same adapter and server used by Cucumber.
#[allow(dead_code)]
#[path = "../tests/cucumber_support/webos.rs"]
mod web_os;

use web_os::{MockWebOsTv, MockWebOsVersion};

const MAX_COMMAND_BYTES: u64 = 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
enum Scenario {
    Stateful,
    PairingRejected,
    Stall,
    Interrupted,
}

impl Scenario {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "stateful" => Ok(Self::Stateful),
            "pairing-rejected" => Ok(Self::PairingRejected),
            "stall" => Ok(Self::Stall),
            "interrupted" => Ok(Self::Interrupted),
            other => Err(format!(
                "unsupported fixture command `{other}`; use stateful, pairing-rejected, stall, interrupted, or stop"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Stateful => "stateful",
            Self::PairingRejected => "pairing-rejected",
            Self::Stall => "stall",
            Self::Interrupted => "interrupted",
        }
    }
}

struct Fixture {
    tv: MockWebOsTv,
}

impl Fixture {
    fn start(scenario: Scenario) -> Self {
        let tv = MockWebOsTv::with_version(MockWebOsVersion::WebOs24Version92261, "HDMI_3");
        match scenario {
            Scenario::Stateful => {}
            Scenario::PairingRejected => tv.reject_pairing(),
            Scenario::Stall => tv.stall_first_tv_response(),
            Scenario::Interrupted => tv.interrupt_restore_and_ack_input_without_unblanking(),
        }
        Self { tv }
    }

    fn restart(self, scenario: Scenario) -> Self {
        drop(self);
        Self::start(scenario)
    }

    fn state(&self, status: &str, scenario: Scenario) -> String {
        let snapshot = self.tv.snapshot();
        format!(
            "{}\n",
            json!({
                "status": status,
                "scenario": scenario.name(),
                "endpoint": "wss://127.0.0.1:3001",
                "power_on": snapshot.power_on,
                "screen_on": snapshot.screen_on,
                "input": snapshot.input,
                "backlight": snapshot.backlight,
                "volume": snapshot.volume,
                "muted": snapshot.muted,
                "connection_count": snapshot.connection_count,
                "pairing_prompt_count": snapshot.pairing_prompt_count,
                "registration_tokens": snapshot.registration_tokens,
            })
        )
    }
}

enum Command {
    Scenario(Scenario),
    Stop,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os();
    let _program = args.next();
    let control_dir = args.next().ok_or("usage: gui_journey_tv <control-dir>")?;
    if args.next().is_some() {
        return Err("usage: gui_journey_tv <control-dir>".into());
    }
    let control_dir = PathBuf::from(control_dir);
    fs::create_dir_all(&control_dir)?;
    let command_path = control_dir.join("command");
    let state_path = control_dir.join("state.json");
    let _ = fs::remove_file(&command_path);
    let _ = fs::remove_file(&state_path);

    let mut scenario = Scenario::Stateful;
    let mut fixture = Fixture::start(scenario);
    atomic_write(&state_path, &fixture.state("ready", scenario))?;
    let mut last_state = String::new();

    loop {
        if let Some(raw_command) = take_command(&command_path)? {
            match parse_command(&raw_command)? {
                Command::Scenario(next) => {
                    scenario = next;
                    fixture = fixture.restart(scenario);
                    last_state.clear();
                }
                Command::Stop => {
                    let stopped = fixture.state("stopped", scenario);
                    drop(fixture);
                    atomic_write(&state_path, &stopped)?;
                    return Ok(());
                }
            }
        }

        let state = fixture.state("ready", scenario);
        if state != last_state {
            atomic_write(&state_path, &state)?;
            last_state = state;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn parse_command(raw: &str) -> Result<Command, String> {
    match raw.trim() {
        "stop" => Ok(Command::Stop),
        value => Ok(Command::Scenario(Scenario::parse(value)?)),
    }
}

fn take_command(path: &Path) -> io::Result<Option<String>> {
    let processing = path.with_file_name(format!(
        ".command.{}.processing",
        TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    match fs::rename(path, &processing) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let result = (|| {
        let mut file = File::open(&processing)?;
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(MAX_COMMAND_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_COMMAND_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fixture command is too large",
            ));
        }
        String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    })();
    let _ = fs::remove_file(processing);
    result.map(Some)
}

fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let temporary = parent.join(format!(".{file_name}.{sequence}.tmp"));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(contents.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
