# Development

This document covers building, local installation, validation, release tooling, and contributor-facing repository details.

## Build Prerequisites

Compiling the Rust runtime requires:

- a Rust toolchain with `cargo`
- a working C toolchain

Compiling and testing the GTK frontend additionally requires:

- GTK 4.14 or newer development files
- libadwaita 1.5 or newer development files
- `pkg-config`
- `glib-compile-resources` (provided by the GLib development tools)
- a graphical session or virtual display for renderer tests
- `xdotool` for native pointer tests and the executable launch smoke test
- `pgrep` (`procps` on Debian, `procps-ng` on Fedora/Arch) for installed GUI process checks
- AT-SPI 2 and its Python bindings (`python3-pyatspi` on Debian/Fedora,
  `python-atspi` on Arch) for observable GUI behavior tests

Python is a development/test dependency for release tooling, GUI observers,
and legacy fixtures. The pinned historical installer smoke also needs `venv`
and `pip` support. Fresh and native installations use the bundled Rust binaries
and do not provision a Python environment.

Backend-specific tools used in development and local testing:

- readable `/dev/input/event*` devices for local gamepad activity testing
- readable `/dev/hidraw*` devices when testing the Logitech G923 raw HID fallback

For GNOME end-to-end work, the running session also needs the full GNOME contract:

- GNOME Shell
- `org.gnome.ScreenSaver`
- `org.gnome.Mutter.IdleMonitor`

Native monitoring honors the opt-in **Allow apps to prevent idle blanking**
setting through independent GNOME SessionManager, PowerDevil and optional KWin inhibition
capabilities. Activity tests require Mutter user-active watches; SessionManager
availability must not determine whether activity monitoring works. Private-bus
fixtures cover the integrated gate. The [desktop validation record](desktop-session-validation.md)
documents live GNOME/Plasma coverage. Plasma uses both PowerDevil and native KWin
inhibition. See [KWin integration](kwin-integration.md) for plugin delivery and
the supported prebuilt, local-build and absent-source modes.

The C toolchain is required because `cargo build` now compiles vendored
`libdbus` as part of the dependency graph. On common Linux distributions that
usually means:

- Debian/Ubuntu/Pop!_OS: `build-essential`
- Fedora: `gcc`
- Arch: `base-devel`

## Build

Build the runtime from source with:

```bash
cargo build --release -p lg-buddy
```

Build the GTK frontend from source with:

```bash
cargo build --release -p lg-buddy-gui
```

Run the normal Overview directly with:

```bash
cargo run -p lg-buddy-gui
```

Run the brightness deep link directly with:

```bash
cargo run -p lg-buddy-gui -- brightness
```

After building both workspace binaries, the installed app launcher can be
exercised with `cargo run -p lg-buddy`; it resolves `lg-buddy-gui` beside the
running CLI executable and launches its normal Overview entrypoint.
`cargo run -p lg-buddy -- brightness` remains the brightness-focused deep link.
`LG_BUDDY_GUI` overrides that companion path for relocation and subprocess
tests. Both graphical launch paths use this GTK executable. Headless brightness
get/set commands operate directly through the runtime.

The local installer accepts the GUI and runtime as separate build artifacts.
Official release bundles ship and verify both. Fresh installs and upgrades
check GTK/libadwaita versions and offer to install missing packages through the
distribution's package manager before validating and installing the binary pair.

The fresh release-bundle installer installs the payload and launches the installed
GUI for pairing and service setup through the [shared completion flow](onboarding.md).
Both GUI and CLI preserve desired settings when a later step fails or is cancelled.
The central GUI setup gate consumes the session daemon's published assessment;
only daemon-verified completion admits normal pages. Run `cargo run -p lg-buddy
--example setup_assessment` in the desktop session to inspect the native checks
directly without changing the published result.
Use `install.sh --headless` for terminal installation and `lg-buddy setup` to
resume. `configure.sh` forwards to setup. Configured installations and upgrades
retain their saved settings.
The application menu offers on-demand diagnostics with Refresh, Copy, and Save.
The report starts with current settings, service and activity state, fresh
inhibition-source readings, and read-only running KWin/Qt version and plugin-path
inspection. Bounded, timestamped service logs and a dated tail of native KWin
setup output follow in a separate section. Setup failures also retain sanitized
detail in GUI session events, including failures before a setup log exists.
Refreshing the report does not change the monitor's decisions or retry setup.
`diagnostics.rs` orchestrates domain collectors in `diagnostics/`: desktop,
monitor, inhibition, settings, services, TV, application identity, KWin, and journals.
Each collector has isolated tests with explicit inputs or injected transports;
subprocess bounds and report rendering are tested separately. Run them with
`cargo test -p lg-buddy --lib diagnostics::`; `runtime_entrypoints` tests the
integrated report against a running monitor on a private session bus.
Complete journey verification remains tracked in
[issue #129](https://github.com/Staphylococcus/LG_Buddy/issues/129).

Official release builds inject version identity into the binary:

```bash
LG_BUDDY_RELEASE_VERSION=X.Y.Z LG_BUDDY_BUILD_COMMIT="$(git rev-parse HEAD)" cargo build --release -p lg-buddy
```

Without those environment variables, `lg-buddy --version` reports the Cargo
package version with `channel: dev` and `commit: unknown`.
Supported channel values are `dev`, `prerelease`, and `stable`.

The resulting binary will be at:

```text
./target/release/lg-buddy
```

The [Nix package](nix.md) builds both binaries with pinned dependencies, installs
their assets and setup inspection helper, and wraps the GTK companion. Its flake
check runs runtime library tests and exercises the installed GUI through both the
wrapper and a profile symlink on a private bus and virtual display.

## Install a Locally Built Binary

The installer below is for release-bundle layouts. For `.deb` and `.rpm`
builds, use [Native packages](#native-packages). Package-managed first-run setup
and configuration bindings are described in [onboarding](onboarding.md#package-owned-service-assets).

`install.sh` is installer-only. It does not build the runtime or GUI.

To install binaries you built yourself:

```bash
./install.sh \
  --runtime-binary ./target/release/lg-buddy \
  --gui-binary ./target/release/lg-buddy-gui
```

To install from a release bundle instead, extract the archive and run:

```bash
./install.sh
```

Before changing anything, the bundle installer checks its complete system write
and removal plan with the candidate runtime's private `installer-preflight`
command. It queries each destination through the shared package database and
uses filesystem mount facts to reject read-only targets. Owned or conflicting
paths and unavailable ownership information all refuse installation with a
path-specific error. Recursive cleanup and copied KWin payload members are
included, even when the destination does not exist yet.
Legacy service enablement and alias links are collected before preflight and
only those checked links are removed during cleanup.

This check runs before dependency installation, temporary installation files,
user config/selection writes, or service changes. The authorized graphical
upgrade helper repeats it before system mutations. A preflight refusal leaves
the installation and user state unchanged; a later I/O failure still follows
the existing partial-upgrade recovery contract. Existing shared directories
are reused without changing their modes.

## Native packages

Both builders stage the same verified release bundle, including its exact
runtime/GUI identity and optional desktop integration assets. They validate
archive paths, types and metadata without running archive scripts. Build on
the appropriate baseline so native dependency metadata matches that system:

- **Ubuntu 24.04:** `python3`, `dpkg-dev`, `libgtk-4-1` and `libadwaita-1-0`.
- **Fedora 44:** `python3` and `rpm-build`.

Set `SOURCE_DATE_EPOCH` to the release source commit's Unix timestamp. Run the
appropriate command against the bundle built from that commit:

```bash
# Ubuntu
python3 scripts/build_debian_package.py \
  --archive dist/lg-buddy-2.0.0-beta.4-x86_64-unknown-linux-musl.tar.gz \
  --source-date-epoch "$SOURCE_DATE_EPOCH" --output-dir dist

# Fedora
python3 scripts/build_rpm_package.py \
  --archive dist/lg-buddy-2.0.0-beta.4-x86_64-unknown-linux-musl.tar.gz \
  --source-date-epoch "$SOURCE_DATE_EPOCH" --output-dir dist
```

Repeating a build with identical input, tools, dependency database and timestamp
must produce identical bytes. Both recipes derive library dependencies from
the binaries and declare the GTK/libadwaita minimums in the
[README](../README.md#before-you-install). Python is a build/test dependency.
Native version ordering, filenames and official packaging revisions are
described in [release assets](release-process.md#native-release-assets).

Package scripts install payload noninteractively and may reload the machine
unit database. They never activate services, scan homes, pair a TV, convert
settings, restart the TV lifecycle oneshot or launch UI. The installed
application's [onboarding flow](onboarding.md#package-owned-service-assets) owns
activation and dynamic bindings. Packages must not claim profiles, credentials
or dynamic drop-ins, even through DEB conffiles or RPM ghost entries.
Upgrades preserve activation policy; removal retains configuration and user
data. See the [user guide](user-guide.md#uninstall) for removal/reset procedures
and [architecture](architecture-overview.md#native-package-updates) for ownership
and privileged update boundaries.

### Package tests

Run the payload and native version-ordering tests without root:

```bash
python3 scripts/test_build_debian_package.py
python3 scripts/test_build_rpm_package.py
```

Native ordering checks additionally need `dpkg` or Fedora's `python3-rpm`.
CI builds each package twice and runs its lifecycle with real package tools.
For local lifecycle checks, build revisions 1 and 2 (`--revision 2`) and run
these commands **only inside a disposable Ubuntu/Fedora container** with root,
Docker and the explicit test flag:

```bash
LG_BUDDY_DISPOSABLE_PACKAGE_TEST=1 bash scripts/test-debian-package.sh \
  /packages/lg-buddy_2.0.0-beta.4-1_amd64.deb \
  /packages/lg-buddy_2.0.0-beta.4-2_amd64.deb

LG_BUDDY_DISPOSABLE_PACKAGE_TEST=1 bash scripts/test-rpm-package.sh \
  /packages/lg-buddy-2.0.0-beta.4-1.x86_64.rpm \
  /packages/lg-buddy-2.0.0-beta.4-2.x86_64.rpm
```

The suites check conflicts, install, upgrade/reinstall, ownership and payload
verification, GTK launch, owner-only startup conversion, and retained data
through removal. Package operations must leave current/stale profiles and
credentials unchanged; conversion belongs to the installed GUI/user daemon.

`scripts/test-debian-update.py` also exercises installed APT and DNF updates,
including root digest refusal, declined confirmation, retained data and
normal-user GUI handoff. CI supplies the debug transport fixtures and candidate
package. Fedora 44 uses `--expect-new-rpm-dependency libglvnd-gles` to cover new
dependencies and checks signed, unsigned, tampered and symlink dependency RPMs.
Fedora 43 uses `--expect-unsupported-rpm-platform` to require refusal before HTTP
or mutation. See the native jobs in [CI](../.github/workflows/ci.yml) for setup.

These fixtures substitute HTTP transport and authorization. Public release
updates and real desktop authorization/service/reboot checks belong to the
[release process](release-process.md#public-package-and-legacy-canaries).
Record test-run results in CI artifacts or the PR.

## Validation

Useful checks during development:

```bash
cargo test -p lg-buddy --lib
cargo test -p lg-buddy --test cucumber
dbus-run-session -- xvfb-run -a bash ./scripts/test-gui-launch.sh ./target/debug/lg-buddy-gui
dbus-run-session -- xvfb-run -a bash ./scripts/test-gui-launch.sh ./target/debug/lg-buddy
cargo build --locked -p lg-buddy --example gui_journey_tv
dbus-run-session -- xvfb-run -a bash ./scripts/test-installed-gui.sh ./target/debug/lg-buddy ./target/debug/lg-buddy-gui ./target/debug/examples/gui_journey_tv
dbus-run-session -- xvfb-run -a env ADW_DISABLE_PORTAL=1 GDK_BACKEND=x11 GDK_DEBUG=no-portals NO_AT_BRIDGE=1 cargo test -p lg-buddy-gui -- --test-threads=1
cargo clippy --workspace --all-targets --all-features -- -D warnings
bash -n install.sh uninstall.sh configure.sh bin/LG_Buddy_Common scripts/build-release-bundle.sh scripts/test-gui-launch.sh scripts/test-installed-gui.sh scripts/test-release-bundle.sh scripts/test-cross-version-upgrade.sh scripts/test-production-upgrade-canary.sh scripts/publish-release-assets.sh
python3 scripts/test_release_promotion.py
python3 scripts/test_record_github_release_responses.py
```

The GUI launch checks cover both no-argument normal Overview launch and the
brightness deep link, including focus handoff from another view. Installed
smoke keeps the desktop entry, runtime, and GUI together.

PR CI runs verification and the KWin checks first. Verification builds GUI
journey fixtures using its existing GNU debug dependency graph; the bundle job
builds only the optimized runtime and GUI. Ubuntu runs the release-bundle,
installed GUI, historical upgrade and v1.8 startup-migration suites in separate
matrix jobs, each with a
20-minute limit. Those jobs and Fedora/Arch checks consume the same artifacts
in parallel. The required `bundle-smoke-test` check passes only when artifact
production, every Ubuntu smoke suite, native package jobs, DNF dependency and
Fedora 43 refusal checks, and complete
release-asset assembly succeed;
failures, timeouts, and skipped
jobs fail the check.

The root ownership checks execute the library test binary that Cargo already
built for the ordinary suite. The TV fixture shares the GNU debug build and
glibc baseline of the `0.0.0` update-journey app pair, avoiding another musl
dependency build. Fedora and Arch exercise the same downloaded fixture.
Rust dependency caches are keyed by job, compiler, Cargo inputs and build
environment. GitHub scopes PR caches to that PR, so they accelerate subsequent
revisions and reruns; a new PR may still start cold. Workspace binaries retain
their normal builds and embedded candidate identity checks.

Optional hardware smoke for gamepad activity:

```bash
LG_BUDDY_GAMEPAD_SMOKE_SECS=20 cargo test -p lg-buddy --lib \
  session::gamepad::tests::hardware_smoke_reports_real_gamepad_activity \
  -- --ignored --nocapture
```

Run that from a desktop session that has read access to the connected
controllers. The test uses the production gamepad activity source and requires
manual input during the capture window. To smoke-test hotplug behavior, start
the monitor and connect or disconnect a controller; the gamepad source should
refresh without restarting the service. The production monitor also performs a
periodic reconciliation scan for missed device events.

For gamepad subsystem internals and adapter contribution guidance, see
[gamepad-subsystem.md](gamepad-subsystem.md).

## Release Tooling

Build a release bundle locally with:

```bash
LG_BUDDY_RELEASE_VERSION=0.0.0-dev \
LG_BUDDY_BUILD_COMMIT="$(git rev-parse HEAD)" \
cargo build --locked --release -p lg-buddy --target x86_64-unknown-linux-musl
LG_BUDDY_RELEASE_VERSION=0.0.0-dev \
LG_BUDDY_BUILD_COMMIT="$(git rev-parse HEAD)" \
cargo build --locked --release -p lg-buddy-gui --target x86_64-unknown-linux-gnu
./scripts/build-release-bundle.sh \
  --target x86_64-unknown-linux-musl \
  --gui-target x86_64-unknown-linux-gnu \
  --version 0.0.0-dev
```

The builder requires a full release commit and expects the matching release
artifacts to exist under:

```text
./target/<target>/release/lg-buddy
./target/<gui-target>/release/lg-buddy-gui
```

Release CI puts the complete pinned KWin matrix in `target/kwin-bridges/` and
passes `--require-kwin-matrix` to the bundler. For the same check locally, download
the matrix artifacts built from the exact current inputs or generate them with
`python3 scripts/kwin_matrix.py build --directory target/kwin-bridges` using Nix.
See [KWin integration](kwin-integration.md#building-and-checking-coverage) for the
pins, loader tests and commands for individual targets.

An ad hoc local bundle may omit `--require-kwin-matrix`. A matching Plasma
development host can provide a local artifact with
`bash data/kwin/build.sh data/kwin/source target/kwin-bridges`; the bundle smoke
requires at least one prebuilt and verifies its metadata/source identity. A local
bundle without prebuilts still supports compilation/ordinary source absence.

Smoke test a generated release bundle with:

```bash
dbus-run-session -- xvfb-run -a ./scripts/test-release-bundle.sh \
  --archive ./dist/lg-buddy-0.0.0-dev-x86_64-unknown-linux-musl.tar.gz
```

The smoke test validates `release-manifest.json` against the archive name and
bundled runtime and GUI before running installer code. It then installs into a temporary
root and exercises upgrade refusal, native environment cleanup, healthy legacy
preservation, unhealthy legacy refusal, owned-file
replacement, service ordering, GTK/libadwaita dependency confirmation and
refusal, installed identity, mocked GUI read/apply/failure/cancel behavior,
lifecycle topology, and uninstall cleanup without mutating the host installation.
The supported Fedora and Arch lanes repeat the dependency flow with their native
package-manager mappings.
The native installer runs use `test-without-python.sh` to restrict command
lookup to native host tools. Python remains available to the outer test harness.

For the complete installed GUI journey, build the local TV fixture and pass it
to the existing smoke:

```bash
cargo build --locked -p lg-buddy --example gui_journey_tv
dbus-run-session -- xvfb-run -a bash scripts/test-installed-gui.sh \
  target/debug/lg-buddy target/debug/lg-buddy-gui \
  target/debug/examples/gui_journey_tv
```

The CI bundle lane additionally builds a `0.0.0` debug runtime and GUI with
`--features gui-test-fixtures` and passes the newer canonical candidate archive
as the fourth argument. This enables deterministic manual checks and real
installation/handoff within the same smoke. The HTTP fixture is loopback-only,
and this feature cannot be used in a release build. Run as a regular user with
unprivileged user namespaces available; the test never targets the host's
installation or live services. `LG_BUDDY_KEEP_GUI_SMOKE=1` retains failure
evidence in the printed temporary directory.

Run the cross-version smoke with explicit previous and candidate archives:

```bash
./scripts/test-cross-version-upgrade.sh \
  --previous-archive /path/to/previous.tar.gz \
  --previous-sha256 <pinned-digest> \
  --previous-tag <tag> --previous-version <version> \
  --previous-channel <channel> --previous-target <target> \
  --previous-commit <sha> \
  --candidate-archive /path/to/candidate.tar.gz \
  --candidate-tag <tag> --candidate-version <version> \
  --candidate-channel <channel> --candidate-target <target> \
  --candidate-commit <sha>
```

The production upgrade canary is CI-only because it requires the candidate to
already be published. It records sanitized GitHub responses as a workflow
artifact so the observed production shapes can be replayed offline.

Run the focused manifest contract tests with:

```bash
python3 scripts/test_release_bundle_manifest.py
```

Run the mock-backed draft staging, reviewed publication, and retry contract tests
with:

```bash
python3 scripts/test_publish_release_assets.py
```

After building both native packages from the same release bundle on their
supported baselines, generate and verify the complete asset set on Fedora 44
with `dpkg`, `rpm` and Python installed. Supply the exact binary source commit
and a supported release version; the helper refuses mismatched identities.
The publisher dry run performs these checks without writing to GitHub:

```bash
python3 scripts/release_assets.py --dist-dir ./dist --tag "v$RELEASE_VERSION" --commit "$RELEASE_COMMIT" --write-checksums
GH_RELEASE_DRY_RUN=1 ./scripts/publish-release-assets.sh stage-draft --dist-dir ./dist --tag "v$RELEASE_VERSION" --commit "$RELEASE_COMMIT"
```

Official releases are created only through a reviewed `dev` promotion PR. For
the branch contract and recovery process, see
[release-process.md](release-process.md).

## Repository Layout

| Path | Purpose |
| --- | --- |
| `crates/lg-buddy/src/lib.rs` | CLI parsing and command dispatch |
| `crates/lg-buddy/src/commands.rs` | Runtime command entrypoints and dependency assembly |
| `crates/lg-buddy/src/brightness.rs` | Toolkit-neutral brightness read/write flow and production adapters |
| `crates/lg-buddy/src/overview.rs` | Toolkit-neutral Overview state, intents, and capability operations |
| `crates/lg-buddy/src/tvs.rs` | TV collection, selection, pairing coordination, and local profile operations |
| `crates/lg-buddy/src/pairing.rs` | First-TV pairing workflow, validation, and cancellation |
| `crates/lg-buddy/src/pairing_store.rs` | TV profile and native credential persistence with rollback |
| `crates/lg-buddy/src/settings_view.rs` | Settings presentation, intents, reads, and serialized mutations |
| `crates/lg-buddy/src/navigation.rs` | Desktop destinations and selected view |
| `crates/lg-buddy/src/events.rs` | Canonical runtime event vocabulary |
| `crates/lg-buddy/src/policy.rs` | Policy outcome, action, no-action, diagnostic, and state-transition types |
| `crates/lg-buddy/src/presentation/` | Toolkit-neutral GUI presentation declarations owned by the application |
| `crates/lg-buddy/src/application.rs` | Cross-view application coordinator and typed transitions |
| `crates/lg-buddy-gui/src/lib.rs` | GTK application controller, worker bridge, and command-line reactivation |
| `crates/lg-buddy-gui/src/window.rs` | Adwaita window, navigation stack, About dialog, and shared dialogs |
| `crates/lg-buddy-gui/src/overview.rs` | Overview summary, brightness, volume, mute, and focus rendering |
| `crates/lg-buddy-gui/src/tvs.rs` | TV list/details rendering, adaptive navigation, and unpair dialog |
| `crates/lg-buddy-gui/src/pairing.rs` | Native first-TV pairing dialog and progress rendering |
| `crates/lg-buddy-gui/src/settings.rs` | Native settings rows, editors, and feedback rendering |
| `crates/lg-buddy/src/screen.rs` | Session screen blank/restore policy |
| `crates/lg-buddy/src/lifecycle.rs` | Startup, shutdown, system sleep, and system resume policy |
| `crates/lg-buddy/src/runtime_phase.rs` | Runtime sleep-phase provider abstraction |
| `crates/lg-buddy/src/session/runner.rs` | Session monitor loop |
| `crates/lg-buddy/src/session/inactivity.rs` | Session inactivity deadline and phase synthesis |
| `crates/lg-buddy/src/inhibition.rs` | Push/pull inhibition, preference override, release timing and cancellable Boolean blanking gate |
| `crates/lg-buddy/src/session/gamepad/` | Gamepad activity discovery, device-event refresh, adapters, capture, registry, and policy |
| `crates/lg-buddy/src/session_bus.rs` | Generic D-Bus transport used by session and system event sources |
| `crates/lg-buddy/src/sources/linux/logind.rs` | Linux logind lifecycle and current-session lock-state adapter |
| `crates/lg-buddy/src/sources/linux/network_manager.rs` | NetworkManager pre-down lifecycle source adapter |
| `crates/lg-buddy/src/sources/desktop/gnome.rs` | GNOME backend integration |
| `crates/lg-buddy/src/sources/desktop/gnome/inhibition.rs` | Independent SessionManager inhibition capability |
| `crates/lg-buddy/src/sources/desktop/kwin.rs` | Optional pull inhibition through the native KWin plugin |
| `data/kwin/` | Plugin source, compatible artifact selection and optional setup |
| `data/gnome-shell/` | Optional GNOME Shell extension with a Quick Settings brightness slider |
| `crates/lg-buddy/src/sources/desktop/powerdevil.rs` | Independent pull inhibition through PowerDevil's effective screen policy |
| `crates/lg-buddy/src/sources/desktop/wayland.rs` | Native Wayland idle/activity provider |
| `crates/lg-buddy/src/tv.rs` | TV transport boundary and facade |
| `crates/lg-buddy/src/web_os/` | Native webOS client, profile-bound TV adapter, domain operations, and test support |
| `crates/lg-buddy/src/wol.rs` | Native Wake-on-LAN support |
| `crates/lg-buddy-gui/` | GTK 4/libadwaita executable, application lifecycle, and thin presentation renderer |
| `configure.sh` | Interactive configuration tool |
| `install.sh` | Installer for existing runtime and GUI binaries |
| `uninstall.sh` | Uninstaller |
| `scripts/release_bundle_manifest.py` | Release-bundle identity manifest creator and validator |
| `scripts/build-release-bundle.sh` | Release bundle builder |
| `scripts/test-installed-gui.sh` | Installed GTK launcher and removal smoke test |
| `scripts/test-release-gui-behavior.sh` | Display-backed installed GUI behavior smoke with externally observed presentation-state gates |
| `scripts/test-release-gui-accessibility.py` | External AT-SPI presentation-state and accessibility contract probe for the installed GTK GUI |
| `scripts/xwd_mean.py` | Standard-library XWD luminance probe for release theme smoke tests |
| `scripts/test-release-linkage.sh` | Static runtime and GNU GUI linkage baseline check |
| `scripts/test-release-bundle.sh` | Release bundle smoke test |
| `scripts/test-gnome-extension.sh` | GNOME Shell extension metadata, syntax and gjs behavior tests (`scripts/test_gnome_extension.js`) |
| `scripts/test-cross-version-upgrade.sh` | Pinned previous-to-candidate archive upgrade smoke test |
| `scripts/test-production-upgrade-canary.sh` | Post-publication production GitHub upgrade canary |
| `scripts/record_github_release_responses.py` | Sanitized production response recorder for offline mocks |
| `scripts/publish-release-assets.sh` | Verified GitHub release draft staging and publication helper |
| `scripts/release_promotion.py` | Promotion version, branch, and tag validator |
| `.github/workflows/ci.yml` | CI validation workflow |
| `.github/workflows/promotion-pr.yml` | Read-only promotion PR contract check using the target branch's validator |
| `.github/workflows/release.yml` | Post-merge build, draft staging, approval, and publication workflow |
| `bin/LG_Buddy_Common` | Shared shell config helper used by setup scripts |
| `systemd/` | Installed unit files and tmpfiles config, including the logind lifecycle service |
| `docs/architecture-overview.md` | Runtime architecture |
| `docs/defaults-and-configuration.md` | Product defaults and persistent configuration guidance |
| `docs/gamepad-subsystem.md` | Gamepad activity architecture and adapter guidance |
| `docs/gui-target-architecture.md` | Current application-owned presentation and GTK renderer boundary |
| `docs/runtime-event-handler-map.md` | Top-level system, desktop, and runtime event handler map |
| `docs/session-backend-model.md` | Session source semantics, ownership, and observation contract |
| `docs/testing-strategy.md` | Test strategy and scope |
| `docs/webos-testing.md` | Native webOS evidence and mock testing strategy |
