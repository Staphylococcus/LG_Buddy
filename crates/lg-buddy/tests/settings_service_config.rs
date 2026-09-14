mod support;

use dbus::blocking::Connection;
use dbus::channel::MatchingReceiver;
use dbus_crossroads::{Crossroads, MethodErr};
use lg_buddy::settings::{SettingsMutationFailure, SettingsMutationStage};
use lg_buddy::settings_view::{
    BehaviorSetting, EnvironmentSettingsBackend, SettingsApplication, SettingsBackend,
    SettingsIntent,
};
use std::fs;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::thread;
use std::time::Duration;
use support::{ExecutableScript, TestConfigFile, TestEnv};

struct SystemdEnvironment {
    address: String,
    daemon: i32,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SystemdEnvironment {
    fn new(environment: Vec<String>, files: Vec<(String, bool)>, unset: Vec<String>) -> Self {
        let (address, daemon) = support::start_private_session_bus();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker_address = address.clone();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let connection = Connection::new_address(&worker_address).unwrap();
            let mut crossroads = Crossroads::new();
            let manager = crossroads.register("org.freedesktop.systemd1.Manager", |builder| {
                builder.method(
                    "LoadUnit",
                    ("name",),
                    ("unit",),
                    |_, _: &mut (), (name,): (String,)| {
                        if name != "LG_Buddy_screen.service" {
                            return Err(MethodErr::failed("unit not found"));
                        }
                        Ok((dbus::Path::new("/org/freedesktop/systemd1/unit/screen").unwrap(),))
                    },
                );
            });
            let service = crossroads.register("org.freedesktop.systemd1.Service", |builder| {
                builder
                    .property::<Vec<String>, _>("Environment")
                    .get(move |_, _: &mut ()| Ok(environment.clone()));
                builder
                    .property::<Vec<(String, bool)>, _>("EnvironmentFiles")
                    .get(move |_, _: &mut ()| Ok(files.clone()));
                builder
                    .property::<Vec<String>, _>("UnsetEnvironment")
                    .get(move |_, _: &mut ()| Ok(unset.clone()));
            });
            crossroads.insert("/org/freedesktop/systemd1", &[manager], ());
            crossroads.insert("/org/freedesktop/systemd1/unit/screen", &[service], ());
            connection.start_receive(
                dbus::message::MatchRule::new_method_call(),
                Box::new(move |message, connection| {
                    crossroads.handle_message(message, connection).unwrap();
                    true
                }),
            );
            connection
                .request_name("org.freedesktop.systemd1", false, true, false)
                .unwrap();
            ready_tx.send(()).unwrap();
            while !worker_stop.load(Ordering::SeqCst) {
                connection.process(Duration::from_millis(10)).unwrap();
            }
        });
        ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        Self {
            address,
            daemon,
            stop,
            worker: Some(worker),
        }
    }
}

impl Drop for SystemdEnvironment {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
        unsafe {
            libc::kill(self.daemon, libc::SIGTERM);
        }
    }
}

#[test]
fn gui_idle_activation_uses_the_service_config_before_persisting() {
    let config = TestConfigFile::new("service-config-activation");
    let config_path = config
        .path()
        .with_file_name("config with $dollars and 'quotes'.env");
    let alias = config.path().with_file_name("config-alias.env");
    std::os::unix::fs::symlink(&config_path, &alias).unwrap();
    let other = config.path().with_file_name("other.env");
    fs::write(&other, "").unwrap();
    let root = config.path().parent().unwrap().join("installed");
    let pointer = root.join("usr/lib/lg-buddy/config-path");
    let calls = config.path().with_extension("calls");
    let systemctl = ExecutableScript::new(
        "activation-systemctl",
        "systemctl",
        r#"#!/bin/sh
[ "$1" = --user ] || exit 99
printf '%s\n' "$2" >> "$LG_BUDDY_TEST_CALLS"
case "$2" in
  cat|is-enabled) exit 0 ;;
  is-active) [ "$3" = --quiet ] && [ "$4" = graphical-session.target -o -e "$LG_BUDDY_TEST_ACTIVE" ] ;;
  enable|start)
    grep -q '^screen_idle_blank=disabled$' "$LG_BUDDY_CONFIG" || exit 98
    [ "$2" != start ] || touch "$LG_BUDDY_TEST_ACTIVE" ;;
  restart) grep -q '^screen_idle_blank=enabled$' "$LG_BUDDY_CONFIG" ;;
  *) exit 99 ;;
esac
"#,
    );
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_CONFIG", &config_path);
    env.set("LG_BUDDY_INSTALL_ROOT", &root);
    env.set("LG_BUDDY_SYSTEMCTL", systemctl.path());
    env.set("LG_BUDDY_TEST_CALLS", &calls);
    let active = config.path().with_extension("active");
    env.set("LG_BUDDY_TEST_ACTIVE", &active);
    env.remove("LG_BUDDY_SKIP_SYSTEMD_ACTIONS");
    let original = "# preserve settings\nscreen_idle_blank=disabled\nscreen_idle_timeout=731\n";

    for (label, declaration, files, unset, has_pointer, succeeds) in [
        (
            "already active",
            Some(&config_path),
            vec![],
            vec![],
            false,
            true,
        ),
        (
            "declarative",
            Some(&config_path),
            vec![],
            vec![],
            false,
            true,
        ),
        ("native", Some(&config_path), vec![], vec![], true, true),
        ("symlink", Some(&alias), vec![], vec![], false, true),
        (
            "mismatched service",
            Some(&other),
            vec![],
            vec![],
            true,
            false,
        ),
        ("missing declaration", None, vec![], vec![], true, false),
        (
            "removed declaration",
            Some(&config_path),
            vec![],
            vec!["LG_BUDDY_CONFIG".into()],
            false,
            false,
        ),
        (
            "environment file override",
            Some(&config_path),
            vec![("/etc/override.env".into(), false)],
            vec![],
            false,
            false,
        ),
    ] {
        fs::write(&config_path, original).unwrap();
        let _ = fs::remove_file(&calls);
        let _ = fs::remove_file(&active);
        let _ = fs::remove_file(&pointer);
        let initially_active = label == "already active";
        if initially_active {
            fs::write(&active, "").unwrap();
        }
        if has_pointer {
            fs::create_dir_all(pointer.parent().unwrap()).unwrap();
            fs::write(&pointer, config_path.as_os_str().as_encoded_bytes()).unwrap();
        }
        let declaration = declaration
            .map(|path| format!("LG_BUDDY_CONFIG={}", path.display()))
            .into_iter()
            .collect();
        let systemd = SystemdEnvironment::new(declaration, files, unset);
        env.set("DBUS_SESSION_BUS_ADDRESS", &systemd.address);
        let backend = EnvironmentSettingsBackend;
        let (mut app, opening) = SettingsApplication::open();
        app.complete_read(opening.read_operation().unwrap(), backend.read_settings())
            .unwrap();
        let started = app
            .handle_intent(SettingsIntent::SetEnabled {
                setting: BehaviorSetting::ScreenIdleBlank,
                enabled: true,
            })
            .unwrap();
        let operation = started.mutation_operation().unwrap().clone();
        let mut stages = vec![];
        let result = backend.write_setting(operation, &mut |stage| stages.push(stage));
        if succeeds {
            let outcome = result.unwrap_or_else(|error| panic!("{label}: {error:?}"));
            assert!(outcome.apply().is_ok(), "{label}: {:?}", outcome.apply());
            assert_eq!(
                fs::read_to_string(&config_path).unwrap(),
                original.replace("disabled", "enabled")
            );
            let calls = fs::read_to_string(&calls).unwrap();
            assert_eq!(
                calls.lines().any(|action| action == "start"),
                !initially_active
            );
            assert!(calls.lines().any(|action| action == "restart"));
        } else {
            assert!(
                matches!(result, Err(SettingsMutationFailure::Activation(_))),
                "{label}: {result:?}"
            );
            assert!(
                !stages.contains(&SettingsMutationStage::Persisting),
                "{label}"
            );
            assert_eq!(
                fs::read_to_string(&config_path).unwrap(),
                original,
                "{label}"
            );
            assert!(!calls.exists(), "{label}: rejected before service mutation");
        }
    }
}
