//! Installer/session-setup operations for the optional KWin plugin.
//! The inhibition adapter never calls these operations.

use std::io::{self, Write};

use crate::session_bus::{
    get_name_owner, new_session_bus_client, BusMethodCall, BusValue, SessionBusClient,
    SessionBusError,
};
use crate::sources::desktop::kwin::{INTERFACE, PATH, SERVICE};

const KWIN: &str = "org.kde.KWin";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KWinBridgeCommand {
    Info,
    Check,
    Load(String),
    Unload(String),
}

impl KWinBridgeCommand {
    pub fn parse(args: &[String]) -> Option<Self> {
        match args {
            [action] if action == "info" => Some(Self::Info),
            [action] if action == "check" => Some(Self::Check),
            [action, id] if valid_plugin_id(id) => match action.as_str() {
                "load" => Some(Self::Load(id.clone())),
                "unload" => Some(Self::Unload(id.clone())),
                _ => None,
            },
            _ => None,
        }
    }
}

fn valid_plugin_id(id: &str) -> bool {
    id.starts_with("lg_buddy_inhibition_")
        && id.len() <= 100
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn invalid(reason: &str) -> SessionBusError {
    SessionBusError::Transport(reason.into())
}

fn version_field<'a>(info: &'a str, field: &str) -> Result<&'a str, SessionBusError> {
    let version = info
        .lines()
        .find_map(|line| line.strip_prefix(field))
        .map(str::trim)
        .ok_or_else(|| invalid("KWin did not report its version"))?;
    if version.split('.').count() != 3
        || !version.split('.').all(|part| {
            !part.is_empty() && part.len() <= 3 && part.bytes().all(|b| b.is_ascii_digit())
        })
    {
        return Err(invalid("unsupported KWin/Qt version format"));
    }
    Ok(version)
}

fn plugin_directory(exists: impl Fn(&str) -> bool) -> Option<&'static str> {
    // KWin deliberately restricts /proc inspection. Use conventional Qt plugin
    // locations and verify the actual load; no privileged process inspection.
    [
        "/usr/lib64/qt6/plugins",
        "/usr/lib/x86_64-linux-gnu/qt6/plugins",
        "/usr/lib/aarch64-linux-gnu/qt6/plugins",
        "/usr/lib/qt6/plugins",
    ]
    .into_iter()
    .find(|directory| exists(&format!("{directory}/kwin/plugins")))
}

fn check(bus: &mut impl SessionBusClient, owner: &str) -> Result<String, SessionBusError> {
    if !bus.name_has_owner(SERVICE)? || get_name_owner(bus, SERVICE)? != owner {
        return Err(invalid("KWin source is absent"));
    }
    bus.call_method(BusMethodCall::new(owner, PATH, INTERFACE, "IsInhibited"))?
        .single_bool()?;
    let version = bus
        .call_method(BusMethodCall::new(owner, PATH, INTERFACE, "BuildVersion"))?
        .single_string()?
        .to_string();
    let build = bus
        .call_method(BusMethodCall::new(owner, PATH, INTERFACE, "BuildId"))?
        .single_string()?
        .to_string();
    if get_name_owner(bus, KWIN)? != owner || get_name_owner(bus, SERVICE)? != owner {
        return Err(invalid("KWin changed while checking the bridge"));
    }
    if build.len() != 64 || !build.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("KWin bridge has no supported build identity"));
    }
    Ok(format!("{version}\t{build}"))
}

pub fn run(command: KWinBridgeCommand, writer: &mut impl Write) -> io::Result<()> {
    let result = (|| -> Result<Option<String>, SessionBusError> {
        let mut bus = new_session_bus_client()?;
        if !bus.name_has_owner(KWIN)? {
            return if command == KWinBridgeCommand::Info {
                Ok(None)
            } else {
                Err(invalid("KWin is absent"))
            };
        }
        let owner = get_name_owner(&mut bus, KWIN)?;
        match command {
            KWinBridgeCommand::Info => {
                let reply = bus.call_method(BusMethodCall::new(
                    &owner,
                    "/KWin",
                    KWIN,
                    "supportInformation",
                ))?;
                let info = reply.single_string()?;
                if !info.lines().any(|line| line == "Operation Mode: Wayland") {
                    return Ok(None);
                }
                let version = version_field(info, "KWin version:")?;
                let qt = version_field(info, "Qt Version:")?;
                let directory = plugin_directory(|path| std::path::Path::new(path).is_dir())
                    .unwrap_or("unsupported");
                if get_name_owner(&mut bus, KWIN)? != owner {
                    return Err(invalid("KWin changed while inspecting integration"));
                }
                Ok(Some(format!("{version}\t{qt}\t{directory}\t{owner}")))
            }
            KWinBridgeCommand::Check => check(&mut bus, &owner).map(Some),
            KWinBridgeCommand::Load(id) => {
                let loaded = bus
                    .call_method(
                        BusMethodCall::new(
                            &owner,
                            "/Plugins",
                            "org.kde.KWin.Plugins",
                            "LoadPlugin",
                        )
                        .with_body(vec![BusValue::String(id)]),
                    )?
                    .single_bool()?;
                if !loaded {
                    return Err(invalid("KWin did not load this plugin"));
                }
                check(&mut bus, &owner).map(Some)
            }
            KWinBridgeCommand::Unload(id) => {
                bus.call_method(
                    BusMethodCall::new(&owner, "/Plugins", "org.kde.KWin.Plugins", "UnloadPlugin")
                        .with_body(vec![BusValue::String(id)]),
                )?;
                Ok(None)
            }
        }
    })();
    match result {
        Ok(Some(value)) => writeln!(writer, "{value}"),
        Ok(None) => Ok(()),
        Err(error) => Err(io::Error::other(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_ids_cannot_escape_the_plugin_directory_or_target_other_plugins() {
        for id in [
            "../nightlight",
            "nightlight",
            "lg_buddy_inhibition_../../x",
            "lg_buddy_inhibition_1/other",
        ] {
            assert!(KWinBridgeCommand::parse(&["load".into(), id.into()]).is_none());
        }
        assert!(
            KWinBridgeCommand::parse(&["load".into(), "lg_buddy_inhibition_1000_abcd".into()])
                .is_some()
        );
    }

    #[test]
    fn versions_are_parsed_from_running_compositor_information() {
        let info = "KWin version: 6.7.4\nQt Version: 6.10.2\nQt compile version: 6.10.1\n";
        assert_eq!(version_field(info, "KWin version:").unwrap(), "6.7.4");
        assert_eq!(version_field(info, "Qt Version:").unwrap(), "6.10.2");
        for invalid in [
            "KWin version: ../6.7.4",
            "KWin version: 6.7.4 extra",
            "no version",
        ] {
            assert!(version_field(invalid, "KWin version:").is_err());
        }
    }

    #[test]
    fn plugin_location_is_conventional_and_optional() {
        assert_eq!(
            plugin_directory(|p| p == "/usr/lib64/qt6/plugins/kwin/plugins"),
            Some("/usr/lib64/qt6/plugins")
        );
        assert_eq!(
            plugin_directory(|p| p == "/usr/lib/x86_64-linux-gnu/qt6/plugins/kwin/plugins"),
            Some("/usr/lib/x86_64-linux-gnu/qt6/plugins")
        );
        assert_eq!(plugin_directory(|_| false), None);
    }
}
