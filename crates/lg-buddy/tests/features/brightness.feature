Feature: Brightness
  LG Buddy should provide a manual OLED brightness control for the configured TV.

  Scenario: Brightness sets the TV OLED brightness when the TV is reachable
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 72
    And the TV is reachable over ping
    And the brightness dialog returns 65
    When I run the command "brightness"
    Then the command succeeds
    And stdout contains "Set OLED pixel brightness to 65%."
    And the TV client received "get_picture_settings"
    And the TV client received "set_settings"
    And the TV brightness is 65

  Scenario: Brightness exits cleanly when the dialog is cancelled
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 44
    And the TV is reachable over ping
    And the brightness dialog is cancelled
    When I run the command "brightness"
    Then the command succeeds
    And the TV client received "get_picture_settings"
    And the TV client did not receive "set_settings"
    And the TV brightness is 44

  Scenario: Brightness fails when the TV is unreachable
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV is unreachable over ping
    And the brightness error dialog is available
    When I run the command "brightness"
    Then the command fails
    And stderr contains "TV is not reachable"
    And the TV client did not receive "set_settings"
