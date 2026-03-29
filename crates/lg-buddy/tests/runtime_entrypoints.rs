mod support;

use lg_buddy::commands::{run_screen_off, run_screen_on};
use support::{MockBscpylgtv, RuntimeStateLayout, TestConfigFile, TestEnv};

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
    assert_eq!(
        mock.calls()
            .into_iter()
            .map(|call| call.command)
            .collect::<Vec<_>>(),
        vec!["get_input".to_string(), "turn_screen_off".to_string()]
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
