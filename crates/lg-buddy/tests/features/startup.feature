Feature: Startup
  LG Buddy should restore or initialize the TV output on session startup, against a
  clean native lg_webos config (the 1.x bscpylgtv fixture is rejected by the
  migration gate since #256).

  Scenario: Startup waits for network before setting the configured input
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And nm-online succeeds
    And startup delays are disabled
    When I run the command "startup boot"
    Then the command succeeds
    And nm-online was invoked with "-q -t 60"
    And the TV input is HDMI_2
    And the native webOS TV received exactly 1 requests to "ssap://tv/switchInput"

  Scenario: Startup continues even when nm-online fails
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And nm-online fails with status 1
    And startup delays are disabled
    When I run the command "startup boot"
    Then the command succeeds
    And nm-online was invoked with "-q -t 60"
    And the TV input is HDMI_2
    And the native webOS TV received exactly 1 requests to "ssap://tv/switchInput"

  Scenario: Wake mode skips when the system marker is missing
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And startup delays are disabled
    When I run the command "startup wake"
    Then the command succeeds
    And stdout contains "Wake from sleep: TV was not on our input. Skipping."
    And the system marker is absent
    And the native webOS TV received exactly 0 requests to "ssap://tv/switchInput"

  Scenario: Auto mode restores the configured input when the system marker exists
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And the system marker exists
    And startup delays are disabled
    When I run the command "startup auto"
    Then the command succeeds
    And stdout contains "Wake from sleep: LG Buddy turned TV off. Restoring."
    And stdout contains "TV turned on and set to HDMI_2."
    And the system marker is absent
    And the TV input is HDMI_2
    And the native webOS TV received exactly 1 requests to "ssap://tv/switchInput"

  Scenario: Boot mode clears an existing system marker and sets the configured input
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And the system marker exists
    And startup delays are disabled
    When I run the command "startup boot"
    Then the command succeeds
    And stdout contains "Cold boot: Turning TV on and switching to HDMI_2."
    And stdout contains "TV turned on and set to HDMI_2."
    And the system marker is absent
    And the TV input is HDMI_2
    And the native webOS TV received exactly 1 requests to "ssap://tv/switchInput"
