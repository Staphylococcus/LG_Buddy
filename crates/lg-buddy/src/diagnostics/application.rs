//! Application build identity.

use super::DiagnosticSection;
use crate::version::VersionInfo;
use std::env;

pub(super) fn collect() -> DiagnosticSection {
    collect_from(VersionInfo::current(), env::consts::OS, env::consts::ARCH)
}

fn collect_from(version: VersionInfo, os: &str, architecture: &str) -> DiagnosticSection {
    let mut body = String::new();
    body.push_str("version: ");
    body.push_str(version.version());
    body.push('\n');
    body.push_str("channel: ");
    body.push_str(version.channel().as_str());
    body.push('\n');
    body.push_str("commit: ");
    body.push_str(version.commit().unwrap_or("unknown"));
    body.push('\n');
    body.push_str("target OS: ");
    body.push_str(os);
    body.push('\n');
    body.push_str("target architecture: ");
    body.push_str(architecture);
    body.push('\n');
    DiagnosticSection::new("Application and build", body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::version::ReleaseChannel;

    #[test]
    fn build_identity_does_not_depend_on_the_host_environment() {
        let version = VersionInfo::for_testing("1.8.0", ReleaseChannel::Stable, Some("abc123"));
        assert_eq!(collect_from(version, "linux", "aarch64").body(),
            "version: 1.8.0\nchannel: stable\ncommit: abc123\ntarget OS: linux\ntarget architecture: aarch64\n");
        let version = VersionInfo::for_testing("1.8.0", ReleaseChannel::Dev, None);
        assert!(collect_from(version, "linux", "x86_64")
            .body()
            .contains("commit: unknown"));
    }
}
