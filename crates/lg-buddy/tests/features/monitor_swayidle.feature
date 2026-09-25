Feature: swayidle monitor
  LG Buddy consumes the legacy swayidle backend through the shared monitor policy.
  A stale 1.x config (or an explicit swayidle backend override) is rejected by the
  migration gate before any runtime work; the no-delegation behavior is still
  verified against a clean native config.

  Scenario: automatic monitoring never delegates to installed swayidle
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And swayidle is installed
    And swayidle will emit an idle timeout
    And GNOME monitor stays open for 0.2 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "no native activity source available"
    And stdout does not contain "Using swayidle backend"
    And the TV screen is visible

  Scenario: a legacy swayidle backend override is rejected before runtime work
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And the existing config sets screen backend "swayidle"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And swayidle is installed
    When I run the command "monitor"
    Then the command fails
    And stderr contains "v2 migration required"
    And stderr contains "screen_backend=swayidle"
    And the TV screen is visible
