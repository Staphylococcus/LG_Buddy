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

CI builds the pinned x86_64 matrix below and tests each artifact in its matching
headless KWin. Fedora 43 and 44 additionally test the portable outputs using only
distribution runtime packages. Matching versions do not establish compatibility
with every downstream build: an actual loader rejection proceeds through the
existing fallback chain.

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

## Prebuilt target matrix

The target starts at **Plasma/KWin 6.4.0**, matching the native Wayland activity
floor established in [#84](https://github.com/Staphylococcus/LG_Buddy/issues/84).
KWin 6.4.0 implements `ext_idle_notifier_v1` version 2 and its input-only request;
the runtime still probes that protocol and an advertised seat rather than gating
activity on a desktop version. The inhibition getter and native plugin factory
also exist in 6.4.0. See upstream's
[idle-notify implementation](https://github.com/KDE/kwin/blob/v6.4.0/src/wayland/idlenotify_v1.cpp),
[getter](https://github.com/KDE/kwin/blob/v6.4.0/src/input.h), and
[versioned plugin interface](https://github.com/KDE/kwin/blob/v6.4.0/src/plugin.h).

Target **every stable patch release from that floor**, initially on x86_64 to
match the application bundle. The release-tag snapshot on 2026-09-13 is:

| Plasma/KWin series | Exact versions to build (inclusive) | Version count |
| --- | --- | ---: |
| 6.4 | 6.4.0 through 6.4.6 | 7 |
| 6.5 | 6.5.0 through 6.5.6 | 7 |
| 6.6 | 6.6.0 through 6.6.6 | 7 |
| 6.7 | 6.7.0 through 6.7.5 | 6 |
| **Total** | | **27** |

The [KDE KWin release tags](https://github.com/KDE/kwin/tags) define the exact
versions. Releases below 6.4.0 and development, beta or release-candidate tags
are outside this prebuilt target. Add subsequent stable releases without dropping
earlier targets; changing the floor is a separate support decision. Downstream
backports remain governed by runtime capability discovery.

Each version needs a separately compiled plugin: the plugin interface includes
the full KWin version. `scripts/kwin-matrix/targets.json` pins upstream revisions,
source hashes, Qt patch versions and KDE build environments. Moving distribution
repositories are used for additional compatibility tests, not historical builds.

Upstream 6.4 requires Qt 6.8, 6.5 requires Qt 6.9, and 6.6/6.7 require Qt 6.10
(verified in `CMakeLists.txt` for all 27 target tags). Qt and the system-library
baseline are pinned together, with KDE dependencies selected for each KWin series.
Later patch releases also require matching Plasma library versions. Where they
postdate the KDE recipe snapshot, the manifest pins maintenance releases of those
libraries while retaining the same Qt and Frameworks baseline.

### Qt combinations and size budget

As of 2026-09-13, the newest stable Qt minor is 6.11, with patch release 6.11.2
in the [official release index](https://download.qt.io/official_releases/qt/).
Enumerating every Qt minor from each KWin series' minimum through 6.11 gives
this conservative candidate grid for one x86_64 system-library baseline:

| KWin targets | Qt minor build variants | Candidate artifacts |
| --- | --- | ---: |
| 6.4.0 through 6.4.6 | 6.8, 6.9, 6.10, 6.11 | 28 |
| 6.5.0 through 6.5.6 | 6.9, 6.10, 6.11 | 21 |
| 6.6.0 through 6.6.6 | 6.10, 6.11 | 14 |
| 6.7.0 through 6.7.5 | 6.10, 6.11 | 12 |
| **Total** | | **75** |

Qt patch releases do not normally require separate artifacts: its
[plugin loader rules](https://doc.qt.io/qt-6/deployment-plugins.html) allow patches
within a minor and plugins built with an older minor, while refusing a plugin
built with a newer minor than the host. Qt's
[binary compatibility contract](https://doc.qt.io/qt-6/qt-releases.html#binary-compatibility)
also requires compatible toolchains, system environments and Qt configurations.
KWin's private interface and downstream builds still require actual loader tests.

The original Fedora stripped plugins were 27,560 bytes; independently XZ-compressed,
they are 6,288 and 6,300 bytes. At those measured sizes, 75 artifacts would be
about **1.97 MiB unpacked or 461 KiB compressed**, excluding metadata/archive
overhead. Two system-library baselines would budget 150 artifacts, about
**3.94 MiB unpacked or 923 KiB compressed**. These are size estimates, not measured
expanded bundles. The count is bounded by the chosen versions, architectures and
library baselines; arbitrary downstream builds are handled by the existing
fallback chain. New stable versions extend the dated grid.

### Initial shipped variants

The 75 combinations above are a size ceiling, not a requirement to port historical
Plasma dependencies to every newer Qt. Initial builds cover all 27 KWin versions
on their upstream minimum Qt minor, plus the current Qt 6.11 baseline for 6.7:

| KWin series | Pinned Qt versions | Artifacts |
| --- | --- | ---: |
| 6.4 | 6.8.3 | 7 |
| 6.5 | 6.9.2 | 7 |
| 6.6 | 6.10.2 | 7 |
| 6.7 | 6.10.2, 6.11.2 | 12 |
| **Total** | | **33** |

For example, Frameworks 6.15's KArchive does not compile with Qt 6.11's changed
`QString::arg` overloads. We do not carry unrelated historical KDE ports merely
to populate that theoretical grid. Older-Qt plugins remain candidates on newer
Qt; Fedora's loader checks exercise that reuse. Additional variants should answer
an observed compatibility gap. No KWin version in the stated range is dropped.

### Building and checking coverage

Nix is used only for the CI build environments. Exported plugins contain no Nix
store references or runtime search paths. Nix is not installed on users' machines
by bridge setup, and the local compilation fallback keeps its existing tools.

```sh
# Generate and load every target (a cold build compiles the matching KWin builds).
python3 scripts/kwin_matrix.py build --directory target/kwin-bridges

# Require every pinned variant and verify its source, toolchain and loader record.
python3 scripts/kwin_matrix.py verify --directory target/kwin-bridges

# Build one combination for investigation, in a separate output directory.
python3 scripts/kwin_matrix.py build --directory target/kwin-probe \
  --kwin-version 6.4.0 --qt-minor 6.8
```

The reusable `kwin-bridge.yml` workflow groups builds by KWin series and Qt minor.
It caches only artifacts that passed the loader test, keyed by their exact build
and test inputs. Each artifact includes `metadata.tsv` and `build.json`, recording
the source revision, toolchain pins, plugin checksum and successful loader check.
A final coverage job rejects missing, duplicate, stale or corrupt artifacts.

CI and release bundling use `--require-kwin-matrix`; a partial collection cannot
produce their release bundle. The copied payload is checked again and includes
`docs/kwin/matrix.json`. Ad hoc local bundles may omit that flag and retain the
ordinary local-build/no-source fallbacks.

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
