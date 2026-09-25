Feature: NetworkManager sleep hook
  LG Buddy should only power off the TV from the NetworkManager pre-down hook when the host is actually entering sleep.

  Scenario: sleep skips TV control when NetworkManager is not entering sleep
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And journalctl does not report a pending NetworkManager sleep request
    And sleep retry delays are disabled
    When I run the command "sleep"
    Then the command succeeds
    And the native webOS TV received exactly 0 requests to "ssap://com.webos.applicationManager/getForegroundAppInfo"
    And the native webOS TV received exactly 0 requests to "ssap://system/turnOff"
    And the system marker is absent

  Scenario: sleep powers off the TV when NetworkManager is entering sleep on the configured input
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And journalctl reports a pending NetworkManager sleep request
    And sleep retry delays are disabled
    When I run the command "sleep"
    Then the command succeeds
    And the native webOS TV received exactly 1 requests to "ssap://com.webos.applicationManager/getForegroundAppInfo"
    And the native webOS TV received exactly 1 requests to "ssap://system/turnOff"
    And the system marker exists
    And the TV is powered off

  Scenario: sleep skips when the TV is on a different input
    Given a temporary LG Buddy config using input HDMI_3
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the system marker exists
    And journalctl reports a pending NetworkManager sleep request
    And sleep retry delays are disabled
    When I run the command "sleep"
    Then the command succeeds
    And the native webOS TV received exactly 1 requests to "ssap://com.webos.applicationManager/getForegroundAppInfo"
    And the native webOS TV received exactly 0 requests to "ssap://system/turnOff"
    And the system marker is absent
    And the TV is powered on
