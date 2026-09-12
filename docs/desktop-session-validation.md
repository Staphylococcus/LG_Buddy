# GNOME/Plasma session validation

Recorded on 2026-09-12 for [#217](https://github.com/Staphylococcus/LG_Buddy/issues/217).
The complete issue is delivered together in
[PR #231](https://github.com/Staphylococcus/LG_Buddy/pull/231): the automatic
startup fix, concurrent TV fixture, automated checks and this validation record.
The earlier fixture and VM recovery gaps have been resolved. This validates
#217's lifecycle scope; release promotion remains a separate step.

## Acceptance and evidence

| #217 requirement | Evidence |
| --- | --- |
| GNOME → Plasma → GNOME with unchanged configuration and no manual LG Buddy restart | Completed with the combined candidate. Graphical logout stopped the old monitor; each login automatically started the next. See the round-trip table below. |
| Concurrent non-graphical login and retained user manager | User manager PID 865 and invocation `7dcdcf6564ff4bbfbb1d5878d3ba49af` survived both switches. SSH session 177 remained present through both transitions; lingering was enabled. |
| Old connections, observations and replies cannot affect the next login | Both old monitor processes exited before the next started. An existing screen ownership marker survived each logout and was restored on new input. Runtime and adapter tests additionally reject cancelled queries, previous owners and obsolete protocol objects. |
| Late/restarted interfaces recover independently; failure, quiet and cancellation semantics are preserved | Live PowerDevil replacement and the private-bus/runtime tests below cover recovery, independent activity/inhibition, quiet observations, failed reads, cancellation and stale replies. The startup race found in this validation has regression coverage. |
| Idle/restore, gamepad, lock/unlock and suspend/resume on each leg | All three legs passed with both monitor and lifecycle services running. Every suspend powered the fixture off, resumed the guest, sent WoL and restored HDMI_3 through a controlled readiness delay. Fresh lock/unlock passed afterward. |
| Disabled monitoring preserves notifications and GUI/CLI | Separate passive-mode checks passed on GNOME and Plasma: the session endpoint forwarded a real desktop `Notify`, the installed GUI stayed running, CLI volume returned `20`, and the TV stayed on beyond the idle timeout. Configuration was restored afterward. |

The supported boundary is one local graphical session per account alongside
SSH/console logins. Existing logind eligibility checks reject remote,
non-graphical, inactive and other-user sessions; ambiguous graphical matches
produce a diagnostic. Their tests are in
[`sources/linux/logind.rs`](../crates/lg-buddy/src/sources/linux/logind.rs).
No desktop-wide selector or new session-binding layer was introduced.

## Environment and candidate identity

The dedicated Incus VM `lg-buddy-desktop-217` runs Fedora 44, GDM, six virtual
CPUs, 8 GiB RAM and a 32 GiB disk. The graphical account is `lgtest` (UID 1001).
SELinux remained enforcing. Only this VM was changed during the final matrix;
the earlier physical Hearth pass is recorded separately below.

| Component | Version / configuration |
| --- | --- |
| GNOME Shell / Mutter | 50.4 |
| gnome-session / GDM | 50.1 / 50.3 |
| Plasma / KWin / PowerDevil | 6.7.5 |
| systemd / kernel | 259.8 / 7.1.12 |
| QEMU CPU / display | EPYC / QXL |
| LG Buddy | Local GNU debug build from `5e3522986672e177233c18102605830376c1c863`; reports 1.7.0, channel `dev`, commit `5e35229` |

The initial installation used the normal installer and
[CI bundle 34664139930](https://github.com/Staphylococcus/LG_Buddy/actions/runs/34664139930).
The final runtime and fixture were built from the combined branch and installed
in the existing unit paths. Copies were relocated to Fedora's dynamic loader
and `/usr/lib64` with `patchelf`, then hashes were verified inside the guest.
The installed GUI came from the original bundle; this change does not modify it.
These are runtime checks, not validation of a newly published release bundle.

```text
Source commit: 5e3522986672e177233c18102605830376c1c863
VM runtime:    c7fcc08a6925975ead73dccce7c28916e0368a9ff750c097121042f14e955b35
VM fixture:    86430cf2d8cbf7ba7edc3c813931460a28a8b377610990e3d99ad8a3d4e5c68e
Configuration: ecc582d5a7b52dd56952ef0ef41682e4a5f0f7d1d9f172c3399c1e580fc52b6e
```

Configuration used `screen_backend=auto`, idle blanking and inhibitor honoring
enabled, a 10-second idle timeout, conservative restore, system sleep/wake
enabled and automatic updates disabled. TV actions targeted the repository's
TLS fixture at `127.0.0.1:3001`, HDMI_3, with test MAC `02:00:00:00:02:17`.
No physical TV was targeted by the VM.

## Complete round trip

No configuration edits, desktop selection in LG Buddy, binary replacements or
manual LG Buddy restarts occurred between the first GNOME leg and the final
GNOME leg. Normal desktop logout and GDM authentication selected each desktop.
The same system lifecycle process, PID 2315 with invocation
`ed8f3665210c48e082c4cce10cb2b69c`, remained running throughout.

Times below come from the guest journal and are UTC.

| Leg | Screen monitor PID | TV power-off before s2idle | Resume | HDMI_3 restored |
| --- | --- | --- | --- | --- |
| First GNOME | 2319 | 18:57:46 | 18:57:54 | 18:58:02 |
| Plasma | 11680 | 19:05:03 | 19:05:11 | 19:05:19 |
| Return GNOME | 22011 | 19:08:23 | 19:08:31 | 19:08:40 |

Every leg exercised:

1. Input restore followed by two independent inhibitors. The TV remained on
   for 12 seconds with both clients, then another 12 seconds after one exited.
2. Release of the final inhibitor, at least 9.5 seconds without blanking, then
   blanking after the full configured inactivity interval.
3. Gamepad restore and a second gamepad event six seconds later, verifying
   that fresh gamepad input reset the deadline. Ordinary input restored the
   subsequent blank. Input entered through Linux `uinput` and production adapters.
4. A fresh explicit lock, confirmed TV blank, unlock and input restore before
   suspend, then the same check after resume.
5. Real guest s2idle with both LG Buddy services active. A virtual power button
   resumed the VM. The lifecycle journal confirmed power-off before sleep,
   WoL after resume, a retry during the fixture's eight-second readiness delay,
   successful HDMI_3 restoration and reacquisition of its sleep delay inhibitor.

Each suspend incremented the fixture's power-off and effective-wake counters
exactly once, with no pairing prompt. Both services retained their PIDs and
invocation IDs, with zero restarts. The monitor deferred sleep-lock and early
post-resume TV actions to lifecycle. The fixture supported both persistent
clients together; old connections were invalidated by power-off.

Each logout began with an idle-blanked TV. The next desktop started with
`screen_off_by_us` still present and restored it on fresh input. No ownership
marker was discarded to make the test pass. The final input restore cleared
ownership state, and the configuration and binary hashes still matched.

The harness initially made invalid assumptions about unlock controls. Plasma
did not honor `loginctl unlock-session`, and temporary keyboard/password-entry
sequences were unreliable. Its successful checks used password authentication
through QEMU's existing keyboard. GNOME checks used its working logind unlock
control and fresh input; successful password unlock was also observed on the
first leg. The return-GNOME acceptance does not claim automated password-entry
coverage. Failed helper attempts were preserved in the logs, corrected in the
same running desktop, and did not require any LG Buddy restart.

Passive-mode checks on both desktops were performed separately from this
unchanged-configuration round trip. Disabling `screen.idle_blank` used the normal
settings apply path, which restarts the user service into its passive mode. `ShowUpdateNotification`
returned `sent`, its actual `org.freedesktop.Notifications.Notify` call was
captured, the installed GUI process remained active and CLI volume returned
`20`. The TV remained on beyond the timeout. Re-enabling monitoring restored the
original configuration hash. This is process/API verification, not visual GUI
acceptance.

## Adapter recovery and inhibition boundary

Existing automated coverage was rerun with the combined change:

- [`tests/inhibition.rs`](../crates/lg-buddy/tests/inhibition.rs) runs production
  capabilities against a private bus without activity services: late GNOME
  appearance, owner loss/recovery, overlapping inhibitors, quiet shutdown,
  current PowerDevil reads, cancellation, timeout and owner replacement during
  a delayed reply. Failed/absent sources contribute no inhibitor and do not
  manufacture a release.
- [`tests/runtime_entrypoints.rs`](../crates/lg-buddy/tests/runtime_entrypoints.rs)
  verifies that a running monitor recovers after Mutter disappears and returns,
  restores an existing ownership marker once, and discards a pre-suspend clear
  answer after resume when a new inhibitor is active.
- [GNOME inhibition tests](../crates/lg-buddy/src/sources/desktop/gnome/inhibition.rs)
  cover periodic reconciliation, retained quiet/pending observations, queued
  owner changes, failed reads and cancellation.
- [Wayland activity tests](../crates/lg-buddy/src/sources/desktop/wayland.rs)
  cover input during setup, cancellation of stalled/quiet initialization and
  rejection of obsolete notifications for a reused seat name.
- [Gate tests](../crates/lg-buddy/src/inhibition.rs) and the
  [inhibition scenarios](../crates/lg-buddy/tests/features/idle_inhibition.feature)
  cover combined capabilities, preference bypass, release timing, input
  cancellation and independent activity when inhibition support is missing.

Earlier live Plasma route checks passed for ScreenSaver inhibition, portal Idle
flag 8, imported logind `idle` blockers, and user suppression/re-enabling through
PowerDevil. Restarting PowerDevil and creating a new inhibitor recovered without
restarting the monitor. This restart check also passed on the final candidate,
with a changed bus owner, unchanged monitor invocation and inhibition holding
the TV on beyond its idle timeout. GNOME uses SessionManager `IsInhibited(8)`;
Plasma uses effective PowerDevil `HasInhibition(4)`.

Native-only mpv inhibition was separately reproduced with `--no-config
--no-audio --loop-file=inf --vo=wlshm` and a generated clip. Its trace created a
`zwp_idle_inhibitor_v1`, while PowerDevil reported no effective screen inhibitor
and LG Buddy blanked after ten seconds. KWin's native route is distinct; it
must not be inferred from portal or ScreenSaver success. **PowerDevil is the
agreed Plasma inhibition boundary for the MVP.** Native-only KWin coverage is
outside #217's gate. Idle-notify remains an activity interface; activity and
inhibition meet only at `can_blank()`.

## VM blockers diagnosed and removed

The earlier stalls were investigated with both LG Buddy services stopped:

- With virtio GPU, GNOME suspend/resume succeeded, but the following Plasma
  login stalled. Both the GNOME greeter and KWin were blocked in the kernel's
  `virtio_gpu_queue_ctrl_sgs` path while creating display resources. The screen
  monitor and lifecycle service both had PID 0. Switching this test VM to QXL
  allowed the same post-suspend desktop switch to complete.
- A subsequent baseline suspend failed to freeze one `mount` process belonging
  to `incus-agent.service`. It was blocked in a virtio 9p request while mounting
  the agent share. The unused agent already failed under SELinux; it was masked
  and the guest cold-booted to clear the blocked process. The complete matrix
  above then succeeded with both LG Buddy services running.

These reproduce concrete environment failures independently of LG Buddy; no
production workaround was added for them. They do not establish a root cause
for every earlier interrupted run. The test VM retains the working QXL display
and masked unused agent; SELinux stays enforcing and access uses SSH. QXL was
configured with Incus's documented
[QEMU device override](https://linuxcontainers.org/incus/docs/main/reference/instance_options/#override-qemu-configuration).
The earlier CPU-passthrough crashes were avoided with the EPYC virtual CPU model.

After validation, temporary timers, inhibitor clients and the QMP control
listener were removed. A fresh GNOME boot confirmed both LG Buddy services and
the fixture active, an unlocked session, working input restore, unchanged
candidate/configuration hashes and no remaining test timers.

## Physical Hearth observations

The earlier [hardware report](https://github.com/Staphylococcus/LG_Buddy/issues/217#issuecomment-5647209853)
tested runtime `c50f3eab67ef9214e3173284189dcb5cd0979ad7` in both services on
Hearth: NixOS 26.05, GNOME 50.4, kernel 7.2.3 and a real LG TV on HDMI_3.
That candidate has the same production startup fix as the final combined branch;
the subsequent changes extend test infrastructure and documentation.

- At 16:27:45 UTC lifecycle powered the TV off before deep/S3 suspend. Resume
  reached lifecycle at 16:28:19; network recovery and WoL began at 16:28:25.
  HDMI_3 restoration succeeded at 16:28:39 after two failed attempts. The user
  confirmed the picture returned and unlocked normally.
- Lock tests before and after suspend blanked successfully. Timed TV unblank
  commands succeeded, but mouse/keyboard input was needed to show the login
  screen. In the first test GNOME's HDMI output was asleep (`PowerSaveMode=3`),
  explaining the user's “no signal” observation despite TV unblank.
- Monitor PID 1716243 and lifecycle PID 1716211 survived with zero restarts.
  The monitor deferred sleep and early-resume actions to lifecycle, and ownership
  markers cleared. No compositor stall was observed in that cycle.

An RTC alarm and a helper requesting HDMI-output wake were used; the observations
do not isolate whether manual input helped resume the PC. A delayed emergency
TV-wake helper failed before issuing any TV command; lifecycle had already
restored the TV. Successful TV unblank does not itself prove a visible HDMI
signal. The fixture deliberately keeps those two facts separate.

Hearth's inhibitor honoring remained disabled, so this hardware pass makes no
inhibition claim. The original installed 1.7.0 runtime, services and unchanged
configuration were restored, with test overrides/timers removed. The final VM
work did not change Hearth. Physical Plasma, reporter-specific validation,
additional desktops and simultaneous graphical logins are not claimed here.

## Changes, automated validation and retained evidence

The production fix keeps automatic monitoring composed when a native interface
appears between the initial probes and the legacy fallback probe. Explicit
backend selection and automatic `swayidle` compatibility retain their existing
behavior. Regression tests cover late GNOME and Wayland availability.

The fixture extension supports concurrent clients, power-off invalidation,
state-preserving wake, exact WoL matching and delayed/explicit readiness.
The real-CLI fixture smoke is now part of CI. These are controlled transport
faults, not an emulation of exact LG firmware timing. See
[Native webOS testing](webos-testing.md) for fixture controls.

The combined candidate passed 1,023 runtime unit tests, 151 Cucumber scenarios /
1,885 steps, runtime integration tests, formatting, workspace Clippy with
warnings denied and the real-CLI fixture smoke. One physical gamepad smoke test
is ignored; the VM exercised virtual gamepad input through the production path.
All jobs passed in
[CI run 34711926512](https://github.com/Staphylococcus/LG_Buddy/actions/runs/34711926512),
including packaged GUI checks on Fedora and Arch. Later documentation-only
commits do not change the tested runtime or fixture.

Local commands:

```sh
cargo fmt --all -- --check
cargo test -p lg-buddy
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build -p lg-buddy --example gui_journey_tv
python3 scripts/test-tv-fixture.py target/debug/lg-buddy target/debug/examples/gui_journey_tv
```

Raw VM journals, snapshots, helper scripts, binary hashes, baseline kernel
stacks and CI/local test output are retained under
`~/.local/state/lg-buddy-validation/217-2026-09-12/whole-issue/`. The complete
matrix is recorded across `full-matrix.log`, `matrix-continuation.log`,
`matrix-final.log` and `gnome-return-final.log`; these are continuations in one
boot with the same LG Buddy installation, including the recorded unlock-helper
corrections. Earlier investigation and hardware evidence remain under the
parent directory and its `hearth/` subdirectory. This record supersedes earlier
progress notes that left joint lifecycle or desktop recovery unvalidated.
