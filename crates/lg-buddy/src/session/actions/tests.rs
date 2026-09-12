use super::RuntimeActionExecutor;
use crate::config::{load_config, HdmiInput, TvPlatform};
use crate::session::runner::SessionEventDispatcher;
use crate::session::SessionEvent;
use crate::tv::{CurrentInput, SelectedTvClient, TvClient, TvClientBuildOptions, TvErrorKind};
use crate::web_os::test_support::{
    WebOsTestInput, WebOsTestScenario, WebOsTestServer, WebOsTestVersion,
};
use std::env;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const VALID_ACCESS_TOKEN: &str = "webos-test-access-token";

fn test_lock() -> &'static Mutex<()> {
    crate::session::test_env_lock()
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl EnvGuard {
    fn for_fixture(fixture: &Fixture) -> Self {
        let mut guard = Self {
            previous: Vec::new(),
        };
        guard.set("LG_BUDDY_CONFIG", fixture.config_path.as_os_str());
        guard.set(
            "LG_BUDDY_SESSION_RUNTIME_DIR",
            fixture.session_dir.as_os_str(),
        );
        guard.set(
            "LG_BUDDY_SYSTEM_RUNTIME_DIR",
            fixture.system_dir.as_os_str(),
        );
        guard
    }

    fn set(&mut self, key: &'static str, value: &std::ffi::OsStr) {
        self.previous.push((key, env::var_os(key)));
        env::set_var(key, value);
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
    }
}

struct Fixture {
    root: PathBuf,
    config_path: PathBuf,
    session_dir: PathBuf,
    system_dir: PathBuf,
}

impl Fixture {
    fn new(ip: Ipv4Addr) -> Self {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = env::temp_dir().join(format!(
            "lg-buddy-runtime-actions-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create runtime action fixture");
        let fixture = Self {
            config_path: root.join("config.env"),
            session_dir: root.join("session"),
            system_dir: root.join("system"),
            root,
        };
        fixture.write_config(ip, "aa:bb:cc:dd:ee:ff", TvPlatform::LgWebOs);
        let token_path = fixture
            .config_path
            .parent()
            .expect("fixture config parent")
            .join("tvs/primary/access-token.json");
        fs::create_dir_all(token_path.parent().expect("fixture token parent"))
            .expect("create fixture token directory");
        fs::write(
            token_path,
            format!("{{\n  \"access_token\": \"{VALID_ACCESS_TOKEN}\"\n}}\n"),
        )
        .expect("write fixture token");
        fixture
    }

    fn write_config(&self, ip: Ipv4Addr, mac: &str, platform: TvPlatform) {
        fs::write(
            &self.config_path,
            format!(
                "tvs_primary_ip={ip}\n\
tvs_primary_mac={mac}\n\
tvs_primary_input=HDMI_3\n\
tvs_primary_platform={}\n\
screen_backend=auto\n\
screen_idle_timeout=300\n\
system_sleep_wake_policy=disabled\n",
                platform.as_str()
            ),
        )
        .expect("write fixture config");
    }

    fn set_value(&self, key: &str, value: &str) {
        let contents = fs::read_to_string(&self.config_path).expect("read fixture config");
        let prefix = format!("{key}=");
        let mut found = false;
        let mut updated = contents
            .lines()
            .map(|line| {
                if line.starts_with(&prefix) {
                    found = true;
                    format!("{key}={value}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>();
        if !found {
            updated.push(format!("{key}={value}"));
        }
        fs::write(&self.config_path, format!("{}\n", updated.join("\n")))
            .expect("update fixture config");
    }

    fn config(&self) -> crate::config::Config {
        load_config(&self.config_path).expect("load fixture config")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn server_at(last_octet: u8) -> WebOsTestServer {
    WebOsTestServer::active_tls_at(
        WebOsTestVersion::WebOs24Version92261,
        WebOsTestInput::Hdmi3,
        SocketAddr::from(([127, 0, 0, last_octet], 3001)),
    )
}

fn read_input(
    owner: &mut RuntimeActionExecutor,
    config_path: &Path,
    config: &crate::config::Config,
    options: TvClientBuildOptions,
) -> CurrentInput {
    owner
        .tv_client(config_path, config, options)
        .expect("build runtime TV client")
        .current_input()
        .expect("read fixture input")
}

#[test]
fn runtime_owner_lazily_reuses_native_session_across_screen_events() {
    let _lock = test_lock().lock().expect("runtime action test lock");
    let ip = Ipv4Addr::new(127, 0, 0, 2);
    let server = server_at(2);
    let fixture = Fixture::new(ip);
    let _env = EnvGuard::for_fixture(&fixture);
    let mut dispatcher = SessionEventDispatcher::new(RuntimeActionExecutor::default());

    assert_eq!(server.snapshot().connection_count, 0);

    let mut output = Vec::new();
    dispatcher
        .dispatch_event(&mut output, SessionEvent::Idle)
        .expect("screen blank dispatch should succeed");
    let snapshot = server.snapshot();
    assert_eq!(snapshot.connection_count, 1);
    assert_eq!(
        snapshot.registration_tokens,
        vec![Some(VALID_ACCESS_TOKEN.to_string())]
    );
    assert!(
        crate::state::ScreenOwnershipMarker::from_env(crate::state::StateScope::Session)
            .expect("session marker")
            .exists()
    );

    fixture.set_value("screen_idle_timeout", "600");
    let mut output = Vec::new();
    dispatcher
        .dispatch_event(&mut output, SessionEvent::Active)
        .expect("screen restore should succeed");
    let snapshot = server.snapshot();
    assert_eq!(snapshot.connection_count, 1);
    assert_eq!(
        snapshot.registration_tokens,
        vec![Some(VALID_ACCESS_TOKEN.to_string())]
    );
    assert_eq!(snapshot.power_state, crate::web_os::WebOsPowerState::Active);
    assert!(
        !crate::state::ScreenOwnershipMarker::from_env(crate::state::StateScope::Session)
            .expect("session marker")
            .exists()
    );

    server.finish();
}

#[test]
fn closed_session_recovers_input_before_next_idle_decision() {
    use crate::web_os::WebOsPowerState;

    let _lock = test_lock().lock().expect("runtime action test lock");
    for (address, input, expected_state) in [
        (8, WebOsTestInput::Hdmi2, WebOsPowerState::Active),
        (9, WebOsTestInput::Hdmi3, WebOsPowerState::ScreenOff),
    ] {
        let server = server_at(address);
        let fixture = Fixture::new(Ipv4Addr::new(127, 0, 0, address));
        let _env = EnvGuard::for_fixture(&fixture);
        let mut dispatcher = SessionEventDispatcher::new(RuntimeActionExecutor::default());
        let mut output = Vec::new();
        dispatcher
            .dispatch_event(&mut output, SessionEvent::Idle)
            .unwrap();
        dispatcher
            .dispatch_event(&mut output, SessionEvent::Active)
            .unwrap();
        assert_eq!(server.snapshot().connection_count, 1);

        // Model switching inputs on the TV while the monitor's socket closes.
        server.set_input(input);
        server.close_active_connections();
        output.clear();
        dispatcher
            .dispatch_event(&mut output, SessionEvent::Idle)
            .unwrap();

        let snapshot = server.snapshot();
        assert_eq!(snapshot.power_state, expected_state);
        assert_eq!(snapshot.input, input);
        assert_eq!(snapshot.connection_count, 2);
        assert_eq!(
            snapshot.registration_tokens,
            vec![Some(VALID_ACCESS_TOKEN.to_string()); 2]
        );
        assert!(!snapshot
            .request_uris
            .iter()
            .any(|uri| uri == "ssap://system/turnOff"));
        assert!(!String::from_utf8_lossy(&output).contains("Falling back to power_off"));
        server.finish();
    }
}

#[test]
fn runtime_owner_reconnects_after_a_server_closed_session() {
    let _lock = test_lock().lock().expect("runtime action test lock");
    let ip = Ipv4Addr::new(127, 0, 0, 3);
    let server = server_at(3);
    server.set_scenario(WebOsTestScenario::RestoreSessionInterruptedAndInputAckLeavesScreenOff);
    let fixture = Fixture::new(ip);
    let _env = EnvGuard::for_fixture(&fixture);
    let mut owner = RuntimeActionExecutor::default();
    let config = fixture.config();

    let error = owner
        .tv_client(
            &fixture.config_path,
            &config,
            TvClientBuildOptions::production(),
        )
        .expect("build runtime TV client")
        .current_input()
        .expect_err("closed session must report a transport failure");
    assert_eq!(error.kind(), TvErrorKind::Transport);
    assert_eq!(server.snapshot().connection_count, 1);

    server.set_scenario(WebOsTestScenario::StatefulTv);
    let config = fixture.config();
    assert_eq!(
        read_input(
            &mut owner,
            &fixture.config_path,
            &config,
            TvClientBuildOptions::production(),
        ),
        CurrentInput::Hdmi(HdmiInput::Hdmi3)
    );
    let snapshot = server.snapshot();
    assert_eq!(snapshot.connection_count, 2);
    assert_eq!(
        snapshot.registration_tokens,
        vec![Some(VALID_ACCESS_TOKEN.to_string()); 2]
    );

    server.finish();
}

#[test]
fn runtime_owner_rebinds_when_profile_identity_or_build_options_change() {
    let _lock = test_lock().lock().expect("runtime action test lock");
    let first_server = server_at(5);
    let second_server = server_at(6);
    let first_fixture = Fixture::new(Ipv4Addr::new(127, 0, 0, 5));
    let second_fixture = Fixture::new(Ipv4Addr::new(127, 0, 0, 6));
    let mut owner = RuntimeActionExecutor::default();
    let options = TvClientBuildOptions::production();

    let assert_input = |owner: &mut RuntimeActionExecutor, fixture: &Fixture, options| {
        assert_eq!(
            read_input(owner, &fixture.config_path, &fixture.config(), options),
            CurrentInput::Hdmi(HdmiInput::Hdmi3)
        );
    };
    assert_input(&mut owner, &first_fixture, options);
    assert_eq!(first_server.snapshot().connection_count, 1);

    first_fixture.set_value("tvs_primary_mac", "aa:bb:cc:dd:ee:66");
    assert_input(&mut owner, &first_fixture, options);
    assert_eq!(first_server.snapshot().connection_count, 2);

    first_fixture.set_value("tvs_primary_ip", "127.0.0.6");
    assert_input(&mut owner, &first_fixture, options);
    assert_eq!(second_server.snapshot().connection_count, 1);

    first_fixture.set_value("tvs_primary_platform", TvPlatform::Bscpylgtv.as_str());
    assert!(matches!(
        owner
            .tv_client(&first_fixture.config_path, &first_fixture.config(), options)
            .expect("build legacy client"),
        SelectedTvClient::Bscpylgtv(_)
    ));
    first_fixture.set_value("tvs_primary_platform", TvPlatform::LgWebOs.as_str());
    assert_input(&mut owner, &first_fixture, options);
    assert_eq!(second_server.snapshot().connection_count, 2);

    for (changed_options, expected_connections) in [
        (options.with_command_timeout(Duration::from_secs(1)), 3),
        (options, 4),
        (options.stored_token_only(), 5),
        (options, 6),
    ] {
        assert_input(&mut owner, &first_fixture, changed_options);
        assert_eq!(
            second_server.snapshot().connection_count,
            expected_connections
        );
    }

    // Change only the profile path, keeping TV identity and options identical.
    second_fixture.set_value("tvs_primary_mac", "aa:bb:cc:dd:ee:66");
    assert_input(&mut owner, &second_fixture, options);
    assert_eq!(second_server.snapshot().connection_count, 7);

    first_server.finish();
    second_server.finish();
}

#[test]
fn runtime_owner_discards_client_when_profile_cannot_be_loaded() {
    let _lock = test_lock().lock().expect("runtime action test lock");
    let server = server_at(7);
    let fixture = Fixture::new(Ipv4Addr::new(127, 0, 0, 7));
    let _env = EnvGuard::for_fixture(&fixture);
    let mut owner = RuntimeActionExecutor::default();
    let config = fixture.config();
    let options = TvClientBuildOptions::production();
    read_input(&mut owner, &fixture.config_path, &config, options);
    assert_eq!(server.snapshot().connection_count, 1);

    let contents = fs::read(&fixture.config_path).expect("read fixture config");
    fs::remove_file(&fixture.config_path).expect("remove profile");
    assert!(owner.load_config().is_err());
    fs::write(&fixture.config_path, contents).expect("restore profile");
    read_input(&mut owner, &fixture.config_path, &config, options);
    assert_eq!(server.snapshot().connection_count, 2);
    server.finish();
}
