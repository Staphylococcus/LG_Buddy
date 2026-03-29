# Rust Runtime POC Scope

## Purpose

This POC is intended to validate whether LG Buddy should move its runtime logic from shell scripts to a Rust binary while preserving the current user-facing behavior.

The immediate goal is not a full rewrite. The goal is to prove that a Rust runtime can:

- load the existing LG Buddy configuration format
- manage LG Buddy state more cleanly than the current script set
- shell out to `bscpylgtvcommand` as a transitional TV control backend
- support backend-specific session logic without turning into another pile of glue scripts
- be tested in CI with meaningful unit and integration coverage

## Why This POC Exists

The current shell implementation is effective for a narrow automation workflow, but long-term growth will be constrained by:

- duplicated bootstrap and environment detection across multiple scripts
- hardcoded command paths and runtime paths
- control flow mixed directly with side effects
- limited unit-test seams around power, wake, idle, and screen state transitions
- increasing complexity as more desktop environments and companion-app features are added

This POC is a decision point, not a commitment to complete the rewrite.

## POC Goals

The POC should demonstrate the following:

1. A single Rust binary can replace a meaningful slice of the current runtime behavior.
2. The binary can consume the current `config.env` format without requiring users to migrate configuration.
3. TV operations can be routed through `bscpylgtvcommand` cleanly enough to avoid blocking early development.
4. State management can move from ad hoc script coordination to explicit runtime logic.
5. The runtime can be tested without depending on a real TV for every behavior check.

## In Scope

The POC should include:

- a Cargo project for an `lg-buddy` runtime binary
- config loading compatible with the current config file fields:
  - `tv_ip`
  - `tv_mac`
  - `input`
  - `screen_backend`
  - `screen_idle_timeout`
- a small command abstraction around `bscpylgtvcommand`
- a small Wake-on-LAN abstraction
- explicit state-file handling for "screen turned off by us"
- enough subcommands or modes to prove the runtime shape

Recommended initial command set:

- `startup`
- `shutdown`
- `screen-off`
- `screen-on`
- `detect-backend`

Recommended initial backend scope:

- config parsing
- backend detection for `auto`, `gnome`, and `swayidle`
- no full GNOME event loop rewrite in the first POC pass

## Out of Scope

The POC should not attempt all of the following at once:

- rewriting `install.sh` or `uninstall.sh`
- removing the Python dependency
- replacing `bscpylgtvcommand` with a native Rust WebOS client
- shipping KDE, Hyprland, or additional DE integrations in the first pass
- reproducing full LG Companion feature parity
- replacing all existing shell scripts immediately
- packaging, release automation, or distro integration work

## Proposed Architecture

The POC should bias toward a small core with explicit boundaries.

Suggested modules:

- `config`
  - parse existing config file
  - validate values
  - resolve config path
- `state`
  - resolve runtime state directory
  - create, remove, and inspect state files
- `tv`
  - invoke `bscpylgtvcommand`
  - parse command output where needed
- `wol`
  - send Wake-on-LAN through an external command or native packet implementation
- `screen_backend`
  - detect backend
  - keep event-loop logic out of scope for the first pass
- `commands`
  - startup/shutdown/screen-off/screen-on flows

## Desktop Backend Strategy

Desktop-environment integrations should be modular adapters, not separate copies of LG Buddy logic.

The Rust runtime should own:

- config loading and validation
- state-file handling
- TV control
- Wake-on-LAN
- retry and recovery policy
- lifecycle decision logic

Desktop backends should only translate environment-specific signals into runtime events.

Backend categories:

- native API backends
  - use platform APIs directly when the desktop environment itself is the integration surface
- external-tool backends
  - delegate to existing tools when those tools already solve the problem well

Initial direction:

- `gnome` should be treated as a native API backend using D-Bus
- `swayidle` should be treated as an external-tool backend

The medium-term goal is not to reimplement tools like `swayidle` in Rust. The goal is to keep LG Buddy policy and state management in Rust while integrating with the right backend for each desktop environment.

## Compatibility Strategy

The POC should preserve compatibility where it materially reduces migration cost.

Compatibility targets:

- existing config file shape
- existing input naming such as `HDMI_1`
- existing backend names: `auto`, `gnome`, `swayidle`
- existing operational assumptions around state files

It is acceptable for the POC to coexist in the repository with the current shell scripts during development rather than replacing them immediately.

The POC is not intended to be installed or enabled in parallel with the existing shell scripts for the same lifecycle events. The goal is staged migration and behavior comparison, not concurrent runtime ownership.

## Test Strategy

The POC should prove that Rust improves testability in practice, not just in theory.

Minimum expected coverage areas:

- config parsing and validation
- backend detection logic
- state-file transitions
- startup/shutdown/screen-off/screen-on policy logic
- TV client command construction

Testing approach:

- unit tests for parsing and policy logic
- integration tests with stubbed `bscpylgtvcommand`
- no real-TV dependency for normal CI
- optional manual smoke test against a real TV after the POC is functional

## Acceptance Criteria

The POC is successful if:

1. The Rust binary can run the scoped commands against the current config format.
2. The POC removes hardcoded runtime coupling enough to make tests straightforward.
3. Shelling out to `bscpylgtvcommand` is operationally clean and not an immediate architecture blocker.
4. The code structure is clearly more extensible than the current script layout.
5. The repo ends with a credible migration path instead of a dead-end experiment.

## Current Decisions

The current working decisions for the POC are:

- The runtime should be one Rust binary with subcommands. If a long-running session mode is needed later, it should live in the same binary rather than in a separate implementation.
- Wake-on-LAN should be abstracted immediately and is expected to move to a native Rust implementation rather than remain a permanent external dependency.
- `bscpylgtvcommand` should be used as the transitional TV control backend behind a Rust abstraction. It is acceptable for the POC and early migration, but it should not define the final architecture.
- Desktop-environment integrations should be modular backend adapters with a narrow responsibility: translating desktop-specific signals into LG Buddy runtime events.
- GNOME should be the first full native backend rewrite after the POC because it exercises the richest session API surface and best validates the runtime architecture.
- `swayidle` should remain an external-tool backend in the medium term. The Rust runtime should integrate with it rather than reimplement its idle-management behavior.
- Runtime state paths and command paths should be overridable through environment variables or flags for testability and nonstandard environments.

## Recommended Next Step

After this document, the next implementation step should be to scaffold the Cargo project and build the smallest vertical slice:

- load config
- detect backend
- invoke `screen-off` and `screen-on` flows through a stub-friendly TV command layer

That slice is large enough to validate architecture and testing, but still small enough to discard if the direction proves wrong.
