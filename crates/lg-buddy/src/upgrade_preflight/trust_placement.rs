//! Resolves the trust roots an installed-state check runs under.
//!
//! Given the host layout and the owning UIDs, this returns the `TrustedRoot`
//! for each of the three installation domains the preflight evaluates: the
//! system tree, the user's home tree, and the user's unit/config tree. The
//! user unit domain is the only one with a decision: when the user's config
//! home lives inside the user's home the units are trusted as part of the
//! home, otherwise the config home is its own trusted root.

use super::observation::InstalledLayout;
use super::path_safety::TrustedRoot;

#[derive(Debug, Clone, Copy)]
pub(super) struct TrustPlacement<'a> {
    pub(super) system_trust: TrustedRoot<'a>,
    pub(super) user_trust: TrustedRoot<'a>,
    pub(super) user_units_trust: TrustedRoot<'a>,
}

pub(super) fn trust_placement<'a>(
    layout: &'a InstalledLayout,
    system_owner_uid: u32,
    user_owner_uid: u32,
) -> TrustPlacement<'a> {
    let system_trust = TrustedRoot::strict(&layout.system_root, system_owner_uid);
    let user_trust = TrustedRoot::owned(&layout.user_home, user_owner_uid);
    let user_units_trust = if layout.user_config_home.starts_with(&layout.user_home) {
        user_trust
    } else {
        TrustedRoot::owned(&layout.user_config_home, user_owner_uid)
    };
    TrustPlacement {
        system_trust,
        user_trust,
        user_units_trust,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::observation::InstalledLayout;
    use super::super::path_safety::TrustedRoot;
    use super::trust_placement;

    fn layout(config_home: &str) -> InstalledLayout {
        InstalledLayout {
            system_root: PathBuf::from("/usr"),
            user_home: PathBuf::from("/home/u"),
            user_config_home: PathBuf::from(config_home),
        }
    }

    #[test]
    fn system_and_user_roots_are_strict_and_owned() {
        let layout = layout("/home/u/.config");
        let roots = trust_placement(&layout, 0, 1000);
        assert_eq!(
            roots.system_trust,
            TrustedRoot::strict(&layout.system_root, 0),
            "system tree is checked under a strict, root-owned trust root"
        );
        assert_eq!(
            roots.user_trust,
            TrustedRoot::owned(&layout.user_home, 1000),
            "user home is checked under an owned, user trust root"
        );
    }

    #[test]
    fn user_units_follow_config_home_when_config_is_outside_home() {
        let layout = layout("/custom/config");
        let roots = trust_placement(&layout, 0, 1000);
        assert_eq!(
            roots.user_units_trust,
            TrustedRoot::owned(&layout.user_config_home, 1000),
            "config outside the home is its own trusted root"
        );
        assert_ne!(
            roots.user_units_trust, roots.user_trust,
            "the units root must not fall back to the home root"
        );
    }

    #[test]
    fn user_units_follow_home_when_config_is_inside_home() {
        let layout = layout("/home/u/.config");
        let roots = trust_placement(&layout, 0, 1000);
        assert_eq!(
            roots.user_units_trust, roots.user_trust,
            "a config home inside the home is trusted as part of the home"
        );
    }
}
