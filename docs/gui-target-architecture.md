# LG Buddy GUI Target Architecture

This document defines the target frontend architecture for the first-party
Linux GUI. It anchors the brightness MVP in
[#127](https://github.com/Staphylococcus/LG_Buddy/issues/127) and the later GUI
increments under
[#22](https://github.com/Staphylococcus/LG_Buddy/issues/22).

This is a target-state document, not a description of the current Zenity
implementation. For the architecture that exists today, see
[Architecture overview](architecture-overview.md).

## Decisions

1. GTK 4 is the first-party renderer.
2. The application owns one typed, toolkit-neutral declaration for each
   screen and accepts semantic user intents in return.
3. The GTK layer contains no business logic or consequential effects.
4. The GUI calls the Rust application in-process. It does not communicate
   through CLI output, a daemon, HTTP, or a serialized UI protocol.
5. CLI and service paths remain headless and do not link GTK.
6. GTK uses standard controls, system typography, system colors, native
   focus behavior, and accessibility semantics with minimal custom styling.
7. The declaration vocabulary stays small and specific to LG Buddy. It is not
   a general-purpose widget toolkit.

The central distinction is between declaring what the user can currently see
and do, and deciding what those actions mean. The application owns both the
declaration and the meaning. GTK only realizes the declaration using native
widgets and translates widget events back into semantic intents.

## Migration Baseline

The current `brightness` prompt path in `commands.rs` is a useful behavioral
baseline, but not the target boundary:

| Current implementation | Target state |
| --- | --- |
| `BrightnessUi` exposes one blocking prompt and an error dialog | The application publishes state and accepts intents over time |
| `ZenityBrightnessUi` shells out to `zenity` | GTK renders typed application declarations |
| `CurrentExeBrightnessCli` shells back into `lg-buddy brightness get/set` | The GUI process calls an in-process brightness application operation |
| The prompt wrapper owns reachability, read fallback, notifications, and orchestration together | Those decisions live in an explicit application flow behind the presentation contract |
| Tests fake the prompt, nested CLI, ping, and notification collaborators | Application tests drive state and operations directly; renderer tests consume presentation fixtures |
| Cucumber substitutes a Zenity executable | A thin display-backed smoke covers the real GTK launch boundary |

The migration should preserve observable product behavior unless the MVP issue
explicitly changes it. It should not preserve the subprocess structure merely
because current tests encode that structure.

## Target Boundary

```mermaid
flowchart LR
    ENTRY["Desktop entry or GUI launcher"] --> COMPOSE["lg-buddy-gui<br/>composition root"]

    subgraph Frontend["GTK frontend"]
        RENDER["GTK renderer<br/>widgets, focus, layout, accessibility"]
    end

    subgraph Contract["Application-owned presentation contract"]
        VIEW["Typed presentation state"]
        INTENT["Semantic user intents"]
    end

    subgraph Application["LG Buddy application"]
        FLOW["Brightness flow<br/>state transitions and effect decisions"]
        OPS["Brightness operations<br/>config, TV access, notifications"]
    end

    subgraph Domain["Existing domain and adapters"]
        TV["TvDevice picture API"]
        WEBOS["Selected TV adapter"]
    end

    FLOW --> VIEW --> RENDER
    RENDER --> INTENT --> FLOW
    FLOW --> OPS --> TV --> WEBOS
    COMPOSE --> RENDER
    COMPOSE --> FLOW
```

The arrows define the dependency direction:

- presentation types belong to the application, not GTK
- GTK depends on those types
- application and domain code do not depend on GTK
- GTK does not call TV, config, settings, notification, or service modules
- the composition root may construct both sides but contains no product
  decisions

## Target Repository Shape

The smallest useful compile-time boundary is a separate GUI binary crate:

```text
crates/lg-buddy/
  src/
    presentation/
      mod.rs
      brightness.rs
    ...existing application, domain, and adapter modules...

crates/lg-buddy-gui/
  Cargo.toml
  src/
    lib.rs
    main.rs
    brightness.rs
```

`crates/lg-buddy` remains the GTK-free library and CLI/service binary. It owns
the presentation contracts, state transitions, dependency construction, and
operations. Its normal tests must continue to build on a host without GTK
development packages.

`crates/lg-buddy-gui` owns the libadwaita application and window shell, GTK
widgets, renderer, and main-loop bridge. Libadwaita supplies the native GNOME
appearance and system color-scheme integration; product presentation remains
application-owned. It depends on `lg-buddy`, GTK, and libadwaita, never the
reverse. The GUI crate should consume one public application entrypoint rather
than assembling TV or configuration dependencies itself.

The installed graphical executable is `lg-buddy-gui`. The desktop entry runs
`/usr/bin/lg-buddy` with no arguments; the runtime locates the matching GUI
beside it and launches `lg-buddy-gui` with no arguments for normal Overview.
`lg-buddy brightness` remains a brightness-focused deep link: it launches
`lg-buddy-gui brightness` and selects the brightness control even when another
view is already open. `lg-buddy brightness get` and `lg-buddy brightness set`
remain direct headless commands and never inspect or launch the GUI. During the
compatibility window, only an absent GUI on the `brightness` path falls back to
the retained Zenity flow; a plain no-argument launch requires the GUI. A present
but invalid GUI installation, or a GUI process that starts and fails, is
reported directly without opening Zenity or performing a second TV operation.
That launcher handoff does not become the frontend/backend contract: once
`lg-buddy-gui` starts, GTK and the application communicate only through
in-process Rust types.

This split also keeps GTK runtime linkage out of systemd services and the
headless CLI. Release bundles and packages must ship the GUI executable and
declare its real GTK runtime dependencies separately from the existing CLI
binary.

The headless binary retains its static `x86_64-unknown-linux-musl` release
target. The GTK binary is a separate dynamically linked
`x86_64-unknown-linux-gnu` artifact. Ubuntu 24.04 is the oldest release-bundle
build and runtime baseline: GTK 4.14, libadwaita 1.5, and GLIBC 2.39. The source
contract remains limited to GTK 4.10 APIs and libadwaita 1.5, but compatibility
below the tested bundle baseline is not claimed. Fedora 43 and current Arch
validate the same built artifact on newer supported userspaces. The release
manifest and embedded ELF identities verify both artifacts, so the GUI does
not force the service and CLI binary to adopt its linkage model.

## Presentation Contract

### Screen-specific declarations

The contract starts with concrete screen models. The brightness MVP should not
begin with a generic tree of rows, widgets, properties, callbacks, or stringly
typed component names.

A representative contract shape is:

```rust
pub struct BrightnessPresentation {
    pub title: String,
    pub status: BrightnessStatus,
    pub control: Option<BrightnessControl>,
    pub primary_action: ActionPresentation,
    pub cancel_action: ActionPresentation,
}

pub struct BrightnessControl {
    pub label: String,
    pub current: OledBrightness,
    pub proposed: OledBrightness,
    pub minimum: u8,
    pub maximum: u8,
    pub step: u8,
    pub enabled: bool,
}

pub enum BrightnessStatus {
    Loading,
    Ready,
    Applying,
    Failed(UserFacingError),
}

pub struct ActionPresentation {
    pub label: String,
    pub enabled: bool,
    pub intent: BrightnessIntent,
}

pub struct UserFacingError {
    pub summary: String,
    pub detail: Option<String>,
}
```

This is semantic data, not a GTK widget tree:

- `BrightnessControl` means “let the user propose a bounded brightness value,”
  not “construct this exact slider with these pixels.”
- each action declares its intent and availability without choosing a GTK
  widget hierarchy or asking the renderer to infer what a label means.
- `BrightnessStatus` tells the renderer what state exists without exposing a
  transport error or asking the renderer to infer policy.
- copy is plain text. GTK markup and widget-specific properties do not cross
  the boundary.

The renderer chooses the standard GTK representation for each semantic role.
Shared presentation primitives should be extracted only after another screen
needs the same semantics. Similar appearance alone is not enough reason to
create a generic abstraction.

### Semantic intents

GTK returns only intents that express what the user requested:

```rust
pub enum BrightnessIntent {
    Propose(u8),
    Apply,
    Retry,
    Cancel,
}
```

`Propose` carries the raw bounded-control value so the application remains the
only layer that validates it into `OledBrightness`. The renderer does not turn
`Apply` into a TV call, decide whether retry is allowed, or close the window
because a callback happened to succeed. The application handles the intent and
publishes the next presentation or an explicit close outcome.

Window-close requests map to `Cancel`. Programmatic widget changes must not
create accidental user intents. The GTK adapter may suppress signal feedback
while applying a presentation; that is rendering mechanics, not business
logic.

### Application outcomes

The application publishes a closed set of outcomes to the host:

```rust
pub enum BrightnessFrontendUpdate {
    Present(BrightnessPresentation),
    Close,
}
```

The contract is internal and typed. It is not serialized or independently
versioned. The backend and frontend change atomically in the workspace, and
the Rust compiler enforces contract compatibility.

If a later requirement needs an external process boundary, that is a separate
architecture decision. It must not be anticipated by adding identifiers,
schema versions, JSON, or transport errors to this contract.

## Application Ownership

The brightness application flow owns:

- configuration loading and validation
- construction of the selected TV client
- reachability policy, if retained
- reading and validating the current brightness
- fallback or recovery behavior when the read fails
- the proposed value and whether Apply is available
- the loading, ready, applying, failed, and completed transitions
- prevention of duplicate or stale operations
- cancellation semantics
- error normalization and recovery actions
- the TV write and its postcondition behavior
- success or failure notifications
- diagnostics and exit status

The GTK layer owns only:

- selecting standard GTK widgets for the declared semantic roles
- widget creation, placement, sizing, and responsive layout
- rendering application-provided text and state
- focus order, keyboard accelerators, and mnemonic wiring
- accessibility roles, labels, descriptions, and relationships
- routing widget signals to semantic intents
- presenting or closing the window when instructed
- respecting system font, scale, color, and theme settings

GTK callbacks must not:

- parse configuration or command output
- construct a TV client or call `TvDevice`
- perform a ping or other reachability check
- validate, clamp, or silently replace a brightness value
- decide when an action is enabled
- translate transport failures into user messages
- retry, notify, persist, log product outcomes, or control services
- branch on domain errors to choose the next workflow state

Simple renderer assertions that protect toolkit invariants are allowed. For
example, receiving an invalid declared range should fail a renderer test rather
than be repaired with a second set of product rules.

## Brightness Flow

The application flow is an explicit state machine even if its implementation
remains small:

| Current state | Input | Application responsibility | Next presentation |
| --- | --- | --- | --- |
| Opening | application start | begin the current-value operation | Loading |
| Loading | read succeeds | store current and proposed value | Ready |
| Loading | read fails | apply the defined recovery or fallback policy | Ready or Failed |
| Ready | `Propose(value)` | validate and store the proposal | Ready |
| Ready | `Apply` | capture the proposal and start one write | Applying |
| Applying | write succeeds | record success and complete notification policy | Close or completed state |
| Applying | write fails | normalize the failure and expose recovery | Failed |
| Failed | `Retry` | retry the application-defined operation | Loading or Applying |
| Any open state | `Cancel` | cancel or detach safely without writing new state | Close |

The exact current product behavior should be preserved while moving it behind
this boundary unless #127 explicitly changes it. In particular, cancellation
must not write TV state, and `brightness get` and `brightness set` retain their
existing CLI contracts. Existing reachability, read-fallback, and notification
behavior must be treated as application policy during migration, never copied
into GTK.

An operation result is accepted only for the operation instance that is still
current. A late completion after cancel, retry, or shutdown cannot reopen the
window, overwrite a newer proposal, or report success for the wrong request.

## Main Loop And Blocking Work

GTK objects stay on the GTK main thread. TV discovery, connection, pairing,
reads, writes, subprocess compatibility calls, and network checks never run in
a GTK signal callback or otherwise block the main loop. This follows GTK's
[threading model](https://docs.gtk.org/gtk4/section-threading.html).

The target event path is:

1. A GTK signal is translated into a `BrightnessIntent`.
2. The application accepts or rejects the intent from its current state.
3. Any blocking application effect runs on a worker owned by the application
   host.
4. Its typed completion returns to the application state machine.
5. The application publishes a new `BrightnessFrontendUpdate`.
6. The GTK main loop renders that update.

The chosen channel or executor is an implementation detail. It must provide a
bounded, shutdown-safe path and must not leak GLib or GTK types into
`crates/lg-buddy`. Only one brightness effect is in flight at a time. The
application remains authoritative even when the renderer has already disabled
a button.

## GTK Rendering Rules

The MVP should look like a normal GTK utility rather than introduce an LG
Buddy-specific widget language or theme.

| Declared meaning | GTK responsibility |
| --- | --- |
| Screen title | Application window title and visible heading where appropriate |
| Brightness percentage | Standard bounded adjustment control with a visible value |
| Loading or applying | Standard busy indication and insensitive affected controls |
| Primary action | Standard button using the declared label and enabled state |
| Cancel | Standard secondary action and window-close behavior |
| Failure | Standard inline error/status presentation with declared recovery action |

Renderer rules:

- use GTK widgets before custom widgets
- use natural sizing and standard spacing rather than fixed pixel layouts
- preserve visible labels and logical focus order
- make the full flow keyboard-operable
- expose accessible names, descriptions, values, and relationships
- do not encode state using color alone
- follow the active system theme and scaling
- avoid custom CSS unless a concrete GTK limitation requires it
- keep platform chrome, focus visuals, animation, and control behavior under
  GTK ownership

The minimum GTK API level must be selected from the oldest supported Linux
distribution baseline, not from the newest API available on a development
machine. Raising that baseline belongs with packaging validation.

## Error, Cancellation, And Shutdown Semantics

Domain and adapter errors remain typed inside the application. Before a failure
crosses the presentation boundary, the application converts it into safe,
actionable text and declares which recovery intents are available. The
renderer never displays debug representations or searches error strings.

Secrets, access tokens, and unredacted protocol payloads must not enter a
presentation type. Detailed diagnostics may be logged through the existing
application diagnostics path, while the presentation receives only the detail
needed by the user.

Closing the window emits `Cancel`; it is not permission for the renderer to
kill a worker or assume that an operation was undone. The application decides
whether a pending operation can be cancelled, must be detached, or has already
completed. Shutdown closes intent/update channels cleanly, ignores obsolete
completions, and never leaves a GTK callback waiting for a worker.

Failure to initialize GTK or connect to a graphical session is a launcher
failure. It should produce a concise diagnostic and nonzero exit status without
changing the headless CLI behavior.

## Contract Testing Strategy

The GUI follows the repository's three-layer
[testing strategy](testing-strategy.md). The majority of behavior remains
testable without GTK.

### 1. Module behavior: application presentation and state

Pure or narrowly injected tests in `crates/lg-buddy` cover:

- the initial Loading declaration
- successful current-value loading
- the defined read-failure recovery or fallback
- proposal changes and Apply availability
- invalid values being rejected before presentation
- Apply producing exactly one operation
- duplicate Apply being ignored while busy
- success, write failure, retry, and cancellation transitions
- late operation completions being ignored
- safe user-facing error normalization
- GTK-free construction and equality of every presentation state

These tests assert semantic state and emitted effects, not widget classes,
pixels, screenshots, or callback order.

### 2. Module interoperability: application operations and renderer contract

Application integration tests use injected brightness operations and the
existing TV test boundaries to prove that intents reach the real application
path without invoking the CLI as a subprocess. They cover configuration,
selected TV adapters, pairing/recovery, current-value reads, writes,
notifications, and representative failures at the abstraction that owns each
behavior.

The GTK crate has a reusable renderer contract suite. It feeds representative
`BrightnessPresentation` fixtures into the renderer and observes the semantic
surface:

- the expected controls, labels, values, status, and enabled states exist
- focus order and keyboard activation are correct
- accessible roles, names, values, and descriptions are present
- widget signals emit exactly the corresponding `BrightnessIntent`
- applying a new presentation does not emit accidental intents
- busy, failure, retry, scaling, and light/dark theme states remain usable

Renderer tests use application-owned fixtures; they do not rebuild the state
machine in a GTK fake. A display-backed CI lane may provide the GTK environment,
but TV and network dependencies remain mocked at their existing boundaries.

### 3. User needs: thin graphical journey

A small acceptance layer proves only the user-visible boundary:

- the desktop entry opens normal Overview without a terminal
- `lg-buddy brightness` focuses the brightness control, including from another
  selected view
- the current value becomes visible
- changing and applying a value reaches the application once
- cancellation performs no write
- an unreachable TV or failed write leaves actionable feedback
- the window remains responsive during blocking TV work

Installed-GUI smoke proves that both executables and the desktop entry are
installed together, the no-argument launcher opens normal Overview without a
terminal, the brightness deep link selects its control, and removal preserves
user state. A missing GUI fails the plain launcher while retaining the Zenity
fallback for `brightness`. Release-bundle smoke separately proves that the
distributed archive contains both executables and declares the required GTK
runtime dependencies. Neither layer should duplicate the application
state-machine matrix.

Screenshots may support design review, but they are not the primary contract:
system themes, fonts, and rendering legitimately vary. Automated assertions
should prefer semantic controls, accessibility state, and user intents.

Real TV testing remains targeted. It is required only when a change claims
different visible TV behavior or when the existing mock contract is unclear;
ordinary renderer work must not require hardware.

### Contract matrix

| Contract | Owner | Primary proof |
| --- | --- | --- |
| Presentation and intent semantics | `lg-buddy` application | GTK-free module tests |
| State transitions and effect decisions | `lg-buddy` application | Pure/injected state-machine tests |
| TV operation behavior | Existing TV domain and adapters | Existing unit, protocol, and characterization tests |
| Application-to-operation wiring | `lg-buddy` application | Integration tests with injected dependencies |
| Semantic declaration to GTK mapping | `lg-buddy-gui` renderer | Reusable renderer contract suite |
| Desktop launch and runtime dependencies | Packaging/release surface | Display-backed bundle smoke |
| Visible TV outcome | Product boundary | Selected acceptance and hardware checks |

No test should need to mock a contract below the layer under test when a
shared repository harness already represents that boundary.

## Implementation Method

The MVP moves toward the target in independently reviewable, observable
slices. Implementation details remain acceptance criteria within the slice
that first needs them:

1. [#140](https://github.com/Staphylococcus/LG_Buddy/issues/140) opens the GTK
   window from an application-owned Loading declaration and establishes the
   crate, renderer, lifecycle, and display-backed test boundaries.
2. [#141](https://github.com/Staphylococcus/LG_Buddy/issues/141) retrieves and
   displays the current brightness, establishing backend-to-frontend state
   flow and the non-blocking operation boundary.
3. [#142](https://github.com/Staphylococcus/LG_Buddy/issues/142) lets the user
   apply brightness, establishing semantic intents and frontend-to-backend
   state flow.
4. [#143](https://github.com/Staphylococcus/LG_Buddy/issues/143) routes the
   existing desktop and interactive CLI touchpoints to the GTK window.
5. [#144](https://github.com/Staphylococcus/LG_Buddy/issues/144) integrates the
   GUI with install, upgrade, and removal behavior.
6. [#145](https://github.com/Staphylococcus/LG_Buddy/issues/145) ships the GUI
   in release bundles and adds release-artifact smoke coverage.

Each slice must leave the existing `brightness get`, `brightness set`, service,
and compatibility paths green. The Zenity implementation remains available in
the v1.5.0 slice; removing it is tracked separately by
[#130](https://github.com/Staphylococcus/LG_Buddy/issues/130).

## Overview Increment

[#172](https://github.com/Staphylococcus/LG_Buddy/issues/172) replaces the
brightness-specific root with an application-owned `OverviewPresentation`.
Overview is a single view showing the primary TV summary, brightness, volume,
and mute. Navigation, pairing, profile management, and behavior settings belong
to later slices; this increment adds no hidden navigation shell.

The stable `lg-buddy brightness` path opens Overview focused on brightness.
Two compact rows contain a brightness icon and slider, and a mute button and
volume slider. Moving a slider submits its value; the sound icon toggles mute
and reflects mute state. There are no Apply buttons or duplicate value labels.
Changes keep the window open. Volume changes use the same set-then-unmute
operation as the headless command. If unmuting fails after the volume changed,
the presentation retains the changed level and offers recovery
for the remaining mute operation.

The core owns capability-specific loading, busy, success, and failure state.
One failed read cannot remove controls whose state is already available.
Unknown numeric volume is represented explicitly while mute remains usable.
The TV summary shows a dot and connection label: green Connected, yellow
Connecting while reads are pending, or red Disconnected. Individual control
errors retain their own recovery feedback. Native reads and writes use stored
credentials without opening pairing prompts.

Configuration and TV work run on workers. Sliders stay adjustable while writes
are pending, with the latest requested value retained for the next write.
The core accepts a completion only for its active operation; closing or
shutting down invalidates pending results.
Closing does not undo a write already dispatched, and the GUI host lets that
write finish without reopening the view.

Headless tests own operation and recovery policy. GTK tests cover the expanded
mapping and main-loop bridge; installed-GUI smoke exercises keyboard control,
independent errors, the open-after-success behavior, and accessibility. CLI and
service paths remain GTK-free.

## TVs and Navigation Increment

[#173](https://github.com/Staphylococcus/LG_Buddy/issues/173) adds TVs as the
second destination alongside Overview. The application owns the available
destinations, selected destination, TV collection, selected TV, and local
profile state. GTK maps these to a native `AdwViewSwitcher` and `AdwViewStack`,
moving navigation to `AdwViewSwitcherBar` at narrow widths. Switching views
preserves Overview controls and pending operations; late results cannot steal
focus from TVs.

TVs reads the existing primary profile through the settings and credential
adapters on a worker. Missing TV configuration produces an explanatory blank
state with a standard symbolic display icon. Invalid or incomplete configuration
remains an error. One configured TV opens directly to its details without a
sidebar or add-TV action. Credential labels describe local observations; a
stored token or legacy file does not establish authenticated access to a TV.
After local details are available, a separate bounded read retrieves `modelName`
from the TV's system information. The application uses it as the display name;
unavailable or invalid responses retain the profile-name fallback. Late results
cannot replace a different selection or a closed view. This does not persist a
new name or turn local credential metadata into a pairing-health check.

The renderer accepts multi-profile fixtures and shows an adaptive
`AdwNavigationSplitView` only for more than one presented TV. Selection remains
an application intent. Production storage still yields zero or one primary
profile; this increment adds no schema, pairing, repair, or configuration writes.

## First-TV Pairing Increment

[#174](https://github.com/Staphylococcus/LG_Buddy/issues/174) adds **Pair a TV**
only to the zero-TV blank state. It opens an adaptive `AdwDialog` with a grouped
form for the IPv4 address, MAC address, and managed HDMI input; this flow uses
native webOS. Its action-dialog header contains Cancel, a centered title, and
Pair; it has no separate close button or footer actions. The dialog keeps
navigation behind the modal, adapts to smaller windows, and routes Cancel,
Escape, and native dismissal through the same application intent. Successful
pairing closes the dialog to reveal TV details and shows a “TV paired successfully”
toast after verification and saving.
During pairing, a thin progress bar beneath the header shows completed workflow
phases: connecting at 0%, TV confirmation at 25%, verification at 50%, and saving
at 75%. These milestones are not time estimates; success closes the dialog.
The form and Pair button stay disabled until the attempt ends. General failures
produce a toast inside the dialog; recovery guidance remains visible in the
form. Editing, retrying, or dismissing the form clears the failure toast.
Validation errors remain inline and do not generate toasts.
The setup guidance marks TV On With Mobile / Wake-on-LAN as required, and a
static IP address and Always Ready as strongly recommended. The application
owns field validation, connecting, confirmation guidance, capability verification,
failure recovery, and cancellation. GTK declares no network or persistence
policy and never shells out to the CLI.

The foreground application backend pairs into memory, then chooses and sequences
power, audio, and OLED brightness verification before saving. The webOS client
provides authentication and cancellable typed reads; it does not decide which
capabilities the pairing workflow requires. Both pairing and profile persistence refuse
root execution. The existing `tvs/primary/access-token.json` location is used,
and the configuration file is published atomically after the token. A failed
configuration save restores the prior credential state. A process crash between
the two publications can leave an orphan token, but no partial TV configuration;
a subsequent pairing attempt replaces that orphan after verification.

Cancellation is accepted until saving begins. Operation identities reject late
progress and results from cancelled attempts. Once publication starts, dialog
dismissal is disabled; the worker is allowed to finish on application
window close. Success shows the new TV details and reloads Overview without
accepting its earlier empty-state
results. A toolkit-independent application coordinator owns this cross-view
refresh and coordinates application close with pairing cancellation. GTK only
executes declared operations and renders updates. An unexpected worker exit is
reported to the coordinator as an internal failure, not as a TV connection error.
No second-TV flow, discovery, legacy-platform pairing, or service setup
is included.

Headless workflow tests cover validation, confirmation, rejection, timeout,
verification failure, cancellation, and persistence. Protocol fixtures exercise
the native webOS exchange; GTK fixtures cover the form and intent mapping, and
the installed accessibility check covers the blank-state CTA, modal form,
validation feedback, and dismissal through Escape and the header's Cancel button.

## Settings Editing Increment

[#176](https://github.com/Staphylococcus/LG_Buddy/issues/176) adds **Settings**
as the third destination in development. It was outside the released
`1.6.0-beta.1` scope, which remains Overview, TVs, and first-TV pairing.
[#177](https://github.com/Staphylococcus/LG_Buddy/issues/177) extends that view
with editing while preserving the same application-owned boundary.

`SettingsApplication` owns loading, failure, retry, refresh on entry, and
mutation state. Its backend reads and writes `SettingsStore` on worker threads;
operation identities reject stale completions and results after close.
Configuration reads and writes do not depend on a configured or reachable TV.
Settings writes are serialized with TV management so concurrent operations
cannot overwrite each other's configuration snapshots.

The application builds three groups—Screen, Sleep & Wake, and Updates—from
seven behavior settings. Descriptions, defaults, accepted values, and
effective-value sources come from the existing registry and store. GTK shows
the registry descriptions and typed feedback; application code owns metadata
validation and feedback decisions. Invalid values remain invalid rather than
silently defaulting. The rows declare native editors and commit policy:
toggles and bounded choices use
`OnChange`, while the numeric idle timeout uses `OnFinalize` (Enter or focus
loss). The view has no detail accordion, reset action, Save button, or Cancel
workflow.

GTK maps the presentation to a native `AdwPreferencesPage`, preference groups,
direct `AdwSwitchRow`, `AdwComboRow`, and `AdwActionRow` editors. It renders
values through those native controls, plus error/warning feedback and retry
actions when needed. Controls remain enabled during ordinary edits; the
application queues accepted changes and saves them in order, including when
the window closes. GTK receives completion and error results, without rendering
intermediate mutation stages or moving focus. Unchanged rows are left alone. GTK does not parse configuration, call
the settings CLI, or implement validation or service policy. A shared typed settings
executor is used by both CLI and GUI to validate, persist, and apply mutations.
A validation or persistence failure restores the previous row value. A
successful save followed by an apply failure keeps the saved value, shows a
warning, and offers **Retry apply**. Missing or inactive user units are
reported precisely; the workflow does not attempt privileged repair.

Headless tests cover registry/store presentation and the shared mutation
executor, including validation, persistence, apply failure, and retry. Renderer
tests cover native editors, commit timing, choice-menu interaction, feedback,
and narrow layout. The installed AT-SPI smoke verifies the third tab, direct
keyboard access to editors, numeric draft and Enter validation, externally
changed configuration, and preservation of the settings file.

## Installed Overview Entrypoint Increment

[#178](https://github.com/Staphylococcus/LG_Buddy/issues/178) makes the normal
installed application entrypoint the no-argument `lg-buddy` command. The desktop
entry therefore runs `/usr/bin/lg-buddy`, which launches `lg-buddy-gui` with no
arguments and opens normal Overview. Direct `lg-buddy-gui` invocation with no
arguments has the same behavior. `lg-buddy brightness` remains the focused
brightness deep link and must reselect brightness from another view. Only that
deep link keeps the missing-GUI Zenity fallback; the plain app launch requires
the GUI. `lg-buddy --help` and `lg-buddy help` remain CLI help, and existing
headless CLI, service, and update paths remain unchanged.

This is an unreleased entrypoint change. The released `1.6.0-beta.1` desktop
entry still invokes `brightness`. The complete GUI first-run, service, and
update journey remains tracked in
[#129](https://github.com/Staphylococcus/LG_Buddy/issues/129).

## Evolution Rules

Later GUI areas follow the same method:

- add a screen-specific application declaration and semantic intents
- keep navigation and multi-step workflow state in the application
- reuse domain operations rather than CLI strings
- add GTK-free contract/state tests first
- implement GTK mapping and its renderer contract tests second
- extract a shared semantic presentation type only after genuine reuse appears
- change the application contract and every renderer atomically

A frontend change that requires GTK to understand config keys, TV transports,
service commands, update rules, or migration policy indicates that the
application contract is missing a semantic state or intent. Fix the contract
instead of teaching the renderer the rule.

## Non-goals

- a general-purpose declarative UI framework
- a serialized UI schema or runtime-loaded screen definition
- a custom theme, widget set, or design system
- a local daemon or frontend protocol
- moving existing domain or policy behavior into GUI code
- replacing existing operational CLI or service entrypoints
- implementing settings, pairing, diagnostics, or first-run setup in the
  brightness MVP
- implementing the complete GUI first-run, service, or update journey tracked
  in [#129](https://github.com/Staphylococcus/LG_Buddy/issues/129)
- removing Zenity in the MVP

GTK templates or builder files may be used internally by the GTK renderer.
They are renderer implementation details and do not replace the
application-owned presentation contract.

## MVP Architectural Acceptance

The brightness MVP satisfies this architecture when:

- the core crate compiles and tests without GTK
- the GTK crate imports application presentation types but the core imports no
  GTK or GLib types
- GTK callbacks emit semantic intents and perform no TV, config, notification,
  service, validation, or workflow work
- blocking operations cannot stall the GTK main loop
- presentation states and transitions have complete GTK-free coverage
- the GTK renderer passes the shared semantic, keyboard, accessibility,
  scaling, and theme contract
- the desktop entry launches the GUI and the bundle supplies its runtime
  dependencies
- CLI, headless service behavior, and the retained Zenity compatibility path
  remain unchanged
