Feature: System sleep hook
  LG Buddy should power off the TV before system sleep when ownership rules require it.

  Scenario: sleep-pre powers off the TV when the configured input is active
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And stdout contains "TV is on HDMI_3. Turning off for sleep."
    And the TV client received "get_input"
    And the TV client received "power_off"
    And the system marker exists
    And the TV is powered off

  Scenario: sleep-pre skips when the TV is on a different input
    Given a temporary LG Buddy config using input HDMI_3
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And the system marker exists
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And stdout contains "TV is on HDMI_2 (not HDMI_3). Skipping."
    And the TV client received "get_input"
    And the TV client did not receive "power_off"
    And the system marker is absent
    And the TV is powered on

  Scenario: sleep-pre falls back to power-off when the TV input cannot be queried
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV will fail "get_input" 4 times with status 1 and stderr "offline"
    And sleep retry delays are disabled
    When I run the command "sleep-pre"
    Then the command succeeds
    And stdout contains "Could not query TV input. Attempting power_off fallback."
    And the TV client received "get_input"
    And the TV client received "power_off"
    And the system marker exists
