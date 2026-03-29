use crate::cucumber_support::world::LgBuddyWorld;
use cucumber::{given, then, when};

#[given(regex = r#"a temporary LG Buddy config using input (HDMI_[1-4])"#)]
fn temporary_config(world: &mut LgBuddyWorld, input: String) {
    world.create_config(&input);
}

#[given("LG Buddy session runtime is isolated")]
fn isolated_runtime(world: &mut LgBuddyWorld) {
    world.create_runtime();
}

#[given("a mock TV client")]
fn mock_tv_client(world: &mut LgBuddyWorld) {
    world.create_mock_tv();
}

#[given("the TV is reachable over ping")]
fn tv_is_reachable_over_ping(world: &mut LgBuddyWorld) {
    world.install_ping_stub(true);
}

#[given("the TV is unreachable over ping")]
fn tv_is_unreachable_over_ping(world: &mut LgBuddyWorld) {
    world.install_ping_stub(false);
}

#[given(regex = r#"the TV is on input (HDMI_[1-4])"#)]
fn tv_on_input(world: &mut LgBuddyWorld, input: String) {
    world.tv_mut().set_input(&input);
}

#[given(regex = r#"the TV backlight is (\d+)"#)]
fn tv_backlight(world: &mut LgBuddyWorld, value: u8) {
    world.tv_mut().set_backlight(u64::from(value));
}

#[given(regex = r#"the brightness dialog returns (\d+)"#)]
fn brightness_dialog_returns(world: &mut LgBuddyWorld, value: u8) {
    world.install_brightness_ui_stub(Some(value));
}

#[given("the brightness dialog is cancelled")]
fn brightness_dialog_is_cancelled(world: &mut LgBuddyWorld) {
    world.install_brightness_ui_stub(None);
}

#[given("the brightness error dialog is available")]
fn brightness_error_dialog_is_available(world: &mut LgBuddyWorld) {
    world.install_brightness_ui_stub(None);
}

#[given("the TV screen is blanked")]
fn tv_screen_blanked(world: &mut LgBuddyWorld) {
    world.tv_mut().set_screen_on(false);
}

#[given("the session marker exists")]
fn session_marker_exists_given(world: &mut LgBuddyWorld) {
    world.create_session_marker();
}

#[given("the system marker exists")]
fn system_marker_exists_given(world: &mut LgBuddyWorld) {
    world.create_system_marker();
}

#[given(regex = r#"the TV will fail "([^"]+)" with status (\d+) and stderr "([^"]+)""#)]
fn tv_failure(world: &mut LgBuddyWorld, command: String, status: u64, stderr: String) {
    world.tv_mut().queue_error(&command, status as i64, &stderr);
}

#[given(regex = r#"the TV will fail "([^"]+)" (\d+) times with status (\d+) and stderr "([^"]+)""#)]
fn tv_failure_repeated(
    world: &mut LgBuddyWorld,
    command: String,
    times: u64,
    status: u64,
    stderr: String,
) {
    for _ in 0..times {
        world.tv_mut().queue_error(&command, status as i64, &stderr);
    }
}

#[given("the executable PATH is isolated")]
fn executable_path_isolated(world: &mut LgBuddyWorld) {
    world.isolate_path();
}

#[given("GNOME Shell is available")]
fn gnome_shell_available(world: &mut LgBuddyWorld) {
    world.install_gnome_shell_stub();
}

#[given("GNOME reports the session idle")]
fn gnome_reports_idle(world: &mut LgBuddyWorld) {
    world.gnome_monitor_emit_idle();
}

#[given("GNOME reports the session active")]
fn gnome_reports_active(world: &mut LgBuddyWorld) {
    world.gnome_monitor_emit_active();
}

#[given("GNOME requests screen wake")]
fn gnome_requests_screen_wake(world: &mut LgBuddyWorld) {
    world.gnome_monitor_emit_wake_requested();
}

#[given("GNOME idle monitor would soon report recent user activity")]
fn gnome_idle_monitor_reports_recent_user_activity(world: &mut LgBuddyWorld) {
    world.gnome_idle_monitor_reports_recent_user_activity();
}

#[given("GNOME monitor stays open briefly for background polling")]
fn gnome_monitor_stays_open_briefly(world: &mut LgBuddyWorld) {
    world.gnome_monitor_stays_open_briefly();
}

#[given("swayidle is installed")]
fn swayidle_installed(world: &mut LgBuddyWorld) {
    world.install_swayidle_stub();
}

#[given("swayidle will emit an idle timeout")]
fn swayidle_will_emit_timeout(world: &mut LgBuddyWorld) {
    world.swayidle_emits_timeout();
}

#[given("swayidle will emit a resume event")]
fn swayidle_will_emit_resume(world: &mut LgBuddyWorld) {
    world.swayidle_emits_resume();
}

#[given(regex = r#"the backend override is "([^"]+)""#)]
fn backend_override(world: &mut LgBuddyWorld, backend: String) {
    world.set_backend_override(&backend);
}

#[given("startup delays are disabled")]
fn startup_delays_disabled(world: &mut LgBuddyWorld) {
    world.disable_startup_delays();
}

#[given("nm-online succeeds")]
fn nm_online_succeeds(world: &mut LgBuddyWorld) {
    world.install_nm_online_stub(0);
}

#[given(regex = r#"nm-online fails with status (\d+)"#)]
fn nm_online_fails(world: &mut LgBuddyWorld, status: u64) {
    world.install_nm_online_stub(status as i64);
}

#[given("sleep retry delays are disabled")]
fn sleep_retry_delays_disabled(world: &mut LgBuddyWorld) {
    world.disable_sleep_delays();
}

#[given("reboot detection reports no pending reboot")]
fn reboot_not_pending(world: &mut LgBuddyWorld) {
    world.install_systemctl_stub(false);
}

#[given("reboot detection reports a pending reboot")]
fn reboot_pending(world: &mut LgBuddyWorld) {
    world.install_systemctl_stub(true);
}

#[given("journalctl reports a pending NetworkManager sleep request")]
fn journalctl_reports_sleep_requested(world: &mut LgBuddyWorld) {
    world.install_journalctl_stub(true);
}

#[given("journalctl does not report a pending NetworkManager sleep request")]
fn journalctl_reports_no_sleep_requested(world: &mut LgBuddyWorld) {
    world.install_journalctl_stub(false);
}

#[when(regex = r#"I run the command "([^"]+)""#)]
fn run_command(world: &mut LgBuddyWorld, command: String) {
    world.run_named_command(&command);
}

#[then("the command succeeds")]
fn command_succeeds(world: &mut LgBuddyWorld) {
    assert!(
        world.command_result().success,
        "command failed\nstdout:\n{}\nstderr:\n{}",
        world.command_result().stdout,
        world.command_result().stderr
    );
}

#[then("the command fails")]
fn command_fails(world: &mut LgBuddyWorld) {
    assert!(
        !world.command_result().success,
        "command unexpectedly succeeded\nstdout:\n{}\nstderr:\n{}",
        world.command_result().stdout,
        world.command_result().stderr
    );
}

#[then(regex = r#"stdout contains "([^"]+)""#)]
fn stdout_contains(world: &mut LgBuddyWorld, expected: String) {
    assert!(
        world.command_result().stdout.contains(&expected),
        "stdout was: {}",
        world.command_result().stdout
    );
}

#[then(regex = r#"stderr contains "([^"]+)""#)]
fn stderr_contains(world: &mut LgBuddyWorld, expected: String) {
    assert!(
        world.command_result().stderr.contains(&expected),
        "stderr was: {}",
        world.command_result().stderr
    );
}

#[then(regex = r#"nm-online was invoked with "([^"]+)""#)]
fn nm_online_invoked_with(world: &mut LgBuddyWorld, expected: String) {
    let argv = expected.split_whitespace().collect::<Vec<_>>();
    world.assert_nm_online_invoked_with(&argv);
}

#[then(regex = r#"stdout is "([^"]+)""#)]
fn stdout_is(world: &mut LgBuddyWorld, expected: String) {
    assert_eq!(world.command_result().stdout.trim(), expected);
}

#[then("the session marker exists")]
fn session_marker_exists_then(world: &mut LgBuddyWorld) {
    world.runtime().assert_session_marker_exists();
}

#[then("the session marker is absent")]
fn session_marker_absent(world: &mut LgBuddyWorld) {
    world.runtime().assert_session_marker_absent();
}

#[then("the system marker exists")]
fn system_marker_exists_then(world: &mut LgBuddyWorld) {
    world.runtime().assert_system_marker_exists();
}

#[then("the system marker is absent")]
fn system_marker_absent(world: &mut LgBuddyWorld) {
    world.runtime().assert_system_marker_absent();
}

#[then(regex = r#"the TV input is (HDMI_[1-4])"#)]
fn tv_input_is(world: &mut LgBuddyWorld, input: String) {
    assert_eq!(world.tv().state_snapshot().input, input);
}

#[then(regex = r#"the TV brightness is (\d+)"#)]
fn tv_brightness_is(world: &mut LgBuddyWorld, value: u8) {
    assert_eq!(world.tv().state_snapshot().backlight, value);
}

#[then("the TV is powered off")]
fn tv_is_powered_off(world: &mut LgBuddyWorld) {
    assert!(!world.tv().state_snapshot().power_on);
}

#[then("the TV is powered on")]
fn tv_is_powered_on(world: &mut LgBuddyWorld) {
    assert!(world.tv().state_snapshot().power_on);
}

#[then("the TV screen is blanked")]
fn tv_screen_is_blanked(world: &mut LgBuddyWorld) {
    assert!(!world.tv().state_snapshot().screen_on);
}

#[then("the TV screen is visible")]
fn tv_screen_is_visible(world: &mut LgBuddyWorld) {
    assert!(world.tv().state_snapshot().screen_on);
}

#[then(regex = r#"the TV client received "([^"]+)""#)]
fn tv_client_received(world: &mut LgBuddyWorld, command: String) {
    assert!(
        world
            .tv()
            .calls()
            .iter()
            .any(|call| call.command == command),
        "calls were: {:?}",
        world.tv().calls()
    );
}

#[then(regex = r#"the TV client did not receive "([^"]+)""#)]
fn tv_client_did_not_receive(world: &mut LgBuddyWorld, command: String) {
    assert!(
        world
            .tv()
            .calls()
            .iter()
            .all(|call| call.command != command),
        "calls were: {:?}",
        world.tv().calls()
    );
}
