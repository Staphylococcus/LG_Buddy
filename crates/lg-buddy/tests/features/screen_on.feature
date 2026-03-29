Feature: Screen on
  LG Buddy should restore the TV output when it owns the prior idle action.

  Scenario: Missing marker skips wake
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    When I run the command "screen-on"
    Then the command succeeds
    And stdout contains "State file not found."
    And the TV client did not receive "turn_screen_on"

  Scenario: Owned marker unblanks the screen
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the TV screen is blanked
    And the session marker exists
    When I run the command "screen-on"
    Then the command succeeds
    And stdout contains "Screen unblank succeeded."
    And the session marker is absent
    And the TV client received "turn_screen_on"

  Scenario: Active screen state falls back to immediate input restore
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the session marker exists
    When I run the command "screen-on"
    Then the command succeeds
    And stdout contains "Immediate input restore succeeded."
    And the session marker is absent
    And the TV client received "turn_screen_on"
    And the TV client received "set_input"
