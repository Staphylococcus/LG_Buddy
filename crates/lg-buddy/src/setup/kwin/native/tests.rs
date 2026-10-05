use super::*;

struct Fixture(PathBuf);

#[test]
fn source_identity_matches_the_published_sha256sum_manifest() {
    let fixture = Fixture::new();
    assert_eq!(
        source_id(&fixture.0.join("payload/source")).unwrap(),
        "8066a1c10e1874aaa7123ee0f8ccd06611b399bbb4aed04b0d18c6fee521a82c"
    );
}

#[test]
fn checksums_reject_symlinks_and_special_files_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    let symlink = fixture.0.join("symlink");
    std::os::unix::fs::symlink(fixture.0.join("payload/source/main.cpp"), &symlink).unwrap();
    assert!(digest(&symlink).is_err());
    let fifo = fixture.0.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(digest(&fifo).is_err());
}

#[test]
fn system_install_validates_artifacts_and_preserves_existing_files() {
    let fixture = Fixture::new();
    let source = fixture.0.join("payload/source/main.cpp");
    let destination = fixture.0.join("plugins/plugin.so");
    let hash = digest(&source).unwrap();
    assert!(install_artifact(&source, &destination, &"0".repeat(64)).is_err());
    assert!(!destination.exists());
    install_artifact(&source, &destination, &hash).unwrap();
    let metadata = fs::metadata(&destination).unwrap();
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.mode() & 0o777, 0o644);
    install_artifact(&source, &destination, &hash).unwrap();
    fs::write(&destination, "corrupted").unwrap();
    assert!(install_artifact(&source, &destination, &hash).is_err());
    assert_eq!(fs::read_to_string(&destination).unwrap(), "corrupted");
    fs::remove_file(&destination).unwrap();
    std::os::unix::fs::symlink(&source, &destination).unwrap();
    assert!(install_artifact(&source, &destination, &hash).is_err());
    assert_eq!(fs::read_to_string(&source).unwrap(), "main.cpp");
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "lg-buddy-native-kwin-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir_all(root.join("payload/source")).unwrap();
        fs::create_dir_all(root.join("plugins/kwin/plugins")).unwrap();
        for name in ["CMakeLists.txt", "main.cpp", "metadata.json"] {
            fs::write(root.join("payload/source").join(name), name).unwrap();
        }
        Self(root)
    }
    fn engine(&self) -> Provisioner<Fake> {
        let source = source_id(&self.0.join("payload/source")).unwrap();
        Provisioner {
            host: Fake {
                root: self.0.join("plugins"),
                source: source.clone(),
                actions: vec![],
                ready: false,
                info: true,
                build_fails: false,
                reject: false,
                deny_remove: false,
                authorization: 0,
                managed: None,
            },
            payload: self.0.join("payload"),
            state: self.0.join("state"),
            cache: self.0.join("cache"),
            uid: 1000,
            arch: "x86_64".into(),
            source,
            session: None,
            allow_dependencies: false,
        }
    }
    fn artifact(&self, directory: &str, content: &str) -> PathBuf {
        let directory = self.0.join(directory);
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("plugin.so"), content).unwrap();
        let identity = source_id(&self.0.join("payload/source")).unwrap();
        fs::write(
            directory.join("metadata.tsv"),
            format!(
                "6.7.5\t6.11.2\tx86_64\t{identity}\t{}\n",
                digest(&directory.join("plugin.so")).unwrap()
            ),
        )
        .unwrap();
        directory
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Fake {
    root: PathBuf,
    source: String,
    actions: Vec<String>,
    ready: bool,
    info: bool,
    build_fails: bool,
    reject: bool,
    deny_remove: bool,
    authorization: u8,
    managed: Option<u8>,
}
impl Host for Fake {
    fn bridge(&mut self, command: KWinBridgeCommand) -> Result<String> {
        self.actions.push(format!("{command:?}"));
        match command {
            KWinBridgeCommand::Info => Ok(if self.info {
                format!("6.7.5\t6.11.2\t{}\t:1.42", self.root.display())
            } else {
                String::new()
            }),
            KWinBridgeCommand::Check if !self.ready => Err(1),
            KWinBridgeCommand::Unload(_) => {
                self.ready = false;
                Ok(String::new())
            }
            KWinBridgeCommand::Load(id) => {
                if self.reject
                    || fs::read_to_string(self.root.join("kwin/plugins").join(format!("{id}.so")))
                        .unwrap()
                        == "rejected"
                {
                    return Err(1);
                }
                self.ready = true;
                Ok(format!("6.7.5\t{}", self.source))
            }
            _ => Ok(format!("6.7.5\t{}", self.source)),
        }
    }
    fn privileged(&mut self, args: &[OsString]) -> Result<()> {
        self.actions.push(format!("privileged {args:?}"));
        if self.authorization != 0 {
            return Err(self.authorization);
        }
        match args[0].to_str().unwrap() {
            "--system-install" => {
                fs::copy(
                    &args[4],
                    self.root
                        .join("kwin/plugins")
                        .join(format!("{}.so", args[3].to_str().unwrap())),
                )
                .unwrap();
            }
            "--system-remove" => {
                if self.deny_remove {
                    return Err(1);
                }
                let _ = fs::remove_file(
                    self.root
                        .join("kwin/plugins")
                        .join(format!("{}.so", args[3].to_str().unwrap())),
                );
            }
            "--system-dependencies" => self.build_fails = false,
            _ => panic!("unexpected action"),
        }
        Ok(())
    }
    fn mutation(&mut self) -> Result<()> {
        self.actions.push("mutation".into());
        Ok(())
    }
    fn configure(&mut self, id: &str, enabled: bool) -> Result<()> {
        self.actions.push(format!("configure {id} {enabled}"));
        Ok(())
    }
    fn build(&mut self, _: &Path, cache: &Path, _: &str) -> Result<()> {
        self.actions.push("build".into());
        if self.build_fails {
            return Err(1);
        }
        fs::create_dir_all(cache.join("local")).unwrap();
        fs::write(cache.join("local/plugin.so"), "local").unwrap();
        let hash = digest(&cache.join("local/plugin.so")).unwrap();
        fs::write(
            cache.join("local/metadata.tsv"),
            format!("6.7.5\t6.11.2\tx86_64\t{}\t{hash}\n", self.source),
        )
        .unwrap();
        Ok(())
    }
    fn managed(&self) -> Option<u8> {
        self.managed
    }
    fn root_supported(&self, root: &Path) -> bool {
        root == self.root
    }
}

#[test]
fn compatible_prebuilt_installs_without_building_and_removal_uses_receipts() {
    let fixture = Fixture::new();
    fixture.artifact("payload/prebuilt/candidate", "prebuilt");
    let mut engine = fixture.engine();
    engine.execute("--foreground").unwrap();
    assert!(!engine
        .host
        .actions
        .iter()
        .any(|a| a == "build" || a.contains("--system-dependencies")));
    assert_eq!(
        fs::read_dir(engine.state.join("plugins")).unwrap().count(),
        1
    );
    let actions = engine.host.actions.len();
    engine.execute("--foreground").unwrap();
    assert!(!engine.host.actions[actions..]
        .iter()
        .any(|a| a.starts_with("privileged") || a == "mutation"));
    engine.execute("--remove").unwrap();
    assert_eq!(
        fs::read_dir(engine.state.join("plugins")).unwrap().count(),
        0
    );
    assert_eq!(
        fs::read_dir(engine.host.root.join("kwin/plugins"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn removal_serializes_cache_cleanup_even_without_previous_setup_state() {
    let fixture = Fixture::new();
    let mut engine = fixture.engine();
    fs::create_dir_all(&engine.cache).unwrap();
    assert!(!engine.state.exists());
    engine.execute("--remove").unwrap();
    assert!(engine.state.join("setup.lock").is_file());
    assert!(!engine.cache.exists());
    assert_eq!(engine.host.actions, ["mutation"]);
}

#[test]
fn incompatible_corrupt_and_rejected_artifacts_fall_back_to_local_then_cached_builds() {
    for mode in [
        "wrong-version",
        "wrong-source",
        "newer-qt",
        "corrupt",
        "rejected",
    ] {
        let fixture = Fixture::new();
        let directory = fixture.artifact("payload/prebuilt/candidate", mode);
        let metadata = directory.join("metadata.tsv");
        let text = fs::read_to_string(&metadata).unwrap();
        match mode {
            "wrong-version" => fs::write(&metadata, text.replace("6.7.5", "6.7.4")).unwrap(),
            "wrong-source" => fs::write(
                &metadata,
                text.replace(
                    &source_id(&fixture.0.join("payload/source")).unwrap(),
                    &"0".repeat(64),
                ),
            )
            .unwrap(),
            "newer-qt" => fs::write(&metadata, text.replace("6.11.2", "6.12.0")).unwrap(),
            "corrupt" => fs::write(directory.join("plugin.so"), "modified").unwrap(),
            _ => (),
        }
        let mut engine = fixture.engine();
        engine.execute("--foreground").unwrap();
        assert!(engine.host.actions.iter().any(|a| a == "build"), "{mode}");
        assert_eq!(
            fs::read_dir(engine.host.root.join("kwin/plugins"))
                .unwrap()
                .count(),
            1
        );
        engine.host.ready = false;
        engine.host.actions.clear();
        fs::remove_dir_all(fixture.0.join("payload/prebuilt")).unwrap();
        engine.execute("--foreground").unwrap();
        assert!(!engine.host.actions.iter().any(|a| a == "build"));
    }
}

#[test]
fn dependency_consent_and_authorization_failures_stop_the_fallback_chain() {
    let fixture = Fixture::new();
    let mut engine = fixture.engine();
    engine.host.build_fails = true;
    assert_eq!(engine.execute("--foreground"), Err(77));
    assert!(!engine
        .host
        .actions
        .iter()
        .any(|a| a.contains("--system-dependencies")));
    engine.allow_dependencies = true;
    engine.execute("--foreground").unwrap();
    assert_eq!(
        engine
            .host
            .actions
            .iter()
            .filter(|a| a.contains("--system-dependencies"))
            .count(),
        1
    );
    for code in [126, 127] {
        let fixture = Fixture::new();
        fixture.artifact("payload/prebuilt/candidate", "prebuilt");
        let mut engine = fixture.engine();
        engine.host.authorization = code;
        assert_eq!(engine.execute("--foreground"), Err(code));
        assert!(!engine.host.actions.iter().any(|a| a == "build"));
    }
}

#[test]
fn denied_cleanup_keeps_receipts_for_retry_and_exhausted_setup_is_incomplete() {
    let fixture = Fixture::new();
    fixture.artifact("payload/prebuilt/candidate", "rejected");
    let mut engine = fixture.engine();
    engine.host.deny_remove = true;
    engine.host.build_fails = true;
    engine.allow_dependencies = true;
    engine.host.reject = true;
    assert_eq!(engine.execute("--foreground"), Err(1));
    assert!(fs::read_dir(engine.state.join("plugins")).unwrap().count() > 0);
    engine.host.deny_remove = false;
    engine.execute("--remove").unwrap();
    assert_eq!(
        fs::read_dir(engine.state.join("plugins")).unwrap().count(),
        0
    );
}

#[test]
fn inspection_is_read_only_and_nixos_ostree_never_provision() {
    let fixture = Fixture::new();
    let mut engine = fixture.engine();
    engine.host.info = false;
    assert_eq!(engine.execute("--status"), Err(2));
    assert!(!engine.state.exists());
    assert!(!engine.cache.exists());
    engine.host.info = true;
    for code in [5, 6] {
        engine.host.managed = Some(code);
        assert_eq!(engine.execute("--status"), Err(code));
        assert_eq!(engine.execute("--foreground"), Err(code));
        engine.execute("").unwrap();
        assert!(!engine.state.exists());
        assert!(!engine.cache.exists());
    }
    engine.host.ready = true;
    engine.execute("--status").unwrap();
    assert!(!engine
        .host
        .actions
        .iter()
        .any(|a| a.starts_with("privileged") || a == "build" || a == "mutation"));
}

#[test]
fn passive_login_only_loads_an_existing_verified_plugin() {
    let fixture = Fixture::new();
    let directory = fixture.artifact("payload/prebuilt/candidate", "prebuilt");
    let mut engine = fixture.engine();
    let id = plugin_id(engine.uid, &digest(&directory.join("plugin.so")).unwrap());
    fs::copy(
        directory.join("plugin.so"),
        engine
            .host
            .root
            .join("kwin/plugins")
            .join(format!("{id}.so")),
    )
    .unwrap();
    engine.execute("").unwrap();
    assert!(engine.host.ready);
    assert!(!engine.state.exists());
    assert!(!engine.cache.exists());
    assert!(!engine
        .host
        .actions
        .iter()
        .any(|a| a.starts_with("privileged") || a.starts_with("configure") || a == "build"));
}

#[test]
fn kernel_lock_excludes_competing_setup_and_releases_duplicated_descriptors() {
    let fixture = Fixture::new();
    let lock = setup_lock(&fixture.0.join("state")).unwrap();
    assert_eq!(
        setup_lock(&fixture.0.join("state")).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    let inherited = lock.file().try_clone().unwrap();
    drop(lock);
    setup_lock(&fixture.0.join("state")).unwrap();
    drop(inherited);
    let other = fixture.0.join("unsafe");
    fs::create_dir(&other).unwrap();
    std::os::unix::fs::symlink(fixture.0.join("state/setup.lock"), other.join("setup.lock"))
        .unwrap();
    assert!(setup_lock(&other).is_err());
}

#[test]
fn identities_and_privileged_paths_cannot_escape_the_owned_plugin() {
    let hash = "a".repeat(64);
    assert!(owned_id(1000, &plugin_id(1000, &hash)));
    for id in [
        "../other",
        "lg_buddy_inhibition_1000_bad",
        &plugin_id(1001, &hash),
    ] {
        assert!(!owned_id(1000, id));
    }
    assert!(!supported_root(Path::new("/tmp/plugins")));
    assert!(!supported_root(Path::new(
        "/usr/lib64/qt6/plugins/../other"
    )));
    for args in [
        vec!["--system-install".into()],
        vec![
            "--system-remove".into(),
            "1000".into(),
            "/tmp".into(),
            plugin_id(1000, &hash).into(),
        ],
        vec!["--system-dependencies".into(), "../6.7.5".into()],
    ] {
        assert_eq!(system_action(&args), Err(1));
    }
}
