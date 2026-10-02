Feature: System sleep hook
  LG Buddy should power off the TV before system sleep when ownership rules require it.

  Scenario: sleep-pre skips without native credentials after daemon conversion
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_3 with brightness 100
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    And stdout contains "Stored TV authentication is unavailable"
    And the native webOS TV received no requests
    And the system marker is absent
    And the TV is powered on

  Scenario: sleep skips TV work without credentials after daemon conversion
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_3 with brightness 100
    And sleep retry delays are disabled
    And journalctl reports a pending NetworkManager sleep request
    When I run the command "sleep"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    And stdout contains "Stored TV authentication is unavailable"
    And the native webOS TV received no requests
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

  Scenario: Daemon conversion does not require an online TV
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    And the native webOS TV powers off
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    And stdout contains "Stored TV authentication is unavailable"
    And the native webOS TV received no requests
    And the system marker is absent
    And the TV is powered off
