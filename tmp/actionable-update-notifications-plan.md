# Actionable Update Notifications Plan

Temporary working plan for the update-notification button branch.

## Goal

Make manual update notifications session-owned and actionable.

`lg-buddy updates check --notify` should remain an on-demand CLI check, but the
desktop notification lifecycle should be owned by the already-running
user-session LG Buddy process. That process should send the notification, keep
the notification id and update payload in memory, listen for notification action
signals, and handle user actions.

This avoids a one-shot CLI process waiting around for one notification and
avoids an out-of-band filesystem handoff.

## Current State

- `LG_Buddy_screen.service` runs the long-lived user-session process as
  `lg-buddy monitor`.
- That process already connects to D-Bus as a client/subscriber for desktop
  session integrations.
- LG Buddy does not currently own a well-known session D-Bus name or export an
  LG Buddy object/interface.
- `lg-buddy updates check --notify` is a one-shot CLI command:
  - checks GitHub releases
  - prints the update result
  - sends a passive desktop notification when an update is available
  - keeps no notification state after `Notify` returns
- `notifications.rs` can send passive notifications and serialize action
  buttons, but nothing handles `ActionInvoked` or `NotificationClosed`.

## Locked Design Direction

- Use D-Bus for the CLI-to-session handoff.
- The long-running user-session LG Buddy process owns actionable notification
  lifecycle.
- Do not use a filesystem handoff for pending notification data.
- Do not introduce a per-notification waiter process.
- Keep this branch focused on manual `updates check --notify`; no scheduling or
  timers yet.
- Do not add notification settings, opt-out actions, or repeat-notification
  suppression in this slice.

## Target State Spec

### User-Facing Behavior

`lg-buddy updates check`

- Checks GitHub releases on demand.
- Prints the same update status as today.
- Does not send a desktop notification.

`lg-buddy updates check --notify`

- Checks GitHub releases on demand.
- Prints the same update status as today.
- If no update is available, does not contact the session notification surface.
- If an update is available, asks the running LG Buddy session process to show
  and manage the update notification.
- If the session notification surface is unavailable, fails loudly after printing
  the update result.

The running session process owns delivery:

- If notification actions are supported, send an actionable notification with a
  `View Release` button.
- If notification actions are not supported, send a passive notification with
  the release URL in the body.

### Button Behavior

Actionable notification buttons:

- `View Release`
  - opens the release URL through the system opener
  - clears the pending in-memory notification mapping

Close, expire, or dismiss:

- clears the pending in-memory notification mapping

Notification dispatch failure:

- does not store a pending notification mapping
- returns a clear notification failure to the CLI handoff caller

### Session D-Bus Surface

Tentative naming:

```text
bus name:   io.github.Staphylococcus.LGBuddy
object:     /io/github/Staphylococcus/LGBuddy/Session
interface:  io.github.Staphylococcus.LGBuddy.Session1
method:     ShowUpdateNotification
```

The exact names can change before implementation, but the shape should be:

- one small LG Buddy-owned session interface
- one method for this slice
- typed Rust request/response structs at the internal boundary
- D-Bus serialization kept at the edge

The request should include enough release facts for the session process to own
the notification:

- update check channel: `stable` or `prerelease`
- current version and channel
- latest version and channel
- release URL

The response should distinguish:

- notification sent
- failed, with an explicit error

### Update Cache

No cache shape changes are required in this slice.

The existing update cache remains responsible for GitHub ETag and latest-release
metadata only. Repeat-notification suppression and last-notified metadata should
be introduced with scheduled notifications or notification preferences, not with
manual `updates check --notify`.

### Failure Semantics

- API/check failures stop before notification handoff.
- Result rendering happens before deferred notification/cache failures are
  returned.
- Missing session D-Bus owner for `--notify` is a notification failure, not a
  successful no-op.
- Notification delivery failure is loud and does not store pending notification
  state.
- Release opener failure during `View Release` is loud and should preserve the
  underlying opener error.
- Existing update-cache failures should keep the current update-check semantics:
  they should not prevent notification handoff, but they should still be
  reported loudly after useful work is attempted.

## Non-Goals

- No automatic update installation.
- No scheduled update checks.
- No systemd timer in this branch.
- No release-note rendering.
- No notification settings or opt-out button.
- No repeat-notification suppression.
- No update cache shape change.
- No general daemon/session-process refactor beyond the minimum session D-Bus
  surface needed for update notifications.
- No filesystem handoff for notification request state.
- No per-notification waiter process.

## Deliverable Slices By Subsystem

Each slice should extend behavior without weakening existing behavior. Keep
commits scoped to the subsystem boundary named by the slice.

### Slice 1: Session D-Bus Service Foundation

Boundary: session D-Bus service hosting inside the long-running user-session LG
Buddy process.

Deliverables:

- Add a production LG Buddy session D-Bus service surface.
- Own a well-known session bus name while the user-session process is running.
- Export a minimal session object/interface.
- Add a method dispatch loop that can run as a component of the session process
  without blocking screen monitoring.
- Prefer reusing existing D-Bus infrastructure where practical.
- If `dbus-crossroads` is the cleanest production service implementation,
  promote it from dev-dependency to dependency deliberately.

Tests:

- session service claims the expected well-known bus name on a private bus
- `ShowUpdateNotification` method can be called on a private bus
- malformed request payloads are rejected with clear D-Bus errors
- service startup failure is reported clearly
- existing monitor behavior still starts and processes session events

Acceptance criteria:

- `lg-buddy monitor` remains the installed user service entrypoint for this
  branch.
- There is one LG Buddy-owned session D-Bus surface.
- No update notification logic is required in this slice beyond a stub handler.

### Slice 2: Update Notification Handoff Contract

Boundary: typed request/response contract and CLI-side D-Bus client.

Deliverables:

- Add an internal `UpdateNotificationRequest` containing:
  - selected update check channel
  - current version
  - current release channel
  - latest version
  - latest release channel
  - release URL
- Add an internal `UpdateNotificationOutcome` containing:
  - sent
  - failed
- Serialize the request over the LG Buddy session D-Bus method.
- Parse the response into typed outcomes.
- Keep D-Bus-specific strings and signatures at the edge.

Tests:

- valid request serializes and round-trips through the session method boundary
- invalid channel values are rejected
- invalid semver strings are rejected before the session owner stores state
- invalid or empty URL is rejected
- CLI client reports missing session owner as a clear notification handoff error

Acceptance criteria:

- `updates.rs` can depend on a trait-like notification handoff boundary in
  tests, not on a real session bus.

### Slice 3: Notification Action Signal Handling

Boundary: `notifications.rs` plus D-Bus signal parsing support.

Deliverables:

- Parse Freedesktop notification action signals:
  - `ActionInvoked(notification_id, action_key)`
  - `NotificationClosed(notification_id, reason)`
- Extend bus value support for the D-Bus value shapes used by notification
  signals, especially `u32`.
- Keep action-signal parsing generic; do not hard-code update behavior into
  transport parsing.
- Add a session-owned registry:
  - `notification_id -> pending update notification payload`

Tests:

- parses `ActionInvoked` with id and action key
- parses `NotificationClosed` with id and close reason
- ignores unrelated signals
- ignores unknown notification ids
- close/expire/dismiss removes pending notification state
- handles duplicate/late signals deterministically

Acceptance criteria:

- Passive notification delivery still works.
- Action signal parsing can be tested without a real desktop notification
  server.

### Slice 4: Session-Owned Update Notification Dispatcher

Boundary: session notification owner/orchestrator.

Deliverables:

- Implement `ShowUpdateNotification` in the session process.
- Query notification capabilities.
- If actions are supported, send notification with:
  - `view-release`
- If actions are not supported, send passive body text with the release URL.
- Store the notification id and payload in memory after successful dispatch.
- Handle action outcomes:
  - `view-release` opens the URL
  - close/expire/dismiss removes pending state
- Remove pending notification state after terminal outcome.

Tests:

- supported actions attach the `View Release` button
- unsupported actions send passive body with release URL
- notification send failure returns failure and does not store pending state
- `view-release` invokes opener and clears pending state
- opener failure returns a clear failure
- close/expire/dismiss clears pending state

Acceptance criteria:

- The session process owns notification id mapping.
- No one-shot process waits for notification signals.
- No pending notification state is stored in filesystem handoff files.

### Slice 5: Updates CLI Integration

Boundary: `updates.rs` command orchestration.

Deliverables:

- Keep the public CLI shape:
  - `updates check [--channel stable|prerelease] [--notify]`
- On update available with `--notify`, call the LG Buddy session handoff client.
- Do not call the session surface when no update is available.
- Preserve result output before deferred failures.
- Return nonzero when handoff fails.

Tests:

- plain `updates check` does not hand off
- `--notify` with no update does not hand off
- `--notify` with available update sends one handoff request
- handoff request contains current/latest versions, channels, URL, and selected
  update check channel
- missing session owner returns deferred notification failure after result output
- handoff failure preserves clear error text

Acceptance criteria:

- Existing update API/channel/cache behavior is unchanged.
- Manual check remains useful even when notification delivery fails.

### Slice 6: Documentation And Architecture

Boundary: user-facing docs and architecture docs.

Deliverables:

- Update `docs/user-guide.md` for:
  - `updates check --notify`
  - `View Release` notification action
- Update `docs/architecture-overview.md` for:
  - LG Buddy-owned session D-Bus surface
  - session-owned notification lifecycle
  - update CLI as producer, session process as notification owner
- Keep implementation minutiae out of the main README unless the README already
  references the affected CLI behavior.

Tests:

- documentation examples match actual CLI keys and command shape
- no stale references to passive-only update notifications remain

Acceptance criteria:

- Docs match the implemented behavior.

### Slice 7: End-To-End Validation

Boundary: integration/cucumber coverage where practical.

Deliverables:

- Add private D-Bus test coverage for the session method if practical.
- Add runtime entrypoint coverage for `updates check --notify` handoff behavior.
- Keep tests deterministic; no reliance on the real desktop notification
  daemon, real GitHub API, or real user config/cache.

Validation:

- `cargo fmt --all --check`
- `cargo clippy -p lg-buddy --all-targets --all-features -- -D warnings`
- `cargo test -p lg-buddy`
- targeted cucumber/runtime tests for the new behavior

Acceptance criteria:

- The branch can be reviewed as a complete manual `View Release` notification
  feature, with scheduling, preferences, and opt-out left as deliberate
  follow-ups.

## Follow-Up Candidates

- `Never Notify Again` action.
- `updates.notifications` setting.
- Repeat-notification suppression with last-notified cache metadata.
- Scheduled update checks.

## Open Decisions To Lock Before Implementation

- Exact D-Bus bus/object/interface names.
- Whether to promote `dbus-crossroads` to a production dependency or implement
  the small method surface directly with `dbus`.
- Whether `View Release` uses `xdg-open` specifically and, if so, how we declare
  that runtime dependency.
- Whether successful notification handoff should print an extra CLI line or
  remain silent after the normal update result.
