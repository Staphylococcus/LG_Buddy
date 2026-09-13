# KWin native inhibition

Plasma exposes two current inhibition routes. LG Buddy queries PowerDevil's
effective screen policy and, when available, KWin's native Wayland inhibitor set.
The KWin plugin reads `InputRedirection::idleInhibitors()`, including KWin's
visibility policy. It neither creates inhibitors nor supplies activity events.

The source in `sources/desktop/kwin.rs` makes a fresh pull query for each pending
blanking decision. Discovery, unique-owner validation, bounded calls, cancellation
and recovery stay inside the adapter. Only observed inhibition blocks blanking.
Absence or a failed read leaves this source out of the available set without
creating a release, input event or TV restore. The existing honoring preference
and release delay remain inside the shared inhibition gate.

## Delivery and compatibility

The native installer installs an optional `LG_Buddy_kwin.service` user unit and
the bridge payload under `/usr/lib/lg-buddy/kwin`. At installation, upgrade and
fresh graphical login, setup tries:

1. A compatible bundled prebuilt.
2. A cached local build, or compilation against matching installed development
   files. Only this fallback may request build dependencies using the installer's
   existing sudo/graphical authorization route.
3. Ordinary operation with no KWin source if no working bridge is obtained.

Setup runs separately from the activity monitor. A runtime query or reconnection
never starts a compiler or package manager. The application remains usable while
setup runs and when setup cannot provide the source.

Native plugin compatibility requires the running KWin's full major/minor/patch
version. Metadata also checks CPU architecture, plugin-source identity, the
artifact checksum and Qt 6 compatibility (a plugin built with a newer Qt minor
is skipped). Those checks select candidates; actual KWin loading and successful
getter/build-identity replies establish availability. Both D-Bus names must belong
to the same compositor during setup verification.

The initial CI matrix builds against the current Fedora 43 and Fedora 44 x86_64
repositories. Each artifact records the exact KWin/Qt versions resolved in that
run, and must load into a real headless KWin before bundling. This is not a promise
of compatibility with every package carrying the same KWin version: rejection
on a different system-library baseline proceeds to the next candidate/local build.
Expanding the prebuilt matrix does not change runtime policy.

Plugin files use a per-user, content-addressed name in the system Qt plugin
directory. Installation uses the existing privilege route, verifies the copied
bytes and refuses package-owned files. Distinct names allow local compilation
after Qt has rejected another candidate. Successful loading enables that exact
plugin in the user's `kwinrc` for subsequent Plasma logins.

Local builds live under `$XDG_CACHE_HOME/lg-buddy/kwin` (normally
`~/.cache/lg-buddy/kwin`). Version, architecture, source identity, Qt compatibility
and checksum are checked on reuse. A KWin or LG Buddy update causes setup to
reconsider the candidates; mismatched development files cannot build a plugin
for an older still-running compositor. No incidental KWin upgrade is requested.
Removal receipts and setup details live under `$XDG_STATE_HOME/lg-buddy/kwin`.
Rejected and superseded installed files are removed; denied removal retains a
receipt for retry. Uninstall removes this user's enabled plugins and local cache,
while respecting package ownership.

Automatic host mutation is skipped on NixOS, OSTree and unsupported Qt layouts.
Such installations may supply the same bridge through their platform packaging;
the runtime adapter discovers it independently. Other desktops normally have no
KWin source. These are supported coverage modes. Expected availability belongs
to setup/validation, independently of the blanking Boolean.

The release bundle keeps the source, metadata, helper scripts and prebuilts under
`docs/kwin/` because older verified bundle readers permit additional payload there.
They are covered by the existing archive checksum and safe extraction contract.

## Validation

`scripts/test-kwin-setup.sh` exercises the production selection/fallback path
with isolated build, privilege and loader boundaries: prebuilt without compilation,
missing/incompatible/corrupt/rejected prebuilts, local-build cache reuse, failed
build/install/load and cleanup retry. Private D-Bus tests in `tests/inhibition.rs`
exercise the actual Rust adapter, independent PowerDevil/KWin Booleans, owner
replacement, delayed replies, cancellation, failure and recovery.

`scripts/test-kwin-plugin.sh` loads each CI artifact into a disposable headless
KWin, calls its getter and build identity, unloads it and checks compositor survival.
`scripts/test-kwin-desktop.py` requires a disposable Plasma installation with the
repository's stateful TV and uinput fixtures, a ten-second idle deadline and
honoring enabled. It tests native mpv playback before/after monitor startup,
overlap, full release delay, visibility policy, gamepad restore, PowerDevil with
KWin absent, KWin loss/recovery, and disabled honoring. It records the runtime
hash and source identity with its observations. Session transitions and explicit
lock/sleep remain covered by the [desktop lifecycle matrix](desktop-session-validation.md).

Useful installed setup checks:

```sh
lg-buddy kwin-bridge info
lg-buddy kwin-bridge check
journalctl --user -u LG_Buddy_kwin.service
cat ~/.local/state/lg-buddy/kwin/setup.log
```

The internal `kwin-bridge check` command reports build identity only after reading
a valid current Boolean from the compositor. Its absence result is a setup
diagnostic, not an inhibition verdict.
