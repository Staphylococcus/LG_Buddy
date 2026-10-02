Feature: TV auth context
  LG Buddy should derive one consistent user-owned auth context for TV helper calls.

  Scenario: screen off uses the config-owned auth context by default
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the inherited user environment is cleared
    When I run the command "screen off"
    Then the command succeeds
    And the native webOS TV received exactly 1 requests to "ssap://com.webos.service.tvpower/power/turnOffScreen"

  Scenario: sleep-pre uses the config-owned auth context
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And the inherited user environment is cleared
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And the system marker exists
    And the TV is powered off
