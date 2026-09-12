# LG Buddy Testing Strategy

This document keeps the testing strategy practical.

The repository does not need a large test taxonomy. It needs confidence in three things:

1. modules behave as expected within their own scope
2. modules interoperate correctly
3. user needs are actually met

Everything in the strategy should serve one of those three questions.

## 1. Module Behavior

This layer asks:

- does each module do its own job correctly?
- does it fail clearly when inputs are invalid or dependencies misbehave?
- can we trust the module in isolation before wiring it into a larger flow?

This is where most tests should live.

### What belongs here

- config parsing and validation
- path resolution
- state marker behavior
- Wake-on-LAN packet construction
- backend selection rules
- GNOME signal-to-event mapping
- native Wayland registry, seat, and resumed-notification mapping
- logind current-session selection and `LockedHint` mapping
- gamepad device discovery, device-event filtering, raw event mapping, registry
  behavior, and activity policy
- TV command output parsing
- screen and lifecycle policy branching, retry logic, and state-transition
  outcomes

### How to test it

- pure unit tests where possible
- small trait-based fakes for internal collaborators
- subprocess mocks only when the module’s own responsibility includes an external process boundary

### Current examples

- `crates/lg-buddy/src/config.rs`
- `crates/lg-buddy/src/state.rs`
- `crates/lg-buddy/src/backend.rs`
- `crates/lg-buddy/src/sources/desktop/gnome.rs`
- `crates/lg-buddy/src/sources/desktop/wayland.rs`
- `crates/lg-buddy/src/wol.rs`
- `crates/lg-buddy/src/tv.rs`
- `crates/lg-buddy/src/commands.rs`
- `crates/lg-buddy/src/screen.rs`
- `crates/lg-buddy/src/lifecycle.rs`
- `crates/lg-buddy/src/runtime_phase.rs`
- `crates/lg-buddy/src/sources/linux/network_manager.rs`

### Design rule

If a bug can be explained entirely within one module, the first test that catches it should usually live at this layer.

## 2. Module Interoperability

This layer asks:

- do the modules work together through their real boundaries?
- do config, env overrides, state directories, subprocesses, and command orchestration behave correctly together?
- do our mocks match the external contracts we actually depend on?

This is the place for integration tests and contract tests.

### What belongs here

- runtime entrypoints loading a real temporary `config.env`
- settings CLI writes feeding normal runtime config loading and apply behavior
- command flows using real env overrides
- runtime state directories and marker files
- subprocess contracts to external tools
- backend detection against mocked command/process boundaries
- GNOME source behavior against a private session-bus harness
- native Wayland provider capability and registry-churn behavior
- logind lifecycle, current-session lock state, and NetworkManager gate behavior
  against a private system-bus harness
- desktop and auxiliary gamepad activity resetting one LG Buddy-owned deadline
- update-install orchestration ordering against an injected runtime, including
  refusal, decline, concurrent acquisition, candidate preflight, installer,
  identity mismatch, cleanup, and success paths

### How to test it

- use the shared Rust harness in `crates/lg-buddy/tests/support/mod.rs`
- use contract mocks for external dependencies
- keep the tests black-box enough to validate boundaries, but still fast enough for normal development

### Current examples

- `crates/lg-buddy/tests/mock_bscpylgtvcommand.rs`
- `crates/lg-buddy/tests/runtime_entrypoints.rs`
- `tools/mock_bscpylgtvcommand.py`

### Contract-mock rule

Mock the API surface we consume, not the whole system behind it.

Examples:

- the TV mock reproduces `bscpylgtvcommand` command line, exit status, stdout, and stderr behavior that LG Buddy cares about
- GNOME monitor/runtime tests should use the private session-bus harness for
  ScreenSaver signals and Mutter user-active watches
- native Wayland provider tests should model registry discovery, protocol-version
  rejection, every advertised seat, input-notification resumed activity during
  setup, rejection of obsolete protocol objects, and provider loss without
  requiring a compositor
- logind lifecycle/runtime tests should use the private system-bus harness for
  `PreparingForSleep` and `PrepareForSleep` behavior

If a contract shape is unclear, probe the real dependency and update the mock.

## 3. User Needs

This layer asks:

- does LG Buddy do what the user expects?
- does the visible behavior match the product promise?
- do key user scenarios still work end to end?

This is the thinnest layer, but it is the one that keeps the other two honest.

### What belongs here

- readable acceptance scenarios for the main flows
- hardware smoke checks for visible TV behavior
- host-level checks for install/service wiring when those are part of the user experience

### How to test it

- use a small number of acceptance scenarios
- keep them focused on important user outcomes
- stay mock-backed by default
- use real hardware only when the actual visible behavior matters

### Cucumber fits here

Cucumber should be treated as a user-needs tool, not as a separate testing philosophy.

It is useful when we want to express scenarios like:

- when the configured HDMI input is active and the user goes idle, LG Buddy blanks the TV and records ownership
- when the user returns after LG Buddy blanked the TV, LG Buddy restores the screen
- when the graphical session locks, LG Buddy blanks the TV without waiting for
  the inactivity timeout
- when aggressive restore policy is enabled, wake/activity can restore even without a marker
- when GNOME is available, backend detection resolves to `gnome`
- when fresh configuration accepts the default `lg_webos` platform, pairing
  stores the credential before setup completes
- when an existing profile has no platform value, configuration preserves and
  materializes the `bscpylgtv` compatibility fallback
- when native credentials are missing or stale, ordinary TV commands pair or
  repair them as part of the operation
- when native credentials are missing, shutdown and suspend-related commands
  skip immediately without connecting or opening a pairing prompt

It is not the right place for:

- detailed retry/backoff cases
- low-level parsing
- most contract-shape validation
- installer internals

So cucumber sits on top of the first two layers:

- it reuses module-behavior confidence
- it reuses interoperability harnesses and mocks
- it expresses user-visible outcomes in readable form

## Applying The Strategy To This Repo

The GTK frontend applies these same layers without moving application
behavior into GUI tests. Its presentation contract, renderer boundary, and
test split are defined in
[GUI target architecture](gui-target-architecture.md).

### Rust runtime core

Primary concern:

- module behavior

Secondary concern:

- module interoperability

Examples:

- `config.rs`, `state.rs`, `tv.rs`, `backend.rs`, `screen.rs`,
  `lifecycle.rs`, `runtime_phase.rs`, `sources/linux/network_manager.rs`

### External tool boundaries

Primary concern:

- module interoperability

Examples:

- `bscpylgtvcommand`
- later, possibly `systemctl` and `swayidle`

### Native webOS boundary

The native webOS client tests use one stateful server for complete webOS frames,
device state, and protocol-fault scenarios. Characterization tests keep its TV
behavior aligned with observed hardware evidence.

`session/actions/tests.rs` uses this same server through the production client
builder to verify runtime ownership across events: lazy connection, authenticated
session reuse, later reconnection after closure, and replacement after profile
or operation-policy changes. These tests use isolated loopback addresses on
the standard webOS TLS port and share the session tests' environment lock.

Cucumber adds the process-level product boundary. It runs the real `lg-buddy`
binary against the same stateful server over TLS on the standard webOS port.
The scenarios exercise the production unsigned registration manifest and
alert-backed Luna brightness payload while the server enforces exact
firmware-profile behavior and device state transitions. The server also retains
the legacy direct SSAP brightness path as an observed webOS24 behavior; its
webOS26 profile rejects the blacklisted legacy certificate and direct SSAP
write. These are two service-invocation paths over one websocket transport, not
two production routes. Authentication history and pairing prompts are recorded
for assertions. The scenarios cover opt-in and credential outcomes plus
representative brightness, screen, input, and power operations; detailed
transport faults remain in the native client tests. Volume and mute scenarios
exercise the same native audio endpoints characterized against local hardware.
The process-level fixture binds `127.0.0.1:3001`, matching the production TV
endpoint, so that port must be free while the serial Cucumber suite runs.

The evidence workflow, response ownership rules, and semantic scenario model
are documented in
[webos-testing.md](webos-testing.md).

### Desktop backend work

Primary concern:

- source-owned connection/process setup, validation, polling, and observation
  mapping

Secondary concern:

- cross-source orchestration, inactivity state, and policy dispatch in the
  runner

Examples:

- GNOME signal mapping
- GNOME monitor setup, sender ownership and one-shot user-active watches over
  the session-bus seam, independent of SessionManager availability
- overlapping source observations, adapter recovery, stale input and original
  observation times surviving delayed delivery
- native Wayland protocol-version and seat discovery
- native Wayland resumed-notification and registry-removal mapping
- gamepad activity integration with the LG Buddy inactivity deadline
- the Boolean inhibition gate, including overlapping sources, playback
  before/after startup, a full timeout after the last observed release, and
  cancelled input/configuration/lifecycle attempts without stale actions
- screen runtime-phase eligibility over the private logind system-bus seam
- logind lock state entering the shared blanked state without making unlock a
  restore trigger, while observation-time tests cover pre-lock, post-lock grace,
  boundary, and accepted desktop and auxiliary activity
- logind lock monitoring rebinding and reconciling after logind changes its
  unique D-Bus owner
- swayidle production timeout/resume process arguments

Source-specific tests live with their source modules. Runner tests should use
normalized observations and focus on multiplexing or policy behavior rather
than reconstructing provider buses and process protocols.

Push inhibition is tested independently of activity. Colocated tests in
`inhibition.rs` cover Boolean aggregation and diagnostics; GNOME's inhibition
module covers startup synchronization, queued changes, owner validation,
cancellation, release history and periodic reconciliation of missed additions
and removals. `tests/inhibition.rs` runs the production
worker on the shared private D-Bus fixture with only SessionManager present. It
checks existing and later playback inhibitors, overlap, quiet subscriptions,
loss/recovery and prompt permission reads during a slow query. Only observed
inhibition denies permission; tests verify that pending reads retain the last
value and source loss removes its contribution. These are
component checks. `idle_inhibition.feature` exercises the real monitor and TV
policy with private GNOME/PowerDevil services: playback, release delay, neutral
query failure, delayed replies cancelled by gamepad input, restore and lock.

Pull inhibition has colocated contract and adapter tests for fresh queries,
Boolean aggregation, neutral absence/failure, bounded diagnostics, cancellation,
owner replacement and release history. The same private-bus integration test
also runs PowerDevil's production capability against `MockPowerDevil`, which
exposes only `HasInhibition(4)` and emits no signals. It covers initial and later
inhibition, effective clear answers, failure recovery, transport timeout, worker
cancellation during delayed replies, and replacement by a new unique owner.
The mock supplies effective policy; it does not prove Plasma's filtering,
overlap handling or application route coverage.

Preference-section tests in `inhibition.rs` cover the runtime configuration's
default and explicit values, its existing invalid-value fallback, and independence
from desktop selection and activity policy. `tests/settings_operations.rs`
exercises set, disable, re-enable and reset through the real settings writer and
a mocked systemctl restart. It verifies that the restart sees the saved file and
that the inhibition evaluator agrees with the effective setting after reload.
Preference evaluation has no source or release-timing dependency. Facade tests
cover the override, release timing, cancellation on policy changes, and bounded
retries. Engine tests use only Boolean gates and verify that denial changes
neither the activity deadline nor the phase. Private-bus composition tests check
GNOME and PowerDevil together through the production facade.

For live Plasma validation of #223, record Plasma/PowerDevil and application
versions and the application's inhibition route, then inspect effective state:

```sh
busctl --user call org.kde.Solid.PowerManagement \
  /org/kde/Solid/PowerManagement/PolicyAgent \
  org.kde.Solid.PowerManagement.PolicyAgent HasInhibition u 4
```

Check playback that starts before the first query and after a clear query, two
overlapping inhibitors with only one ending, and Plasma's per-application
suppression/reenabling. Allow PowerDevil's own activation delay to pass. Record
the effective answer after each change and repeat after PowerDevil restarts.
Test at least the KDE portal idle route and ScreenSaver D-Bus route where used
by the reporter's applications. Test native Wayland-only inhibition separately;
do not infer its coverage from portal success. The source-traced route table in
[Session backend model](session-backend-model.md#powerdevil-route-coverage) is
not live validation. This work was implemented on GNOME; the Plasma checks and
remaining native Wayland coverage belong to #216/#223 before MVP completion.

Native Wayland changes also require manual checks on Plasma/KWin and at least
one other target compositor. Verify that explicit and automatic `wayland`
detection and monitor startup succeed, unsupported capability or connection
cases report a precise reason, and automatic native monitoring composes available
sources. Existing swayidle fallback coverage remains until #132. Release-facing changes must
keep the static x86_64 musl build and release-bundle smoke test green, including
preservation and deprecation reporting for an existing `swayidle` config.
For the completed inhibition integration (#225), start real video playback
before and after the monitor: disabled
must still blank, enabled must remain visible past the timeout, and stopping
playback must leave the screen visible for a fresh full timeout before blanking.

### Gamepad activity

Subsystem design and adapter guidance live in
[gamepad-subsystem.md](gamepad-subsystem.md).

Primary concern:

- module behavior for device discovery, device-event filtering, evdev event
  mapping, device adapter support detection, per-device state, and activity
  policy

Secondary concern:

- module interoperability in the shared native-session runner path
- runner refresh scheduling when device events arrive or reconciliation is due

Discovery coverage should include event-node filtering, readable-device
failures, sysfs hidraw mapping, device metadata propagation, device-event
parsing, adapter reader specs, and refresh debounce/reconciliation behavior.
Real hotplug is useful for manual validation but should not be required by the
default suite.

Hardware validation:

- use the ignored smoke test when changing real input-device behavior:

```bash
LG_BUDDY_GAMEPAD_SMOKE_SECS=20 cargo test -p lg-buddy --lib \
  session::gamepad::tests::hardware_smoke_reports_real_gamepad_activity \
  -- --ignored --nocapture
```

That test intentionally requires local readable input devices and manual
controller movement. It is not part of the default suite.

### Shell, systemd, and install flow

Primary concern:

- user needs

Secondary concern:

- module interoperability

These should not dominate the Rust test suite, but they still matter because installation and service wiring remain part of the real user path.

The release-bundle smoke test covers the current installed lifecycle topology:
the logind lifecycle service remains installed, the NetworkManager pre-down hook
remains installed, and legacy systemd sleep hooks are absent. Its upgrade phase
proves refusal before sudo, skips configuration, preserves config and native
credentials byte-for-byte, removes obsolete native-profile environments,
preserves healthy legacy environments, refuses unhealthy ones before privilege,
replaces the owned bundle assets, checks service action order, and
verifies the installed runtime against the candidate bytes and identity.

The installer dependency smoke uses package-manager fixtures to verify that
missing GTK/libadwaita packages are installed and the runtime probe succeeds
before candidate identity validation. The real probe also runs without a display.
Fresh native installs and native upgrades run with Python, pip, and bscpylgtv
absent from command lookup; the outer harness retains its Python tools. The
pinned cross-version smoke repeats healthy preservation and unhealthy refusal
for explicit and missing-key legacy profiles, then native cleanup.
The installed GUI smoke verifies the desktop
entry's no-argument `lg-buddy` launch opens the existing pairing prompt without
navigation for an unconfigured installation, and normal Overview for a saved TV.
`lg-buddy brightness` selects the brightness control even when another view is
already open. The application presentation/intent tests cover brightness read,
apply, cancellation, and failures; GTK tests cover rendering and intent routing.
Launcher tests separately cover a damaged installation with a missing GUI
executable and direct headless brightness get/set operations.
Parser coverage keeps bare launch separate from `--help` and `help`, which
remain global CLI help. Existing headless CLI, service, and update paths remain
covered by their current tests. First-run application tests cover saved-profile
navigation, default behavior activation after pairing, declined or unavailable
behaviors remaining off, Settings retries, and preserving existing settings.
Storage and service-boundary tests verify that pairing publication remains valid
when a behavior activation fails. Installer fixtures verify handoff to the
installed executable and preservation of existing configuration.

With the `gui_journey_tv` example supplied, the same installed smoke drives
native webOS pairing against the existing local TLS TV fixture. It covers
rejection, cancellation, interrupted setup, successful default activation,
declined activation followed by Settings retry, re-pairing with retained
preferences, and relaunch with an offline saved TV. Diagnostics is opened
before and after pairing and while offline; its visible report, clipboard,
and saved file must agree and exclude credentials when an observation fails.

The update variant supplies a candidate from `build-release-bundle.sh` and
debug binaries built with `gui-test-fixtures`. Only their HTTP transport is
redirected to local GitHub-shaped responses. Saved-channel selection, fresh
release resolution, archive and identity verification, compatibility checks,
the shipped installer, and executable handoff use the normal application path.
The test checks manual results with automatic checks disabled, cancelled
confirmation, corrupt downloads, declined authorization, and successful
replacement and relaunch with unchanged settings and credentials. The feature
is forbidden in release builds; published artifacts use the default features.

Diagnostics tests cover on-demand collection before pairing, partial reports,
service-state distinctions, bounded subprocess reads, and credential exclusion.
Application tests cover retained safe failures, refresh/close races, exact
copy/save snapshots, chooser cancellation, and failed exports. Native GTK
scenarios exercise the menu, collection responsiveness, report selection,
keyboard focus, adaptive layout, clipboard contents, and file export through
the worker boundary. Tests inject probes and reports without changing live
services or querying a real TV.

The focused release-manifest suite covers deterministic serialization, schema
and critical-field handling, duplicate and missing fields, canonical identity
formats, archive layout, and runtime/GUI target and identity mismatches. The
bundle smoke test exercises the same validator against both generated and
installed executables, verifies their static/dynamic linkage split, and drives
the installed GTK window through normal launch and brightness deep-link paths
plus mocked read, apply, failure/retry, and cancel paths under Xvfb. Fedora and
Arch lanes repeat installed launch checks with
keyboard-only behavior, external AT-SPI role/name/value checks, visibly distinct
light/dark rendering, and 1x/2x window-geometry coverage. The display-backed
renderer suite separately asserts the same GTK semantics directly at the widget
boundary.

These are controlled installed tests: systemd observations and authorization
decisions are fixtures, and elevated installer operations run in an isolated
user namespace and installation root. The supported-distro lanes verify the
actual payload, desktop dependencies, authorization command boundary, and
handoff on those distributions. They do not prove a real desktop PolicyKit
dialog, live service lifecycle, TV authorization, or sleep/wake on hardware;
those still require supported-host verification. NixOS development runs are
not evidence of official NixOS support.

Lifecycle activation selects `systemctl` only from `/usr/bin/systemctl` or
`/run/current-system/sw/bin/systemctl`. The settings integration test shadows
`systemctl` on PATH and overrides `LG_BUDDY_SYSTEMCTL` to verify that neither
controls the executable passed to privileged authorization.

The Ubuntu bundle smoke and the Fedora and Arch installation lanes also exercise
GUI runtime dependency handling. They prove that an unconfirmed install does not
invoke the package manager or GUI, an accepted install requests the correct
native package names before GUI identity validation, insufficient versions abort
before LG Buddy mutation, and already-satisfied hosts perform no package action.

The Rust release-bundle acquisition suite covers exact asset selection, fresh
release metadata, bounded responses and downloads, GitHub and published digest
agreement, lightweight and annotated tags, restrictive staging and locking,
hostile archive types and paths, manifest identity, non-executing embedded
binary identity, and cleanup on success or failure. Run it with:

```bash
cargo test -p lg-buddy release_bundle::tests --lib
```

The normal suite replays GitHub release-response shapes through both a valid
current-contract bundle and the observed historical `v1.4.0-beta.1` metadata.
The historical payload is reduced to a deterministic pre-manifest archive and
must still be rejected at the manifest boundary without contacting GitHub.

The upgrade-preflight module uses injected process, service-manager,
filesystem, and ownership facts around a real temporary-root installation
fixture. Its focused suite covers a passing mutable FHS layout plus symlinked,
mounted, incompletely or wrongly owned, untrusted-writable, read-only,
hard-linked, legacy, conflicting-drop-in, malformed-candidate, and
unavailable-service-manager refusals. Table-driven cases exercise every path
policy's permission contract and every declared candidate input. Candidate
containment cases reject untrusted and non-sticky shared-writable ancestors
while preserving root-owned sticky temporary directories. Virtualenv mutation
checks are conditional on native-profile environment removal and refuse
unsafe roots or nested mount points before clearing. Run it with:

```bash
cargo test -p lg-buddy upgrade_preflight::tests --lib
```

The initial and candidate checks are deliberately non-mutating. Orchestration
tests for their consumers must separately prove that a refusal prevents release
client, confirmation, sudo, and installer effects.

The cross-version bundle smoke test adds the real release boundary that a
same-bundle reinstall cannot cover. It verifies the pinned public
`v1.4.0-beta.2` digest and identity before extraction, installs it into an
isolated root and home, populates non-default settings and native credentials,
and upgrades to an explicit candidate archive. It checks initial and candidate
refusals before network, sudo, or mutation, then verifies preserved user state,
candidate-owned file replacement, service action order, and final identity.

The mock-backed release publisher suite keeps draft staging and publication as
separate capabilities. It proves that staging can create and resume a complete
draft but cannot publish it, while publication cannot create or upload anything.
Publication requires reviewed non-placeholder notes and an exact remote asset
match, changes only the draft state, and preserves the reviewed title and notes.
Checksum, manifest, classification, unexpected-asset, partial-upload, corrupted
upload, retry, and already-published paths are covered without GitHub access.

After a prerelease is public, `production-prerelease-canary` installs the same
baseline and drives its real `updates install` command through a PTY against
GitHub. It checks removal of the obsolete Python environment and preservation
of configuration and native credentials. It then clears the update cache and
proves that the newly installed candidate sees itself as GitHub's newest
published release. The canary records
that sanitized newest-release response, the release-by-tag response, tag ref,
and asset redirects as a workflow artifact. Signed redirect queries and URL
userinfo are never retained. The observed beta.2 newest-release fields also
live in `crates/lg-buddy/testdata/github/` and are replayed by the normal offline
Rust suite. The canary is supplemental post-publication evidence and is not a
prerequisite for stable promotion.

## Current Practical Gaps

The most important remaining gaps are:

- real-host validation for installer and service wiring beyond the release-bundle
  temporary-root smoke test
- broader validation of the remaining shell setup surface
- any future coverage needed for richer `swayidle` hooks beyond `timeout` and `resume`

## Near-Term Priorities

The next testing work should be:

1. keep strengthening module-behavior tests where runtime logic is still moving
2. keep hardware smoke checks targeted and documented near the code path they validate
3. decide how much of the installer and service wiring deserves automated host validation
4. add targeted coverage only if new backend or setup behavior is introduced

## Default Developer Loop

The day-to-day loop should stay simple:

1. `cargo fmt --all`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test -p lg-buddy`

That loop covers most of the first two questions:

- do modules behave correctly?
- do the important runtime boundaries interoperate correctly?

The third question, user needs, should be covered by a small acceptance layer and selected smoke checks, not by trying to force every test into daily local runs.
