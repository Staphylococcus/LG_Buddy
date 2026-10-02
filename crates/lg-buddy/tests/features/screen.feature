Feature: Screen
  LG Buddy should expose manual screen blanking and restoration through the public CLI.

  Scenario: Screen off works after daemon conversion
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    When I run the command "screen off"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    And the TV screen is blanked
    And the session marker exists

  Scenario: Screen on works after daemon conversion
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    And the native webOS TV screen is blanked
    And the session marker exists
    When I run the command "screen on"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    And the TV screen is visible
    And the session marker is absent

  Scenario: Screen help describes the public commands
    When I run the command "screen --help"
    Then the command succeeds
    And stdout contains "screen off"
    And stdout contains "screen on"
    And stdout does not contain "screen-off"
    And stdout does not contain "screen-on"

  Scenario: Global help exposes screen without flat compatibility aliases
    When I run the command "--help"
    Then the command succeeds
    And stdout contains "screen off"
    And stdout contains "screen on"
    And stdout does not contain "screen-off"
    And stdout does not contain "screen-on"

  Scenario: Invalid screen commands show scoped usage
    When I run the command "screen toggle"
    Then the command fails
    And the command exits with status 2
    And stderr contains "unknown screen command `toggle`"
    And stderr contains "screen off"
    And stderr contains "screen on"

  Scenario: Flat screen compatibility aliases remain operational
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    When I run the command "screen-off"
    Then the command succeeds
    And the TV screen is blanked
    And the session marker exists
    When I run the command "screen-on"
    Then the command succeeds
    And the TV screen is visible
    And the session marker is absent
    And the native webOS TV received exactly 1 requests to "ssap://com.webos.service.tvpower/power/turnOffScreen"
    And the native webOS TV received exactly 1 requests to "ssap://com.webos.service.tvpower/power/turnOnScreen"
