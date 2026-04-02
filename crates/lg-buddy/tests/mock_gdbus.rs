mod support;

use std::process::Command;

use support::{MockGdbus, MockGdbusInvocation};

#[test]
fn mock_name_has_owner_reflects_shell_availability() {
    let mock = MockGdbus::new("mock-gdbus-name-has-owner");

    let available = run_mock(
        &mock,
        [
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            "org.gnome.Shell",
        ],
    );
    assert!(available.status.success());
    assert_eq!(stdout(&available), "(true,)\n");

    mock.set_shell_available(false);
    let unavailable = run_mock(
        &mock,
        [
            "call",
            "--session",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.NameHasOwner",
            "org.gnome.Shell",
        ],
    );
    assert!(unavailable.status.success());
    assert_eq!(stdout(&unavailable), "(false,)\n");
}

#[test]
fn mock_wait_reflects_shell_availability() {
    let mock = MockGdbus::new("mock-gdbus-wait");

    let available = run_mock(
        &mock,
        ["wait", "--session", "--timeout", "2", "org.gnome.Shell"],
    );
    assert!(available.status.success());

    mock.set_shell_available(false);
    let unavailable = run_mock(
        &mock,
        ["wait", "--session", "--timeout", "2", "org.gnome.Shell"],
    );
    assert!(!unavailable.status.success());
}

#[test]
fn mock_screen_saver_and_idle_monitor_calls_match_expected_shapes() {
    let mock = MockGdbus::new("mock-gdbus-service-calls");
    mock.set_idle_monitor_idletime(777);

    let screen_saver = run_mock(
        &mock,
        [
            "call",
            "--session",
            "--dest",
            "org.gnome.ScreenSaver",
            "--object-path",
            "/org/gnome/ScreenSaver",
            "--method",
            "org.gnome.ScreenSaver.GetActive",
        ],
    );
    assert!(screen_saver.status.success());
    assert_eq!(stdout(&screen_saver), "(false,)\n");

    let idle_monitor = run_mock(
        &mock,
        [
            "call",
            "--session",
            "--dest",
            "org.gnome.Mutter.IdleMonitor",
            "--object-path",
            "/org/gnome/Mutter/IdleMonitor/Core",
            "--method",
            "org.gnome.Mutter.IdleMonitor.GetIdletime",
        ],
    );
    assert!(idle_monitor.status.success());
    assert_eq!(stdout(&idle_monitor), "(uint64 777,)\n");

    mock.set_screen_saver_available(false);
    let missing_screen_saver = run_mock(
        &mock,
        [
            "call",
            "--session",
            "--dest",
            "org.gnome.ScreenSaver",
            "--object-path",
            "/org/gnome/ScreenSaver",
            "--method",
            "org.gnome.ScreenSaver.GetActive",
        ],
    );
    assert!(!missing_screen_saver.status.success());
    assert!(stderr(&missing_screen_saver).contains("org.gnome.ScreenSaver is unavailable"));

    mock.set_idle_monitor_available(false);
    let missing_idle_monitor = run_mock(
        &mock,
        [
            "call",
            "--session",
            "--dest",
            "org.gnome.Mutter.IdleMonitor",
            "--object-path",
            "/org/gnome/Mutter/IdleMonitor/Core",
            "--method",
            "org.gnome.Mutter.IdleMonitor.GetIdletime",
        ],
    );
    assert!(!missing_idle_monitor.status.success());
    assert!(stderr(&missing_idle_monitor).contains("org.gnome.Mutter.IdleMonitor is unavailable"));
}

#[test]
fn mock_monitor_replays_lines_and_records_invocations() {
    let mock = MockGdbus::new("mock-gdbus-monitor");
    mock.push_monitor_line("signal org.gnome.ScreenSaver.ActiveChanged (true,)");
    mock.push_monitor_line("member=WakeUpScreen");

    let output = run_mock(
        &mock,
        [
            "monitor",
            "--session",
            "--dest",
            "org.gnome.ScreenSaver",
            "--object-path",
            "/org/gnome/ScreenSaver",
        ],
    );

    assert!(output.status.success());
    assert_eq!(
        stdout(&output),
        "signal org.gnome.ScreenSaver.ActiveChanged (true,)\nmember=WakeUpScreen\n"
    );
    assert_eq!(
        mock.invocations(),
        vec![MockGdbusInvocation {
            argv: vec![
                "monitor".to_string(),
                "--session".to_string(),
                "--dest".to_string(),
                "org.gnome.ScreenSaver".to_string(),
                "--object-path".to_string(),
                "/org/gnome/ScreenSaver".to_string(),
            ],
        }]
    );
}

fn run_mock<const N: usize>(mock: &MockGdbus, args: [&str; N]) -> std::process::Output {
    Command::new(mock.command_path())
        .args(mock.command_args())
        .args(args)
        .output()
        .expect("run mock gdbus")
}

fn stdout(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout utf8")
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr utf8")
}
