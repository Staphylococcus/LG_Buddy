//! Native KWin provisioning. Inspection and passive login never mutate the host.
//! The authorization owner brokers privileged operations; this process owns the
//! per-user setup lock throughout installation, compilation, and removal.
use crate::kwin_bridge::{self, KWinBridgeCommand};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, BufRead, Read, Write},
    os::{
        fd::FromRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

type Result<T> = std::result::Result<T, u8>;
fn failure(error: impl std::fmt::Display) -> u8 {
    eprintln!("LG Buddy KWin setup: {error}");
    1
}
fn digest(path: &Path) -> io::Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn source_id(source: &Path) -> io::Result<String> {
    // Match the published sha256sum manifest identity byte for byte.
    let mut hash = Sha256::new();
    for name in ["CMakeLists.txt", "main.cpp", "metadata.json"] {
        hash.update(format!("{}  {name}\n", digest(&source.join(name))?));
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn version(value: &str) -> Option<[u32; 3]> {
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let mut parsed = [0; 3];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        parsed[i] = part.parse().ok()?;
    }
    Some(parsed)
}
fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn plugin_id(uid: u32, hash: &str) -> String {
    format!("lg_buddy_inhibition_{uid}_{hash}")
}
fn owned_id(uid: u32, id: &str) -> bool {
    id.strip_prefix(&format!("lg_buddy_inhibition_{uid}_"))
        .is_some_and(hex)
}
fn supported_root(root: &Path) -> bool {
    matches!(
        root.to_str(),
        Some(
            "/usr/lib64/qt6/plugins"
                | "/usr/lib/qt6/plugins"
                | "/usr/lib/x86_64-linux-gnu/qt6/plugins"
                | "/usr/lib/aarch64-linux-gnu/qt6/plugins"
        )
    )
}
fn metadata_files(directory: &Path) -> io::Result<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let file = entry.path().join("metadata.tsv");
            if fs::symlink_metadata(&file).is_ok_and(|m| m.is_file()) {
                files.push(file);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn setup_lock(directory: &Path) -> io::Result<crate::setup::lock::FlowLock> {
    fs::create_dir_all(directory)?;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::other("unsafe KWin setup directory"));
    }
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    crate::setup::lock::FlowLock::try_acquire(&directory.join("setup.lock"))
}

#[derive(Clone)]
struct Session {
    kwin: String,
    qt: String,
    root: PathBuf,
}
struct Artifact {
    directory: PathBuf,
    hash: String,
}

trait Host {
    fn bridge(&mut self, command: KWinBridgeCommand) -> Result<String>;
    fn privileged(&mut self, args: &[OsString]) -> Result<()>;
    fn mutation(&mut self) -> Result<()>;
    fn configure(&mut self, id: &str, enabled: bool) -> Result<()>;
    fn build(&mut self, source: &Path, cache: &Path, kwin: &str) -> Result<()>;
    fn managed(&self) -> Option<u8>;
    fn start_log(&mut self, _path: &Path) -> Result<()> {
        Ok(())
    }
    fn root_supported(&self, root: &Path) -> bool {
        supported_root(root)
    }
}

struct Provisioner<H> {
    host: H,
    payload: PathBuf,
    state: PathBuf,
    cache: PathBuf,
    uid: u32,
    arch: String,
    source: String,
    session: Option<Session>,
    allow_dependencies: bool,
}
impl<H: Host> Provisioner<H> {
    fn inspect(&mut self) -> Result<()> {
        if self.uid == 0 {
            return Err(2);
        }
        let info = self.host.bridge(KWinBridgeCommand::Info)?;
        if info.trim().is_empty() {
            return Err(2);
        }
        let fields: Vec<_> = info.trim_end().split('\t').collect();
        if fields.len() != 4 || version(fields[0]).is_none() || version(fields[1]).is_none() {
            return Err(1);
        }
        self.session = Some(Session {
            kwin: fields[0].into(),
            qt: fields[1].into(),
            root: fields[2].into(),
        });
        self.source = source_id(&self.payload.join("source")).map_err(failure)?;
        if self
            .host
            .bridge(KWinBridgeCommand::Check)
            .is_ok_and(|reply| reply.trim_end() == format!("{}\t{}", fields[0], self.source))
        {
            return Ok(());
        }
        if version(fields[0]).unwrap()[0] != 6 {
            return Err(7);
        }
        if let Some(code) = self.host.managed() {
            return Err(code);
        }
        if !self.host.root_supported(Path::new(fields[2])) {
            return Err(8);
        }
        Err(3)
    }
    fn artifact(&self, metadata: &Path) -> Option<Artifact> {
        let metadata_text = fs::read_to_string(metadata).ok()?;
        let f: Vec<_> = metadata_text.trim_end().split('\t').collect();
        let session = self.session.as_ref()?;
        if f.len() != 5
            || f[0] != session.kwin
            || f[2] != self.arch
            || f[3] != self.source
            || !hex(f[4])
        {
            return None;
        }
        let candidate_qt = version(f[1])?;
        let current_qt = version(&session.qt)?;
        if candidate_qt[0] != 6 || current_qt[0] != 6 || candidate_qt[1] > current_qt[1] {
            return None;
        }
        let directory = metadata.parent()?.to_owned();
        if digest(&directory.join("plugin.so")).ok()? != f[4] {
            return None;
        }
        Some(Artifact {
            directory,
            hash: f[4].into(),
        })
    }
    fn remove_plugin(&mut self, root: &Path, id: &str) -> Result<()> {
        if !owned_id(self.uid, id) {
            return Ok(());
        }
        self.host.mutation()?;
        let _ = self.host.bridge(KWinBridgeCommand::Unload(id.into()));
        let _ = self.host.configure(id, false);
        if self.host.root_supported(root) {
            match self.host.privileged(&[
                "--system-remove".into(),
                self.uid.to_string().into(),
                root.as_os_str().into(),
                id.into(),
            ]) {
                Ok(()) => {
                    let _ = fs::remove_file(self.state.join("plugins").join(format!("{id}.tsv")));
                }
                Err(code @ (126 | 127)) => return Err(code),
                Err(_) => (),
            }
        }
        Ok(())
    }
    fn remove_previous(&mut self) -> Result<()> {
        let directory = self.state.join("plugins");
        if !directory.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(directory).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            if !entry.file_type().map_err(failure)?.is_file()
                || entry.path().extension().is_none_or(|e| e != "tsv")
            {
                continue;
            }
            let receipt = fs::read_to_string(entry.path()).map_err(failure)?;
            if let Some((root, id)) = receipt.trim_end().split_once('\t') {
                self.remove_plugin(Path::new(root), id)?;
            }
        }
        Ok(())
    }
    fn try_artifacts(&mut self, directory: &Path, install: bool) -> Result<bool> {
        for metadata in metadata_files(directory).map_err(failure)? {
            let Some(artifact) = self.artifact(&metadata) else {
                continue;
            };
            let session = self.session.as_ref().unwrap().clone();
            let id = plugin_id(self.uid, &artifact.hash);
            let installed = session.root.join("kwin/plugins").join(format!("{id}.so"));
            if digest(&installed).map_or(true, |hash| hash != artifact.hash) {
                if !install {
                    continue;
                }
                match self.host.privileged(&[
                    "--system-install".into(),
                    self.uid.to_string().into(),
                    session.root.as_os_str().into(),
                    id.clone().into(),
                    artifact.directory.join("plugin.so").into_os_string(),
                ]) {
                    Ok(()) => (),
                    Err(code @ (126 | 127)) => return Err(code),
                    Err(_) => continue,
                }
            }
            if install {
                self.host.mutation()?;
                fs::write(
                    self.state.join("plugins").join(format!("{id}.tsv")),
                    format!("{}\t{id}\n", session.root.display()),
                )
                .map_err(failure)?;
            }
            let loaded = self
                .host
                .bridge(KWinBridgeCommand::Load(id.clone()))
                .is_ok_and(|reply| {
                    reply.trim_end() == format!("{}\t{}", session.kwin, self.source)
                });
            if !install {
                if loaded {
                    return Ok(true);
                }
                continue;
            }
            if !loaded || self.host.configure(&id, true).is_err() {
                self.remove_plugin(&session.root, &id)?;
                continue;
            }
            return Ok(true);
        }
        Ok(false)
    }
    fn provision(&mut self) -> Result<()> {
        self.remove_previous()?;
        if self.try_artifacts(&self.payload.join("prebuilt"), true)?
            || self.try_artifacts(&self.cache.clone(), true)?
        {
            return Ok(());
        }
        let kwin = self.session.as_ref().unwrap().kwin.clone();
        if self
            .host
            .build(&self.payload.join("source"), &self.cache, &kwin)
            .is_err()
        {
            if !self.allow_dependencies {
                return Err(77);
            }
            match self
                .host
                .privileged(&["--system-dependencies".into(), kwin.clone().into()])
            {
                Ok(()) => {
                    let _ = self
                        .host
                        .build(&self.payload.join("source"), &self.cache, &kwin);
                }
                Err(code @ (126 | 127)) => return Err(code),
                Err(_) => (),
            }
        }
        if self.try_artifacts(&self.cache.clone(), true)? {
            return Ok(());
        }
        Err(failure("KWin source remains unavailable after setup"))
    }
    fn execute(&mut self, mode: &str) -> Result<()> {
        if mode == "--remove" {
            let _lock = setup_lock(&self.state).map_err(failure)?;
            self.remove_previous()?;
            if self.cache.exists() {
                self.host.mutation()?;
                fs::remove_dir_all(&self.cache).map_err(failure)?;
            }
            return Ok(());
        }
        let status = self.inspect();
        if mode == "--status" {
            return status;
        }
        match status {
            Ok(()) => return Ok(()),
            Err(3) => (),
            Err(code) => return if mode.is_empty() { Ok(()) } else { Err(code) },
        }
        if mode.is_empty() {
            if !self.try_artifacts(&self.payload.join("prebuilt"), false)? {
                self.try_artifacts(&self.cache.clone(), false)?;
            }
            return Ok(());
        }
        let _lock = setup_lock(&self.state).map_err(failure)?;
        match self.inspect() {
            Ok(()) => return Ok(()),
            Err(3) => (),
            Err(code) => return Err(code),
        }
        fs::create_dir_all(self.state.join("plugins")).map_err(failure)?;
        fs::create_dir_all(&self.cache).map_err(failure)?;
        fs::set_permissions(&self.cache, fs::Permissions::from_mode(0o700)).map_err(failure)?;
        self.host.start_log(&self.state.join("setup.log"))?;
        self.provision()
    }
}

struct NativeHost {
    payload: PathBuf,
    mode: String,
    broker: Option<File>,
    mutated: bool,
    log: Option<File>,
}
impl NativeHost {
    fn request(&mut self, operation: &str, args: &[OsString]) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let output = self.broker.as_mut().ok_or(1u8)?;
        write!(output, "{operation}\0{}\0", args.len()).map_err(failure)?;
        for arg in args {
            output
                .write_all(arg.as_os_str().as_bytes())
                .map_err(failure)?;
            output.write_all(&[0]).map_err(failure)?;
        }
        output.flush().map_err(failure)?;
        let mut reply = Vec::new();
        io::stdin()
            .lock()
            .take(16)
            .read_until(0, &mut reply)
            .map_err(failure)?;
        if reply.pop() != Some(0) {
            return Err(1);
        }
        let code: u8 = std::str::from_utf8(&reply)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or(1u8)?;
        if code == 0 {
            Ok(())
        } else {
            Err(code)
        }
    }
}
impl Host for NativeHost {
    fn bridge(&mut self, command: KWinBridgeCommand) -> Result<String> {
        let mut output = Vec::new();
        kwin_bridge::run(command, &mut output).map_err(|error| {
            if let Some(log) = &mut self.log {
                let _ = writeln!(log, "{error}");
            }
            failure(error)
        })?;
        String::from_utf8(output).map_err(failure)
    }
    fn privileged(&mut self, args: &[OsString]) -> Result<()> {
        if self.broker.is_some() {
            return self.request("privileged", args);
        }
        let helper = self.payload.join("setup.sh");
        let mut command;
        if unsafe { libc::geteuid() } == 0 {
            return system_action(args);
        }
        let cached = Path::new("/usr/bin/sudo").is_file()
            && Command::new("/usr/bin/sudo")
                .args(["-n", "/usr/bin/true"])
                .output()
                .is_ok_and(|o| o.status.success());
        if self.mode == "noninteractive" && !cached {
            return Err(127);
        }
        if cached || self.mode == "terminal" || self.mode == "noninteractive" {
            command = Command::new("/usr/bin/sudo");
            if cached || self.mode == "noninteractive" {
                command.arg("-n");
            }
            command.arg("/bin/bash").arg(helper);
        } else {
            command = Command::new("/usr/bin/pkexec");
            command.arg("--disable-internal-agent").arg(helper);
        }
        let status = command.args(args).status().map_err(|error| {
            eprintln!("LG Buddy KWin authorization: {error}");
            127
        })?;
        if status.success() {
            Ok(())
        } else {
            Err(status.code().unwrap_or(1) as u8)
        }
    }
    fn mutation(&mut self) -> Result<()> {
        if self.broker.is_some() && !self.mutated {
            self.request("mutation", &[])?;
        }
        self.mutated = true;
        Ok(())
    }
    fn configure(&mut self, id: &str, enabled: bool) -> Result<()> {
        for name in ["kwriteconfig6", "kwriteconfig"] {
            let mut command = Command::new(name);
            command.stdin(Stdio::null());
            command.args([
                "--file",
                "kwinrc",
                "--group",
                "Plugins",
                "--key",
                &format!("{id}Enabled"),
            ]);
            command.arg(if enabled { "true" } else { "--delete" });
            match command.status() {
                Ok(status) => return if status.success() { Ok(()) } else { Err(1) },
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(failure(error)),
            }
        }
        Err(1)
    }
    fn build(&mut self, source: &Path, cache: &Path, kwin: &str) -> Result<()> {
        build(source, cache, kwin, self.log.as_ref())
    }
    fn start_log(&mut self, path: &Path) -> Result<()> {
        self.log = Some(
            OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
                .map_err(failure)?,
        );
        Ok(())
    }
    fn managed(&self) -> Option<u8> {
        if Path::new("/etc/NIXOS").exists() {
            Some(5)
        } else if Path::new("/run/ostree-booted").exists() {
            Some(6)
        } else {
            None
        }
    }
}

struct BuildDirectory(PathBuf);
impl Drop for BuildDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn status(command: &mut Command) -> Result<()> {
    if command.status().map_err(failure)?.success() {
        Ok(())
    } else {
        Err(1)
    }
}
fn build_status(command: &mut Command, log: Option<&File>) -> Result<()> {
    command.stdin(Stdio::null());
    if let Some(log) = log {
        command
            .stdout(log.try_clone().map_err(failure)?)
            .stderr(log.try_clone().map_err(failure)?);
    }
    status(command)
}
fn build(source: &Path, cache: &Path, expected: &str, log: Option<&File>) -> Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    fs::create_dir_all(cache).map_err(failure)?;
    let directory = cache.join(format!(
        ".build-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&directory).map_err(failure)?;
    let temporary = BuildDirectory(directory);
    let source = fs::canonicalize(source).map_err(failure)?;
    let identity = source_id(&source).map_err(failure)?;
    build_status(
        Command::new("cmake")
            .arg("-S")
            .arg(source)
            .arg("-B")
            .arg(&temporary.0)
            .arg("-DCMAKE_BUILD_TYPE=Release")
            .arg(format!("-DLG_BUDDY_KWIN_BUILD_ID={identity}")),
        log,
    )?;
    let info = fs::read_to_string(temporary.0.join("build-info.tsv")).map_err(failure)?;
    let f: Vec<_> = info.trim_end().split('\t').collect();
    if f.len() != 4
        || f[0] != expected
        || version(f[0]).is_none()
        || version(f[1]).is_none_or(|v| v[0] != 6)
        || f[3] != identity
        || !f[2].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(failure("development files do not match the running KWin"));
    }
    build_status(
        Command::new("cmake")
            .arg("--build")
            .arg(&temporary.0)
            .args(["--parallel", "2"]),
        log,
    )?;
    let plugin = temporary.0.join("lg_buddy_inhibition.so");
    build_status(
        Command::new("strip").arg("--strip-unneeded").arg(&plugin),
        log,
    )?;
    let hash = digest(&plugin).map_err(failure)?;
    let artifact = cache.join(format!("{}-{}-{}-{hash}", f[0], f[1], f[2]));
    fs::create_dir_all(&artifact).map_err(failure)?;
    fs::copy(plugin, artifact.join("plugin.so")).map_err(failure)?;
    fs::set_permissions(
        artifact.join("plugin.so"),
        fs::Permissions::from_mode(0o644),
    )
    .map_err(failure)?;
    fs::write(
        artifact.join("metadata.tsv"),
        format!("{}\t{}\t{}\t{identity}\t{hash}\n", f[0], f[1], f[2]),
    )
    .map_err(failure)?;
    Ok(())
}

fn package_owned(path: &Path) -> bool {
    [
        ("/usr/bin/rpm", vec!["-qf", "--"]),
        ("/usr/bin/dpkg-query", vec!["-S"]),
        ("/usr/bin/pacman", vec!["-Qo", "--"]),
    ]
    .into_iter()
    .any(|(program, args)| {
        Command::new(program)
            .args(args)
            .arg(path)
            .output()
            .is_ok_and(|o| o.status.success())
    })
}
fn system_action(args: &[OsString]) -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(1);
    }
    // Privileged commands never resolve caller-controlled executables.
    std::env::set_var("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
    let action = args.first().and_then(|a| a.to_str()).ok_or(1u8)?;
    if action == "--system-dependencies" {
        if args.len() != 2 {
            return Err(1);
        }
        return dependencies(args[1].to_str().ok_or(1u8)?);
    }
    let expected = if action == "--system-install" {
        5
    } else if action == "--system-remove" {
        4
    } else {
        return Err(1);
    };
    if args.len() != expected {
        return Err(1);
    }
    let uid: u32 = args[1].to_str().and_then(|s| s.parse().ok()).ok_or(1u8)?;
    let root = Path::new(&args[2]);
    let id = args[3].to_str().ok_or(1u8)?;
    if !owned_id(uid, id) || !supported_root(root) {
        return Err(1);
    }
    let directory = root.join("kwin/plugins");
    if !fs::symlink_metadata(&directory).is_ok_and(|m| m.is_dir()) {
        return Err(1);
    }
    let destination = directory.join(format!("{id}.so"));
    if package_owned(&destination) {
        return Err(1);
    }
    if action == "--system-remove" {
        return match fs::remove_file(destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failure(error)),
        };
    }
    let source = Path::new(&args[4]);
    let hash = id.rsplit('_').next().unwrap();
    install_artifact(source, &destination, hash)?;
    if Path::new("/usr/sbin/restorecon").is_file() {
        status(Command::new("/usr/sbin/restorecon").arg(&destination))?;
    }
    Ok(())
}

fn install_artifact(source: &Path, destination: &Path, hash: &str) -> Result<()> {
    if digest(source).map_err(failure)? != hash {
        return Err(1);
    }
    match fs::symlink_metadata(destination) {
        Ok(metadata) => {
            return if metadata.is_file() && digest(destination).map_err(failure)? == hash {
                Ok(())
            } else {
                Err(1)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (),
        Err(error) => return Err(failure(error)),
    }
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(source)
        .map_err(failure)?;
    if !input.metadata().map_err(failure)?.is_file() {
        return Err(1);
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(destination)
        .map_err(failure)?;
    let copied = (|| {
        io::copy(&mut input, &mut output).map_err(failure)?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o644)).map_err(failure)?;
        if digest(destination).map_err(failure)? != hash {
            return Err(1);
        }
        Ok(())
    })();
    if copied.is_err() {
        let _ = fs::remove_file(destination);
    }
    copied
}
fn dependencies(expected: &str) -> Result<()> {
    if version(expected).is_none_or(|v| v[0] != 6)
        || Path::new("/etc/NIXOS").exists()
        || Path::new("/run/ostree-booted").exists()
    {
        return Err(1);
    }
    let current = Command::new("/usr/bin/kwin_wayland")
        .arg("--version")
        .output()
        .map_err(failure)?;
    if !current.status.success()
        || String::from_utf8_lossy(&current.stdout)
            .split_whitespace()
            .last()
            != Some(expected)
    {
        return Err(1);
    }
    if Path::new("/usr/bin/dnf").is_file() {
        let package = Command::new("/usr/bin/rpm")
            .args(["-q", "--qf", "%{VERSION}-%{RELEASE}", "kwin"])
            .output()
            .map_err(failure)?;
        if !package.status.success() {
            return Err(1);
        }
        status(
            Command::new("/usr/bin/dnf")
                .args(["--setopt=install_weak_deps=False", "install", "-y"])
                .arg(format!(
                    "kwin-devel-{}",
                    String::from_utf8_lossy(&package.stdout).trim()
                ))
                .args([
                    "cmake",
                    "gcc-c++",
                    "extra-cmake-modules",
                    "qt6-qtbase-devel",
                    "libepoxy-devel",
                    "libdrm-devel",
                ]),
        )
    } else if Path::new("/usr/bin/apt-get").is_file() {
        let package = Command::new("/usr/bin/dpkg-query")
            .args(["-W", "-f=${Version}", "kwin-common"])
            .output()
            .map_err(failure)?;
        if !package.status.success() {
            return Err(1);
        }
        status(
            Command::new("/usr/bin/apt-get")
                .args(["install", "-y", "--no-install-recommends"])
                .arg(format!(
                    "kwin-dev={}",
                    String::from_utf8_lossy(&package.stdout).trim()
                ))
                .args([
                    "cmake",
                    "g++",
                    "make",
                    "pkg-config",
                    "extra-cmake-modules",
                    "qt6-base-dev",
                    "qt6-declarative-dev",
                    "libepoxy-dev",
                    "libdrm-dev",
                    "libvulkan-dev",
                ]),
        )
    } else if Path::new("/usr/bin/pacman").is_file() {
        status(Command::new("/usr/bin/pacman").args([
            "-S",
            "--needed",
            "--noconfirm",
            "gcc",
            "cmake",
            "make",
            "pkgconf",
            "extra-cmake-modules",
            "qt6-base",
            "qt6-declarative",
            "wayland",
            "libepoxy",
            "libdrm",
            "vulkan-headers",
        ]))
    } else {
        Err(1)
    }
}

pub(crate) fn run(args: &[String]) -> Result<()> {
    let executable = std::env::current_exe().map_err(failure)?;
    let prefix = executable.parent().and_then(Path::parent).ok_or(1u8)?;
    let mut payload = prefix.join("lib/lg-buddy/kwin");
    let mut mode = "";
    let mut authorization = "interactive";
    let mut broker = false;
    let mut allow = false;
    let mut remaining = args.iter();
    while let Some(arg) = remaining.next() {
        match arg.as_str() {
            "--payload-dir" => payload = remaining.next().ok_or(1u8)?.into(),
            "--broker" => broker = true,
            "--status" | "--foreground" | "--remove" if mode.is_empty() => mode = arg,
            "--terminal" => authorization = "terminal",
            "--noninteractive" => authorization = "noninteractive",
            "--allow-dependencies" => allow = true,
            value if value.starts_with("--system-") => {
                let mut system = vec![OsString::from(value)];
                system.extend(remaining.map(OsString::from));
                return system_action(&system).map_err(|_| 1);
            }
            _ => return Err(failure("invalid internal KWin setup arguments")),
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or(1u8)?;
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"))
        .join("lg-buddy/kwin");
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"))
        .join("lg-buddy/kwin");
    let broker = if broker {
        // Keep the protocol pipe separate from compiler and user diagnostics.
        let fd = unsafe { libc::dup(libc::STDOUT_FILENO) };
        if fd < 0 {
            return Err(failure(io::Error::last_os_error()));
        }
        let output = unsafe { File::from_raw_fd(fd) };
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(failure(io::Error::last_os_error()));
        }
        if unsafe { libc::dup2(5, libc::STDOUT_FILENO) } < 0
            || unsafe { libc::dup2(6, libc::STDERR_FILENO) } < 0
        {
            return Err(failure(io::Error::last_os_error()));
        }
        Some(output)
    } else {
        None
    };
    Provisioner {
        host: NativeHost {
            payload: payload.clone(),
            mode: authorization.into(),
            broker,
            mutated: false,
            log: None,
        },
        payload,
        state,
        cache,
        uid: unsafe { libc::getuid() },
        arch: std::env::consts::ARCH.into(),
        source: String::new(),
        session: None,
        allow_dependencies: allow,
    }
    .execute(mode)
}

#[cfg(test)]
mod tests;
