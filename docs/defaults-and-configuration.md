# LG Buddy Defaults And Configuration

This document defines how LG Buddy should choose defaults and expose advanced
behavior.

The rule is: default installs should be useful without asking users to make
policy decisions during installation. Advanced behavior should be controlled by
documented values in the existing `config.env` file, but only when the
configuration point is worth its long-term cost.

## Product Stance

LG Buddy should prefer sensible defaults over installer prompts.

LG Buddy should also prefer fewer configuration options over broad
configurability. Every option becomes maintenance debt: it expands the behavior
matrix, increases regression risk, complicates documentation, and makes support
harder. A config key should exist only when the non-default behavior is a real
user need and the project is willing to preserve and test that behavior.

Required infrastructure is derived from the environment and real behavior
settings. Missing basic services are incomplete setup, not an implicit off
choice; cancelling authorization must not create such a preference. The
[onboarding contract](onboarding.md) records how desired behavior and observed
readiness are reconciled across initial, headless and resumed setup.

Installer prompts are appropriate for required facts that cannot be inferred,
such as the TV IP address, TV MAC address, configured HDMI input, or an explicit
path to a locally built runtime binary.

Installer prompts are not the right surface for policy choices such as:

- whether lifecycle automation should run
- whether restore behavior should be conservative or aggressive
- which future optional event sources should participate in monitor behavior

Those choices should have documented defaults and be tunable after installation.
They should become configurable only when the project intentionally accepts the
extra behavior surface.

## Configuration Surface

The primary persistent configuration surface is `config.env`.

The structured settings CLI is an access layer over that same file, not a second
storage location. A dotted setting key such as `screen.restore_policy` maps to
the existing `config.env` key `screen_restore_policy`. This keeps manual config
editing, `configure.sh`, installer preservation, and `lg-buddy settings` on one
durable user-preference store. Generated declarations can manage selected
behavior keys through the overlay described below.

Public CLI discovery lists behavior and TV settings. The legacy `screen.backend`
key is excluded from `settings list` and unqualified `settings describe`, but
explicit commands and diagnostics retain access for compatibility. Existing
overrides are preserved. See the [legacy CLI contract](session-backend-model.md#portable-configuration-and-legacy-overrides).

The settings CLI should not hide malformed public settings. If a public
`config.env` value fails validation, `settings list` and `settings describe`
should show it as invalid, and `settings get <key>` should return the validation
error. Mutation commands may still overwrite or unset the bad value so users can
repair the file through the structured interface. Invalid legacy backend values
remain visible in diagnostics and `settings describe screen.backend`; explicitly
reading or correcting that key retains the same validation behavior.

The current public settings interface exposes one TV through `tv.ip`, `tv.mac`,
`tv.input`, and `tv.platform`. New writes store those values as
`tvs_primary_ip`, `tvs_primary_mac`, `tvs_primary_input`, and
`tvs_primary_platform`. The profile-shaped storage is only an extensibility
point; LG Buddy does not currently expose multiple TVs or TV profile selection.
Existing single-TV keys `tv_ip`, `tv_mac`, and `input` remain readable for
compatibility.

New behavior should use a config key when users may reasonably want to keep a
non-default choice across reinstalls or upgrades. Prefer enum-shaped values over
multiple booleans when the setting describes a mode.

Before adding a config key, answer:

- what real user need requires this to be configurable?
- can a better default remove the need for configuration?
- how many behavior combinations does this add?
- what tests will cover each supported mode?
- can this be documented clearly without exposing implementation details?

If those answers are weak, keep the behavior fixed and improve the default
instead.

Good shapes:

```ini
tvs_primary_ip=192.168.1.100
tvs_primary_mac=aa:bb:cc:dd:ee:ff
tvs_primary_input=HDMI_2
tvs_primary_platform=lg_webos
screen_restore_policy=conservative
screen_idle_blank=enabled
system_sleep_wake_policy=enabled
updates_auto_check=enabled
updates_channel=stable
```

Avoid adding installer-only state for product behavior. Environment variables
can still be useful for tests, release-bundle smoke checks, and non-interactive
packaging flows, but they should write or preserve `config.env` when they
represent durable user preference.

## Declaratively managed settings

A generated overlay can authoritatively manage individual behavior settings.
For a selected canonical config `/path/config.env`, its binding is
`/path/config.env.declarative.json`. The binding may be a symlink to a generated
file, including a Nix store path; readers resolve it afresh on every load.
Ordinary config-selector symlink protections are unchanged.

```json
{
  "version": 1,
  "source": "nix",
  "settings": {
    "screen.idle_timeout": 300,
    "screen.honor_idle_inhibitors": true
  }
}
```

Omit `source`, or use `generic`, for other declarative generators. Attribution
selects presentation text; it grants no installation or file-mutation authority.
The application reads validated data and never evaluates Nix or sources shell
code. Integers, strings and toggle booleans are validated by the existing setting
registry. Unknown or duplicate keys, unknown fields, unsupported schema versions,
invalid values and non-scalar data are rejected. Data must be a regular file of
at most 64 KiB, owned by root or the selected config's owner, without group/other
write permission.
The binding and its directory must also belong to root or the config owner;
unsafe directory redirection permissions are rejected.

The supported keys are `screen.backend`, `screen.idle_blank`,
`screen.honor_idle_inhibitors`, `screen.idle_timeout`, `screen.restore_policy`,
`system.sleep_wake_policy`, `updates.auto_check` and `updates.channel`.
TV connection fields and native credentials remain user-owned and cannot be
declared here.

For declared keys, precedence is declaration, then saved preference, then
default. The declaration also takes precedence over the legacy backend and idle
timeout environment overrides. The underlying preference is never replaced by
the declared value. CLI reads expose the effective value/source; set/unset and
GUI edits/resets are rejected before publication or service activation. Other
keys remain writable. Editors, pairing and migration publication retain the
existing writer/ownership checks and cannot overwrite managed preferences.

In Settings, managed values remain visible and their controls and reset actions
are disabled. A Nix-attributed overlay shows:

> Some settings are disabled because they are managed declaratively in your nix configuration

Other generators receive a generic declarative-management notice. Removing a
key restores saved/default resolution and editing. An empty settings object or
removal of the binding removes management; the last displayed managed key also
removes the notice. A dangling binding, unreadable target or malformed configured
overlay is an error, never permission to edit.

Generators should publish complete generations through atomic replacement of
the stable binding and serialize publication with existing config writers using
the selected config's `.<filename>.pairing.lock` (the same `flock` inode; retain
its owner and never replace it). Readers never copy declarations into user state.
After publication, activation must restart/reload affected session services so
their startup-time policy changes; GUI refresh and per-action lifecycle readers
load the new generation directly. Declaration removal uses the same publication
and activation boundary. Migration that would change a managed preference is
refused rather than silently rewriting it; convert legacy configuration before
introducing such a declaration.

### Publication and service activation

Declarative integrations use the installed runtime's activation protocol:

```sh
# Machine activation, as root; CONFIG is a canonical absolute user-state path.
lg-buddy config-binding publish-machine CONFIG

# User activation, as the selected config's owner.
lg-buddy config-binding publish-user CONFIG GENERATION.json
# After the matching user units have been loaded/reloaded:
lg-buddy config-binding refresh-user CONFIG
```

`publish-machine` writes the root-owned regular `/etc/lg-buddy/config-path`
record. `publish-user` writes the invoking user's regular
`$XDG_CONFIG_HOME/lg-buddy/config-path` record (or the ordinary HOME fallback)
and atomically replaces the config's declaration binding with a symlink to the
validated generation. Do not install these selector records through symlinked
file declarations. Existing selector protections remain in force. All entry
points must use the same canonical CONFIG; units also declare it explicitly in
`LG_BUDDY_CONFIG`. Conflicting existing selections require explicit migration.

Both commands accept an absent config. Machine publication creates no user
directories or state. User publication may create private parent directories and
inert writer-lock markers, but creates no config, TV profile or credentials.
Normal first-run pairing remains responsible for user state. Generations contain
only the supported behavior settings, never credentials or copied preferences.
They must be complete read-only files. A source symlink is resolved to its
specific generation before publication; readers continue to reload the stable
binding rather than caching a Nix store target.

User publication validates the candidate before changing discovery or policy,
then revalidates under the same config-writer lock used by pairing/settings.
It also serializes activation of the user's selector. Repeat publication is
idempotent and retains existing selector/lock inodes. An interrupted or failed
later step can be retried without resetting preferences or pairing.

Publication and service refresh are deliberately separate. A module publishes
first, then reloads/activates its matching unit declarations, then calls
`refresh-user` in a reachable user manager. The helper verifies the screen
service's effective config binding and restarts only an already-active
`LG_Buddy_screen.service`, after releasing publication locks. It never enables
units, starts an inactive graphical session, or restarts the boot/shutdown
oneshot. Lifecycle policy is already re-read by the lifecycle monitor and each
event handler. A fresh or offline session picks up the generation when it starts.
If a live refresh fails, the published generation remains authoritative; retry
refresh rather than rewriting saved settings or reporting successful activation.

For removal, use `publish-user CONFIG -` under the same ordering, followed by
refresh for an active session. This removes only the declaration binding;
preferences, credentials and the user's config selection remain. Republishing
an earlier immutable generation provides declaration rollback. This does not
roll back writable user data or qualify a native NixOS generation rollback.

The public NixOS/Home Manager modules and native rebuild/rollback qualification
remain the final integration stage in #253.

## Installer Behavior

The installer should:

- ask only for required setup facts
- write defaults for policy settings when a config file is created
- preserve existing valid config values on reinstall
- avoid asking policy questions that have sensible defaults
- document advanced changes instead of presenting them as install-time choices

When migrating old behavior to a new runtime path, the installer should clean up
legacy files and services that would conflict with the new default behavior. If
a user has opted out through config, the installer should honor that persisted
choice.

## Current Examples

`tvs_primary_platform` selects the control platform for the active TV profile:

- fresh profiles select `lg_webos` and pair before the configuration is saved
- a missing platform or explicit `bscpylgtv` identifies stale 1.x
  configuration; the next user screen daemon start converts it locally to `lg_webos`
- one-shot TV commands remain gated until the daemon converts the configuration;
  foreground TV operations can then pair or repair native credentials when needed
- shutdown, suspend, resume, startup, and network-teardown handling use stored
  credentials only and skip promptly when no credential is available
- this is the only platform selector; there is no separate backend, adapter,
  or migration override

`screen_restore_policy` follows this model:

- `conservative` is the default
- `aggressive` is available for users who want LG Buddy to reclaim the TV more
  assertively
- the choice lives in `config.env`
- it is writeable through `lg-buddy settings set screen.restore_policy <value>`
  because the command can apply screen-monitor changes

`screen_idle_blank` follows the same policy model:

- automatic session idle blank/restore defaults to enabled
- in supported environments, a reported session lock also requests the same
  immediate blank policy; unlock never requests restore
- a lock-triggered blank ignores desktop and gamepad activity for a fixed one
  second before accepting activity-driven restore; this is a safety constant,
  not a user setting
- users who want update notifications without session-driven TV control can set
  `screen_idle_blank=disabled`
- the installed user-session service still runs so notification handoff remains
  available
- the supported values are `enabled` and `disabled`
- there is no separate session-lock setting

`system_sleep_wake_policy` follows the same model:

- automatic system sleep/wake handling should default to enabled
- users who do not want it should opt out through `config.env` or
  `lg-buddy settings set system.sleep_wake_policy disabled`
- the installer should not ask every user whether sleep/wake automation should
  be enabled
- the supported values are `enabled` and `disabled`
- lifecycle service and NetworkManager hook installation are integration
  topology, while this setting controls runtime policy

`updates_auto_check` follows the same opt-out model:

- automatic update checks should default to enabled
- background checks should be low-frequency, currently weekly with randomized
  delay
- the installer should not ask every user whether update checks should run
- users who do not want background checks should opt out through `config.env` or
  `lg-buddy settings set updates.auto_check disabled`
- disabling automatic checks disables/stops the user timer instead of merely
  suppressing notifications after doing the work

`updates_channel` is the single release-channel preference for update
operations:

- `stable` is the default for manual and background checks
- `prerelease` explicitly opts all checks into considering both prerelease and
  stable releases
- the installed binary's own release channel does not override this preference
