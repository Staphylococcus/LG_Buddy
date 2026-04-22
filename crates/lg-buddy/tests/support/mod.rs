use serde_json::{json, Map, Value};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
pub struct MockBscpylgtv {
    _temp_dir: TestDir,
    state_path: PathBuf,
}

#[allow(dead_code)]
impl MockBscpylgtv {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let state_path = temp_dir.path().join("state.json");
        let mock = Self {
            _temp_dir: temp_dir,
            state_path,
        };
        mock.save_state(json!({
            "power_on": true,
            "screen_on": true,
            "input": "HDMI_3",
            "backlight": 50,
            "plan": {},
            "calls": [],
        }));
        mock
    }

    pub fn state_path(&self) -> &Path {
        &self.state_path
    }

    pub fn command_path(&self) -> &'static str {
        "python3"
    }

    pub fn command_args(&self) -> Vec<String> {
        vec![
            Self::script_path().to_string_lossy().into_owned(),
            "--state".to_string(),
            self.state_path.to_string_lossy().into_owned(),
        ]
    }

    pub fn set_power_on(&self, value: bool) {
        self.patch_state(json!({ "power_on": value }));
    }

    pub fn set_screen_on(&self, value: bool) {
        self.patch_state(json!({ "screen_on": value }));
    }

    pub fn set_input(&self, value: &str) {
        self.patch_state(json!({ "input": value }));
    }

    pub fn set_backlight(&self, value: u64) {
        self.patch_state(json!({ "backlight": value }));
    }

    pub fn queue_success(&self, command: &str, stdout: &str) {
        self.queue_step(
            command,
            json!({
                "result": "success",
                "stdout": stdout,
            }),
        );
    }

    pub fn queue_error(&self, command: &str, status: i64, stderr: &str) {
        self.queue_step(
            command,
            json!({
                "result": "error",
                "status": status,
                "stderr": stderr,
            }),
        );
    }

    pub fn queue_active_screen_error(&self, command: &str) {
        self.queue_step(
            command,
            json!({
                "result": "active_screen_error",
            }),
        );
    }

    pub fn queue_powered_off_error(&self, command: &str) {
        self.queue_step(
            command,
            json!({
                "result": "powered_off_error",
            }),
        );
    }

    pub fn queue_set_input_wake_success(&self) {
        self.queue_step(
            "set_input",
            json!({
                "result": "success",
                "stdout": "{'returnValue': True}\n",
                "state_update": {
                    "power_on": true
                }
            }),
        );
    }

    pub fn calls(&self) -> Vec<MockInvocation> {
        self.load_state()
            .get("calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(MockInvocation::from_value)
            .collect()
    }

    pub fn state_snapshot(&self) -> MockStateSnapshot {
        let state = self.load_state();
        MockStateSnapshot {
            power_on: state
                .get("power_on")
                .and_then(Value::as_bool)
                .expect("mock state power_on bool"),
            screen_on: state
                .get("screen_on")
                .and_then(Value::as_bool)
                .expect("mock state screen_on bool"),
            input: state
                .get("input")
                .and_then(Value::as_str)
                .expect("mock state input string")
                .to_string(),
            backlight: state
                .get("backlight")
                .and_then(Value::as_u64)
                .expect("mock state backlight integer") as u8,
        }
    }

    pub fn command_wrapper(&self, label: &str) -> ExecutableScript {
        let python_path = shell_quote(&python3_path());
        let script_path = shell_quote(&Self::script_path());
        let state_path = shell_quote(&self.state_path);
        let body =
            format!("#!/bin/sh\nexec {python_path} {script_path} --state {state_path} \"$@\"\n");

        ExecutableScript::new(label, "mock-bscpylgtvcommand", &body)
    }

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tools")
            .join("mock_bscpylgtvcommand.py")
    }

    fn queue_step(&self, command: &str, step: Value) {
        let mut state = self.load_state();
        let plan = state
            .as_object_mut()
            .expect("mock state object")
            .entry("plan")
            .or_insert_with(|| Value::Object(Map::new()));
        let plan = plan.as_object_mut().expect("plan object");
        let steps = plan
            .entry(command.to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        steps.as_array_mut().expect("plan command array").push(step);
        self.save_state(state);
    }

    fn patch_state(&self, patch: Value) {
        let mut state = self.load_state();
        let state_object = state.as_object_mut().expect("mock state object");
        let patch_object = patch.as_object().expect("state patch object");
        for (key, value) in patch_object {
            state_object.insert(key.clone(), value.clone());
        }
        self.save_state(state);
    }

    fn load_state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.state_path).expect("read mock state"))
            .expect("parse mock state")
    }

    fn save_state(&self, state: Value) {
        fs::write(
            &self.state_path,
            serde_json::to_string_pretty(&state).expect("serialize mock state"),
        )
        .expect("write mock state");
    }
}

#[allow(dead_code)]
pub struct MockSwayidle {
    _temp_dir: TestDir,
    state_path: PathBuf,
}

#[allow(dead_code)]
impl MockSwayidle {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let state_path = temp_dir.path().join("state.json");
        let mock = Self {
            _temp_dir: temp_dir,
            state_path,
        };
        mock.save_state(json!({
            "help_mode": "systemd",
            "emissions": [],
            "invocations": [],
        }));
        mock
    }

    pub fn command_path(&self) -> &'static str {
        "python3"
    }

    pub fn command_args(&self) -> Vec<String> {
        vec![
            Self::script_path().to_string_lossy().into_owned(),
            "--state".to_string(),
            self.state_path.to_string_lossy().into_owned(),
        ]
    }

    pub fn command_wrapper(&self, label: &str) -> ExecutableScript {
        let python_path = shell_quote(&python3_path());
        let script_path = shell_quote(&Self::script_path());
        let state_path = shell_quote(&self.state_path);
        let body =
            format!("#!/bin/sh\nexec {python_path} {script_path} --state {state_path} \"$@\"\n");

        ExecutableScript::new(label, "swayidle", &body)
    }

    pub fn disable_systemd_hooks_in_help(&self) {
        self.patch_state(json!({ "help_mode": "minimal" }));
    }

    pub fn queue_timeout_emission(&self) {
        self.queue_emission("timeout");
    }

    pub fn queue_resume_emission(&self) {
        self.queue_emission("resume");
    }

    pub fn queue_before_sleep_emission(&self) {
        self.queue_emission("before-sleep");
    }

    pub fn queue_after_resume_emission(&self) {
        self.queue_emission("after-resume");
    }

    pub fn invocations(&self) -> Vec<MockSwayidleInvocation> {
        self.load_state()
            .get("invocations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(MockSwayidleInvocation::from_value)
            .collect()
    }

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tools")
            .join("mock_swayidle.py")
    }

    fn queue_emission(&self, emission: &str) {
        let mut state = self.load_state();
        let emissions = state
            .as_object_mut()
            .expect("mock swayidle state object")
            .entry("emissions")
            .or_insert_with(|| Value::Array(Vec::new()));
        emissions
            .as_array_mut()
            .expect("emissions array")
            .push(Value::String(emission.to_string()));
        self.save_state(state);
    }

    fn patch_state(&self, patch: Value) {
        let mut state = self.load_state();
        let state_object = state.as_object_mut().expect("mock state object");
        let patch_object = patch.as_object().expect("state patch object");
        for (key, value) in patch_object {
            state_object.insert(key.clone(), value.clone());
        }
        self.save_state(state);
    }

    fn load_state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.state_path).expect("read mock state"))
            .expect("parse mock state")
    }

    fn save_state(&self, state: Value) {
        fs::write(
            &self.state_path,
            serde_json::to_string_pretty(&state).expect("serialize mock state"),
        )
        .expect("write mock state");
    }
}

#[allow(dead_code)]
pub struct MockGdbus {
    _temp_dir: TestDir,
    state_path: PathBuf,
}

#[allow(dead_code)]
impl MockGdbus {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let state_path = temp_dir.path().join("state.json");
        let mock = Self {
            _temp_dir: temp_dir,
            state_path,
        };
        mock.save_state(json!({
            "shell_available": true,
            "screen_saver_available": true,
            "idle_monitor_available": true,
            "idle_monitor_idletime": 1500,
            "idle_monitor_idletime_plan": [],
            "monitor_lines": [],
            "monitor_sleep_secs": 0.0,
            "invocations": [],
        }));
        mock
    }

    pub fn command_path(&self) -> &'static str {
        "python3"
    }

    pub fn command_args(&self) -> Vec<String> {
        vec![
            Self::script_path().to_string_lossy().into_owned(),
            "--state".to_string(),
            self.state_path.to_string_lossy().into_owned(),
        ]
    }

    pub fn command_wrapper(&self, label: &str) -> ExecutableScript {
        let python_path = shell_quote(&python3_path());
        let script_path = shell_quote(&Self::script_path());
        let state_path = shell_quote(&self.state_path);
        let body =
            format!("#!/bin/sh\nexec {python_path} {script_path} --state {state_path} \"$@\"\n");

        ExecutableScript::new(label, "gdbus", &body)
    }

    pub fn set_shell_available(&self, value: bool) {
        self.patch_state(json!({ "shell_available": value }));
    }

    pub fn set_screen_saver_available(&self, value: bool) {
        self.patch_state(json!({ "screen_saver_available": value }));
    }

    pub fn set_idle_monitor_available(&self, value: bool) {
        self.patch_state(json!({ "idle_monitor_available": value }));
    }

    pub fn set_idle_monitor_idletime(&self, value: u64) {
        self.patch_state(json!({ "idle_monitor_idletime": value }));
    }

    pub fn set_idle_monitor_idletime_plan(&self, values: &[u64]) {
        let plan = values.iter().copied().map(Value::from).collect::<Vec<_>>();
        self.patch_state(json!({ "idle_monitor_idletime_plan": plan }));
    }

    pub fn queue_idle_monitor_idletime(&self, value: u64) {
        let mut state = self.load_state();
        let plan = state
            .as_object_mut()
            .expect("mock gdbus state object")
            .entry("idle_monitor_idletime_plan")
            .or_insert_with(|| Value::Array(Vec::new()));
        plan.as_array_mut()
            .expect("idle monitor idletime plan array")
            .push(Value::from(value));
        self.save_state(state);
    }

    pub fn set_monitor_sleep_secs(&self, value: f64) {
        self.patch_state(json!({ "monitor_sleep_secs": value }));
    }

    pub fn clear_monitor_lines(&self) {
        self.patch_state(json!({ "monitor_lines": [] }));
    }

    pub fn push_monitor_line(&self, line: &str) {
        let mut state = self.load_state();
        let monitor_lines = state
            .as_object_mut()
            .expect("mock gdbus state object")
            .entry("monitor_lines")
            .or_insert_with(|| Value::Array(Vec::new()));
        monitor_lines
            .as_array_mut()
            .expect("monitor lines array")
            .push(Value::String(line.to_string()));
        self.save_state(state);
    }

    pub fn invocations(&self) -> Vec<MockGdbusInvocation> {
        self.load_state()
            .get("invocations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(MockGdbusInvocation::from_value)
            .collect()
    }

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tools")
            .join("mock_gdbus.py")
    }

    fn patch_state(&self, patch: Value) {
        let mut state = self.load_state();
        let state_object = state.as_object_mut().expect("mock state object");
        let patch_object = patch.as_object().expect("state patch object");
        for (key, value) in patch_object {
            state_object.insert(key.clone(), value.clone());
        }
        self.save_state(state);
    }

    fn load_state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.state_path).expect("read mock state"))
            .expect("parse mock state")
    }

    fn save_state(&self, state: Value) {
        fs::write(
            &self.state_path,
            serde_json::to_string_pretty(&state).expect("serialize mock state"),
        )
        .expect("write mock state");
    }
}

#[allow(dead_code)]
pub struct MockNmOnline {
    _temp_dir: TestDir,
    state_path: PathBuf,
}

#[allow(dead_code)]
impl MockNmOnline {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let state_path = temp_dir.path().join("state.json");
        let mock = Self {
            _temp_dir: temp_dir,
            state_path,
        };
        mock.save_state(json!({
            "status": 0,
            "invocations": [],
        }));
        mock
    }

    pub fn command_wrapper(&self, label: &str) -> ExecutableScript {
        let python_path = shell_quote(&python3_path());
        let script_path = shell_quote(&Self::script_path());
        let state_path = shell_quote(&self.state_path);
        let body =
            format!("#!/bin/sh\nexec {python_path} {script_path} --state {state_path} \"$@\"\n");

        ExecutableScript::new(label, "mock-nm-online", &body)
    }

    pub fn set_status(&self, status: i64) {
        self.patch_state(json!({ "status": status }));
    }

    pub fn invocations(&self) -> Vec<MockNmOnlineInvocation> {
        self.load_state()
            .get("invocations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(MockNmOnlineInvocation::from_value)
            .collect()
    }

    fn script_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tools")
            .join("mock_nm_online.py")
    }

    fn patch_state(&self, patch: Value) {
        let mut state = self.load_state();
        let state_object = state.as_object_mut().expect("mock state object");
        let patch_object = patch.as_object().expect("state patch object");
        for (key, value) in patch_object {
            state_object.insert(key.clone(), value.clone());
        }
        self.save_state(state);
    }

    fn load_state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.state_path).expect("read mock state"))
            .expect("parse mock state")
    }

    fn save_state(&self, state: Value) {
        fs::write(
            &self.state_path,
            serde_json::to_string_pretty(&state).expect("serialize mock state"),
        )
        .expect("write mock state");
    }
}

#[allow(dead_code)]
pub struct TestEnv {
    _guard: MutexGuard<'static, ()>,
    original_values: Vec<(OsString, Option<OsString>)>,
}

#[allow(dead_code)]
impl TestEnv {
    pub fn new() -> Self {
        Self {
            _guard: env_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            original_values: Vec::new(),
        }
    }

    pub fn set<K, V>(&mut self, key: K, value: V)
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        let key = key.as_ref().to_os_string();
        self.remember_original_value(&key);
        env::set_var(&key, value.as_ref());
    }

    pub fn remove<K>(&mut self, key: K)
    where
        K: AsRef<OsStr>,
    {
        let key = key.as_ref().to_os_string();
        self.remember_original_value(&key);
        env::remove_var(&key);
    }

    fn remember_original_value(&mut self, key: &OsStr) {
        if self.original_values.iter().any(|(saved, _)| saved == key) {
            return;
        }

        self.original_values
            .push((key.to_os_string(), env::var_os(key)));
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        for (key, value) in self.original_values.iter().rev() {
            match value {
                Some(value) => env::set_var(key, value),
                None => env::remove_var(key),
            }
        }
    }
}

#[allow(dead_code)]
pub struct TestConfigFile {
    _temp_dir: TestDir,
    path: PathBuf,
}

#[allow(dead_code)]
impl TestConfigFile {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let path = temp_dir.path().join("config.env");
        Self {
            _temp_dir: temp_dir,
            path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_contents(&self, contents: &str) {
        fs::write(&self.path, contents).expect("write temp config");
    }

    pub fn append_line(&self, line: &str) {
        let mut contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == ErrorKind::NotFound => String::new(),
            Err(err) => panic!("read temp config: {err}"),
        };
        if !contents.is_empty() && !contents.ends_with('\n') {
            contents.push('\n');
        }
        contents.push_str(line);
        contents.push('\n');
        self.write_contents(&contents);
    }

    pub fn write_sample(&self, input: &str) {
        self.write_contents(&sample_config_contents(input));
    }
}

#[allow(dead_code)]
pub fn sample_config_contents(input: &str) -> String {
    format!(
        "tv_ip=192.0.2.42\n\
tv_mac=aa:bb:cc:dd:ee:ff\n\
input={input}\n\
screen_backend=auto\n\
screen_idle_timeout=300\n"
    )
}

#[allow(dead_code)]
pub struct RuntimeStateLayout {
    _temp_dir: TestDir,
    root: PathBuf,
}

#[allow(dead_code)]
impl RuntimeStateLayout {
    pub fn new(label: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let root = temp_dir.path().to_path_buf();
        Self {
            _temp_dir: temp_dir,
            root,
        }
    }

    pub fn session_dir(&self) -> PathBuf {
        self.root.join("session")
    }

    pub fn system_dir(&self) -> PathBuf {
        self.root.join("system")
    }

    pub fn session_marker_path(&self) -> PathBuf {
        self.session_dir().join("screen_off_by_us")
    }

    pub fn system_marker_path(&self) -> PathBuf {
        self.system_dir().join("screen_off_by_us")
    }

    pub fn create_session_marker(&self) {
        self.create_marker(&self.session_marker_path());
    }

    pub fn create_system_marker(&self) {
        self.create_marker(&self.system_marker_path());
    }

    pub fn assert_session_marker_exists(&self) {
        assert!(
            self.session_marker_path().is_file(),
            "expected session marker at {}",
            self.session_marker_path().display()
        );
    }

    pub fn assert_session_marker_absent(&self) {
        assert!(
            !self.session_marker_path().exists(),
            "did not expect session marker at {}",
            self.session_marker_path().display()
        );
    }

    pub fn assert_system_marker_exists(&self) {
        assert!(
            self.system_marker_path().is_file(),
            "expected system marker at {}",
            self.system_marker_path().display()
        );
    }

    pub fn assert_system_marker_absent(&self) {
        assert!(
            !self.system_marker_path().exists(),
            "did not expect system marker at {}",
            self.system_marker_path().display()
        );
    }

    fn create_marker(&self, path: &Path) {
        let parent = path.parent().expect("marker parent");
        fs::create_dir_all(parent).expect("create marker parent");
        fs::write(path, []).expect("write marker");
    }
}

#[allow(dead_code)]
pub struct ExecutableScript {
    _temp_dir: TestDir,
    path: PathBuf,
}

#[allow(dead_code)]
impl ExecutableScript {
    pub fn new(label: &str, file_name: &str, body: &str) -> Self {
        let temp_dir = TestDir::new(label);
        let path = temp_dir.path().join(file_name);
        fs::write(&path, body).expect("write executable script");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("set script permissions");
        }

        Self {
            _temp_dir: temp_dir,
            path,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockInvocation {
    pub tv_ip: String,
    pub command: String,
    pub args: Vec<String>,
    pub key_file_path: Option<String>,
    pub user: Option<String>,
}

impl MockInvocation {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock invocation object");
        Self {
            tv_ip: object
                .get("tv_ip")
                .and_then(Value::as_str)
                .expect("invocation tv_ip string")
                .to_string(),
            command: object
                .get("command")
                .and_then(Value::as_str)
                .expect("invocation command string")
                .to_string(),
            args: object
                .get("args")
                .and_then(Value::as_array)
                .expect("invocation args array")
                .iter()
                .map(|value| value.as_str().expect("invocation arg string").to_string())
                .collect(),
            key_file_path: object
                .get("key_file_path")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            user: object
                .get("user")
                .and_then(Value::as_str)
                .map(ToString::to_string),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct MockStateSnapshot {
    pub power_on: bool,
    pub screen_on: bool,
    pub input: String,
    pub backlight: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct MockSwayidleInvocation {
    pub argv: Vec<String>,
    pub wait: bool,
    pub debug: bool,
    pub config_path: Option<String>,
    pub seat: Option<String>,
    pub events: Vec<MockSwayidleEvent>,
}

impl MockSwayidleInvocation {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock swayidle invocation object");
        Self {
            argv: object
                .get("argv")
                .and_then(Value::as_array)
                .expect("invocation argv array")
                .iter()
                .map(|value| value.as_str().expect("argv string").to_string())
                .collect(),
            wait: object
                .get("wait")
                .and_then(Value::as_bool)
                .expect("invocation wait bool"),
            debug: object
                .get("debug")
                .and_then(Value::as_bool)
                .expect("invocation debug bool"),
            config_path: object
                .get("config_path")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            seat: object
                .get("seat")
                .and_then(Value::as_str)
                .map(ToString::to_string),
            events: object
                .get("events")
                .and_then(Value::as_array)
                .expect("invocation events array")
                .iter()
                .map(MockSwayidleEvent::from_value)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct MockGdbusInvocation {
    pub argv: Vec<String>,
}

impl MockGdbusInvocation {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock gdbus invocation object");
        Self {
            argv: object
                .get("argv")
                .and_then(Value::as_array)
                .expect("invocation argv array")
                .iter()
                .map(|value| value.as_str().expect("argv string").to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct MockNmOnlineInvocation {
    pub argv: Vec<String>,
}

impl MockNmOnlineInvocation {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock nm-online invocation object");
        Self {
            argv: object
                .get("argv")
                .and_then(Value::as_array)
                .expect("invocation argv array")
                .iter()
                .map(|value| value.as_str().expect("argv string").to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MockSwayidleEvent {
    Timeout {
        timeout: u64,
        command: String,
        resume: Option<String>,
    },
    BeforeSleep {
        command: String,
    },
    AfterResume {
        command: String,
    },
    Lock {
        command: String,
    },
    Unlock {
        command: String,
    },
    Idlehint {
        timeout: u64,
    },
}

impl MockSwayidleEvent {
    fn from_value(value: &Value) -> Self {
        let object = value.as_object().expect("mock swayidle event object");
        let kind = object
            .get("kind")
            .and_then(Value::as_str)
            .expect("event kind string");

        match kind {
            "timeout" => Self::Timeout {
                timeout: object
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .expect("timeout value"),
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("timeout command")
                    .to_string(),
                resume: object
                    .get("resume")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            },
            "before-sleep" => Self::BeforeSleep {
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("before-sleep command")
                    .to_string(),
            },
            "after-resume" => Self::AfterResume {
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("after-resume command")
                    .to_string(),
            },
            "lock" => Self::Lock {
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("lock command")
                    .to_string(),
            },
            "unlock" => Self::Unlock {
                command: object
                    .get("command")
                    .and_then(Value::as_str)
                    .expect("unlock command")
                    .to_string(),
            },
            "idlehint" => Self::Idlehint {
                timeout: object
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .expect("idlehint timeout"),
            },
            other => panic!("unsupported mock swayidle event kind `{other}`"),
        }
    }
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

        fs::create_dir_all(&path).expect("create temp dir");
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

fn env_lock() -> &'static Mutex<()> {
    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    ENV_LOCK.get_or_init(|| Mutex::new(()))
}

fn python3_path() -> PathBuf {
    static PYTHON3_PATH: OnceLock<PathBuf> = OnceLock::new();

    PYTHON3_PATH
        .get_or_init(|| {
            find_command_in_path("python3")
                .or_else(|| find_command_in_path("python"))
                .or_else(find_python3_in_standard_locations)
                .unwrap_or_else(|| PathBuf::from("python3"))
        })
        .clone()
}

fn find_command_in_path(command: &str) -> Option<PathBuf> {
    if command.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(command);
        return path.is_file().then_some(path);
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| {
        let candidate = dir.join(command);
        candidate.is_file().then_some(candidate)
    })
}

fn find_python3_in_standard_locations() -> Option<PathBuf> {
    [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/bin/python3",
        "/usr/bin/python",
        "/usr/local/bin/python",
        "/bin/python",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|candidate| candidate.is_file())
}

fn shell_quote(path: &Path) -> String {
    let rendered = path.to_string_lossy().replace('\'', "'\"'\"'");
    format!("'{rendered}'")
}
