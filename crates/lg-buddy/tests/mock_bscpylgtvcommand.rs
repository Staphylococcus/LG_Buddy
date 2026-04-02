mod support;

use lg_buddy::config::HdmiInput;
use lg_buddy::tv::{BscpylgtvCommandClient, TvClient, TvError};
use std::net::Ipv4Addr;
use support::MockBscpylgtv;

#[test]
fn mock_get_input_matches_real_shape() {
    let mock = MockBscpylgtv::new("mock-get-input");
    let client = mock_client(&mock);

    let input = client
        .get_input(ip("10.0.0.39"))
        .expect("mock get_input should succeed");

    assert_eq!(input, "com.webos.app.hdmi3");
}

#[test]
fn mock_set_input_returns_realistic_success_payload() {
    let mock = MockBscpylgtv::new("mock-set-input");
    let client = mock_client(&mock);

    let output = client
        .set_input(ip("10.0.0.39"), HdmiInput::Hdmi2)
        .expect("mock set_input should succeed");

    assert_eq!(output.stdout(), "{'returnValue': True}\n");
    assert_eq!(mock.state_snapshot().input, "HDMI_2");
    assert!(mock.state_snapshot().screen_on);
}

#[test]
fn mock_set_settings_updates_backlight() {
    let mock = MockBscpylgtv::new("mock-set-settings");
    let client = mock_client(&mock);

    let output = client
        .set_oled_brightness(ip("10.0.0.39"), 70)
        .expect("mock set_oled_brightness should succeed");

    assert_eq!(output.stdout(), "{'returnValue': True}\n");
    assert_eq!(mock.state_snapshot().backlight, 70);
}

#[test]
fn mock_get_picture_settings_includes_backlight() {
    let mock = MockBscpylgtv::new("mock-get-picture-settings");
    mock.set_backlight(62);
    let client = mock_client(&mock);

    let brightness = client
        .get_oled_brightness(ip("10.0.0.39"))
        .expect("mock get_oled_brightness should succeed");

    assert_eq!(brightness, 62);
}

#[test]
fn mock_turn_screen_on_active_error_matches_real_traceback_shape() {
    let mock = MockBscpylgtv::new("mock-turn-screen-on-active");
    let client = mock_client(&mock);

    let err = client
        .turn_screen_on(ip("10.0.0.39"))
        .expect_err("active screen should fail");

    match err {
        TvError::CommandFailed { status, output, .. } => {
            assert_eq!(status, Some(1));
            assert!(
                output
                    .stderr()
                    .contains("bscpylgtv.exceptions.PyLGTVCmdError"),
                "stderr was: {}",
                output.stderr()
            );
            assert!(
                output.stderr().contains("errorCode': '-102'"),
                "stderr was: {}",
                output.stderr()
            );
        }
        other => panic!("expected command failure, got {other:?}"),
    }
}

#[test]
fn mock_tracks_screen_and_power_state_transitions() {
    let mock = MockBscpylgtv::new("mock-state-transitions");
    let client = mock_client(&mock);

    client
        .turn_screen_off(ip("10.0.0.39"))
        .expect("turn_screen_off should succeed");
    assert!(!mock.state_snapshot().screen_on);

    client
        .turn_screen_on(ip("10.0.0.39"))
        .expect("turn_screen_on should succeed from blank state");
    assert!(mock.state_snapshot().screen_on);

    client
        .power_off(ip("10.0.0.39"))
        .expect("power_off should succeed");
    let state = mock.state_snapshot();
    assert!(!state.power_on);
    assert!(!state.screen_on);
}

#[test]
fn mock_rejects_input_queries_when_powered_off() {
    let mock = MockBscpylgtv::new("mock-powered-off-query");
    let client = mock_client(&mock);

    client
        .power_off(ip("10.0.0.39"))
        .expect("power_off should succeed");

    let err = client
        .get_input(ip("10.0.0.39"))
        .expect_err("get_input should fail when off");

    match err {
        TvError::CommandFailed { output, .. } => {
            assert!(output.stderr().contains("TV is off"));
        }
        other => panic!("expected command failure, got {other:?}"),
    }
}

#[test]
fn mock_records_invocations_and_can_override_outputs() {
    let mock = MockBscpylgtv::new("mock-call-log");
    mock.queue_success("get_input", "\nignored\ncom.webos.app.hdmi2\n");
    let client = mock_client(&mock);

    let input = client
        .get_input(ip("10.0.0.39"))
        .expect("planned get_input should succeed");

    assert_eq!(input, "com.webos.app.hdmi2");
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| (call.tv_ip, call.command, call.args))
            .collect::<Vec<_>>(),
        vec![(
            "10.0.0.39".to_string(),
            "get_input".to_string(),
            Vec::<String>::new(),
        )]
    );
}

fn mock_client(mock: &MockBscpylgtv) -> BscpylgtvCommandClient {
    BscpylgtvCommandClient::with_args(mock.command_path(), mock.command_args())
}

fn ip(value: &str) -> Ipv4Addr {
    value.parse().expect("parse IPv4 address")
}
