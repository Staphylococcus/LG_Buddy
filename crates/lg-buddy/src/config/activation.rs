//! Publication protocol for declarative generators. Unit activation follows publication.
use super::{binding, declarative::Declarations};
use crate::pairing_store::PairingLock;
use crate::settings::{ServiceController, SystemdUserServiceController};
use std::fs;
use std::io;
use std::os::unix::fs::{symlink, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const SCREEN: &str = "LG_Buddy_screen.service";

fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}

/// A declaration may select a not-yet-created config, but never a redirected one.
fn validate_config(config: &Path, owner: Option<u32>, boundary: &Path) -> io::Result<()> {
    binding::safe_destination(config)?;
    match fs::symlink_metadata(config) {
        Ok(metadata) => {
            if !metadata.is_file()
                || metadata.nlink() != 1
                || owner.is_some_and(|uid| metadata.uid() != uid)
                || metadata.mode() & 0o022 != 0
                || metadata.mode() & 0o200 == 0
            {
                return Err(invalid(
                    "selected config must remain a writable regular file belonging to its user",
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    validate_ancestors(
        config
            .parent()
            .ok_or_else(|| invalid("config has no parent"))?,
        owner,
        boundary,
    )
}

fn validate_ancestors(path: &Path, owner: Option<u32>, boundary: &Path) -> io::Result<()> {
    if !path.starts_with(boundary) {
        return Err(invalid(
            "activation path is outside its filesystem boundary",
        ));
    }
    for path in path
        .ancestors()
        .take_while(|path| path.starts_with(boundary))
    {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
                if !metadata.is_dir()
                    || owner.is_some_and(|uid| metadata.uid() != 0 && metadata.uid() != uid)
                    || (metadata.mode() & 0o022 != 0 && !sticky_root)
                {
                    return Err(invalid("activation path has an unsafe directory ancestor"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn validate_selection(record: &Path, config: &Path, owner: u32, boundary: &Path) -> io::Result<()> {
    binding::validate_destination(record, owner, record.parent().unwrap())?;
    validate_ancestors(record.parent().unwrap(), Some(owner), boundary)?;
    if record == config
        || record == super::declarative::binding_path(config)?
        || config == PairingLock::marker_path(record)
        || fs::symlink_metadata(record).is_ok_and(|metadata| metadata.nlink() != 1)
    {
        return Err(invalid(
            "config, selector, declaration and lock markers must be separate unaliased paths",
        ));
    }
    if binding::read(record, owner)?.is_some_and(|existing| existing != config) {
        return Err(invalid("declaration conflicts with the selected config; migrate the existing selection explicitly"));
    }
    Ok(())
}

fn validate_overlay(path: &Path, owner: u32) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.uid() != owner
                || !(metadata.is_file() || metadata.file_type().is_symlink())
                || (metadata.is_file() && metadata.mode() & 0o022 != 0) =>
        {
            Err(invalid(
                "refusing to replace a declaration binding of another owner or unsafe type",
            ))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn generation(path: &Path, config: &Path, record: &Path, owner: u32) -> io::Result<PathBuf> {
    let target = fs::canonicalize(path)?;
    if target == config || target == record || target == super::declarative::binding_path(config)? {
        return Err(invalid(
            "generation must be separate from user state and binding files",
        ));
    }
    Declarations::read_generation(&target, owner)?;
    if fs::metadata(&target)?.mode() & 0o222 != 0 {
        return Err(invalid(
            "publish a complete read-only declaration generation",
        ));
    }
    Ok(target)
}

fn publish_overlay(path: &Path, generation: Option<&Path>) -> io::Result<()> {
    if let Some(generation) = generation {
        if fs::read_link(path).ok().as_deref() == Some(generation) {
            return Ok(());
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = loop {
            let temporary = path.with_file_name(format!(
                ".declaration-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match symlink(generation, &temporary) {
                Ok(()) => break temporary,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(temporary);
            return Err(error);
        }
    } else {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    fs::File::open(path.parent().unwrap())?.sync_all()
}

// Production uses /; unit fixtures supply their own controlled filesystem root.
fn publish_user(
    config: &Path,
    record: &Path,
    source: Option<&Path>,
    boundary: &Path,
) -> io::Result<()> {
    let owner = unsafe { libc::geteuid() };
    validate_config(config, Some(owner), boundary)?;
    validate_selection(record, config, owner, boundary)?;
    let overlay = super::declarative::binding_path(config)?;
    validate_overlay(&overlay, owner)?;
    // Validate before creating discovery records or config directories. Pin a
    // source symlink to its generation, never to a second moving binding.
    let candidate = source
        .map(|path| generation(path, config, record, owner))
        .transpose()?;
    let _selection_lock = PairingLock::for_config(record).map_err(io::Error::other)?;
    let lock = PairingLock::for_config(config).map_err(io::Error::other)?;
    validate_config(config, Some(owner), boundary)?;
    validate_selection(record, config, owner, boundary)?;
    validate_overlay(&overlay, owner)?;
    if let Some(candidate) = &candidate {
        generation(candidate, config, record, owner)?;
    }
    if lock.target() != config {
        return Err(invalid(
            "config path changed during declaration publication",
        ));
    }
    publish_overlay(&overlay, candidate.as_deref())?;
    binding::write(record, config, owner, record.parent().unwrap())
}

fn publish_machine(root: &Path, config: &Path) -> io::Result<()> {
    let owner = if root == Path::new("/") {
        0
    } else {
        unsafe { libc::geteuid() }
    };
    if unsafe { libc::geteuid() } != owner {
        return Err(invalid("machine activation requires root authorization"));
    }
    validate_config(config, None, Path::new("/"))?;
    let record = root.join(binding::MACHINE_BINDING.trim_start_matches('/'));
    if config == record || config == PairingLock::marker_path(&record) {
        return Err(invalid(
            "machine selection must be separate from config and lock state",
        ));
    }
    binding::validate_destination(&record, owner, root)?;
    if binding::installed(root)?.is_some_and(|existing| existing != config) {
        return Err(invalid(
            "declaration conflicts with the existing machine config selection",
        ));
    }
    binding::ensure_parent(&record)?;
    let _lock = PairingLock::for_config(&record).map_err(io::Error::other)?;
    binding::validate_destination(&record, owner, root)?;
    if binding::installed(root)?.is_some_and(|existing| existing != config) {
        return Err(invalid(
            "declaration conflicts with the existing machine config selection",
        ));
    }
    if let Some(existing) = binding::read(&record, owner)? {
        return if existing == config {
            Ok(())
        } else {
            Err(invalid(
                "declaration conflicts with the machine config selection",
            ))
        };
    }
    // No user directories, preferences, credentials or services are created.
    binding::write(&record, config, owner, root)
}

fn refresh_user(
    config: &Path,
    services: &impl ServiceController,
    boundary: &Path,
) -> io::Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        return Err(invalid(
            "refresh the selected user's session as that user, not root",
        ));
    }
    validate_config(config, Some(unsafe { libc::geteuid() }), boundary)?;
    {
        let _lock = PairingLock::for_config(config).map_err(io::Error::other)?;
        Declarations::load(config)?;
    }
    // Verify the loaded unit before using a negative is-active result. An
    // unavailable manager or unverifiable binding must report a failed refresh.
    let selected = services
        .user_service_config_path(SCREEN)
        .map_err(io::Error::other)?;
    let selected = match fs::canonicalize(&selected) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            binding::safe_destination(&selected)?;
            selected
        }
        Err(error) => return Err(error),
    };
    if selected != config {
        return Err(invalid(
            "screen service does not use the declared config; activate the matching units first",
        ));
    }
    if services
        .user_service_is_active(SCREEN)
        .map_err(io::Error::other)?
    {
        // Publication locks have been released. Restart only this active user
        // service: never enable units, start an offline session or power-cycle
        // the machine boot/shutdown oneshot.
        services
            .restart_user_service(SCREEN)
            .map_err(io::Error::other)?;
    }
    Ok(())
}

pub(super) fn run(arguments: &[String]) -> io::Result<()> {
    match arguments {
        [command, config, source] if command == "publish-user" => {
            let record = super::user_binding_path_from_env().ok_or_else(|| invalid("user config home is unavailable"))?;
            publish_user(Path::new(config), &record, (source != "-").then(|| Path::new(source)), Path::new("/"))
        }
        [command, config] if command == "publish-machine" => publish_machine(Path::new("/"), Path::new(config)),
        [command, config] if command == "refresh-user" => {
            let record = super::user_binding_path_from_env().ok_or_else(|| invalid("user config home is unavailable"))?;
            if binding::read(&record, unsafe { libc::geteuid() })?.as_deref() != Some(Path::new(config)) {
                return Err(invalid("user config selection changed; reconcile the declaration before refreshing services"));
            }
            refresh_user(Path::new(config), &SystemdUserServiceController::from_env(), Path::new("/"))
        }
        _ => Err(invalid("expected config-binding publish-user CONFIG GENERATION|-; publish-machine CONFIG; or refresh-user CONFIG")),
    }
}

#[cfg(test)]
mod tests;
