# LG Buddy Session Backend Model

This document defines the target model for desktop session backends.

The goal is to unify providers semantically, not mechanically.

GNOME, `swayidle`, and future backends do not expose the same APIs or the same
event richness. LG Buddy should not force them to look identical at the
transport layer. Instead, the `session` module should define:

- the canonical event meanings LG Buddy cares about
- the capability model for optional behavior
- the ownership model for idle timing

Backend-specific modules should only map their native surface into that shared
contract.

## Design Rules

1. `session` owns semantics.
2. Backend modules own provider-specific mapping.
3. Missing backend capabilities stay missing.
4. LG Buddy does not invent synthetic provider behavior just to fill gaps in the
   interface.

That means a backend can say "I do not emit `WakeRequested`" or "idle timeout is
desktop-managed" without being treated as incomplete.

## Canonical Events

These are the semantic events the runtime should reason about.

| Event | Meaning |
| --- | --- |
| `Idle` | The backend reports the session/display has become idle. |
| `Active` | The backend reports the session/display is active again after an idle period. |
| `WakeRequested` | The backend explicitly requests the display be woken. |
| `UserActivity` | The backend can observe user activity before it emits a normal `Active` transition. |
| `BeforeSleep` | The backend reports that the system is about to suspend. |
| `AfterResume` | The backend reports that the system resumed from suspend. |
| `Lock` | The backend reports that the session should lock or has locked. |
| `Unlock` | The backend reports that the session should unlock or has unlocked. |

### Event Notes

- `Active` and `Unlock` are not the same thing.
  - Some backends can report an active display transition without a session
    unlock event.
- `UserActivity` is earlier and weaker than `Active`.
  - It exists for backends like GNOME + Mutter where LG Buddy may observe fresh
    activity before the desktop emits its normal active/wake signal.
- `WakeRequested` is optional.
  - Some providers expose an explicit wake request.
  - Others only expose idle/resume transitions.

## Capability Model

Backends should advertise what they can actually do.

The target Rust shape is:

```rust
enum IdleTimeoutSource {
    DesktopEnvironment,
    LgBuddyConfigured,
}

struct SessionBackendCapabilities {
    idle_timeout_source: IdleTimeoutSource,
    wake_requested: bool,
    before_sleep: bool,
    after_resume: bool,
    lock_unlock: bool,
    early_user_activity: bool,
}
```

### Capability Meanings

| Capability | Meaning |
| --- | --- |
| `idle_timeout_source` | Who owns the idle timeout policy for this backend. |
| `wake_requested` | Whether the backend can emit `WakeRequested`. |
| `before_sleep` | Whether the backend can emit `BeforeSleep`. |
| `after_resume` | Whether the backend can emit `AfterResume`. |
| `lock_unlock` | Whether the backend can emit `Lock` and `Unlock`. |
| `early_user_activity` | Whether the backend can emit `UserActivity` before `Active`. |

### Idle Timeout Ownership

This needs to be explicit because different providers work differently.

`DesktopEnvironment`
- The compositor or desktop already owns idle timing.
- LG Buddy reacts to the resulting events.
- Example: GNOME.

`LgBuddyConfigured`
- LG Buddy must supply or manage the timeout value.
- The backend tool or adapter consumes that LG Buddy-controlled value.
- Example: `swayidle`.

This is separate from startup and wake retry delays.

Those delays are runtime policy, not session-backend idle policy.

## Provider Map

This is the target mapping for the currently known backends.

| Backend | Idle | Active | WakeRequested | UserActivity | BeforeSleep | AfterResume | Lock/Unlock | Idle Timeout Source |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| GNOME | Yes | Yes | Yes | Yes, when Mutter idle monitor is available | No current surface in LG Buddy | No current surface in LG Buddy | No current surface in LG Buddy | `DesktopEnvironment` |
| `swayidle` | Yes | Yes | No | No direct equivalent | Yes | Yes | Yes, when built with systemd support | `LgBuddyConfigured` |

## Provider-Specific Mapping

### GNOME

Current target mapping:

| Provider surface | Canonical meaning |
| --- | --- |
| `org.gnome.ScreenSaver.ActiveChanged (true,)` | `Idle` |
| `org.gnome.ScreenSaver.ActiveChanged (false,)` | `Active` |
| `org.gnome.ScreenSaver.WakeUpScreen` | `WakeRequested` |
| Mutter idle monitor activity detection | `UserActivity` |

Notes:

- GNOME owns idle timing.
- Mutter support is optional.
- LG Buddy should treat early activity as a capability, not a guarantee.

### `swayidle`

Current target mapping:

| Provider surface | Canonical meaning |
| --- | --- |
| `timeout <n> <cmd>` | `Idle` |
| `resume <cmd>` | `Active` |
| `before-sleep <cmd>` | `BeforeSleep` |
| `after-resume <cmd>` | `AfterResume` |
| `lock <cmd>` | `Lock` |
| `unlock <cmd>` | `Unlock` |

Notes:

- `swayidle` does not provide a clear equivalent of GNOME's `WakeRequested`.
- `swayidle` does not provide a Mutter-style early activity surface.
- LG Buddy owns the configured timeout value for this backend.

## Module Ownership

The intended code split is:

- `crates/lg-buddy/src/session.rs`
  - canonical events
  - capability model
  - backend-neutral traits and errors
- `crates/lg-buddy/src/gnome.rs`
  - GNOME-specific probing and event mapping
- `crates/lg-buddy/src/swayidle.rs`
  - `swayidle`-specific probing and event mapping

This keeps backend-specific details out of runtime policy and prevents each
backend from quietly defining its own semantics.
