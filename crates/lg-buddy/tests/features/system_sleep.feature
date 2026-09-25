Feature: System sleep hook
  LG Buddy should power off the TV before system sleep when ownership rules require it.

  Scenario: sleep-pre requires migration on a legacy bscpylgtv config
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "get_input"
    And the TV client did not receive "power_off"
    And the system marker is absent
    And the TV is powered on

  Scenario: sleep migration gate runs before journal queries and TV work
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And sleep retry delays are disabled
    And journalctl reports a pending NetworkManager sleep request
    When I run the command "sleep"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "get_input"
    And the TV client did not receive "power_off"
    And the system marker is absent
    And the TV is powered on

  Scenario: sleep-pre skips when the TV is on a different input
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the system marker exists
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And stdout contains "TV is on HDMI_2 (not HDMI_3). Skipping."
    And the native webOS TV received exactly 1 requests to "ssap://com.webos.applicationManager/getForegroundAppInfo"
    And the native webOS TV received exactly 0 requests to "ssap://system/turnOff"
    And the system marker is absent
    And the TV is powered on

  Scenario: sleep-pre migration gate runs before the input query fallback
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV will fail "get_input" 4 times with status 1 and stderr "offline"
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "get_input"
    And the TV client did not receive "power_off"
    And the system marker is absent
