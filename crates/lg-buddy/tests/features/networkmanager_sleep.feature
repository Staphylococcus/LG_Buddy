Feature: NetworkManager sleep hook
  LG Buddy should only power off the TV from the NetworkManager pre-down hook when the host is actually entering sleep.

  Scenario: sleep skips TV control when NetworkManager is not entering sleep
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And journalctl does not report a pending NetworkManager sleep request
    And sleep retry delays are disabled
    When I run the command "sleep"
    Then the command succeeds
    And the TV client did not receive "get_input"
    And the TV client did not receive "power_off"
    And the system marker is absent

  Scenario: sleep powers off the TV when NetworkManager is entering sleep on the configured input
    Given a temporary LG Buddy config using input HDMI_2
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_2
    And journalctl reports a pending NetworkManager sleep request
    And sleep retry delays are disabled
    When I run the command "sleep"
    Then the command succeeds
    And the TV client received "get_input"
    And the TV client received "power_off"
    And the system marker exists
    And the TV is powered off

  Scenario: sleep skips when the TV is on a different input
    Given a temporary LG Buddy config using input HDMI_4
    And LG Buddy session runtime is isolated
    And a mock TV client
    And the TV is on input HDMI_1
    And the system marker exists
    And journalctl reports a pending NetworkManager sleep request
    And sleep retry delays are disabled
    When I run the command "sleep"
    Then the command succeeds
    And the TV client received "get_input"
    And the TV client did not receive "power_off"
    And the system marker is absent
    And the TV is powered on
