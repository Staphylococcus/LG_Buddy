//! Read-only compositor information and historical native setup output.

use super::report::{format_utc_timestamp, safe_text, MAX_SECTION_BYTES};
use super::DiagnosticSection;
use crate::kwin_bridge::{self, KWinInfo};
use crate::session_bus::SessionBusError;
use std::fs::OpenOptions;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const MAX_LOG_CAPTURE: u64 = 16 * 1024;
const MAX_LOG_LINES: usize = 40;
const OMITTED: &str = "[Earlier setup output omitted]\n";

pub(super) fn collect() -> Vec<DiagnosticSection> {
    let state = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let log = match setup_log_path(state.as_deref(), home.as_deref()) {
        Some(path) => setup_log(&path),
        None => DiagnosticSection::log("KWin setup (historical)", "State directory unavailable"),
    };
    vec![compositor(kwin_bridge::inspect()), log]
}

fn compositor(result: Result<Option<KWinInfo>, SessionBusError>) -> DiagnosticSection {
    let body = match result {
        Ok(Some(info)) => format!(
            "running KWin version: {}\nrunning Qt version: {}\nQt plugin directory: {}\n",
            info.version,
            info.qt_version,
            info.plugin_directory
                .unwrap_or("not detected (unsupported layout)"),
        ),
        Ok(None) => {
            "No running KWin Wayland compositor; Plasma integration is not applicable.\n".into()
        }
        Err(error) => format!("Read-only compositor inspection unavailable: {error}\n"),
    };
    DiagnosticSection::new("KWin compositor", body)
}

fn setup_log_path(state: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    state
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join(".local/state")))
        .map(|state| state.join("lg-buddy/kwin/setup.log"))
}

fn setup_log(path: &Path) -> DiagnosticSection {
    let body = match read_log_tail(path) {
        Ok(body) => body,
        Err(error) if error.kind() == io::ErrorKind::NotFound => "No setup log found.\n".into(),
        Err(error) => format!("Setup log unavailable: {error}\n"),
    };
    DiagnosticSection::log("KWin setup (historical)", body)
}

fn read_log_tail(path: &Path) -> io::Result<String> {
    // Never block on a substituted pipe or follow a substituted log symlink.
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|time| format_utc_timestamp(time.as_secs()))
        .unwrap_or_else(|| "unavailable".into());
    let mut body = format!(
        "Modified at: {modified}\nHistorical output; may belong to an earlier setup attempt.\nLatest {MAX_LOG_LINES} lines (capture limited to 16 KiB):\n"
    );
    let start = metadata.len().saturating_sub(MAX_LOG_CAPTURE);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(MAX_LOG_CAPTURE).read_to_end(&mut bytes)?;
    // The beginning may be in the middle of a credential-bearing line, with
    // its secret marker outside the capture. Omit that entire partial line.
    let bytes = if start > 0 {
        bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|newline| &bytes[newline + 1..])
            .unwrap_or_default()
    } else {
        &bytes
    };
    let output = String::from_utf8_lossy(bytes);
    let mut remaining = MAX_SECTION_BYTES - body.len() - OMITTED.len() - 1;
    let mut lines = Vec::new();
    for line in output.lines().rev().take(MAX_LOG_LINES) {
        if remaining < 64 {
            break;
        }
        // Redact each complete line before applying its output budget.
        let line = safe_text(line, remaining);
        remaining -= line.len();
        lines.push(line);
    }
    if start > 0 || output.lines().count() > lines.len() {
        body.push_str(OMITTED);
    }
    if metadata.len() == 0 {
        body.push_str("No setup output recorded.\n");
    }
    for line in lines.into_iter().rev() {
        body.push_str(&line);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, FileTimes};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    fn fixture() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-kwin-diagnostics-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn compositor_information_does_not_depend_on_a_loaded_bridge() {
        let section = compositor(Ok(Some(KWinInfo {
            version: "6.7.4".into(),
            qt_version: "6.10.2".into(),
            plugin_directory: Some("/usr/lib64/qt6/plugins"),
            owner: ":1.23".into(),
        })));
        assert!(section.body().contains("running KWin version: 6.7.4"));
        assert!(section.body().contains("running Qt version: 6.10.2"));
        assert!(section.body().contains("/usr/lib64/qt6/plugins"));
        let absent = compositor(Ok(None));
        assert!(absent.body().contains("not applicable"));
        assert!(!absent.body().contains("repair"));
        let failed = compositor(Err(SessionBusError::Transport("inspection failed".into())));
        assert!(failed.body().contains("inspection failed"));
    }

    #[test]
    fn log_discovery_matches_native_setup_state_locations() {
        assert_eq!(
            setup_log_path(Some(Path::new("/state")), Some(Path::new("/home/user"))),
            Some(PathBuf::from("/state/lg-buddy/kwin/setup.log"))
        );
        assert_eq!(
            setup_log_path(None, Some(Path::new("/home/user"))),
            Some(PathBuf::from(
                "/home/user/.local/state/lg-buddy/kwin/setup.log"
            ))
        );
        assert_eq!(setup_log_path(None, None), None);
    }

    #[test]
    fn historical_setup_output_is_dated_redacted_and_separate_from_the_snapshot() {
        let root = fixture();
        let path = root.join("setup.log");
        fs::write(
            &path,
            "compiler failed\nclient-key=private-value\nhttps://private.example/build\n",
        )
        .unwrap();
        File::open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
            .unwrap();
        let section = setup_log(&path);
        assert!(section.body().contains("Modified at: 1970-01-01T00:00:01Z"));
        assert!(section.body().contains("earlier setup attempt"));
        assert!(section.body().contains("compiler failed"));
        let report = super::super::DiagnosticsReport::new(10_000, vec![section]);
        let text = report.text();
        assert!(text.contains("Recent logs"));
        assert!(!text.contains("Current snapshot"));
        assert!(!text.contains("private-value"));
        assert!(!text.contains("private.example"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tail_keeps_recent_failures_and_drops_partial_credential_lines() {
        let root = fixture();
        let path = root.join("setup.log");
        fs::write(
            &path,
            format!(
                "token={}\n{}final loader error\n",
                "private".repeat(5000),
                "older output\n".repeat(100)
            ),
        )
        .unwrap();
        let section = setup_log(&path);
        assert!(section.body().contains("Earlier setup output omitted"));
        assert!(section.body().contains("final loader error"));
        assert!(!section.body().contains("private"));
        assert!(section.body().len() <= MAX_SECTION_BYTES);
        let long_line = format!(
            "{} token=private-suffix\nfinal failure\n",
            "界".repeat(2000)
        );
        fs::write(&path, long_line).unwrap();
        let section = setup_log(&path);
        assert!(section.body().contains("final failure"));
        assert!(section.body().contains("Credential-bearing output omitted"));
        assert!(!section.body().contains("private-suffix"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_empty_and_unreadable_logs_do_not_fail_the_report_or_create_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = fixture();
        let path = root.join("setup.log");
        assert!(setup_log(&path).body().contains("No setup log found"));
        assert!(!path.exists());
        fs::write(&path, "").unwrap();
        assert!(setup_log(&path).body().contains("No setup output recorded"));
        if unsafe { libc::geteuid() } != 0 {
            fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
            assert!(setup_log(&path).body().contains("Setup log unavailable"));
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(setup_log(&path).body().contains("not a regular file"));
        fs::remove_dir(&path).unwrap();
        symlink(root.join("other"), &path).unwrap();
        assert!(setup_log(&path).body().contains("Setup log unavailable"));
        fs::remove_file(&path).unwrap();
        let fifo = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(setup_log(&path).body().contains("not a regular file"));
        fs::remove_dir_all(root).unwrap();
    }
}
