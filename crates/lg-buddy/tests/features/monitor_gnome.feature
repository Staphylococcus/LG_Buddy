Feature: GNOME monitor
  LG Buddy should translate GNOME session signals and idle-monitor activity into TV
  behavior, against a clean native lg_webos config (the 1.x bscpylgtv fixture is
  rejected by the migration gate since #256).

  Scenario: disabled idle blanking keeps the session agent passive
    Given a temporary LG Buddy config using input HDMI_2
    And screen idle blanking is "disabled"
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And gamepad activity is observed after 0 seconds
    And mock system logind reports LockedHint=true
    And GNOME monitor stays open for 0.1 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "screen idle blanking is disabled by config"
    And the TV screen is visible

  Scenario: unavailable idle backends do not suppress the session notification service
    Given a temporary LG Buddy config using input HDMI_2
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME monitor stays open for 0.1 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "session notification service unavailable"
    And stdout contains "screen idle backend unavailable"

  Scenario: Locking the graphical session blanks the TV immediately
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 120 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And mock system logind reports LockedHint=false
    And mock system logind changes LockedHint to true
    And GNOME monitor stays open for 0.6 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Session locked; requesting screen blank."
    And the session marker exists
    And the TV screen is blanked

  Scenario: Unlocking the graphical session does not restore the TV
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 120 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And mock system logind reports LockedHint=true
    And mock system logind changes LockedHint to false
    And GNOME monitor stays open for 0.6 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Session unlocked; no screen restore requested."
    And the session marker exists
    And the TV screen is blanked

  Scenario: A lock-owned blank uses the same timed power-off policy
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 120 seconds
    And the timed power-off grace is 0.2 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And mock system logind reports LockedHint=false
    And mock system logind changes LockedHint to true
    And GNOME monitor stays open for 0.8 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Session locked; requesting screen blank."
    And stdout contains "Timed power-off deadline reached"
    And the session marker exists
    And the TV is powered off

  Scenario: GNOME ScreenSaver idle does not bypass the LG Buddy timeout
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 2 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME reports the session idle
    And GNOME idle monitor will report idletimes "1000"
    And GNOME monitor stays open for 0.6 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "activity source=gnome"
    And stdout does not contain "Session became idle."
    And the session marker is absent
    And the TV screen is visible

  Scenario: Desktop activity resets the LG Buddy timeout
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And genuine desktop input occurs after 0.5 seconds
    And GNOME monitor stays open for 1.2 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout does not contain "Session became idle."
    And the session marker is absent
    And the TV screen is visible

  Scenario: Monitor restart restores an owned blank screen on desktop activity
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 120 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    When I run the command "screen off"
    Then the command succeeds
    Given genuine desktop input occurs after 0.05 seconds
    And GNOME monitor stays open for 0.4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Session event `user-activity` requests screen restore."
    And the session marker is absent
    And the TV screen is visible

  Scenario: LG Buddy blanks after its timeout without activity reports
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And GNOME monitor stays open for 1.2 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "activity source=gnome"
    And the session marker exists
    And the TV screen is blanked

  Scenario: LG Buddy powers off an owned blank screen after the fixed grace period
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And the timed power-off grace is 0.2 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And GNOME monitor stays open for 1.5 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Timed power-off deadline reached"
    And the session marker exists
    And the TV is powered off

  Scenario: Activity cancels the pending power-off before restoring the screen
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And the timed power-off grace is 0.5 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000, 1000, 1000, 1000"
    And gamepad activity is observed after 1.2 seconds
    And GNOME monitor stays open for 1.6 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Activity canceled the pending timed power-off"
    And the session marker is absent
    And the TV screen is visible

  Scenario: Restarting with an ownership marker starts a fresh grace period
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 120 seconds
    And the timed power-off grace is 0.5 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    When I run the command "screen off"
    Then the command succeeds
    Given GNOME monitor stays open for 0.2 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the session marker exists
    And the TV screen is blanked

  Scenario: LG Buddy does not blank repeatedly without intervening activity
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000, 1500, 2000, 2500"
    And GNOME monitor stays open for 1.2 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the session marker exists
    And the TV screen is blanked

  Scenario: Gamepad activity restores a blanked TV while GNOME idletime remains stale
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000, 1000, 1000, 1000"
    And gamepad activity is observed after 1.2 seconds
    And GNOME monitor stays open for 1.6 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Session event `user-activity` requests screen restore."
    And the session marker is absent
    And the TV screen is visible

  Scenario: GNOME lock-screen activity inside the grace period stays blanked
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 120 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And genuine desktop input occurs after 0.5 seconds
    And mock system logind reports LockedHint=true
    And GNOME monitor stays open for 0.8 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout does not contain "Session event `user-activity` requests screen restore."
    And the session marker exists
    And the TV screen is blanked

  Scenario: GNOME inactivity skips TV blanking on a different input
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And GNOME idle monitor will report idletimes "1000"
    And GNOME monitor stays open for 1.2 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Skipping idle action."
    And the session marker is absent
    And the TV screen is visible

  Scenario: GNOME active restores a previously blanked TV output
    Given a temporary LG Buddy config using input HDMI_3
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    When I run the command "screen off"
    Then the command succeeds
    Given GNOME reports the session active
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "requests screen restore"
    And the session marker is absent
    And the TV screen is visible

  Scenario: GNOME wake request restores a previously blanked TV output
    Given a temporary LG Buddy config using input HDMI_3
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    When I run the command "screen off"
    Then the command succeeds
    Given GNOME requests screen wake
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "wake-requested"
    And the session marker is absent
    And the TV screen is visible

  Scenario: GNOME wake request can restore without a session marker in aggressive mode
    Given a temporary LG Buddy config using input HDMI_3
    And the screen restore policy is "aggressive"
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And reboot detection reports no pending reboot
    And the executable PATH is isolated
    And GNOME Shell is available
    When I run the command "power off"
    Then the command succeeds
    Given GNOME requests screen wake
    And screen wake delays are disabled
    And the next input restore attempt powers the TV back on
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Aggressive restore policy is enabled"
    And stdout contains "Screen unblank succeeded."
    And the session marker is absent
    And the TV is powered on
    And the TV screen is visible

  Scenario: GNOME activity wakes a TV that was manually powered off after LG Buddy blanked it
    Given a temporary LG Buddy config using input HDMI_3
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And reboot detection reports no pending reboot
    And the executable PATH is isolated
    And GNOME Shell is available
    When I run the command "screen off"
    Then the command succeeds
    When I run the command "power off"
    Then the command succeeds
    Given GNOME reports the session active
    And screen wake delays are disabled
    And the next input restore attempt powers the TV back on
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Screen unblank succeeded."
    And the session marker is absent
    And the TV is powered on
    And the TV screen is visible

  # Allow watch registration and signal delivery, then stop before a second idle deadline.
  Scenario: GNOME early user activity restores a blanked TV before the session becomes active again
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 2 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    And genuine desktop input occurs after 2.5 seconds
    And GNOME monitor stays open for 4 seconds
    When I run the command "monitor"
    Then the command succeeds
    And the session marker is absent
    And the TV screen is visible

  # Native port of the legacy screen-visibility verification: the monitor's
  # session-active restore hits an interrupting first restore session that
  # acks the input write without unblanking, so the product falls back to the
  # full-wake retry path and only clears the marker once visibility is proven.
  # product outcome (screen visible, marker cleared), the recovery trace, and
  # a no-extra-requests bound: exactly two turnOnScreen and two switchInput
  # requests, the native stand-in for the legacy `turn_screen_on` x2 /
  # `set_input` x2 call-count asserts.
  Scenario: GNOME activity verifies legacy screen visibility after an input acknowledgement
    Given a temporary LG Buddy config using input HDMI_3
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    When I run the command "screen off"
    Then the command succeeds
    And the session marker exists
    And the TV screen is blanked
    Given the native webOS TV interrupts the first restore session and acknowledges input without unblanking
    And screen wake delays are disabled
    And GNOME reports the session active
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "Session event `active` requests screen restore."
    And stdout contains "Screen visibility could not be verified. Falling back to full wake."
    And stdout contains "operations: direct_unblank=failed kind=screen_not_visible"
    And stdout contains "input_attempt_1=failed kind=screen_not_visible"
    And stdout contains "recovery_unblank_1=succeeded"
    And stdout contains "input_retry_1=succeeded"
    And the session marker is absent
    And the TV screen is visible
    And the native webOS TV received exactly 2 requests to "ssap://com.webos.service.tvpower/power/turnOnScreen"
    And the native webOS TV received exactly 2 requests to "ssap://tv/switchInput"

  # Native port of the legacy "restore failure does not retry continuously"
  # scenario. The TV is left blanked by `screen off` (ownership marker
  # preserved), then the TV powers off and becomes unreachable: every
  # connection attempt of the six bounded wake attempts is refused. The
  # monitor performs a single bounded restore, logs the bounded-exhaustion
  # line, and stops: activity stays active after exhaustion, but it only
  # retries on the next restore event, never spinning in a retry loop.
  # The exhaustion line appearing exactly once proves the single bounded
  # cycle: a second restore cycle would emit it again. The "after 6
  # attempts" text alone is a constant, not an observed count, so the
  # bounded-attempt cap is proven by the connection count (15: 1 setup +
  # 2 initial unblank+verify + 12 from six input attempts+verify, no
  # continuous retry), which stays flat when the monitor stays open.
  Scenario: GNOME restore failure does not retry continuously while activity stays active
    Given a temporary LG Buddy config using input HDMI_3
    And the idle timeout is 1 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_3 with brightness 100
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME emits no ScreenSaver signals
    When I run the command "screen off"
    Then the command succeeds
    And the session marker exists
    And the TV screen is blanked
    Given the native webOS TV powers off
    And screen wake delays are disabled
    And genuine desktop input occurs after 1.5 seconds
    And GNOME monitor stays open for 1.8 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "screen restore action failed. screen-on wake sequence failed after 6 attempts" exactly 1 times
    And the native TV connection count is 15
    And the session marker exists
    And the TV is powered off

  Scenario: Native activity works without an inhibition service
    Given a temporary LG Buddy config using input HDMI_2
    And the idle timeout is 1 seconds
    And the existing config selects TV platform "lg_webos"
    And LG Buddy session runtime is isolated
    And a native webOS TV on input HDMI_2 with brightness 90
    And a valid native TV access token is stored
    And the executable PATH is isolated
    And GNOME Shell is available
    And GNOME SessionManager is unavailable
    And honoring app keep-awake requests is "enabled"
    And GNOME emits no ScreenSaver signals
    When I run the command "screen off"
    Then the command succeeds
    Given genuine desktop input occurs after 0.2 seconds
    And GNOME monitor stays open for 0.8 seconds
    When I run the command "monitor"
    Then the command succeeds
    And stdout contains "activity source=gnome"
    And the TV screen is visible
    And the session marker is absent
