# LG Buddy Session Backend Model

This document defines the current desktop session backend model.

The goal is to unify providers semantically, not mechanically.

For the broader map of systemd, lifecycle, desktop, and command-entrypoint
events that consume these semantics, see
[runtime-event-handler-map.md](runtime-event-handler-map.md).

GNOME, native Wayland, `swayidle`, and future backends do not expose the same APIs or the same
event richness. LG Buddy should not force them to look identical at the
transport layer. Instead, the `session` module defines:

- the canonical event meanings LG Buddy cares about
- normalized observations with source identity and observation time

Source modules own their provider-specific connection or process, validation,
polling, and translation into that shared contract. The runner selects sources,
owns worker lifetime and inactivity timing, and dispatches policy.

## Design Rules

1. `session` owns semantics.
2. Source modules own provider-specific runtime mechanics and mapping.
3. Missing provider observations stay missing.
4. LG Buddy does not invent synthetic provider behavior just to fill gaps in the
   interface.
5. Auxiliary input sources belong to the session runtime, not desktop backend
   modules.

That means a source can omit `WakeRequested` without being treated as
incomplete. No source-facing capability object is needed when runtime behavior
does not consume it.

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
  - It exists for native desktop adapters that can expose fresh activity before
    the desktop emits its normal active/wake signal. GNOME + Mutter is the
    current production example.
  - It can also come from auxiliary activity sources owned by the session
    runtime, such as gamepad input that the desktop does not classify as
    activity.
- `WakeRequested` is optional.
  - Some providers expose an explicit wake request.
  - Others only expose idle/resume transitions.
- Lock state is an optional cross-cutting Linux source, not a prerequisite of a
  selected desktop backend. The shared session runtime observes the resolved
  graphical logind session's `LockedHint` when available. Initial or changed
  `true` maps to `Lock`; `false` maps to `Unlock` only after a prior locked
  state. Unlock is informational and never requests screen restore.

## Runtime Contract

Native sources publish activity observations with an `EventSource` and original
observation time. In automatic operation the runner starts GNOME/Mutter and
native Wayland adapters once. Each adapter owns discovery, subscriptions,
validation and reconnection, including interfaces that appear after startup.
Explicit `gnome` and `wayland` configurations restrict activity to that source.
The compatibility `detect-backend` presentation remains until #218; its single
reported value does not select the automatic runtime's source set.

`session/activity.rs` keeps bounded contributions per source and activity
kind. Newer observations replace pending observations of the same kind. Delivery
preserves the original monotonic time without deduplicating across sources or
kinds. The inactivity engine decides how each observation affects policy;
overlapping reports do not extend a deadline beyond their observation times or
repeat an already completed restore. Valid observations survive a later
connection failure; transport loss does not undo activity that already happened.
Connection handles, owner changes and obsolete protocol objects stay private to
each adapter. Input received during setup is delivered immediately without a
separate readiness gate in the collector.

The runner owns one inactivity deadline. Loss of one source leaves the others
running; loss of all native activity sources suspends automatic idle blanking
until an adapter can observe activity again. This policy queries each adapter's
current `ActivityStatus`; that assessment never authorizes or rejects an event.
A quiet usable adapter remains available. Availability, bounded failure reasons
and the last activity time form runtime diagnostics.
Gamepad input, explicit lock and post-blank power-off
remain independent. Reconnection itself does not count as input. Worker shutdown
cancels quiet connections and joins their threads.

`swayidle` still owns its initial timeout and publishes `Idle` and independent
desktop activity to the shared policy. Its existing automatic fallback remains
when no native activity capability is available at startup; #132 removes it.

### Independent inhibition and the blanking gate

An adapter can supply activity, inhibition, or both. Each capability independently
uses push or pull according to its source. Activity observations stay in the
activity path. The native monitor starts GNOME's push inhibition and PowerDevil's
pull inhibition independently of the configured activity sources. Their only
policy meeting point is `can_blank()` at an automatic idle-blanking decision.

The inactivity engine invokes this Boolean gate only when its ordinary activity
deadline is due. A false result leaves the deadline and phase untouched. Explicit
lock, restore/ownership and post-blank power-off retain their existing semantics.
The runner checks deadlines at a bounded 50 ms cadence, so a denied deadline
does not cause a busy loop.

`Inhibition` owns the source sections, honoring preference, release delay and
pending pull work. Push workers live with the facade; one pull worker retains
adapter diagnostic history and services bounded requests. Each attempt has its
own cancellation flag and reply channel. Input, activity availability changes,
logind sleep/resume or owner changes, configuration changes and shutdown discard
affected pending checks. The logind observer supplies cancellation separately
from activity and never duplicates the lifecycle service's TV actions.

`can_blank()` performs no protocol I/O. Pending work returns false for that
attempt; completed failures remain neutral source contributions. Both sections
must allow and the configured idle timeout must have elapsed since the latest
observed release. Release timestamps come from adapters, not from aggregate
Boolean changes, so repeated clear checks, delayed delivery, absence and recovery
do not manufacture releases. Pull-only transitions between checks cannot be
reconstructed. A completed pull answer is consumed once; another attempt needs a
fresh query. Retries are limited to once per second after completion. Push source
changes invalidate pending work; refreshing an unchanged value does not.

`diagnostics()` returns the same evaluation as the most recent Boolean call,
including the preference, evaluated source contributions, pending work and
release deadline. A missing section means it was not evaluated on that call.
Diagnostics are separate from the inactivity engine's decision.

`inhibition.rs` defines `PushInhibitionAdapter`: its worker maintains one source's
state, and `evaluate()` returns a Boolean permission with matching diagnostics
without protocol I/O. `evaluate_push_inhibition()` combines these permissions:
every contributor must allow blanking, and diagnostics retain each result.

GNOME's capability lives in `sources/desktop/gnome/inhibition.rs` and requires
only SessionManager. It subscribes before querying `IsInhibited(8)` and refreshes
that same contribution on `InhibitorAdded`/`InhibitorRemoved`. It validates the
unique owner and reconciles queued changes before publishing a snapshot.
While connected, it also refreshes after 30 seconds without a successful refresh
to correct drift when notifications are missed. Event-driven refreshes restart
that interval. These internal queries update the same maintained contribution;
they do not make GNOME a second pull contributor.

Only observed inhibition denies permission. Startup, absence and source loss
contribute no inhibitor. A healthy quiet subscription and an in-progress refresh
retain the last observed value; a completed read updates it. If reading or
monitoring fails, the adapter drops that source's contribution and retries.
Diagnostics retain the observation time, a bounded failure reason, and the last
observed inhibited-to-clear transition. Source loss and subsequent recovery do
not create an observed release.

`PullInhibitionAdapter::query()` performs a fresh source check on each call.
`evaluate_pull_inhibition()` queries every participating adapter, ANDs their
permissions, and returns the same section result and per-source diagnostic
shape as push evaluation. It runs on a worker; exclusive mutable access orders
requests without a separate request-generation tracker. Cancellation returns
no verdict. A completed result belongs to that attempt and cannot be reused to
authorize a later blank attempt. The facade consumes completions only for the
current attempt and joins the sections.

PowerDevil's capability lives in `sources/desktop/powerdevil.rs`. It opens a
session-bus connection per check, discovers `org.kde.Solid.PowerManagement`
without activating it, and queries `HasInhibition(4)` on
`/org/kde/Solid/PowerManagement/PolicyAgent` using the resolved unique owner.
It rechecks the owner before accepting the answer. No signals or background
worker are required to maintain its state. Cancellation is checked between
method calls and before completion; an in-flight call is bounded by the
transport's one-second timeout. The next request handles discovery/recovery.
Only diagnostic history survives between requests, recording inhibited-to-clear
transitions within the same owner's successful observations. Failed checks,
absence, cancelled in-flight checks and owner replacement break that continuity;
recovery does not manufacture a release. Failures and absence are neutral under
the same observed-inhibition rule as push. GNOME remains one push contribution.

The adapters read no preferences, apply no aggregate release delay, and send no
activity events or TV actions. Their facade owns that reconciliation.

`evaluate_inhibition_preference(&Config)` is a separate, pure section. It reads
the existing effective `screen_honor_idle_inhibitors` value and returns
`bypass_inhibition` with diagnostics identifying the honoring policy. Disabled
honoring (the existing default) returns `true`: bypass source restrictions and
release delay. Enabled honoring returns `false`: the reconciler must evaluate
both. This override must not be ANDed as a third source permission and never
makes a non-idle session eligible to blank. It depends on no source availability,
protocol I/O, activity state or release history. The legacy `swayidle` process
still controls its own initial idle notification and honors its own inhibitors.

CLI and GUI preference edits retain the existing persist-then-restart path for
`LG_Buddy_screen.service`. A successful restart replaces the process and its
pending attempts; the new runtime reads the new configuration. Apply failures
remain reported separately from saved values and use the existing retry path.
The preference evaluator retains no state. The facade consumes the loaded policy
and `configure()` cancels any pending attempt before changing the preference or
release delay. Old completions cannot become authoritative under a new policy.

### PowerDevil route coverage

The query delegates policy to PowerDevil, including activation delays, filtering
and user overrides; LG Buddy does not count requested inhibitors or separately
interpret logind's list. The following routes were traced in upstream source,
not validated in a live Plasma session:

| Application route | Relationship to the effective screen-policy query |
| --- | --- |
| PowerDevil `AddInhibition(4, ...)` | Direct screen-policy contribution; suppressed requests are excluded. [Policy implementation](https://github.com/KDE/powerdevil/blob/c075216f737a47b1979e2c0b679ed597ac8ea131/daemon/powerdevilpolicyagent.cpp#L549-L588) |
| KDE portal `Inhibit` with Idle flag `8` | Translates to PowerDevil policy `4`. Suspend-only flag `4` translates to policy `1`, not screen inhibition. [Portal implementation](https://github.com/KDE/xdg-desktop-portal-kde/blob/9427f3bc8712532f4599dc54b19bd4975609d7f9/src/inhibit.cpp#L165-L182) |
| `org.freedesktop.ScreenSaver.Inhibit` in Plasma | KScreenLocker forwards the request to PowerDevil `AddInhibition(4, ...)`. [KScreenLocker implementation](https://github.com/KDE/kscreenlocker/blob/4f8927000c3f5c5caa52f775580487b5c782132a/interface.cpp#L46-L68) |
| logind `idle` inhibitors | PowerDevil imports qualifying `block` inhibitors, excluding its own; LG Buddy consumes the resulting policy. [Import/filter implementation](https://github.com/KDE/powerdevil/blob/c075216f737a47b1979e2c0b679ed597ac8ea131/daemon/powerdevilpolicyagent.cpp#L423-L501) |
| `org.freedesktop.PowerManagement.Inhibit` | PowerDevil maps this to session interruption policy `1`; it does not by itself inhibit screen policy `4`. [FDO connector](https://github.com/KDE/powerdevil/blob/c075216f737a47b1979e2c0b679ed597ac8ea131/daemon/powerdevilfdoconnector.cpp#L84-L93) |
| Native Wayland idle inhibitor | KWin feeds this into its own input idle-inhibitor set. This is a separate path; PowerDevil coverage is not established. [KWin implementation](https://github.com/KDE/kwin/blob/b3e286c172bb9df7ee8ba8f1ef3a8ca21a8c770f/src/idle_inhibition.cpp#L58-L81) |

These are version-specific source findings, not a promise that every application
uses a covered route. Live Plasma validation and native Wayland coverage remain
open under #216/#223; see [Testing strategy](testing-strategy.md). Idle-notify
remains an activity protocol and is not used as an inhibition query.

## Provider Map

This is the current mapping for the known backends, with implementation status called out explicitly.

| Backend | Idle | Active | WakeRequested | UserActivity | Lock/Unlock | Timing and execution |
| --- | --- | --- | --- | --- | --- | --- |
| GNOME | Observed but not authoritative | Yes | Yes | Yes | Optional logind source | Shared runner owns the configured deadline over ScreenSaver and Mutter observations |
| Native Wayland | Observed but not authoritative | Resumed notification | No | Yes | Optional logind source | Shared runner owns the configured deadline using `ext_idle_notifier_v1` version 2 or newer |
| `swayidle` | Timeout becomes `Idle` | Resume becomes independent desktop activity | No | No direct equivalent | Optional logind source | Source process owns the configured initial timeout; shared runner owns policy and post-blank timing |

## Provider-Specific Mapping

### GNOME

Current mapping:

| Provider surface | Canonical meaning | Current Rust Status |
| --- | --- | --- |
| `org.gnome.ScreenSaver.ActiveChanged (true,)` | Idle observation that cannot bypass LG Buddy's timeout | Implemented |
| `org.gnome.ScreenSaver.ActiveChanged (false,)` | `Active` | Implemented |
| `org.gnome.ScreenSaver.WakeUpScreen` | `WakeRequested` | Implemented |
| Mutter `WatchFired` for the current `AddUserActiveWatch` | `UserActivity` | Implemented |

Notes:

- GNOME activity requires GNOME Shell, `org.gnome.ScreenSaver`, and `org.gnome.Mutter.IdleMonitor`.
- Activity always uses Mutter's one-shot user-active watches, rearmed after each
  signal. SessionManager is neither required nor queried by the activity source.
  Unlike `GetIdletime`, watches do not treat the idle-counter reset on inhibitor
  release as input. When the Mutter owner disappears or changes, the adapter
  reacquires its bus subscriptions and watches internally.
- LG Buddy owns the configured timeout value for this backend.
- LG Buddy owns one inactivity deadline. Desktop, auxiliary, active, and wake
  activity reports reset it; expiry after `screen_idle_timeout` triggers blanking.
- ScreenSaver idle cannot trigger blanking by itself. ScreenSaver active and
  wake signals reset the same LG Buddy deadline and remain restore observations
  evaluated by screen policy.

### Auxiliary Activity Sources

Linux gamepad input is a desktop-independent auxiliary activity source. The
shared session runtime owns its lifecycle and feeds
`UserActivityObserved` into the same inactivity engine as the selected desktop
provider. Resulting runtime events retain the `AuxiliaryInput` source.

The gamepad source owns its device set internally. It performs an initial scan,
refreshes on Linux input-device add, remove, and change events, and periodically
reconciles in case an event is missed. Standard controller input is read from
evdev. Logitech G923 wheel and pedal activity has a narrow raw HID fallback for
hosts where those reports do not appear on the evdev node.

GNOME, native Wayland, and `swayidle` use the shared session runtime. The Wayland
provider owns only its connection, registry, seats, notifications, and activity
facts; it does not acquire gamepad responsibility.

### Session Lock State

Every enabled monitor backend also starts an optional system-bus observer for the
current graphical logind session. Session selection accepts only an active,
local `x11` or `wayland` session in a user class and owned by the current UID.
An explicit `XDG_SESSION_ID` is validated against those rules; without it, LG
Buddy requires exactly one matching session and refuses ambiguous candidates.

The observer resolves logind's current unique bus owner, subscribes to
`org.freedesktop.DBus.Properties.PropertiesChanged` from that owner for the
exact session, and reconciles the initial `LockedHint` before processing changes.
It watches ownership of `org.freedesktop.login1` and repeats session resolution,
subscription, and reconciliation when logind restarts.
A lock enters the existing blanked inactivity state and dispatches
`SessionLocked` from `LinuxLogind`, so configured-input, marker, sleep-phase,
and restore policy remain centralized in `screen.rs`. Unlock performs no screen
action. A lock-triggered blank starts a fixed one-second activity grace period.
Independent desktop or gamepad activity observed before the lock or inside that
period is ignored without canceling the pending timed power-off. At the grace
boundary, the first fresh independent activity can restore the picture while the
lock screen is still shown. The shared policy compares each sample's monotonic
observation time with the lock time, so delayed dispatch does not change the
decision. Provider wake/deactivation signals associated with unlocking do not
restore it; normal inactivity timing resumes from accepted fresh activity.

Known environment support follows whether the desktop or locker maintains
logind's `LockedHint` for the graphical session:

| Environment | Lock observation |
| --- | --- |
| GNOME Shell 40 or newer on Wayland or X11 | Supported |
| KDE Plasma 5.20 or newer on Wayland or X11 | Supported |
| niri built with D-Bus support and a valid `XDG_SESSION_ID` | Supported |
| stock sway with swaylock | Absent by default; `LockedHint` is not maintained |
| Hyprland with hyprlock | Absent by default; `LockedHint` is not maintained |

This behavior is opportunistic. If logind is unavailable or no eligible session
can be resolved, LG Buddy logs a diagnostic and continues ordinary idle/activity
monitoring. If the desktop or locker never updates `LockedHint`, no lock event is
observed and ordinary monitoring likewise continues unchanged. It does not use
desktop-name checks, logind lock-request signals, locker hooks, or a Wayland
session-lock protocol. The logind observer is not a backend eligibility or
selection requirement.

### Native Wayland

The native `wayland` backend requires `ext_idle_notifier_v1` version 2 or newer
and at least one advertised `wl_seat`. It monitors every seat, including
seats that currently advertise no input capabilities, using zero-timeout idle
notifications from `get_input_idle_notification`. Its `resumed` maps to desktop
activity; `idled` remains observational, so only LG Buddy's inactivity deadline
can trigger blanking.

Only `get_input_idle_notification` is used by this activity adapter. Idle-notify
does not supply an inhibition capability; #216 tracks native Wayland inhibition
coverage separately.

Seats are added and removed dynamically. Connection or dispatch loss, removal
of the bound notifier, or removal of the last seat causes the adapter to rebuild
its connection and subscriptions while other adapters keep running. Previously
published observations remain valid. Explicit selection does not enable another native source. Automatic
operation attempts both native interfaces without desktop-name selection.

### `swayidle`

Current mapping:

| Provider surface | Canonical meaning | Current Rust Status |
| --- | --- | --- |
| `timeout <n> <cmd>` | Publish `Idle` to the shared runner | Implemented |
| `resume <cmd>` | Publish independent desktop activity to the shared runner | Implemented |

Notes:

- `swayidle` is deprecated, remains accepted for existing explicit selections,
  and is planned for removal in 2.0.0 after the native provider remains
  field-validated across supported compositors and the 1.x migration window.
- `swayidle` does not provide a clear equivalent of GNOME's `WakeRequested`.
- Its source-owned timeout always honors compositor inhibition, independently
  of `screen.honor_idle_inhibitors`, including when `auto` falls back to it.
  The preference is hidden for explicit `swayidle` selections; this compatibility
  backend does not offer the native default-off behavior.
- `swayidle` does not provide a Mutter-style early activity surface.
- LG Buddy owns the configured timeout value for this backend.
- The shared runner owns lock observation, screen policy, and the post-blank
  power-off deadline. Gamepad activity can cancel that second deadline, but
  does not reset swayidle's source-owned initial timeout.

## Module Ownership

The code split is:

- `crates/lg-buddy/src/session.rs`
  - canonical events
  - normalized source observations
- `crates/lg-buddy/src/session/runner.rs`
  - source selection, worker lifetime, observation multiplexing, shared
    inactivity state, and policy dispatch
- `crates/lg-buddy/src/session/activity.rs`
  - bounded, identified contributions, observation ordering and source diagnostics
- `crates/lg-buddy/src/inhibition.rs`
  - push/pull contracts, source reconciliation, preference, release timing,
    cancellable checks and Boolean gate diagnostics
- `crates/lg-buddy/src/session/actions.rs`
  - action dependency assembly and native TV client ownership across compatible
    events; one-shot commands use the same assembly with a finite lifetime
- `crates/lg-buddy/src/session/gamepad/`
  - desktop-independent auxiliary input discovery and activity observations
- `crates/lg-buddy/src/sources/desktop/gnome.rs`
  - GNOME session-bus connection, subscriptions, owner validation, Mutter
    activity watches, event loop, and observation mapping
- `crates/lg-buddy/src/sources/desktop/gnome/inhibition.rs`
  - independent SessionManager inhibition state, subscriptions and recovery
- `crates/lg-buddy/src/sources/desktop/wayland.rs`
  - native Wayland registry, seat, idle-notification, and activity mapping
- `crates/lg-buddy/src/sources/linux/logind.rs`
  - system lifecycle mapping plus the optional current-session lock observer,
    including bus setup, session resolution, rebinding, and `LockedHint`
    translation
- `crates/lg-buddy/src/sources/desktop/swayidle.rs`
  - production `swayidle` process invocation and timeout/resume fact transport

This keeps backend-specific details out of runtime policy and prevents each
backend from quietly defining its own semantics.
