Feature: App keep-awake requests
  Users can choose whether desktop keep-awake requests prevent automatic blanking.
  Keep-awake requests are not user input and must not restore the TV.

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

  Scenario: Enabled keep-awake support covers playback already active at startup
    Given honoring app keep-awake requests is "enabled"
    And GNOME monitor stays open for 1.3 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"
    And the TV client did not receive "turn_screen_on"
    And the TV screen is visible

  Scenario: An inhibitor added before the blanking deadline survives a slow GNOME refresh
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 0 idle inhibitors
    And GNOME changes to 1 idle inhibitors after 0.8 seconds
    And GNOME delays the next inhibited query by 0.5 seconds
    And GNOME monitor stays open for 1.6 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"
    And the TV screen is visible

  Scenario: Ending one of several keep-awake requests does not permit blanking
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 2 idle inhibitors
    And GNOME changes to 1 idle inhibitors after 0.2 seconds
    And GNOME monitor stays open for 1.5 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_off"
    And the TV client did not receive "turn_screen_on"

  Scenario Outline: Ending the last keep-awake request starts a fresh full timeout
    Given honoring app keep-awake requests is "enabled"
    And GNOME has 2 idle inhibitors
    And GNOME changes to 1 idle inhibitors after 0.2 seconds
    And GNOME changes to 0 idle inhibitors after 1.5 seconds
    And GNOME monitor stays open for <duration> seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client received "turn_screen_off" exactly <blanks> times
    And the TV client did not receive "turn_screen_on"

    Examples:
      | duration | blanks |
      | 2.2      | 0      |
      | 2.9      | 1      |

  Scenario: Releasing inhibition does not restore an already blanked screen
    Given honoring app keep-awake requests is "enabled"
    And the TV screen is blanked
    And the session marker exists
    And GNOME changes to 0 idle inhibitors after 0.2 seconds
    And GNOME idle monitor will report idletimes "1000, 50, 300, 550"
    And GNOME monitor stays open for 0.8 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the TV client did not receive "turn_screen_on"
    And the TV screen is blanked
    And the session marker exists

  Scenario: Gamepad input still restores the screen during inhibition
    Given honoring app keep-awake requests is "enabled"
    And the TV screen is blanked
    And the session marker exists
    And gamepad activity is observed after 0.2 seconds
    And GNOME monitor stays open for 1.6 seconds
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

  Scenario: Genuine desktop input immediately after release restores the screen
    Given honoring app keep-awake requests is "enabled"
    And the TV screen is blanked
    And the session marker exists
    And GNOME changes to 0 idle inhibitors after 0.2 seconds
    And genuine desktop input occurs after 0.3 seconds
    And GNOME monitor stays open for 0.9 seconds
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
