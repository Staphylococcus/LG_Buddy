Feature: Startup
  LG Buddy should restore or initialize the TV output on session startup.

  Scenario: Wake mode skips when the system marker is missing
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And startup delays are disabled
    When I run the command "startup wake"
    Then the command succeeds
    And stdout contains "Wake from sleep: TV was not on our input. Skipping."
    And the system marker is absent
    And the TV client did not receive "set_input"

  Scenario: Auto mode restores the configured input when the system marker exists
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_3
    And the system marker exists
    And startup delays are disabled
    When I run the command "startup auto"
    Then the command succeeds
    And stdout contains "Wake from sleep: LG Buddy turned TV off. Restoring."
    And stdout contains "TV turned on and set to HDMI_2."
    And the system marker is absent
    And the TV input is HDMI_2
    And the TV client received "set_input"

  Scenario: Boot mode clears an existing system marker and sets the configured input
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_4
    And the system marker exists
    And startup delays are disabled
    When I run the command "startup boot"
    Then the command succeeds
    And stdout contains "Cold boot: Turning TV on and switching to HDMI_2."
    And stdout contains "TV turned on and set to HDMI_2."
    And the system marker is absent
    And the TV input is HDMI_2
    And the TV client received "set_input"
