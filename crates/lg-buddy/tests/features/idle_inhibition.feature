Feature: Native activity during the dev inhibition refactor
  Issue #221 removes inhibition from activity. Native honoring is temporarily
  absent on dev until #225; this intermediate runtime cannot be promoted.
  Activity remains independent of inhibition services and preferences.

  Background:
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And GNOME has 1 idle inhibitors

  Scenario: Existing configurations keep blanking during playback
    Given GNOME monitor stays open for 1.3 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off" exactly 1 times
    And the TV screen is blanked

  Scenario: Native activity remains usable with the stored honoring preference enabled
    Given honoring app keep-awake requests is "enabled"
    And GNOME monitor stays open for 1.3 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "native inhibition honoring is temporarily unavailable on dev"
    And the TV client received "turn_screen_off" exactly 1 times
    And the TV screen is blanked

  Scenario: Gamepad input restores independently of desktop inhibition
    Given honoring app keep-awake requests is "enabled"
    And the TV screen is blanked
    And the session marker exists
    And gamepad activity is observed after 0.2 seconds
    And GNOME monitor stays open for 0.8 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_on" exactly 1 times
    And the TV client did not receive "turn_screen_off"
    And the TV screen is visible
    And the session marker is absent

  Scenario: Genuine desktop input restores the screen during inhibition
    Given honoring app keep-awake requests is "enabled"
    And the TV screen is blanked
    And the session marker exists
    And genuine desktop input occurs after 0.2 seconds
    And GNOME monitor stays open for 0.8 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_on" exactly 1 times
    And the TV client did not receive "turn_screen_off"
    And the TV screen is visible
    And the session marker is absent

  Scenario: Repeated desktop input rearms the activity watch beyond the idle timeout
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 0 idle inhibitors
    And the TV screen is blanked
    And the session marker exists
    And genuine desktop input occurs after 0.2 seconds
    And genuine desktop input occurs after 1.0 seconds
    And GNOME monitor stays open for 1.6 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_on" exactly 1 times
    And the TV client did not receive "turn_screen_off"
    And the TV screen is visible
    And the session marker is absent

  Scenario: An explicit lock blanks the screen during inhibition
    Given honoring app keep-awake requests is "enabled"
    And mock system logind reports LockedHint=true
    And GNOME monitor stays open for 0.4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off" exactly 1 times
    And the TV client did not receive "turn_screen_on"
    And the TV screen is blanked
