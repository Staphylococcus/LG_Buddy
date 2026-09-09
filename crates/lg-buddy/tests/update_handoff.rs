use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use lg_buddy::update_flow::{EnvironmentUpdateInstallBackend, UpdateInstallBackend};
use lg_buddy::update_install::InstalledUpdate;
use lg_buddy::updates::UpdateChannel;
use semver::Version;

const CHILD_ENV: &str = "LG_BUDDY_HANDOFF_CHILD";
const MARKER_ENV: &str = "LG_BUDDY_HANDOFF_MARKER";
const INSTALLED_GUI_ENV: &str = "LG_BUDDY_HANDOFF_INSTALLED_GUI";
const DECOY_MARKER_ENV: &str = "LG_BUDDY_HANDOFF_DECOY_MARKER";

#[test]
fn handoff_successor_child() {
    if std::env::var_os(CHILD_ENV).is_none() {
        return;
    }

    let gui_path = std::env::var_os(INSTALLED_GUI_ENV)
        .map(PathBuf::from)
        .expect("handoff child needs the installed GUI path");
    let installed = installed_update(&gui_path);
    let result = EnvironmentUpdateInstallBackend.relaunch(&installed);
    panic!("relaunch returned instead of exec: {result:?}");
}

#[test]
fn relaunch_execs_the_verified_absolute_gui_and_preserves_identity() {
    let temp = TestDirectory::new("lg-buddy-handoff");
    let installed_dir = temp.path.join("installed");
    let decoy_dir = temp.path.join("decoy");
    fs::create_dir_all(&installed_dir).unwrap();
    fs::create_dir_all(&decoy_dir).unwrap();

    let marker = temp.path.join("successor.marker");
    let decoy_marker = temp.path.join("decoy.marker");
    let installed_gui = installed_dir.join("lg-buddy-gui");
    let decoy_gui = decoy_dir.join("lg-buddy-gui");
    write_successor(&installed_gui, false);
    write_successor(&decoy_gui, true);

    let child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("handoff_successor_child")
        .arg("--nocapture")
        .env(CHILD_ENV, "1")
        .env(MARKER_ENV, &marker)
        .env(INSTALLED_GUI_ENV, &installed_gui)
        .env(DECOY_MARKER_ENV, &decoy_marker)
        .env("LG_BUDDY_GUI", &decoy_gui)
        .env("PATH", &decoy_dir)
        .spawn()
        .unwrap();
    let child_pid = child.id();
    let status = child.wait_with_output().unwrap();
    assert!(
        status.status.success(),
        "handoff child failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let report = fs::read_to_string(&marker).expect("the installed GUI must report its identity");
    assert_eq!(report.lines().count(), 1, "only one successor may run");
    let fields = parse_marker(&report);
    assert_eq!(fields.get("count").map(String::as_str), Some("1"));
    assert_eq!(
        fields
            .get("pid")
            .and_then(|value| value.parse::<u32>().ok()),
        Some(child_pid),
        "the successor must replace the child process through exec"
    );
    assert_eq!(
        fields.get("path").map(String::as_str),
        Some(installed_gui.to_str().unwrap()),
        "handoff must use the verified installed GUI path"
    );
    assert_eq!(fields.get("version").map(String::as_str), Some("1.7.0"));
    assert_eq!(fields.get("channel").map(String::as_str), Some("stable"));
    assert_eq!(
        fields.get("target").map(String::as_str),
        Some("x86_64-unknown-linux-gnu")
    );
    assert_eq!(
        fields.get("commit").map(String::as_str),
        Some("newer-commit")
    );
    assert_eq!(
        fields.get("replace").map(String::as_str),
        Some("--gapplication-replace"),
        "handoff must request GApplication name replacement explicitly"
    );
    assert!(
        !decoy_marker.exists(),
        "LG_BUDDY_GUI and PATH decoys must not be used for handoff"
    );

    let missing = temp.path.join("missing-gui");
    let error = EnvironmentUpdateInstallBackend
        .relaunch(&installed_update(&missing))
        .expect_err("a missing installed GUI must return an actionable error");
    assert_eq!(
        error.presentation.summary(),
        "Update installed; restart failed"
    );
    assert!(error.presentation.detail().contains("could not be started"));
    assert!(error
        .diagnostic
        .contains("could not replace the running GUI"));
}

fn installed_update(gui_path: &Path) -> InstalledUpdate {
    InstalledUpdate::from_parts(
        Version::parse("1.7.0").unwrap(),
        UpdateChannel::Stable,
        "v1.7.0",
        "x86_64-unknown-linux-gnu",
        "newer-commit",
        "/unused/lg-buddy",
        gui_path,
    )
}

fn write_successor(path: &Path, decoy: bool) {
    let body = if decoy {
        "#!/bin/sh\nprintf 'decoy\\n' >> \"$LG_BUDDY_HANDOFF_DECOY_MARKER\"\n"
    } else {
        "#!/bin/sh\nprintf 'count=1 pid=%s path=%s version=1.7.0 channel=stable target=x86_64-unknown-linux-gnu commit=newer-commit replace=%s\\n' \"$$\" \"$0\" \"$1\" >> \"$LG_BUDDY_HANDOFF_MARKER\"\n"
    };
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn parse_marker(report: &str) -> std::collections::HashMap<String, String> {
    report
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|field| field.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
