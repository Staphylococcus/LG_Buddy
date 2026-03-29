use serde_json::{json, Map, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
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
        }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockInvocation {
    pub tv_ip: String,
    pub command: String,
    pub args: Vec<String>,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct MockStateSnapshot {
    pub power_on: bool,
    pub screen_on: bool,
    pub input: String,
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
