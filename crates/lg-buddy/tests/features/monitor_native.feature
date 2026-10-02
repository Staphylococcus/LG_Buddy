Feature: Native monitor after legacy backend removal
  Saved swayidle selections convert locally at user daemon startup. Native
  monitoring never launches an external idle process, even if one is installed.

  Scenario: automatic monitoring never delegates to installed swayidle
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And swayidle is installed
    And GNOME monitor stays open for 0.2 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "no native activity source available"
    And swayidle was not invoked
    And the TV screen is visible

  Scenario: a legacy swayidle backend override is converted before runtime work
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And the existing config sets screen backend "swayidle"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And swayidle is installed
    And GNOME monitor stays open for 0.2 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "no native activity source available"
    And swayidle was not invoked
    And the TV screen is visible
    And config.env contains "screen_backend=auto"
