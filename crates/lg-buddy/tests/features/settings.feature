Feature: Settings CLI
  LG Buddy should expose structured settings over the existing config.env file.

  Scenario: settings list shows values and supported operations
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings list"
    Then the command succeeds
    And stdout contains "screen.backend=auto (config.env, read-write, ops: get,describe,set,unset)"
    And stdout contains "screen.restore_policy=conservative (default, read-write, ops: get,describe,set,unset)"
    And stdout contains "system.sleep_wake_policy=enabled (default, read-only, ops: get,describe)"

  Scenario: settings describe shows read-only lifecycle operations
    Given a temporary LG Buddy config using input HDMI_2
    When I run the command "settings describe system.sleep_wake_policy"
    Then the command succeeds
    And stdout contains "system.sleep_wake_policy"
    And stdout contains "mutability: read-only"
    And stdout contains "supported operations: get, describe"

  Scenario: settings set writes config.env and reports skipped apply
    Given a temporary LG Buddy config using input HDMI_2
    And systemd apply actions are skipped
    When I run the command "settings set screen.idle_timeout 600"
    Then the command succeeds
    And stdout contains "screen.idle_timeout=600"
    And stdout contains "apply: skipped systemd apply"
    And config.env contains "screen_idle_timeout=600"

  Scenario: settings set writes a restore policy consumed by screen runtime
    Given a temporary LG Buddy config using input HDMI_3
    And systemd apply actions are skipped
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV screen is blanked
    When I run the command "settings set screen.restore_policy aggressive"
    Then the command succeeds
    And config.env contains "screen_restore_policy=aggressive"
    When I run the command "screen-on"
    Then the command succeeds
    And stdout contains "Aggressive restore policy is enabled"
    And the TV client received "turn_screen_on"
    And the session marker is absent

  Scenario: settings unset removes an override and restores the default
    Given a temporary LG Buddy config using input HDMI_2
    And the screen restore policy is "aggressive"
    And systemd apply actions are skipped
    When I run the command "settings unset screen.restore_policy"
    Then the command succeeds
    And stdout contains "screen.restore_policy unset"
    And config.env does not contain "screen_restore_policy="
    When I run the command "settings get screen.restore_policy"
    Then the command succeeds
    And stdout is "conservative"

  Scenario: settings set restarts an active user screen service
    Given a temporary LG Buddy config using input HDMI_2
    And the user screen service is active
    When I run the command "settings set screen.backend gnome"
    Then the command succeeds
    And stdout contains "apply: restarted LG_Buddy_screen.service"
    And stdout does not contain "Description=LG Buddy Screen Monitor Service"
    And config.env contains "screen_backend=gnome"
    And systemctl was invoked with "--user restart LG_Buddy_screen.service"

  Scenario: read-only lifecycle settings are rejected without changing config.env
    Given a temporary LG Buddy config using input HDMI_2
    And the current config is remembered
    When I run the command "settings set system.sleep_wake_policy disabled"
    Then the command fails
    And stderr contains "setting `system.sleep_wake_policy` does not support `set`"
    And config.env is unchanged
