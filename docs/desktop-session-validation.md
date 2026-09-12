# GNOME/Plasma session validation

Recorded on 2026-09-12 for [#217](https://github.com/Staphylococcus/LG_Buddy/issues/217).
This is a development validation record, not approval to promote the #89 MVP.

## Environment and provenance

The dedicated Incus VM `lg-buddy-desktop-217` runs Fedora 44 with both desktops
installed, GDM, six virtual CPUs, 8 GiB RAM and a 32 GiB disk. Tests use one local
graphical account (`lgtest`, UID 1001), lingering and a concurrent SSH session.
SELinux stays enforcing. The VM tests left other machines unchanged; the separate
Hearth hardware pass below temporarily substituted the runtime in its two services.

| Component | Version |
| --- | --- |
| GNOME Shell / Mutter | 50.4 |
| gnome-session / GDM | 50.1 / 50.3 |
| Plasma / KWin / PowerDevil | 6.7.5 |
| systemd | 259.8 |
| Kernel | 7.1.12 |
| mpv | 0.41.0 |

The initial installation used the runtime, GUI and TV fixture from
[CI run 34664139930](https://github.com/Staphylococcus/LG_Buddy/actions/runs/34664139930).
Its build commit `fb9574c9fd614cf0b6a2ba45e64a47ad99792b7f` and merged `dev`
commit `f777a5fe351dc96cf74443bfd09197e9eff000bd` have the same tree,
`edf1246679f9ade9a02f3fbf6f8d59a6f5d4d312`. The CI version string is
`1.4.0-beta.2.ci`; it is not a published release.

SHA-256 identifiers:

```text
CI bundle: e2b4c5c8a91dfdf49a7cf4a09cf9d1ee94bb4544071adcaa4be5ff0a7ddba82f
CI runtime: a205afd1e01c5ffda70974db7e579ad3a2bfc5ac73831ab49bc3ddb5a51c7404
TV fixture: f8327bd304d86d4a362c8e40f709e24ca360ffcc35a69c586ad3b2407c8f9aea
Patched runtime: 7b516c1816ebd3bfd6b523452341138b9b44458e54cd71101d23a3949d9e8904
runner.rs patch: 0d93542d9ff484af0e7b5055c5fa9da25befe9a0c0da02e52b4923cd4a74b9e6
```

The patched runtime is a local GNU debug build from that merged commit plus the
`runner.rs` startup fix. A copy was relocated to Fedora's dynamic loader and
`/usr/lib64` with `patchelf`; its reported identity remains `dev`, commit
`unknown`. This verifies runtime behavior, not a new release bundle. Hashes were
checked again inside the guest. Original CI binaries were retained for comparison.

## Test setup and observations

The normal installer installed the original bundle and system/user units.
Configuration used `screen_backend=auto`, idle blanking and inhibitor honoring
enabled, a 10-second idle timeout, conservative restore and system sleep/wake
enabled. Updates were disabled. The configuration SHA-256 before and after each
round trip was:

```text
ecc582d5a7b52dd56952ef0ef41682e4a5f0f7d1d9f172c3399c1e580fc52b6e
```

TV actions target the repository's `gui_journey_tv` TLS fixture at
`127.0.0.1:3001`, configured on HDMI_3 with a test MAC address. Assertions read
its atomic `state.json`; no physical TV is involved. Virtual keyboard and
gamepad devices enter through Linux `uinput` and the production input adapters.

Desktop changes use normal GNOME/Plasma logout and GDM authentication/session
selection. No LG Buddy configuration edits or manual service restarts occur
inside a round trip. Snapshots collect `loginctl`, the user manager's PID and
invocation ID, `graphical-session.target`, the screen service PID/environment,
ownership markers, TV state, binary/configuration hashes and the service journal.

The original CI runtime completed GNOME -> Plasma -> GNOME. The user manager
remained PID 891, invocation `eaa64dc0c414414b8de5a7cd4792b7ff`, with SSH session
10 present. Screen monitor PIDs were 3468 -> 6786 -> 9803. Each logout stopped
the old monitor; each graphical login started a new one with the appropriate
environment. Input after login restored an existing blanked-screen marker.

For each desktop leg, the behavioral check:

1. Restores with input and starts two independent inhibitor clients.
2. Verifies the screen remains on for 12 seconds, releases one client, then
   verifies another 12 seconds with the second client still present.
3. Releases the last client, verifies at least 9.5 seconds without blanking,
   then observes blanking after the configured full idle interval.
4. Restores with gamepad input, supplies another gamepad event six seconds
   later and verifies that event resets the deadline; ordinary input restores
the subsequent blank.

All three original legs passed. GNOME uses SessionManager `Inhibit` with idle
flag 8. Plasma uses `org.freedesktop.ScreenSaver.Inhibit`; PowerDevil reports
effective screen inhibition through `HasInhibition(4)`. Observed final-release
delays were about 10.2 seconds on GNOME and 11.2 seconds on Plasma, including
the pull cadence.

After installing the startup fix, a fresh boot completed the same round trip
with user manager PID 901, invocation `fc33d35e1716456297c6b59e90442c1e`, and
SSH session 5 retained. Monitor PIDs were 1404 -> 4879 -> 7356. All three
instances composed native sources; the expected unavailable counterpart did
not disable the working adapter. Runtime and configuration hashes stayed
unchanged. The overlap/gamepad checks passed on patched GNOME, Plasma and
return GNOME. The first patched GNOME behavioral check preceded the fresh
boot; the first GNOME leg in that fresh loop verified startup, idle blanking
and marker recovery at the subsequent Plasma login.

On both Plasma and the final GNOME login, locking blanked the screen promptly;
unlocking alone kept it blank for the two-second observation, and fresh input
restored it. An earlier post-suspend GNOME lock request did not produce the
expected blank; it is not counted as passing. The follow-up below distinguishes an invalid lock-test precondition from a
later compositor stall.

Additional real Plasma route checks found:

- Portal idle flag 8 and ScreenSaver inhibitors both kept the TV fixture on
  beyond the idle timeout, with `HasInhibition(4)` true.
- A logind `idle` block inhibitor appeared in PowerDevil's effective policy.
- `SetInhibitionAllowed(app, reason, false)` made the effective query false
  while the request remained alive; setting it true restored inhibition.
- Restarting `plasma-powerdevil.service`, then creating a new ScreenSaver
  inhibitor, again prevented blanking. Monitor PID 4879 remained unchanged.
  The test waits for the replacement bus owner before asserting its state.

Disabling `screen.idle_blank` on GNOME leaves the passive session service alive.
`ShowUpdateNotification` returned `sent`, with its forwarded `Notify` call
observed on the real desktop bus. The installed GUI process remained active
and the CLI read fixture volume `20`. The screen stayed on beyond the timeout.
The preference was restored and the original configuration hash rechecked.
This is process/API verification, not visual GUI acceptance.

## Startup fix

On the original return to GNOME, automatic monitoring logged `Using GNOME
backend` instead of composing native sources. The first native probes had found
no ready interface; the legacy fallback probed again and found GNOME ready.
That result selected only GNOME for the lifetime of this monitor.

Automatic resolution now converts a native result from that fallback probe to
the composed source set. Explicit backend selection and automatic `swayidle`
compatibility retain their existing behavior. Regression tests reproduce late
GNOME and late Wayland availability between the two probes.

Local validation passed: 1,019 runtime unit tests, 151 Cucumber scenarios,
runtime integration tests, formatting and workspace Clippy with warnings denied.
One hardware gamepad smoke test remains ignored; VM gamepad checks use `uinput`.

## Confirmed native Wayland coverage gap

On Plasma, mpv was started with `--no-config --no-audio --loop-file=inf
--vo=wlshm` and `WAYLAND_DEBUG=1`, playing a generated clip. Its protocol trace
showed `zwp_idle_inhibit_manager_v1.create_inhibitor` for the player surface.
PowerDevil still returned `HasInhibition(4) = false` and empty active
inhibitions. LG Buddy restored on input at 04:16:52 UTC and blanked at 04:17:02
while playback continued.

This is live evidence of a native Wayland inhibition route that the current
PowerDevil adapter does not cover. The subsequent scope decision keeps PowerDevil
as the Plasma inhibition boundary for the 1.8.0 MVP; native-only KWin coverage
does not block #217 or the MVP prerelease. Success through ScreenSaver or portal
routes must not be presented as native Wayland coverage. Idle-notify remains an
activity interface, not an inhibition-state getter.

## VM limitations and remaining acceptance

- Host CPU passthrough caused widespread guest process crashes, including init.
  Selecting the QEMU `EPYC` CPU model allowed the desktop tests to run. The
  virtualization failure's root cause was not established.
- SELinux denies execution of the Incus agent from its `nfs_t`-labelled runtime
  file. SSH was used instead; no permissive mode or broad policy exception was
  introduced.
- GNOME entered and resumed from real guest s2idle. The monitor survived with
  its PID unchanged. RTC wake did not resume this VM; a virtual power button
  did. The TV fixture serves one persistent connection at a time, so the
  monitor occupied the connection while the separate lifecycle service tried
  to act. Complete TV sleep/wake behavior is therefore **not validated** here.
- A later logout in that boot stalled the guest's login machinery during GNOME's
  user-bus restart. LG Buddy had already stopped cleanly. New SSH and console
  logins waited too; the guest required a reboot. This interrupted run is not
  counted as a successful patched round trip or assigned a product root cause.
- Physical TV/Wake-on-LAN behavior, the reporter's application, simultaneous
  graphical sessions and additional desktops are outside this record.

Keep #217 open until the joint lifecycle checks have reliable evidence. Keep
#216/#89 coverage gates explicit; this run does not authorize prerelease or main
promotion.


## Gap investigation follow-up

Later on 2026-09-12, the same VM separated the remaining gaps as follows.

### Native Wayland: no exposed inhibition observation

KWin tag `v6.7.5`, commit `ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7`,
maintains effective native inhibitors in `InputRedirection::m_idleInhibitors`.
Its visibility checks and source updates live in
[`idle_inhibition.cpp`](https://github.com/KDE/kwin/blob/ab7df7ccb7c6af20f4b279cd6220f7cd3d2267d7/src/idle_inhibition.cpp),
which still contains the comment `TODO: notify powerdevil?`.

The inspected D-Bus XML, implementation and scripting wrappers expose neither
that aggregate nor the underlying surface inhibition state. The Wayland
idle-inhibit protocol lets a client create/destroy its own inhibitors; it has
no facility to observe other clients' inhibitors. A supported KWin state
interface, or an upstream bridge to PowerDevil, would let LG Buddy consume the
fact through the existing inhibition boundary. This is an interface gap, not
missing Boolean reconciliation. No production workaround was added.

### Sleep service: isolated path passes

The screen monitor was stopped temporarily to free the fixture's single
connection. A test-only UDP observer accepted the exact 102-byte magic packet
for `02:00:00:00:02:17` and reset the existing fixture to simulate TV wake.
It verified that the fixture was powered off before accepting the packet.

At 14:47:32 UTC, the installed lifecycle service powered off the fixture before
real guest s2idle. At 14:47:56 it resumed and sent that packet; at 14:48:02 it
reported successful HDMI_3 restoration and reacquired its sleep inhibitor.
The monitor was then started again. This verifies the system service's path
with simulated hardware; it does not verify simultaneous monitor/lifecycle
connections or physical WoL. The existing fixture needs concurrent connections
and an explicit simulated-wake operation for the joint test.

### Lock: test precondition and compositor stall are separate

A controlled test began with `LockedHint=yes`: input restored the TV while the
session remained locked, and another `lock-session` request caused no new lock
transition or blank. Explicitly unlocking first, then locking, blanked promptly;
unlock alone left the TV blank, and fresh input restored it. The original lock
test omitted that precondition, so its failure alone did not prove recovery
was broken.

In a separate suspend run, the lifecycle service was stopped temporarily to
isolate the live screen monitor. Monitor PID 10393 and invocation
`be91e0a541974210b53c8910d2236423` survived s2idle without restarting. Its journal
observed the sleep lock, deferred TV actions to lifecycle while sleep was
pending, and handled input after resume. The lifecycle service was restored.
A subsequent genuine lock transition reached LG Buddy and blanked the TV.

Later unlock attempts stopped changing `LockedHint`; GNOME's `GetActive`
request timed out. A debugger snapshot located the wait outside LG Buddy:

```text
gnome-shell main thread:
  g_cond_wait -> run_impl_task_sync_kernel
  -> meta_monitor_manager_native_set_power_save_mode

Mutter KMS thread:
  ioctl -> drmModeAtomicCommit
  -> meta_kms_impl_device_atomic_disable
```

GNOME's cgroup was not frozen. The guest uses virtio GPU, and LG Buddy remained
running with `NRestarts=0`. This identifies the compositor/display wait behind
the observed unresponsiveness; it does not establish the underlying graphics
bug or prove that the earlier logout stall had the same cause. Further joint
validation needs a working VM graphics path as well as the fixture improvements.
Both LG Buddy services and the original configuration were restored after the
isolated tests. After preserving the debugger evidence, the VM was rebooted:
both services were active, GNOME answered `GetActive` again, and the configuration
hash was unchanged. No production changes were made during this follow-up.

## Hearth hardware observations

On 2026-09-12, the physical Hearth host ran the candidate from
[`c50f3ea`](https://github.com/Staphylococcus/LG_Buddy/commit/c50f3eab67ef9214e3173284189dcb5cd0979ad7)
([PR #231](https://github.com/Staphylococcus/LG_Buddy/pull/231)) in both the
screen and lifecycle services. This was a local debug build, reporting version
1.7.0, channel `dev`, and that exact commit; it was not a release installation.
Its SHA-256 was
`5f4019fc329bc559bd5c6eb8d4fe0cd3b7f256a568ac48f8191731686fbf8210`.

Hearth ran NixOS 26.05, kernel 7.2.3 and GNOME Shell 50.4 on an X870 AORUS
ELITE WIFI7 board. The physical display identified as `LG TV SSCR2` over HDMI;
LG Buddy controlled HDMI_3 through native webOS. One local Wayland session and
concurrent non-graphical sessions remained present. Automatic composition used
GNOME activity; the compositor did not advertise the required idle-notify v2
interface. Configuration stayed unchanged: automatic backend, 120-second idle
timeout, aggressive restore, inhibitor honoring disabled, sleep/wake enabled.
This pass therefore does not validate inhibition behavior.

Both candidate services remained running throughout the checks: monitor PID
1716243 and lifecycle PID 1716211, each with zero restarts. Times below are UTC.

| Check | Runtime evidence | Human observation |
| --- | --- | --- |
| Lock before suspend | Lock at 16:24:09; successful TV blank at 16:24:10; timed CLI unblank succeeded at 16:24:24. | The TV showed no signal after the timed recovery. Moving the mouse returned the login screen; normal unlock worked. |
| Joint suspend/resume | Lifecycle completed TV power-off at 16:27:45 before deep/S3 suspend. Resume reached lifecycle at 16:28:19; network became ready and WoL began at 16:28:25. Two restore attempts failed before HDMI_3 restoration succeeded at 16:28:39. | The user confirmed the picture was back and unlocked normally. |
| Lock after suspend | A fresh lock at 16:30:20 blanked successfully. Timed CLI unblank succeeded at 16:30:35. Later input canceled the pending timed power-off and another unblank succeeded. | Mouse or keyboard input was needed before normal unlock. No compositor stall was observed in this cycle. |

During suspend, the screen monitor received the lock event but deferred its TV
action because sleep was pending. Activity immediately after resume likewise
deferred to the lifecycle restore. Lifecycle reacquired its logind sleep delay
inhibitor after restoration, and both ownership markers were absent afterward.
This supplies real-TV evidence for the two services operating together on GNOME.

The first timed recovery woke only the TV: GNOME's `PowerSaveMode` was 3 and the
connected HDMI connector was disabled. A diagnostic write enabled it, but the
user also moved the mouse, so visual recovery cannot be attributed to that write
alone. The final helper included an HDMI-output wake request, yet the user still
reported needing input. Successful TV unblank is not evidence of a visible
desktop while the compositor's output is asleep.

An RTC alarm was armed for the suspend test. The user confirmed return but did
not separately establish whether manual input helped resume the PC. The delayed
emergency TV-wake helper launched after lifecycle had already restored HDMI_3;
it failed before executing LG Buddy because its transient-unit PATH could not
resolve `env`. It contributed no TV action to the successful recovery. These are
test-helper details, not demonstrated LG Buddy failures.

The original installed runtime was restored in both services at 16:31:54–55.
Both services were active, the session was unlocked, a TV read succeeded, and all
test timers and service overrides were removed. Configuration SHA-256 before
and after was
`b1e62287d6cd110f726f595b84255a18940469113c87f1708181fb23e2b2273a`.

Evidence is retained locally under
`~/.local/state/lg-buddy-validation/217-2026-09-12/hearth/`, including service and
kernel journals, exact helper scripts, binary identity and user observations.
This single GNOME hardware pass does not complete #217's GNOME/Plasma matrix.
The next fixture work should support concurrent clients and controlled power-off,
wake and readiness transitions while keeping TV screen state separate from the
desktop's HDMI signal. The fixture was not changed during this pass.

## Concurrent fixture follow-up

The subsequent fixture extension kept the existing central webOS server and
added concurrent clients, power-off invalidation of old connections, and an
explicit wake that preserves settings and history. The process fixture can
receive exact WoL packets for its test MAC and delay registration readiness.
These transport/readiness controls are fault injection, not claims about exact
firmware timing. `screen_on` still describes the TV, independently of the
desktop's HDMI signal. See [Native webOS testing](webos-testing.md) for controls.

The dedicated VM used an eight-second readiness delay and the existing patched
runtime (`7b516c18...` above). The fixture build used for the following two
suspend checks had SHA-256
`0cebd07e0ece0108597a41de67a8c42c98897081d531a52a1a2645ac2b0ee441`.
The local debug executable was relocated to Fedora's loader and library path.
Configuration was unchanged, including inhibitor honoring. A temporary desktop
inhibitor kept automatic idle blanking from interrupting the observation window;
it did not inhibit explicit lock or system sleep.

| Desktop | Power-off before guest s2idle | Resume | HDMI_3 restored | Monitor / lifecycle PIDs |
| --- | --- | --- | --- | --- |
| Plasma | 16:55:35 UTC | 16:55:50 | 16:55:58 | 5004 / 866 |
| GNOME | 16:59:45 UTC | 17:00:20 | 17:00:28 | 1383 / 856 |

Both services remained running with unchanged invocation IDs and zero restarts
in each cycle. The monitor already held a TV connection before sleep. Lifecycle
then completed power-off, received resume, sent WoL and retried once during the
injected delay before restoring HDMI_3 and reacquiring its sleep inhibitor.
The monitor deferred the sleep lock and early post-resume activity to lifecycle.
Each fixture snapshot recorded exactly one power-off and one effective wake,
with no pairing prompts. The final GNOME snapshot had two simultaneous clients.
Virtual power-button requests resumed the guest; this is not RTC-wake validation.

GNOME lock/unlock checks passed before and after suspend: lock blanked, unlock
alone left the fixture blank for two seconds, and input restored it. Plasma's
pre-suspend lock check passed, but its post-resume unlock request did not clear
`LockedHint`; the ScreenSaver query and later logout also failed to complete.
That desktop recovery check remains unresolved despite successful TV recovery.

An earlier GNOME-to-Plasma login after a successful GNOME suspend cycle stalled
before `graphical-session.target`, with Plasma initialization still pending and
LG Buddy waiting to start. A fresh boot allowed Plasma to start. Diagnostics and
console logs were preserved; these interrupted desktop transitions are not
counted as passing or assigned a new product root cause.

The VM was returned to GNOME. Its services were active, the session was unlocked,
GNOME answered `GetActive=false`, and the configuration hash remained unchanged.
The updated fixture is retained in the dedicated VM; temporary inhibitor clients
were removed. The host's physical TV and installed LG Buddy services were not
changed during this fixture work.

Evidence is in
`~/.local/state/lg-buddy-validation/217-2026-09-12/concurrent-fixture/`.
An earlier fixture build also passed the GNOME joint cycle, but the table uses
the later build shared with the Plasma check. Automated coverage includes
shared-client characterization, connection shutdown and worker-failure tests,
controlled power/wake tests, and a real-CLI process smoke wired into CI. Keep
#217 open for the remaining desktop recovery validation.

After those cycles, an explicit `ready` command was added so automated tests can
hold startup pending until their assertions finish. The real-CLI smoke covers
that control; the final full suite passed with 1,021 unit tests, 151 Cucumber
scenarios / 1,885 steps, integration tests, formatting and workspace Clippy.
One hardware gamepad test is ignored. The retained VM fixture was updated to
SHA-256 `a2834f9b6119ff8282b81eb8101edc3c26a404b5b9949da903006b871ee103f2`
without restarting either LG Buddy service. Input after the fixture restart
could not establish recovery because GNOME's ScreenSaver endpoint had again
become unresponsive while idle, with `LockedHint=yes`; both LG Buddy services
were still active. That additional VM failure was recorded and a fresh GNOME
boot used for cleanup. This final control was process-tested; the suspend-cycle
table identifies the earlier automatic-delay build actually used for those cycles.
