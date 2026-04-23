Feature: GNOME monitor
  LG Buddy should translate GNOME session signals and idle-monitor activity into TV behavior.

  Scenario: GNOME ScreenSaver idle still blanks the configured TV input
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

  Scenario: GNOME inactivity blanks the configured TV input when ScreenSaver idle is inhibited
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And GNOME monitor stays open for 1.0 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Using GNOME backend."
    And the TV client received "get_input"
    And the TV client received "turn_screen_off"
    And the session marker exists
    And the TV screen is blanked

  Scenario: GNOME inhibited inactivity does not blank repeatedly while idletime stays high
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000, 1500, 2000, 2500"
    And GNOME monitor stays open for 1.0 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off" exactly 1 times
    And the TV client did not receive "turn_screen_on"
    And the session marker exists
    And the TV screen is blanked

  Scenario: GNOME inactivity skips TV blanking on a different input
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the session marker exists
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And GNOME monitor stays open for 1.0 seconds
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

  Scenario: GNOME wake request can restore without a session marker in aggressive mode
    Given a temporary LG Buddy config using input HDMI_3
    And the screen restore policy is "aggressive"
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV is powered off
    And the next input restore attempt powers the TV back on
    And screen wake delays are disabled
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME requests screen wake
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Aggressive restore policy is enabled"
    And stdout contains "Wake attempt 1 succeeded."
    And the TV client received "turn_screen_on"
    And the TV client received "set_input"
    And the session marker is absent
    And the TV is powered on
    And the TV screen is visible

  Scenario: GNOME restore failure does not retry continuously while activity stays active
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the TV will fail "turn_screen_on" with status 1 and stderr "offline"
    And the TV will fail "set_input" 6 times with status 1 and stderr "not ready"
    And screen wake delays are disabled
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000, 0, 0, 0"
    And GNOME monitor stays open for 1.0 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "screen restore action failed"
    And the TV client received "turn_screen_off" exactly 1 times
    And the TV client received "turn_screen_on" exactly 1 times
    And the TV client received "set_input" exactly 6 times
    And the session marker exists
    And the TV screen is blanked

  Scenario: GNOME activity wakes a TV that was manually powered off after LG Buddy blanked it
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV screen is blanked
    And the TV is powered off
    And the session marker exists
    And the next input restore attempt powers the TV back on
    And screen wake delays are disabled
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME reports the session active
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Sending initial Wake-on-LAN packet"
    And stdout contains "Wake attempt 1 succeeded."
    And the TV client received "turn_screen_on"
    And the TV client received "set_input"
    And the session marker is absent
    And the TV is powered on
    And the TV screen is visible

  Scenario: GNOME early user activity restores a blanked TV before the session becomes active again
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000, 0, 0, 0"
    And GNOME monitor stays open for 1.0 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off" exactly 1 times
    And the TV client received "turn_screen_on" exactly 1 times
    And the session marker is absent
    And the TV screen is visible
