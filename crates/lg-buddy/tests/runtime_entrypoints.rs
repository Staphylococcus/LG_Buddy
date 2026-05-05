mod support;

use lg_buddy::commands::{run_screen_off, run_screen_on, run_system_resume};
use lg_buddy::session::runner::{RuntimeActionExecutor, SessionEventDispatcher};
use lg_buddy::session::SessionEvent;
use lg_buddy::{run_command, Command};
use std::fs;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use support::{
    MockBscpylgtv, MockNmOnline, MockSystemLogind, RuntimeStateLayout, TestConfigFile, TestEnv,
};

#[test]
fn run_screen_off_loads_config_and_uses_session_runtime_override() {
    let mock = MockBscpylgtv::new("entrypoint-screen-off-tv");
    mock.set_input("HDMI_2");
    let wrapper = mock.command_wrapper("entrypoint-screen-off-wrapper");

    let config = TestConfigFile::new("entrypoint-screen-off-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-screen-off-runtime");
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());

    let mut output = Vec::new();
    run_screen_off(&mut output).expect("screen-off should succeed");

    runtime.assert_session_marker_exists();
    let calls = mock.calls();
    assert_eq!(
        calls
            .iter()
            .cloned()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec!["get_input".to_string(), "turn_screen_off".to_string()]
    );
    let expected_key_path = config
        .path()
        .parent()
        .expect("config parent")
        .join(".aiopylgtv.sqlite");
    assert_eq!(
        calls.first().and_then(|call| call.key_file_path.as_deref()),
        Some(expected_key_path.to_str().expect("utf8 key path"))
    );
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("Screen blank command succeeded."));
}

#[test]
fn run_screen_on_loads_config_and_clears_session_marker() {
    let mock = MockBscpylgtv::new("entrypoint-screen-on-tv");
    mock.set_input("HDMI_3");
    mock.set_screen_on(false);
    let wrapper = mock.command_wrapper("entrypoint-screen-on-wrapper");

    let config = TestConfigFile::new("entrypoint-screen-on-config");
    config.write_sample("HDMI_3");

    let runtime = RuntimeStateLayout::new("entrypoint-screen-on-runtime");
    runtime.create_session_marker();

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());

    let mut output = Vec::new();
    run_screen_on(&mut output).expect("screen-on should succeed");

    runtime.assert_session_marker_absent();
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec!["turn_screen_on".to_string()]
    );
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("Screen unblank succeeded."));
}

#[test]
fn run_screen_on_loads_aggressive_config_and_restores_without_session_marker() {
    let mock = MockBscpylgtv::new("entrypoint-screen-on-aggressive-tv");
    mock.set_input("HDMI_3");
    mock.set_screen_on(false);
    let wrapper = mock.command_wrapper("entrypoint-screen-on-aggressive-wrapper");

    let config = TestConfigFile::new("entrypoint-screen-on-aggressive-config");
    config.write_sample("HDMI_3");
    config.append_line("screen_restore_policy=aggressive");

    let runtime = RuntimeStateLayout::new("entrypoint-screen-on-aggressive-runtime");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());

    let mut output = Vec::new();
    run_screen_on(&mut output).expect("screen-on should restore in aggressive mode");

    runtime.assert_session_marker_absent();
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec!["turn_screen_on".to_string()]
    );
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Aggressive restore policy is enabled"));
    assert!(output.contains("Screen unblank succeeded."));
}

#[test]
fn run_system_resume_loads_config_and_clears_system_sleep_attempt() {
    let mock = MockBscpylgtv::new("entrypoint-system-resume-tv");
    let wrapper = mock.command_wrapper("entrypoint-system-resume-wrapper");
    let nm_online = MockNmOnline::new("entrypoint-system-resume-nm-online");
    let nm_online_wrapper = nm_online.command_wrapper("entrypoint-system-resume-nm-online-wrapper");

    let config = TestConfigFile::new("entrypoint-system-resume-config");
    config.write_sample("HDMI_4");

    let runtime = RuntimeStateLayout::new("entrypoint-system-resume-runtime");
    runtime.create_system_marker();
    let attempt_marker = runtime.system_dir().join("system_sleep_attempted");
    fs::write(&attempt_marker, "").expect("create attempt marker");

    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
    env.set("LG_BUDDY_NM_ONLINE", nm_online_wrapper.path());
    env.set("LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS", "0");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_ATTEMPTS", "1");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_DELAY_MS", "0");

    let mut output = Vec::new();
    run_system_resume(&mut output).expect("system resume should succeed");

    runtime.assert_system_marker_absent();
    assert!(!attempt_marker.exists());
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec!["set_input".to_string()]
    );
    assert_eq!(nm_online.invocations().len(), 1);
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("Wake from sleep: LG Buddy turned TV off. Restoring."));
}

#[test]
fn run_nm_pre_down_uses_logind_property_and_retries_idempotently() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-nm-pre-down-logind");
    logind.reset();
    logind.set_preparing_for_sleep(true);

    let mock = MockBscpylgtv::new("entrypoint-nm-pre-down-tv");
    mock.set_input("HDMI_2");
    let wrapper = mock.command_wrapper("entrypoint-nm-pre-down-wrapper");

    let config = TestConfigFile::new("entrypoint-nm-pre-down-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-nm-pre-down-runtime");

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());

    let mut first_output = Vec::new();
    run_command(Command::NetworkManagerPreDown, &mut first_output)
        .expect("NetworkManager pre-down should succeed during system sleep");

    runtime.assert_system_marker_exists();
    runtime.assert_system_sleep_attempt_marker_absent();
    assert_eq!(
        mock.calls()
            .iter()
            .map(|call| call.command.as_str())
            .collect::<Vec<_>>(),
        vec!["get_input", "power_off"]
    );
    assert!(!mock.state_snapshot().power_on);
    assert!(String::from_utf8(first_output)
        .expect("utf8 output")
        .contains("logind is preparing for sleep"));

    let mut second_output = Vec::new();
    run_command(Command::NetworkManagerPreDown, &mut second_output)
        .expect("repeated NetworkManager pre-down should stay idempotent");

    assert_eq!(
        mock.calls()
            .iter()
            .map(|call| call.command.as_str())
            .collect::<Vec<_>>(),
        vec!["get_input", "power_off", "get_input", "power_off"]
    );
    runtime.assert_system_marker_exists();
    runtime.assert_system_sleep_attempt_marker_absent();
    assert!(String::from_utf8(second_output)
        .expect("utf8 output")
        .contains("Could not query TV input"));
}

#[test]
fn run_nm_pre_down_skips_network_disconnect_and_clears_stale_attempt() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-nm-pre-down-not-sleeping-logind");
    logind.reset();
    logind.set_preparing_for_sleep(false);

    let mock = MockBscpylgtv::new("entrypoint-nm-pre-down-not-sleeping-tv");
    let wrapper = mock.command_wrapper("entrypoint-nm-pre-down-not-sleeping-wrapper");

    let config = TestConfigFile::new("entrypoint-nm-pre-down-not-sleeping-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-nm-pre-down-not-sleeping-runtime");
    runtime.create_system_sleep_attempt_marker();

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());

    let mut output = Vec::new();
    run_command(Command::NetworkManagerPreDown, &mut output)
        .expect("ordinary NetworkManager pre-down should fail open");

    runtime.assert_system_sleep_attempt_marker_absent();
    runtime.assert_system_marker_absent();
    assert!(mock.calls().is_empty());
    assert!(String::from_utf8(output)
        .expect("utf8 output")
        .contains("not preparing for sleep"));
}

#[test]
fn session_dispatcher_skips_screen_action_while_logind_reports_sleep_pending() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-monitor-sleep-pending-logind");
    logind.reset();
    logind.set_preparing_for_sleep(true);

    let mock = MockBscpylgtv::new("entrypoint-monitor-sleep-pending-tv");
    mock.set_input("HDMI_2");
    let wrapper = mock.command_wrapper("entrypoint-monitor-sleep-pending-wrapper");

    let config = TestConfigFile::new("entrypoint-monitor-sleep-pending-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-monitor-sleep-pending-runtime");

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());

    let mut output = Vec::new();
    let mut dispatcher = SessionEventDispatcher::new(RuntimeActionExecutor);
    dispatcher
        .dispatch_event(&mut output, SessionEvent::Idle)
        .expect("session idle dispatch should succeed");

    runtime.assert_session_marker_absent();
    assert!(mock.calls().is_empty());
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(
        output.contains("Machine sleep is pending"),
        "output was: {output}"
    );
    assert!(
        output.contains("Skipping session screen action"),
        "output was: {output}"
    );
}

#[test]
fn session_dispatcher_skips_screen_restore_while_system_resume_restore_is_pending() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-monitor-system-restore-pending-logind");
    logind.reset();
    logind.set_preparing_for_sleep(false);

    let mock = MockBscpylgtv::new("entrypoint-monitor-system-restore-pending-tv");
    mock.set_screen_on(false);
    let wrapper = mock.command_wrapper("entrypoint-monitor-system-restore-pending-wrapper");

    let config = TestConfigFile::new("entrypoint-monitor-system-restore-pending-config");
    config.write_sample("HDMI_2");

    let runtime = RuntimeStateLayout::new("entrypoint-monitor-system-restore-pending-runtime");
    runtime.create_session_marker();
    runtime.create_system_marker();

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());

    let mut output = Vec::new();
    let mut dispatcher = SessionEventDispatcher::new(RuntimeActionExecutor);
    dispatcher
        .dispatch_event(&mut output, SessionEvent::Active)
        .expect("session active dispatch should succeed");

    runtime.assert_session_marker_exists();
    runtime.assert_system_marker_exists();
    assert!(mock.calls().is_empty());
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(
        output.contains("System resume restore is pending"),
        "output was: {output}"
    );
    assert!(
        output.contains("Skipping session screen action"),
        "output was: {output}"
    );
}

#[test]
fn run_lifecycle_monitor_uses_logind_resume_signal_and_runtime_restore() {
    let mut env = TestEnv::new();
    let logind = MockSystemLogind::new("entrypoint-lifecycle-logind");
    logind.reset();
    let mock = MockBscpylgtv::new("entrypoint-lifecycle-tv");
    let wrapper = mock.command_wrapper("entrypoint-lifecycle-wrapper");
    let nm_online = MockNmOnline::new("entrypoint-lifecycle-nm-online");
    let nm_online_wrapper = nm_online.command_wrapper("entrypoint-lifecycle-nm-online-wrapper");

    let config = TestConfigFile::new("entrypoint-lifecycle-config");
    config.write_sample("HDMI_4");

    let runtime = RuntimeStateLayout::new("entrypoint-lifecycle-runtime");
    runtime.create_system_marker();
    runtime.create_system_sleep_attempt_marker();

    env.set("DBUS_SYSTEM_BUS_ADDRESS", logind.address());
    env.set("LG_BUDDY_CONFIG", config.path());
    env.set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
    env.set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
    env.set("LG_BUDDY_NM_ONLINE", nm_online_wrapper.path());
    env.set("LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS", "0");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_ATTEMPTS", "1");
    env.set("LG_BUDDY_TV_ROUTE_WAIT_DELAY_MS", "0");

    let (done_tx, done_rx) = mpsc::channel();
    let lifecycle_thread = thread::spawn(move || {
        let mut output = Vec::new();
        let result = run_command(Command::Lifecycle, &mut output).map_err(|err| err.to_string());
        done_tx
            .send((result, output))
            .expect("send lifecycle result");
    });

    wait_until(Duration::from_secs(4), || {
        logind.queue_prepare_for_sleep_signal(false);
        mock.calls().iter().any(|call| call.command == "set_input")
    });

    assert_eq!(
        mock.calls()
            .iter()
            .filter(|call| call.command == "set_input")
            .count(),
        1
    );
    runtime.assert_system_marker_absent();
    runtime.assert_system_sleep_attempt_marker_absent();

    config.append_line("system_sleep_wake_policy=disabled");
    logind.queue_prepare_for_sleep_signal(true);

    let (result, output) = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("lifecycle monitor should exit after config is disabled");
    lifecycle_thread
        .join()
        .expect("join lifecycle monitor thread");
    result.expect("lifecycle monitor should succeed");

    assert!(!nm_online.invocations().is_empty());
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Using logind system lifecycle source"));
    assert!(output.contains("System resumed from sleep"));
    assert!(output.contains("Session event `after-resume` requests wake restore"));
    assert!(output.contains("Wake from sleep: LG Buddy turned TV off. Restoring."));
    assert!(output.contains("stopping lifecycle monitor"));
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return;
        }

        let now = Instant::now();
        assert!(now < deadline, "condition was not met within {:?}", timeout);

        let sleep_for = Duration::from_millis(100).min(deadline.saturating_duration_since(now));
        thread::sleep(sleep_for);
    }
}
