Feature: Volume
  LG Buddy should expose predictable volume and mute controls for the configured TV.

  Background:
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client

  Scenario: Volume requires migration on a legacy bscpylgtv config
    Given the TV volume is 37
    And the TV is unmuted
    When I run the command "volume"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "get_audio_status"

  Scenario: Volume mute read requires migration on a legacy bscpylgtv config
    Given the TV volume is 37
    And the TV is muted
    When I run the command "volume"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "get_audio_status"

  Scenario: Volume unknown-level read requires migration on a legacy bscpylgtv config
    Given the TV volume is unknown
    And the TV is unmuted
    When I run the command "volume"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "get_audio_status"

  Scenario: Setting volume requires migration on a legacy bscpylgtv config
    Given the TV volume is 20
    And the TV is muted
    When I run the command "volume 42"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "set_volume"
    And the TV client did not receive "set_mute"

  Scenario: Stepping volume requires migration on a legacy bscpylgtv config
    Given the TV volume is 20
    And the TV is muted
    When I run the command "volume up"
    Then the command fails
    And stderr contains "v2 migration required"
    Given the TV is muted
    When I run the command "volume down"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "volume_up"
    And the TV client did not receive "volume_down"
    And the TV volume is 20

  Scenario: Mute toggle requires migration on a legacy bscpylgtv config
    Given the TV is unmuted
    When I run the command "volume mute"
    Then the command fails
    And stderr contains "v2 migration required"
    When I run the command "volume mute off"
    Then the command fails
    And stderr contains "v2 migration required"
    When I run the command "volume mute on"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "set_mute"
    And the TV is unmuted

  Scenario: Invalid volume is rejected before touching the TV
    Given the TV volume is 20
    When I run the command "volume 101"
    Then the command fails
    And the command exits with status 2
    And stderr contains "invalid volume"
    And stderr contains "volume <0-100>"
    And the TV client did not receive "set_volume"
    And the TV volume is 20

  Scenario: Stale volume commands leave mute and volume unchanged
    Given the TV volume is 20
    And the TV is muted
    When I run the command "volume up"
    Then the command fails
    And stderr contains "v2 migration required"
    And the TV client did not receive "volume_up"
    And the TV client did not receive "set_mute"
    And the TV volume is 20
    And the TV is muted

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
