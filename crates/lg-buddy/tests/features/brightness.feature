Feature: Brightness
  LG Buddy should provide a manual OLED brightness control for the configured TV.

  Scenario: Brightness launches the GTK window through the stable command
    Given a working GTK brightness GUI
    When I run the command "brightness"
    Then the command succeeds
    And the GTK brightness GUI received "brightness"

  Scenario: A failed GTK launch reports the installation failure
    Given the GTK brightness GUI exits with status 23
    When I run the command "brightness"
    Then the command fails
    And the command exits with status 1
    And stderr contains "installed LG Buddy GUI"
    And stderr contains "exited with status 23"
    And the GTK brightness GUI received "brightness"

  Scenario: Missing GTK GUI reports how to repair the installation without contacting the TV
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the GTK brightness GUI is unavailable
    When I run the command "brightness"
    Then the command fails
    And the command exits with status 1
    And stderr contains "LG Buddy GUI is not installed"
    And stderr contains "install the matching lg-buddy-gui executable"
    And the TV client did not receive "get_picture_settings"
    And the TV client did not receive "set_settings"

  Scenario: Headless brightness remains usable without the GTK GUI
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 58
    And the GTK brightness GUI is unavailable
    When I run the command "brightness get"
    Then the command succeeds
    And stdout is "58"
    When I run the command "brightness set 66"
    Then the command succeeds
    And the TV brightness is 66

  Scenario: Brightness get prints the current OLED brightness
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 58
    And a working GTK brightness GUI
    When I run the command "brightness get"
    Then the command succeeds
    And stdout is "58"
    And the GTK brightness GUI was not launched
    And the TV client received "get_picture_settings"
    And the TV client did not receive "set_settings"

  Scenario: Brightness set updates OLED brightness without opening a dialog
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 44
    And a working GTK brightness GUI
    When I run the command "brightness set 66"
    Then the command succeeds
    And stdout contains "Set OLED pixel brightness to 66%."
    And the GTK brightness GUI was not launched
    And the TV client received "set_settings"
    And the TV brightness is 66

  Scenario: Brightness set rejects invalid values before touching the TV
    Given a temporary LG Buddy config using input HDMI_2
    And a mock TV client
    And the TV backlight is 44
    When I run the command "brightness set 101"
    Then the command fails
    And the command exits with status 2
    And stderr contains "invalid OLED brightness"
    And stderr contains "brightness set <0-100>"
    And the TV client did not receive "get_picture_settings"
    And the TV client did not receive "set_settings"
    And the TV brightness is 44

  Scenario: Brightness help describes the public commands
    When I run the command "brightness --help"
    Then the command succeeds
    And stdout contains "brightness get"
    And stdout contains "brightness set <0-100>"

  Scenario: Global help exposes the brightness family
    When I run the command "--help"
    Then the command succeeds
    And stdout contains "brightness"
    And stdout contains "brightness get"
    And stdout contains "brightness set <0-100>"

  Scenario: Invalid brightness commands show scoped usage
    When I run the command "brightness show"
    Then the command fails
    And the command exits with status 2
    And stderr contains "unknown brightness command `show`"
    And stderr contains "brightness get"
    And stderr contains "brightness set <0-100>"
