use crate::support::{
    ExecutableScript, MockBscpylgtv, MockGdbus, MockNmOnline, MockSwayidle, RuntimeStateLayout,
    TestConfigFile, TestEnv,
};
use cucumber::World;
use std::fmt;
use std::path::Path;
use std::process::Command as ProcessCommand;

#[derive(World, Default)]
pub struct LgBuddyWorld {
    env: Option<TestEnv>,
    config: Option<TestConfigFile>,
    runtime: Option<RuntimeStateLayout>,
    tv: Option<MockBscpylgtv>,
    gdbus: Option<MockGdbus>,
    nm_online: Option<MockNmOnline>,
    swayidle: Option<MockSwayidle>,
    path_scripts: Vec<ExecutableScript>,
    command_result: Option<CommandExecution>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandExecution {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl fmt::Debug for LgBuddyWorld {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LgBuddyWorld")
            .field("config", &self.config.is_some())
            .field("runtime", &self.runtime.is_some())
            .field("tv", &self.tv.is_some())
            .field("gdbus", &self.gdbus.is_some())
            .field("nm_online", &self.nm_online.is_some())
            .field("swayidle", &self.swayidle.is_some())
            .field("path_scripts", &self.path_scripts.len())
            .field("command_result", &self.command_result)
            .finish()
    }
}

impl LgBuddyWorld {
    pub fn create_config(&mut self, input: &str) {
        let config = TestConfigFile::new("cucumber-config");
        config.write_sample(input);
        self.ensure_env().set("LG_BUDDY_CONFIG", config.path());
        self.config = Some(config);
    }

    pub fn create_runtime(&mut self) {
        let runtime = RuntimeStateLayout::new("cucumber-runtime");
        self.ensure_env()
            .set("LG_BUDDY_SESSION_RUNTIME_DIR", runtime.session_dir());
        self.ensure_env()
            .set("LG_BUDDY_SYSTEM_RUNTIME_DIR", runtime.system_dir());
        self.runtime = Some(runtime);
    }

    pub fn create_mock_tv(&mut self) {
        let tv = MockBscpylgtv::new("cucumber-tv");
        let wrapper = tv.command_wrapper("cucumber-tv-wrapper");
        self.ensure_env()
            .set("LG_BUDDY_BSCPYLGTV_COMMAND", wrapper.path());
        self.path_scripts.push(wrapper);
        self.tv = Some(tv);
    }

    pub fn tv(&self) -> &MockBscpylgtv {
        self.tv.as_ref().expect("mock TV configured")
    }

    pub fn tv_mut(&mut self) -> &mut MockBscpylgtv {
        self.tv.as_mut().expect("mock TV configured")
    }

    pub fn runtime(&self) -> &RuntimeStateLayout {
        self.runtime.as_ref().expect("runtime layout configured")
    }

    pub fn command_result(&self) -> &CommandExecution {
        self.command_result
            .as_ref()
            .expect("command result should be present")
    }

    pub fn create_session_marker(&self) {
        self.runtime().create_session_marker();
    }

    pub fn create_system_marker(&self) {
        self.runtime().create_system_marker();
    }

    pub fn isolate_path(&mut self) {
        self.ensure_env().set("PATH", "");
    }

    pub fn set_backend_override(&mut self, backend: &str) {
        self.ensure_env().set("LG_BUDDY_SCREEN_BACKEND", backend);
    }

    pub fn disable_startup_delays(&mut self) {
        self.ensure_env()
            .set("LG_BUDDY_STARTUP_INITIAL_WAKE_DELAY_SECS", "0");
        self.ensure_env()
            .set("LG_BUDDY_STARTUP_RETRY_DELAY_SECS", "0");
    }

    pub fn disable_sleep_delays(&mut self) {
        self.ensure_env()
            .set("LG_BUDDY_SLEEP_RETRY_DELAY_SECS", "0");
    }

    pub fn install_gnome_shell_stub(&mut self) {
        self.ensure_mock_gdbus().set_shell_available(true);
    }

    pub fn gnome_monitor_emit_idle(&mut self) {
        self.ensure_mock_gdbus()
            .push_monitor_line("signal org.gnome.ScreenSaver.ActiveChanged (true,)");
    }

    pub fn gnome_monitor_emit_active(&mut self) {
        self.ensure_mock_gdbus()
            .push_monitor_line("signal org.gnome.ScreenSaver.ActiveChanged (false,)");
    }

    pub fn gnome_monitor_emit_wake_requested(&mut self) {
        self.ensure_mock_gdbus().push_monitor_line(
            "signal time=1.0 sender=:1.2 -> destination=(null destination) serial=2 path=/org/gnome/ScreenSaver; interface=org.gnome.ScreenSaver; member=WakeUpScreen",
        );
    }

    pub fn gnome_idle_monitor_reports_recent_user_activity(&mut self) {
        let gdbus = self.ensure_mock_gdbus();
        gdbus.set_idle_monitor_available(true);
        gdbus.queue_idle_monitor_idletime(1500);
        gdbus.queue_idle_monitor_idletime(0);
    }

    pub fn gnome_monitor_stays_open_briefly(&mut self) {
        self.ensure_mock_gdbus().set_monitor_sleep_secs(1.0);
    }

    pub fn install_swayidle_stub(&mut self) {
        if self.swayidle.is_none() {
            let swayidle = MockSwayidle::new("cucumber-swayidle");
            let wrapper = swayidle.command_wrapper("cucumber-swayidle-wrapper");
            self.prepend_path_script(wrapper);
            self.swayidle = Some(swayidle);
        }
    }

    pub fn install_nm_online_stub(&mut self, status: i64) {
        if self.nm_online.is_none() {
            let nm_online = MockNmOnline::new("cucumber-nm-online");
            let wrapper = nm_online.command_wrapper("cucumber-nm-online-wrapper");
            self.ensure_env().set("LG_BUDDY_NM_ONLINE", wrapper.path());
            self.path_scripts.push(wrapper);
            self.nm_online = Some(nm_online);
        }

        self.nm_online
            .as_ref()
            .expect("mock nm-online configured")
            .set_status(status);
    }

    pub fn assert_nm_online_invoked_with(&self, expected_argv: &[&str]) {
        let expected = expected_argv
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>();
        let invocations = self
            .nm_online
            .as_ref()
            .expect("mock nm-online configured")
            .invocations();
        assert!(
            invocations
                .iter()
                .any(|invocation| invocation.argv == expected),
            "nm-online invocations were: {:?}",
            invocations
        );
    }

    pub fn swayidle_emits_timeout(&mut self) {
        self.install_swayidle_stub();
        self.swayidle
            .as_ref()
            .expect("mock swayidle configured")
            .queue_timeout_emission();
    }

    pub fn swayidle_emits_resume(&mut self) {
        self.install_swayidle_stub();
        self.swayidle
            .as_ref()
            .expect("mock swayidle configured")
            .queue_resume_emission();
    }

    pub fn install_systemctl_stub(&mut self, reboot_pending: bool) {
        let stdout = if reboot_pending {
            "123 reboot.target start running\n"
        } else {
            ""
        };
        let body = format!("#!/bin/sh\ncat <<'EOF'\n{stdout}EOF\n");
        let script = ExecutableScript::new("cucumber-systemctl", "mock-systemctl", &body);
        self.ensure_env().set("LG_BUDDY_SYSTEMCTL", script.path());
        self.path_scripts.push(script);
    }

    pub fn install_journalctl_stub(&mut self, sleep_requested: bool) {
        let stdout = if sleep_requested {
            "manager: sleep: sleep requested\n"
        } else {
            "manager: unrelated state transition\n"
        };
        let body = format!("#!/bin/sh\ncat <<'EOF'\n{stdout}EOF\n");
        let script = ExecutableScript::new("cucumber-journalctl", "mock-journalctl", &body);
        self.ensure_env().set("LG_BUDDY_JOURNALCTL", script.path());
        self.path_scripts.push(script);
    }

    pub fn run_named_command(&mut self, command_line: &str) {
        let args = command_line.split_whitespace().collect::<Vec<_>>();
        let output = ProcessCommand::new(env!("CARGO_BIN_EXE_lg-buddy"))
            .args(args)
            .output()
            .expect("run lg-buddy binary");

        self.command_result = Some(CommandExecution {
            success: output.status.success(),
            stdout: String::from_utf8(output.stdout).expect("utf8 command output"),
            stderr: String::from_utf8(output.stderr).expect("utf8 command stderr"),
        });
    }

    fn prepend_path_script(&mut self, script: ExecutableScript) {
        let dir = script
            .path()
            .parent()
            .expect("script path should have a parent")
            .to_path_buf();
        self.prepend_path_dir(&dir);
        self.path_scripts.push(script);
    }

    fn prepend_path_dir(&mut self, dir: &Path) {
        let current = std::env::var_os("PATH").unwrap_or_default();
        let mut combined = Vec::new();
        combined.push(dir.to_path_buf());
        combined.extend(std::env::split_paths(&current));
        let joined = std::env::join_paths(combined).expect("join PATH entries");
        self.ensure_env().set("PATH", joined);
    }

    fn ensure_env(&mut self) -> &mut TestEnv {
        self.env.get_or_insert_with(TestEnv::new)
    }

    fn ensure_mock_gdbus(&mut self) -> &mut MockGdbus {
        if self.gdbus.is_none() {
            let gdbus = MockGdbus::new("cucumber-gdbus");
            let wrapper = gdbus.command_wrapper("cucumber-gdbus-wrapper");
            self.prepend_path_script(wrapper);
            self.gdbus = Some(gdbus);
        }

        self.gdbus.as_mut().expect("mock gdbus configured")
    }
}
