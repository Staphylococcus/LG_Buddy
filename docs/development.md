# Development

This document covers building, local installation, validation, release tooling, and contributor-facing repository details.

## Build Prerequisites

- a Rust toolchain with `cargo`
- a working C toolchain
- `python3-venv`
- `python3-pip`
- `zenity`

Backend-specific tools used in development and local testing:

- `swayidle` for the `swayidle` monitor backend
- readable `/dev/input/event*` devices for local gamepad activity testing
- readable `/dev/hidraw*` devices when testing the Logitech G923 raw HID fallback

For GNOME end-to-end work, the running session also needs the full GNOME contract:

- GNOME Shell
- `org.gnome.ScreenSaver`
- `org.gnome.Mutter.IdleMonitor`

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

The resulting binary will be at:

```text
./target/release/lg-buddy
```

## Install a Locally Built Binary

`install.sh` is installer-only. It does not build the runtime.

To install a binary you built yourself:

```bash
./install.sh --runtime-binary ./target/release/lg-buddy
```

To install from a release bundle instead, extract the archive and run:

```bash
./install.sh
```

## Validation

Useful checks during development:

```bash
cargo test -p lg-buddy --lib
cargo test -p lg-buddy --test cucumber
cargo clippy -p lg-buddy --all-targets --all-features -- -D warnings
bash -n install.sh uninstall.sh configure.sh bin/LG_Buddy_Common scripts/build-release-bundle.sh scripts/test-release-bundle.sh scripts/publish-release-assets.sh
```

Optional hardware smoke for gamepad activity:

```bash
LG_BUDDY_GAMEPAD_SMOKE_SECS=20 cargo test -p lg-buddy --lib \
  session::gamepad::tests::hardware_smoke_reports_real_gamepad_activity \
  -- --ignored --nocapture
```

Run that from a desktop session that has read access to the connected
controllers. The test uses the production gamepad activity source and requires
manual input during the capture window.

## Release Tooling

Build a release bundle locally with:

```bash
./scripts/build-release-bundle.sh --target x86_64-unknown-linux-gnu --version 0.0.0-dev
```

That script expects the matching release binary to already exist under:

```text
./target/<target>/release/lg-buddy
```

Smoke test a generated release bundle with:

```bash
./scripts/test-release-bundle.sh --archive ./dist/lg-buddy-0.0.0-dev-x86_64-unknown-linux-gnu.tar.gz
```

The smoke test unpacks the archive, verifies expected files are present, runs a non-interactive install into a temporary root, and then runs uninstall assertions against that temporary install.

Dry-run the GitHub release publish step with:

```bash
GH_RELEASE_DRY_RUN=1 ./scripts/publish-release-assets.sh --dist-dir ./dist --tag release-0.0.0-dev
```

For the tagged GitHub release process, see [release-process.md](release-process.md).

## Repository Layout

| Path | Purpose |
| --- | --- |
| `crates/lg-buddy/src/lib.rs` | CLI parsing and command dispatch |
| `crates/lg-buddy/src/commands.rs` | Runtime lifecycle and policy commands |
| `crates/lg-buddy/src/session/runner.rs` | Session monitor loop |
| `crates/lg-buddy/src/session/inactivity.rs` | Session inactivity synthesis and thresholds |
| `crates/lg-buddy/src/session/gamepad/` | Gamepad activity discovery, capture, registry, and policy |
| `crates/lg-buddy/src/session_bus.rs` | Generic session-bus transport seam |
| `crates/lg-buddy/src/gnome.rs` | GNOME backend integration |
| `crates/lg-buddy/src/swayidle.rs` | `swayidle` backend integration |
| `crates/lg-buddy/src/tv.rs` | TV transport boundary and facade |
| `crates/lg-buddy/src/wol.rs` | Native Wake-on-LAN support |
| `configure.sh` | Interactive configuration tool |
| `install.sh` | Installer for an existing binary |
| `uninstall.sh` | Uninstaller |
| `scripts/build-release-bundle.sh` | Release bundle builder |
| `scripts/test-release-bundle.sh` | Release bundle smoke test |
| `scripts/publish-release-assets.sh` | GitHub release publish helper |
| `.github/workflows/ci.yml` | CI validation workflow |
| `.github/workflows/release.yml` | Tagged GitHub release workflow |
| `bin/LG_Buddy_Common` | Shared shell config helper used by setup scripts |
| `systemd/` | Installed unit files and tmpfiles config |
| `docs/architecture-overview.md` | Runtime architecture |
| `docs/session-backend-model.md` | Session backend semantics and capability model |
| `docs/testing-strategy.md` | Test strategy and scope |
