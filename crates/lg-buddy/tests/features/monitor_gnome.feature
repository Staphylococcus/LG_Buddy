Feature: GNOME monitor
  LG Buddy should translate GNOME session signals and idle-monitor activity into TV behavior.

  Scenario: GNOME idle blanks the configured TV input
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME reports the session idle
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Using GNOME backend."
    And the TV client received "get_input"
    And the TV client received "turn_screen_off"
    And the session marker exists
    And the TV screen is blanked

  Scenario: GNOME idle skips TV blanking on a different input
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the session marker exists
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME reports the session idle
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Skipping idle action."
    And the TV client received "get_input"
    And the TV client did not receive "turn_screen_off"
    And the session marker is absent
    And the TV screen is visible

  Scenario: GNOME active restores a previously blanked TV output
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV screen is blanked
    And the session marker exists
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME reports the session active
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "requests screen restore"
    And the TV client received "turn_screen_on"
    And the session marker is absent
    And the TV screen is visible

  Scenario: GNOME wake request restores a previously blanked TV output
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV screen is blanked
    And the session marker exists
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME requests screen wake
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "wake-requested"
    And the TV client received "turn_screen_on"
    And the session marker is absent
    And the TV screen is visible

  Scenario: GNOME early user activity restores a blanked TV before the session becomes active again
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME reports the session idle
    And GNOME idle monitor would soon report recent user activity
    And GNOME monitor stays open briefly for background polling
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off"
    And the TV client received "turn_screen_on"
    And the session marker is absent
    And the TV screen is visible
