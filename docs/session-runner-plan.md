# LG Buddy Session Runner Plan

This document describes the target implementation plan for the Rust session
runner.

The goal is to replace the current shell monitor in
`bin/LG_Buddy_Screen_Monitor` with a Rust user-service runtime that consumes the
shared session backend model.

This is not a historical planning note. It describes the active target design.

## Scope

The session runner replaces the current screen monitor behavior only.

It does not replace:

- startup system service wiring
- shutdown system service wiring
- separate sleep/wake system services
- installer or uninstaller flow

Those remain separate concerns until the monitor loop is complete.

## Target Command Surface

The Rust binary should gain a long-running user-service command:

```text
lg-buddy monitor
```

This command will:

1. load config
2. resolve the active desktop backend
3. start the backend runner
4. consume semantic `SessionEvent`s
5. dispatch runtime actions through the Rust policy layer

For delegated backends such as `swayidle`, the binary should also gain a small
helper command:

```text
lg-buddy session-hook <event>
```

That helper exists so an external tool can trigger semantic events without
duplicating policy logic.

## Runner Responsibilities

The session runner owns:

- backend resolution
- backend startup and lifecycle
- event consumption
- event-to-action dispatch
- logging and failure handling inside the user service

The session runner does not own:

- TV policy implementation
- config parsing
- runtime marker semantics
- Wake-on-LAN implementation

Those already exist elsewhere in the Rust runtime and should stay there.

## Action Dispatch

The first runner slice should dispatch these semantic events:

| SessionEvent | Action |
| --- | --- |
| `Idle` | `screen-off` |
| `Active` | `screen-on` |
| `WakeRequested` | `screen-on` |
| `UserActivity` | `screen-on` |

The runner should understand these additional events, but may initially treat
them as logged no-ops:

- `BeforeSleep`
- `AfterResume`
- `Lock`
- `Unlock`

That keeps the first implementation aligned with the current shell monitor,
which only drives idle/resume/wake behavior.

## Backend Boundary

The current `SessionBackend` trait is capability-focused. The runner needs a
second layer that can actually emit events.

The target shape is:

- capability/query layer
- event-source runner layer

Conceptually:

```rust
trait SessionEventSource {
    fn backend(&self) -> ScreenBackend;
    fn capabilities(&self) -> Result<SessionBackendCapabilities, SessionBackendError>;
    fn run(&mut self, sink: &mut dyn SessionEventSink) -> Result<(), SessionRunnerError>;
}
```

The exact API can vary, but the important point is:

- backend modules emit semantic events
- the session runner owns dispatch

## GNOME Runner

GNOME should be the first real runner implementation.

It should replace the GNOME branch of the shell monitor with Rust behavior:

- wait for GNOME Shell availability
- validate `org.gnome.ScreenSaver`
- optionally enable Mutter-based early activity support
- run `gdbus monitor`
- map monitor lines into `SessionEvent`
- dispatch those events into the shared runner

### Early Activity

When Mutter idle monitor support is available, the GNOME runner should also:

- start a short-lived polling task after `Idle`
- query idletime while LG Buddy still owns the session marker
- emit `UserActivity` when fresh activity is observed
- stop polling when:
  - `Active`
  - `WakeRequested`
  - `UserActivity`
  - marker disappears

This directly replaces the current shell-side early activity watcher.

## `swayidle` Runner

`swayidle` should remain a delegated-tool backend.

LG Buddy should not reimplement idle management for wlroots compositors.

Instead, the Rust runner should:

1. build the desired `swayidle` command line from config and capabilities
2. start `swayidle` as a child process
3. configure its hooks to invoke `lg-buddy session-hook <event>`
4. receive those semantic events in the same runtime dispatch path used by GNOME

### Why This Shape

This keeps:

- one policy engine
- one event-dispatch path
- one semantic model

while still letting `swayidle` own actual idle timing and compositor
integration.

## IPC / Hook Delivery

For delegated backends, LG Buddy needs a way to deliver hook events back into the
long-running `monitor` process.

The preferred options are:

1. Unix-domain socket
2. FIFO

The hook helper command should be thin:

- validate event name
- connect to the monitor IPC endpoint
- send the semantic event

The monitor process should reject invalid or out-of-context events rather than
trusting raw hook input.

## Suggested Module Split

The current single-file `session.rs` model should likely grow into:

- `crates/lg-buddy/src/session/mod.rs`
- `crates/lg-buddy/src/session/model.rs`
- `crates/lg-buddy/src/session/runner.rs`

Backend-specific files remain separate:

- `crates/lg-buddy/src/gnome.rs`
- `crates/lg-buddy/src/swayidle.rs`

## Implementation Order

1. Add `monitor` command and runner skeleton.
2. Add backend-neutral event dispatch logic and tests.
3. Implement GNOME runner using `gdbus monitor`.
4. Add a `mock_gdbus` contract mock.
5. Validate GNOME runner behavior with unit and integration tests.
6. Add `session-hook` command and monitor IPC.
7. Implement delegated `swayidle` child launch and hook delivery.
8. Replace `LG_Buddy_screen.service` with the Rust monitor.

## Testing Plan

The runner should be tested in layers.

### Module Behavior

- event-to-action dispatch logic
- GNOME line-to-event parsing
- `swayidle` hook-to-event mapping
- IPC payload validation

### Module Interoperability

- runner + `mock_gdbus`
- runner + `mock_swayidle`
- runner + config/runtime marker handling

### User Needs

- real GNOME session smoke
- real `swayidle` session smoke
- later, a small number of acceptance scenarios if they add value

## Migration Boundary

The cutover should happen in stages.

1. Keep the shell monitor as the installed default while the Rust GNOME runner is
   proven.
2. Switch the user service to Rust once GNOME runner behavior is validated.
3. Add `swayidle` runner support.
4. Remove `bin/LG_Buddy_Screen_Monitor` only after both backend paths are
   covered.

This keeps the migration incremental and reversible.
