Feature: Shutdown
  LG Buddy should power off the TV only when shutdown semantics require it.

  Scenario: Shutdown powers off the TV when the configured input is active
    Given a temporary LG Buddy config using input HDMI_3
    And a mock TV client
    And the TV is on input HDMI_3
    And reboot detection reports no pending reboot
    When I run the command "shutdown"
    Then the command succeeds
    And stdout contains "TV is on HDMI_3. Turning off for shutdown."
    And the TV client received "get_input"
    And the TV client received "power_off"
    And the TV is powered off

  Scenario: Shutdown skips when the TV is on a different input
    Given a temporary LG Buddy config using input HDMI_3
    And a mock TV client
    And the TV is on input HDMI_2
    And reboot detection reports no pending reboot
    When I run the command "shutdown"
    Then the command succeeds
    And stdout contains "TV is on HDMI_2 (not HDMI_3). Skipping."
    And the TV client received "get_input"
    And the TV client did not receive "power_off"
    And the TV is powered on

  Scenario: Shutdown skips TV control when a reboot is pending
    Given a temporary LG Buddy config using input HDMI_3
    And a mock TV client
    And the TV is on input HDMI_3
    And reboot detection reports a pending reboot
    When I run the command "shutdown"
    Then the command succeeds
    And stdout contains "Reboot; ignoring"
    And the TV client did not receive "get_input"
    And the TV client did not receive "power_off"
    And the TV is powered on
