mod support;

use lg_buddy::settings::{ServiceController, SystemdUserServiceController, UserServiceState};
use support::{ExecutableScript, TestEnv};

const SCREEN: &str = "LG_Buddy_screen.service";

#[test]
fn service_state_propagates_a_timeout_from_each_query() {
    let systemctl = ExecutableScript::new(
        "service-query-timeout",
        "systemctl",
        "#!/bin/sh\n[ \"$2\" != \"$LG_BUDDY_TEST_SLOW_QUERY\" ] || exec sleep 30\nexit 0\n",
    );
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_SYSTEMCTL", systemctl.path());
    let controller = SystemdUserServiceController::from_env();

    for query in ["cat", "is-active", "is-enabled"] {
        env.set("LG_BUDDY_TEST_SLOW_QUERY", query);
        let error = controller.user_service_state(SCREEN).unwrap_err();
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains("service inspection timed out"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains(query), "{diagnostic}");
    }
}

#[test]
fn unsuccessful_statuses_still_describe_missing_and_inactive_services() {
    let systemctl = ExecutableScript::new(
        "service-query-status",
        "systemctl",
        r#"#!/bin/sh
if [ "$2" = cat ]; then exit "$LG_BUDDY_TEST_CAT_STATUS"; fi
exit "$LG_BUDDY_TEST_ACTIVE_STATUS"
"#,
    );
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_SYSTEMCTL", systemctl.path());
    let controller = SystemdUserServiceController::from_env();

    for (cat, active, expected) in [
        ("1", "3", UserServiceState::Missing),
        ("0", "3", UserServiceState::InactiveDisabled),
        ("0", "0", UserServiceState::ActiveOrEnabled),
    ] {
        env.set("LG_BUDDY_TEST_CAT_STATUS", cat);
        env.set("LG_BUDDY_TEST_ACTIVE_STATUS", active);
        assert_eq!(controller.user_service_state(SCREEN).unwrap(), expected);
        assert_eq!(
            controller.user_service_is_active(SCREEN).unwrap(),
            active == "0"
        );
        assert_eq!(
            controller.user_unit_is_enabled(SCREEN).unwrap(),
            active == "0"
        );
        assert_eq!(
            controller.system_lifecycle_is_active().unwrap(),
            active == "0"
        );
        assert_eq!(
            controller.system_unit_is_enabled(SCREEN).unwrap(),
            active == "0"
        );
    }
}

#[test]
fn an_unavailable_inspector_is_not_reported_as_an_inactive_service() {
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_SYSTEMCTL", "/dev/null/missing-systemctl");
    let controller = SystemdUserServiceController::from_env();
    assert!(controller.user_service_state(SCREEN).is_err());
    assert!(controller.user_service_is_active(SCREEN).is_err());
    assert!(controller.user_unit_is_enabled(SCREEN).is_err());
    assert!(controller.system_lifecycle_is_active().is_err());
    assert!(controller.system_unit_is_enabled(SCREEN).is_err());
}

#[test]
fn a_session_query_timeout_does_not_enable_or_start_the_service() {
    let systemctl = ExecutableScript::new(
        "service-session-timeout",
        "systemctl",
        r#"#!/bin/sh
if [ "$2" = is-active ]; then exec sleep 30; fi
printf '%s\n' "$*" >> "$0.actions"
exit 0
"#,
    );
    let mut env = TestEnv::new();
    env.set("LG_BUDDY_SYSTEMCTL", systemctl.path());
    let controller = SystemdUserServiceController::from_env();
    let error = controller.enable_start_user_unit(SCREEN).unwrap_err();
    assert!(error.to_string().contains("service inspection timed out"));
    assert!(!systemctl.path().with_extension("actions").exists());
}
