# Native setup recovery validation

Recorded 2026-10-04 for [#294](https://github.com/Staphylococcus/LG_Buddy/issues/294),
after the #286-#293 recovery fixes. This is installed-candidate validation, not
release publication or physical-TV acceptance.

## Boundaries

All mutations happened inside disposable desktop VMs. The developer's running
installation and Nix configuration were not changed. The only replacement
service in the native journeys was the repository's loopback webOS TV fixture.
Assessment, notification delivery, authorization, systemd and applicable KWin
readiness used the installed application and native system components.

The [installed CI journey](../scripts/test-release-gui-journey.sh) remains useful
deterministic coverage, but its assessment peer is not evidence for this matrix.
Likewise, marker-based NixOS/OSTree tests establish recovery policy contracts,
not native desktop acceptance.

## Candidate identity

Production source: `82ba73ac`, based on `9e741e01`, including all #286-#293 fixes.
The native harness and this record were added afterward. All builds report the
existing development version 1.9.0; this was not changed to imply a release.

| Installed environment | Build and desktop |
| --- | --- |
| Fedora 44, `lg-buddy-recovery-294` | Native Fedora GNU debug build; GNOME 50.5, Plasma/KWin 6.7.5, Qt 6.11.2, GTK 4.22.5, libadwaita 1.9.4; also native Sway Wayland |
| Ubuntu 24.04.5, `lg-buddy-recovery-ubuntu-294` | GNU debug binaries built against Ubuntu 24.04 libraries in the GTK CI image, then installed in the real VM; GNOME 46, GTK 4.14.5, libadwaita 1.5.0; Wayland and X11 |
| Arch Linux image 2026.10.01, `lg-buddy-recovery-arch-294` | Same baseline GNU binaries installed through the real installer; native Plasma/KWin 6.7.5, Qt 6.11.2, GTK 4.22.5, libadwaita 1.9.4; Wayland |
| NixOS ephemeral GNOME VM | Real downstream Nix package and service module, GNU release build, declarative user/system services, bad and repaired system specialisations |

Installed SHA-256, runtime then GUI:

```text
Fedora:
75a59b7cd58723cf18cde9e3d37a56e14090250641a02fc288a5c12575f1d004
84fc68af00cd706476e496aff7b2293a976caa988765dd015b028e683e3411c9
Ubuntu and Arch:
ee3429ece4ac68525a733af03b8d327980dceda50963d94047e62fbc0174325c
f548c77f20c5e8ac3cd3e73b6398b79346439469bdae0690d1294fae2f774f7c
NixOS (GUI is the actual .lg-buddy-gui-wrapped ELF):
aa1f0b1af6045816110fd4236bcce7a1b8af77e06d8d9e02c363f8d0397e7751
e5b3a88f6371e39636b752f8b8ddfd82d4db5649863e103e9909aedc439dc917
```

The Nix test read, but did not edit, the downstream `pkgs/lg-buddy.nix` and
`modules/services/lg-buddy.nix`. Their file digests were respectively
`951014df5a875848cfbae1124c6a771fe734d815370b1c4c958aaa2846078199` and
`6b51e29a2dde079856c5fb2a2a3b17d8022a04867fd0fc334e9dfbbe2dfce93e`.
The test derivation adds util-linux for authorization tests and builds the TV
fixture example; it does not disable package checks. The caller supplies those
downstream files explicitly because LG Buddy does not own a NixOS module here.

## Results

| Selectable case | Native result |
| --- | --- |
| Fresh notification and setup | Ubuntu daemon 4651 revision 1 published Pairing/Services incomplete. Clicking the real GNOME notification body opened the ordinary single-CTA splash, not a setup modal. CTA, real TV pairing and native GNOME Polkit service repair reached revision 3 Complete and the functional UI. |
| Partial repair and authorization denial | Fedora GNOME native Polkit cancellation retained pairing and kept the gate. Reopening resumed the remaining service repair without another pairing prompt; actual authorization and fresh Complete publication admitted the UI. |
| Malformed configuration | Fedora's real notification opened the splash. The existing modal corrected the saved IP and update preference. Missing packaged KWin payload stayed blocked with external repair guidance and Recheck. Restoring the actual payload and clicking Recheck reached daemon 3432 revision 4 Complete without re-pairing. |
| Compatible prebuilt | Fedora Plasma loaded a native compatible prebuilt artifact with an empty cache and zero-byte build log; daemon 5623 revision 12 was Complete. The first unreadable test artifact directory was corrected before claiming this result. |
| Cache and local build | Native Fedora cached repair and local compilation both loaded the real bridge. Declining build-tools consent installed nothing. Accepting used actual KDE Polkit and DNF, then built/loaded the bridge and published Complete. |
| Arch package differences | Compiler/CMake were initially absent. Separate consent invoked real pacman, installed build dependencies, compiled and loaded the bridge, and reached daemon 5639 revision 3 Complete with a connected functional UI. |
| Incompatible loaded bridge | A root-owned source comment deliberately advanced the installed payload identity while the old native bridge stayed loaded. Revision 4 remained Incomplete and the GUI gated; native build/load repair reached revision 5 Complete. Restoring the original source then exercised cached repair through the runner, revision 6 Incomplete to revision 7 Complete. |
| Missing authorization agent | Stopping Fedora's real KDE agent produced authorization failure and Retry, not admission. Restoring the agent and authorizing recovered. Arch also stayed gated when its native Polkit helper socket had not been activated; starting that package-owned socket restored authorization. |
| Old verifier | The verified public v1.9.0 runtime actually returned UnknownMethod for GetSetupAssessment. Current GUI/support files stayed installed. The gate offered installation repair, session restart and Recheck; replacing the runtime and restarting the real service recovered with saved pairing intact. |
| Absent verifier | Stopping Ubuntu's actual screen/session service kept the initially unknown GUI gated. CTA entered the existing flow, repaired service readiness and published Complete before admission. |
| Delayed verifier and retained admission | Actual SIGSTOP/SIGCONT of the daemon exercised bounded initial verification recovery and retention of previously verified admission. Fedora Plasma and Ubuntu, including X11, passed without an assessment replacement peer. |
| Cached GUI open | Opening a ready installed GUI left the daemon instance/revision/result unchanged on Fedora Plasma, Ubuntu Wayland/X11 and Arch Plasma. |
| Non-KWin and headless/login | GNOME X11 and native Sway Wayland reported Plasma NotApplicable and admitted the UI. Genuine headless Fedora CLI setup completed with no KWin/Sway process; the following Sway login automatically started the daemon and admitted the GUI from its stored Complete result. |
| Declarative external repair | NixOS revision 1 identified the exact incorrect update-service configuration binding, offered NixOS build/activate guidance and Recheck, and made no imperative repair. Actual repaired-system activation followed by GTK Recheck reached revision 2 Complete in the same daemon instance and admitted normal navigation. |

The native NixOS acceptance output was
`7gm04n0lh2cqg19ajrd72qxmc2y1hhdl-vm-test-run-lg-buddy-native-setup-recovery`.
Its log records installed hashes, live service PID/ExecStart, both publications,
GTK controls and gate/admitted screenshots. TV reachability is deliberately
separate from setup admission: the Nix activation stopped the test TV unit, so
normal navigation displayed the real disconnected-TV recovery state afterward.

Unsupported Plasma versions/layouts and immutable/OSTree repair boundaries are
covered by the Rust and shell contracts, including refusal to mutate and
external-remedy/Recheck presentation. This matrix does **not** claim a native
Atomic desktop or NixOS Plasma run, nor support for installing into those images.
It also does not claim every desktop/distribution combination or a physical TV.

## Reproduction

Use disposable VMs only. Install the candidate through `install.sh` with real
runtime/GUI paths and without INSTALL_ROOT, SKIP_SYSTEMD or authorizer overrides.
Run the TV example on loopback as the graphical account. Open the GUI through
the actual desktop/user manager so Polkit sees the correct native subject.
Administrative passwords and consent remain real user decisions.

The [native runner](../scripts/test-native-setup-recovery.py) requires Python
dbus/pyatspi, the installed application, a logged-in native desktop, a VM check
and an exact hostname acknowledgment. It refuses root, runtime overrides, an
existing GUI and reuse of an evidence directory. For example:

```sh
python3 scripts/test-native-setup-recovery.py cached-open \
  --disposable-vm DISPOSABLE_HOSTNAME --source CANDIDATE_COMMIT \
  --evidence "$HOME/evidence/cached-open"
```

Select `cached-open`, `retained-admission`, `daemon-delay`, `notification-open`
(GNOME), `repair`, or read-only `inspect`. `repair` starts from a real published
Incomplete result, opens the existing CTA/modal, records changing controls and
waits for native user input/authorization plus fresh Complete admission. It does
not silently approve consent or inject a grant. `notification-open` clicks the
real Shell notification using native Mutter input, not ActionInvoked injection.

Stage one fault at a time in the disposable guest, then select the relevant run:

| Fault/setup | Check and remediation |
| --- | --- |
| Fresh empty configuration | Start the real session service to publish Pairing/Services incomplete; `notification-open`, then `repair`; supply loopback IP/MAC, pair and authorize. |
| Partial repair/denial | Cancel the real Polkit challenge; cancel/reopen the modal and verify retained credential/no new pairing prompt before authorization. |
| Bad saved IP/preference | Edit only the guest configuration, request a real assessment, then `repair`; correct the existing editable fields. |
| Missing helper payload | Back up/rename the guest's installed helper; `repair` must pause with external guidance. Restore payload, Recheck and observe fresh publication. |
| Prebuilt/cache/local Plasma | Stage a compatible root-readable prebuilt, leave an existing compatible cache, or remove both. Unload the real bridge, request assessment, then `repair`. Check live bridge identity and actual build log/package changes, not just the GUI's success message. |
| Stale loaded bridge | Back up the installed source; change a comment to advance its hash, request assessment, then `repair`. Restore only after the run finishes, and repair back to the original identity. |
| Missing agent | Stop the guest's native agent, then `repair`; verify gated failure/Retry. Restore the native agent and authorize. |
| Absent/old daemon | Stop the actual service, or start a verified older release while keeping the current GUI/support payload. Open normal GUI, verify the gate/guidance, restore the candidate runtime and restart/recheck. |
| Headless/desktop switch | Log out/stop the actual graphical session, confirm no compositor remains, run CLI setup, then log in to the next real desktop and select `cached-open`. |

Never clear the notification ledger and call that a new login. Either use an
actual fresh session or introduce a genuinely different published requirement.
Do not modify installed source while a repair mutation is in progress.

The [NixOS fixture](../scripts/test-native-setup-nixos.nix) builds both actual
system specialisations and activates the repaired one **inside its VM**, not on
the host. Supply the downstream package and module implementing
`vas.services.lgBuddy`:

```sh
nix-build scripts/test-native-setup-nixos.nix --no-out-link \
  --arg pkgs 'import /path/to/pinned/nixpkgs {}' \
  --arg packageFile /path/to/nix-config/pkgs/lg-buddy.nix \
  --arg serviceModule /path/to/nix-config/modules/services/lg-buddy.nix \
  --arg src /path/to/candidate --argstr buildCommit CANDIDATE_COMMIT
```

## Cleanup and harness corrections

The runner resumes any daemon it paused, quits only its owned GUI, waits for
bus release **and process exit**, and records remaining user processes. System
changes from native repair intentionally belong to the disposable guest; stop
or discard that VM afterward. Native NixOS tests terminate their QEMU guest.
No test workers or repair locks remain running after guest shutdown.

Guest/harness faults were corrected without production workarounds: GTK entry
text and native Shell coordinates are different coordinate spaces; X11 launches
need the real user-manager display environment; KDE screen unlocking is not
proved by `loginctl unlock-session`; native Polkit subjects must not be created
in an incidental console session; copied root payloads need correct ownership
and readable modes. Nix's Python harness needs pygobject and its AT-SPI/DBus
typelibs. The first assertions incorrectly coupled admission to a reachable TV;
normal navigation, not successful TV reads, establishes admission.

The one product defect found here was GNOME notification-body activation.
`82ba73ac` adds the standard default action to the same trusted, one-shot GUI
launcher. Actual GNOME 46 and 50 body activation then passed.

## Final regression checks

- Full backend suite passed with `RUST_TEST_THREADS=1`: 1,458 unit tests, the
  existing child-only ignored fixture, 155 Cucumber scenarios/1,988 steps, and
  every integration target including all 42 runtime entrypoints.
- All 13 isolated display-backed GTK scenarios passed with the required
  `xdotool`, Xvfb and GTK dependencies; workspace/all-target/all-feature Clippy
  passed with warnings denied.
- Service repair, KWin setup and matrix contract checks passed. The native
  runner compiled and its final process-exit cleanup passed on Arch.

An earlier default-parallel backend repeat had four logind lifecycle/timing
failures; all passed in the serialized full run. These are not evidence of a
fixed production lifecycle defect, and this record does not claim remote CI
results. Serial validation also exposed and fixed the existing lock-test child
readiness framing, which otherwise deadlocked on libtest's progress prefix.
