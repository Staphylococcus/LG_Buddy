Feature: Applications can prevent automatic idle blanking
  Honoring is opt-in. Inhibition gates automatic idle blanking independently of
  activity, with a full idle timeout after the last observed inhibitor release.

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

  Scenario: Playback already running prevents blanking when honoring is enabled
    Given honoring app keep-awake requests is "enabled"
    And GNOME monitor stays open for 1.3 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"
    And the TV screen is visible

  Scenario: Playback started after monitoring prevents blanking
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 0 idle inhibitors
    And GNOME changes to 1 idle inhibitors after 0.3 seconds
    And GNOME monitor stays open for 1.4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"

  Scenario: Missing inhibition support does not remove GNOME activity support
    Given honoring app keep-awake requests is "enabled"
    And GNOME SessionManager is unavailable
    And GNOME monitor stays open for 1.4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off" exactly 1 times

  Scenario: Ending one of two playback inhibitors keeps the screen visible
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 2 idle inhibitors
    And GNOME changes to 1 idle inhibitors after 0.3 seconds
    And GNOME monitor stays open for 1.4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"

  Scenario: The last release waits a full timeout without requiring user input
    Given honoring app keep-awake requests is "enabled"
    And GNOME changes to 0 idle inhibitors after 0.7 seconds
    And GNOME monitor stays open for 1.4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"

  Scenario: Blanking resumes after the release delay
    Given honoring app keep-awake requests is "enabled"
    And GNOME changes to 0 idle inhibitors after 0.3 seconds
    And GNOME monitor stays open for 2.5 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off" exactly 1 times
    And the TV client did not receive "turn_screen_on"
    And the TV screen is blanked

  Scenario: A PowerDevil inhibitor blocks even when GNOME is clear
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 0 idle inhibitors
    And PowerDevil screen inhibition is "active"
    And GNOME monitor stays open for 1.4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"

  Scenario: A failed PowerDevil check contributes no inhibitor
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 0 idle inhibitors
    And PowerDevil screen inhibition is "active"
    And PowerDevil fails its next inhibition query
    And GNOME monitor stays open for 1.4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off" exactly 1 times

  Scenario: Input cancels a delayed clear answer before it can blank
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 0 idle inhibitors
    And PowerDevil screen inhibition is "clear"
    And PowerDevil delays its next inhibition query by 0.6 seconds
    And gamepad activity is observed after 1.2 seconds
    And GNOME monitor stays open for 1.9 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"

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
