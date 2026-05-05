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
durable source of truth.

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
screen_restore_policy=conservative
system_sleep_wake_policy=enabled
```

Avoid adding installer-only state for product behavior. Environment variables
can still be useful for tests, release-bundle smoke checks, and non-interactive
packaging flows, but they should write or preserve `config.env` when they
represent durable user preference.

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

`screen_restore_policy` follows this model:

- `conservative` is the default
- `aggressive` is available for users who want LG Buddy to reclaim the TV more
  assertively
- the choice lives in `config.env`
- it is writeable through `lg-buddy settings set screen.restore_policy <value>`
  because the command can apply screen-monitor changes

`system_sleep_wake_policy` follows the same model:

- automatic system sleep/wake handling should default to enabled
- users who do not want it should opt out through `config.env`
- the installer should not ask every user whether sleep/wake automation should
  be enabled
- the supported values are `enabled` and `disabled`
- it remains read-only through `lg-buddy settings` until the command can safely
  apply lifecycle service and NetworkManager hook ownership changes
