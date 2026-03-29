Feature: Screen off
  LG Buddy should blank the configured TV output when the user goes idle.

  Scenario: Matching input blanks the screen and records ownership
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    When I run the command "screen-off"
    Then the command succeeds
    And stdout contains "Screen blank command succeeded."
    And the session marker exists
    And the TV client received "get_input"
    And the TV client received "turn_screen_off"

  Scenario: Nonmatching input skips idle action
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the session marker exists
    When I run the command "screen-off"
    Then the command succeeds
    And stdout contains "Skipping idle action."
    And the session marker is absent
    And the TV client received "get_input"
    And the TV client did not receive "turn_screen_off"

  Scenario: Blank failure falls back to power off
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the TV will fail "turn_screen_off" with status 1 and stderr "blank failed"
    When I run the command "screen-off"
    Then the command succeeds
    And stdout contains "Fallback power_off succeeded."
    And the session marker exists
    And the TV client received "power_off"
