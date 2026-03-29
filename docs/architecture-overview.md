# LG Buddy Architecture Overview

This document describes the current architecture on the `rust-poc` branch.

It is not a product roadmap. It is a map of what exists today, how the main pieces fit together, and where the remaining migration boundary still sits.

## Repository Shape

The repository currently contains two runtime worlds:

- legacy shell entrypoints and install flow
  - `bin/`
  - `install.sh`
  - `uninstall.sh`
  - `systemd/`
- new Rust runtime workspace
  - `Cargo.toml`
  - `crates/lg-buddy/`

The Rust runtime is the new application core. The shell layer still exists as the current integration and packaging surface.

## High-Level Runtime Shape

The Rust crate is organized as a small core with explicit boundaries:

```text
main.rs
  -> lib.rs
     -> parse CLI arguments
     -> dispatch command
        -> commands.rs
           -> config.rs
           -> state.rs
           -> tv.rs
           -> wol.rs
           -> backend.rs
           -> session.rs / gnome.rs
```

The intended split is:

- `lib.rs`
  - public entry surface for the binary
  - command parsing
  - shared error types
- `commands.rs`
  - lifecycle policy
  - startup, shutdown, screen-off, screen-on flows
  - orchestration of config, state, TV control, and Wake-on-LAN
- `config.rs`
  - config path resolution
  - parsing of the existing `config.env` format
  - typed values for HDMI input, backend, MAC address, and idle timeout
- `state.rs`
  - runtime directory resolution
  - system/session state separation
  - ownership marker management
- `tv.rs`
  - TV transport abstraction
  - subprocess-backed `bscpylgtvcommand` client
  - typed facade for input, screen, and power operations
- `wol.rs`
  - native Wake-on-LAN packet generation and UDP send
- `backend.rs`
  - backend selection and detection
  - `auto`, `gnome`, and `swayidle` support
- `session.rs`
  - backend-neutral session event model
  - capability surface for desktop backends
- `gnome.rs`
  - GNOME-specific capability probing and event mapping
  - currently an interface skeleton, not the full event loop

## Command Model

The binary currently supports these commands:

- `startup [auto|boot|wake]`
- `shutdown`
- `screen-off`
- `screen-on`
- `detect-backend`

`lib.rs` parses the command line into a typed command enum and dispatches into `commands.rs`.

This keeps CLI parsing separate from operational behavior.

## Core Control Flows

### `screen-off`

`screen-off` is an idle policy action.

Flow:

1. Load config.
2. Resolve the session state marker path.
3. Query the TV's current input.
4. If the configured HDMI input is active:
   - try to blank the screen
   - if blanking fails, fall back to `power_off`
   - create the ownership marker on success
5. If another input is active:
   - clear the marker
   - do nothing to the TV

### `screen-on`

`screen-on` is a resume policy action.

Flow:

1. Load config.
2. Resolve the session marker.
3. Skip if the marker is missing.
4. Skip and clear if the marker is stale.
5. Try `turn_screen_on`.
6. If the TV reports the known active-screen error (`-102`), try immediate input restore.
7. Otherwise fall back to Wake-on-LAN plus repeated `set_input` attempts.
8. Clear the marker on success.
9. Leave the marker in place if wake recovery fails.

### `startup`

`startup` handles both cold-boot and wake restoration behavior.

Flow:

1. Load config.
2. Resolve the system-scope marker.
3. Decide behavior from `StartupMode`:
   - `boot`: always restore
   - `wake`: only restore if LG Buddy owns the marker
   - `auto`: treat marker presence as wake, otherwise boot
4. Clear the marker before attempting restore.
5. Send Wake-on-LAN.
6. Retry `set_input` until the TV is reachable on the configured HDMI input or attempts are exhausted.

### `shutdown`

`shutdown` is a guard-rail policy action.

Flow:

1. Load config.
2. Ask `systemctl list-jobs` whether a reboot is pending.
3. If reboot is pending, skip TV power-off.
4. Otherwise query current input.
5. If the configured HDMI input is active, issue `power_off`.
6. If input query fails, still attempt `power_off`.
7. Power-off failures are logged but do not abort shutdown handling.

### `detect-backend`

`detect-backend` resolves the desktop backend to use.

Selection order:

1. `LG_BUDDY_SCREEN_BACKEND` override if present
2. `screen_backend` from config
3. default to `auto`

Detection behavior:

- `auto` prefers GNOME when `gdbus` is available and GNOME Shell is present
- otherwise falls back to `swayidle` if installed
- forced backends validate required commands

## TV Integration Boundary

The TV layer is intentionally split into two levels:

- low-level transport trait: `TvClient`
- higher-level domain facade: `TvDevice`

`TvClient` models the transport operations that the current backend can actually perform:

- `get_input`
- `set_input`
- `power_off`
- `turn_screen_off`
- `turn_screen_on`

`TvDevice` provides a more readable surface to policy code:

- `tv.input().current()`
- `tv.input().set(...)`
- `tv.screen().blank()`
- `tv.screen().unblank()`
- `tv.power().off()`
- `tv.power().wake(...)`

This keeps the subprocess client simple while giving command logic a typed domain API.

### Transitional Backend

The current production-side TV backend is still `bscpylgtvcommand`.

The Rust runtime talks to it through `BscpylgtvCommandClient`, which:

- shells out to the configured command path
- preserves stdout, stderr, and exit status on failure
- parses `get_input` output into a typed `CurrentInput`

This is a transitional integration boundary. It keeps the runtime architecture independent from the current Python CLI without requiring a native WebOS client yet.

## State Model

State is intentionally small.

The runtime currently uses one ownership marker:

- `screen_off_by_us`

That marker answers one question:

- did LG Buddy blank or power off the TV as part of its own policy?

There are two scopes:

- `System`
  - default path under `/run/lg_buddy`
- `Session`
  - default path under `$XDG_RUNTIME_DIR/lg_buddy`
  - fallback under `/run/user/<uid>/lg_buddy`

This is a direct replacement for the earlier ad hoc script coordination pattern.

## Desktop Backend Strategy

Desktop backends are treated as adapters, not owners of policy.

The runtime core owns:

- config
- state
- TV control
- Wake-on-LAN
- retries and recovery behavior
- lifecycle decisions

Desktop backends should only answer questions like:

- which backend is active?
- which session signals are available?
- how should backend-specific signals map into runtime events?

`session.rs` defines the backend-neutral model:

- `Idle`
- `Active`
- `WakeRequested`
- `UserActivity`

`gnome.rs` is the first native backend slice. It currently provides:

- capability probing
- mapping from GNOME D-Bus monitor lines into `SessionEvent`

The full GNOME session monitor/event loop has not been migrated yet.

`swayidle` remains an external-tool backend by design. The current architecture does not aim to reimplement idle management tools that already solve the right problem.

## Configuration and Override Surface

The runtime is designed to be testable and relocatable.

Important environment overrides:

- `LG_BUDDY_CONFIG`
  - explicit config file path
- `LG_BUDDY_SCREEN_BACKEND`
  - force backend selection
- `LG_BUDDY_BSCPYLGTV_COMMAND`
  - override TV command path
- `LG_BUDDY_SYSTEM_RUNTIME_DIR`
  - override system state directory
- `LG_BUDDY_SESSION_RUNTIME_DIR`
  - override session state directory
- `LG_BUDDY_SYSTEMCTL`
  - override the `systemctl` command path used by shutdown logic

These exist mainly so the runtime can be tested without mutating real system paths or depending on globally installed commands.

## Testing Strategy

The test strategy has three layers:

- unit tests for parsing, state, backend selection, and policy
- subprocess-backed integration tests for TV behavior
- manual hardware probes when exact external behavior is unclear

The important current design choice is that TV-facing tests now run against a stateful subprocess mock rather than an in-memory fake.

Relevant test assets:

- `tools/mock_bscpylgtvcommand.py`
- `crates/lg-buddy/tests/support/mod.rs`
- `crates/lg-buddy/tests/mock_bscpylgtvcommand.rs`

That mock preserves the real command/response shapes we have already observed from the installed TV client, so command-policy tests exercise the same subprocess boundary the runtime uses in production.

## Current Migration Boundary

The Rust runtime now covers the core policy slices that were scoped for the POC:

- config loading
- state handling
- TV abstraction
- Wake-on-LAN
- backend detection
- startup
- shutdown
- screen-off
- screen-on
- GNOME backend skeleton

What is not migrated yet:

- installer and uninstaller logic
- systemd unit migration
- full GNOME monitor/event-loop implementation
- additional desktop backends
- native WebOS transport

So the current architecture should be read as:

- Rust owns the new runtime core
- shell still owns installation and current system integration glue
- the next major architectural step is session-backend execution, not another rewrite of the command layer
