Feature: Volume
  LG Buddy should expose predictable volume and mute controls for the configured TV.

  Background:
    Given a temporary LG Buddy config using input HDMI_2

  Scenario: Volume read works after daemon conversion
    Given the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    And the TV volume is 37
    And the TV is unmuted
    When I run the command "volume"
    Then the command succeeds
    And stdout is "37"
    And config.env contains "tvs_primary_platform=lg_webos"

  Scenario: Muted volume read works after daemon conversion
    Given the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    And the TV volume is 37
    And the TV is muted
    When I run the command "volume"
    Then the command succeeds
    And stdout is "mute"
    And config.env contains "tvs_primary_platform=lg_webos"

  Scenario: Unknown-level volume read works after daemon conversion
    Given the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    And the TV volume is unknown
    And the TV is unmuted
    When I run the command "volume"
    Then the command succeeds
    And stdout is "unknown"
    And config.env contains "tvs_primary_platform=lg_webos"

  Scenario: Setting volume works after daemon conversion
    Given the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    And the TV volume is 20
    And the TV is muted
    When I run the command "volume 42"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    And the TV volume is 42

  Scenario: Stepping volume works after daemon conversion
    Given the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    And the TV volume is 20
    And the TV is muted
    When I run the command "volume up"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    And the TV volume is 21
    Given the TV is muted
    When I run the command "volume down"
    Then the command succeeds
    And the TV volume is 20

  Scenario: Mute toggle works after daemon conversion
    Given the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    Given the TV is unmuted
    When I run the command "volume mute"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    When I run the command "volume mute off"
    Then the command succeeds
    When I run the command "volume mute on"
    Then the command succeeds
    And the TV is muted

  Scenario: Invalid volume is rejected before touching the TV
    Given a native webOS TV on input HDMI_2 with brightness 90
    Given the TV volume is 20
    When I run the command "volume 101"
    Then the command fails
    And the command exits with status 2
    And stderr contains "invalid volume"
    And stderr contains "volume <0-100>"
    And the native webOS TV received no requests
    And the TV volume is 20

  Scenario: Volume command applies native write after daemon conversion
    Given the existing config selects TV platform "bscpylgtv"
    And the user screen daemon startup check runs
    And a native webOS TV on input HDMI_2 with brightness 90
    Given the TV volume is 20
    And the TV is muted
    When I run the command "volume up"
    Then the command succeeds
    And config.env contains "tvs_primary_platform=lg_webos"
    And the TV volume is 21
    And the TV is unmuted

  Scenario: Native webOS exposes the same volume and mute behavior
    Given the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    When I run the command "volume"
    Then the command succeeds
    And stdout is "20"
    When I run the command "volume mute on"
    Then the command succeeds
    And the TV is muted
    When I run the command "volume up"
    Then the command succeeds
    And the TV volume is 21
    And the TV is unmuted
    When I run the command "volume 19"
    Then the command succeeds
    And the TV volume is 19
    And the TV is unmuted
    Given the TV is muted
    When I run the command "volume"
    Then the command succeeds
    And stdout is "mute"
    When I run the command "volume down"
    Then the command succeeds
    And the TV volume is 18
    And the TV is unmuted
    When I run the command "volume mute"
    Then the command succeeds
    And the TV is muted
    When I run the command "volume mute off"
    Then the command succeeds
    And the TV is unmuted
    And the native webOS TV received exactly 1 requests to "ssap://audio/volumeDown"
    And the native webOS TV received exactly 6 requests to "ssap://audio/setMute"
    Given the TV volume is unknown
    When I run the command "volume"
    Then the command succeeds
    And stdout is "unknown"

  Scenario: Native webOS does not replay volume when unmuting is rejected
    Given the existing config selects TV platform "lg_webos"
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the TV volume is 20
    And the TV is muted
    And the native webOS TV rejects mute changes
    When I run the command "volume up"
    Then the command fails
    And stderr contains "volume was changed, but unmuting failed"
    And the TV volume is 21
    And the TV is muted
