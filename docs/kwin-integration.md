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

The shared flow, startup assessment and **Complete setup** entry in Settings implement the
[onboarding contract](onboarding.md). Runtime absence remains
supported reduced coverage; an applicable unmet setup requirement remains pending.

The native installer installs an optional `LG_Buddy_kwin.service` user unit and
the bridge payload under `/usr/lib/lg-buddy/kwin`. The Rust application owns
inspection, artifact validation, provisioning, removal, and the kernel file lock.
The installed `setup.sh` is a compatibility launcher for existing layouts and
Polkit policy. The flow-scoped authorization owner brokers native privileged
requests so services and Plasma retain the same authorization subject.
Session activation runs the native entry point to load an already installed
plugin; it never provisions or requests authorization. Explicit foreground setup tries:

1. A compatible bundled prebuilt.
2. A cached local build, or compilation against matching installed development
   files. Installing missing development tools is a separate input request before
   package changes. Both plugin installation and package changes may require
   administrator authorization.
3. Ordinary operation with no KWin source if no working bridge is obtained.

Setup runs separately from the activity monitor. A runtime query or reconnection
never starts a compiler or package manager. The daemon can operate with reduced
coverage when the source is unavailable. The GUI remains gated until applicable
installation requirements are complete; the existing setup modal provides
repair or external-remediation guidance.

The backend consumes a read-only `--status` exit-code protocol from the helper:
0 complete, 2 inapplicable, 3 setup needed, 4 legacy unsupported installation,
5 NixOS-managed bridge, 6 immutable image, 7 unsupported KWin version,
8 unsupported Qt plugin directory, and 1 inspection failure.
`--foreground` performs provisioning; `--allow-dependencies`
supplies explicit dependency consent, and `--noninteractive` forbids interactive
authorization. Exit 77 requests dependency consent, 126 reports explicit
cancellation, and 127 reports that authorization was not granted. An exhausted foreground attempt fails verification rather
than reporting setup complete. These are internal step operations, not separate
CLI onboarding entry points. Named polkit actions describe LG Buddy's purpose.

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
bytes and shares the bundle installer's ownership/mount guard. KWin's loader
requires these files under `/usr/lib*/qt6/plugins/kwin/plugins`. Both
installation and removal refuse package-owned files, conflicting ownership,
unavailable package-database answers, read-only mounts and immutable/declarative
host markers before changing the artifact. Existing package-owned container
directories are only traversed; their ownership and permissions are preserved.
During provisioning, the shipped payload remains verify-only. A bundled prebuilt
and a cached local build use the same guarded copy into KWin's loader location.
Distinct names allow local compilation after Qt has rejected another candidate.
Successful loading enables that exact
plugin in the user's `kwinrc` for subsequent Plasma logins.

Local builds live under `$XDG_CACHE_HOME/lg-buddy/kwin` (normally
`~/.cache/lg-buddy/kwin`). Version, architecture, source identity, Qt compatibility
and checksum are checked on reuse. A KWin or LG Buddy update causes setup to
reconsider the candidates; mismatched development files cannot build a plugin
for an older still-running compositor. No incidental KWin upgrade is requested.
Removal receipts and setup details live under `$XDG_STATE_HOME/lg-buddy/kwin`.
Explicit setup and removal normalize legacy user-owned lock files to mode `0600`
after acquiring their kernel lock, preserving the inode and exclusion with older
helpers. Inspection and passive loading leave lock permissions unchanged.
Rejected and superseded installed files are removed; denied removal retains a
receipt for retry. Uninstall removes this user's enabled plugins and local cache,
while respecting package ownership.

Automatic host mutation requires KWin 6 and a recognized Qt plugin directory.
Unsupported versions or layouts remain incomplete, not inapplicable. A
compatible bridge already loaded in KWin can satisfy setup regardless of who
installed it. GNOME and sessions without KWin are inapplicable, not missing.

Automatic host mutation is skipped on NixOS, OSTree and unsupported Qt layouts.
Such installations may supply the same bridge through their platform packaging;
the runtime adapter discovers it independently. Other desktops normally have no
KWin source. Reduced runtime coverage does not satisfy applicable installation
requirements. On NixOS, repair belongs to declarative configuration, its build
and activation; only a live service/plugin check establishes readiness. OSTree
uses its own supported image/package mechanism, not the Nix service contract.
Managed or unsupported blocks offer read-only Recheck after external changes;
Recheck never invokes the imperative provisioner.

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
match the application bundle. The release-tag snapshot on 2026-11-24 is below.

| Plasma/KWin series | Exact versions to build (inclusive) | Version count |
| --- | --- | ---: |
| 6.4 | 6.4.0 through 6.4.6 | 7 |
| 6.5 | 6.5.0 through 6.5.6 | 7 |
| 6.6 | 6.6.0 through 6.6.7 | 8 |
| 6.7 | 6.7.0 through 6.7.6 | 7 |
| 6.8 | 6.8.0 through 6.8.3 | 4 |
| **Total** | | **33** |

The [KDE KWin release tags](https://github.com/KDE/kwin/tags) define the exact
versions. Releases below 6.4.0 and development, beta or release-candidate tags
are outside this prebuilt target. Add subsequent stable releases without dropping
earlier targets; changing the floor is a separate support decision. Downstream
backports remain governed by runtime capability discovery.

Each version needs a separately compiled plugin: the plugin interface includes
the full KWin version. `scripts/kwin-matrix/targets.json` pins upstream revisions,
source hashes, Qt patch versions and KDE build environments. Moving distribution
repositories are used for additional compatibility tests, not historical builds.

Upstream 6.4 requires Qt 6.8, 6.5 requires Qt 6.9, 6.6 and 6.7 require Qt 6.10,
and 6.8 requires Qt 6.11. Qt and the system-library
baseline are pinned together, with KDE dependencies selected for each KWin series.
Later patch releases also require matching Plasma library versions. Where they
postdate the KDE recipe snapshot, the manifest pins maintenance releases of those
libraries while retaining the same Qt and Frameworks baseline.

### Qt combinations and size budget

The pinned Qt environments run through Qt 6.11.2. Enumerating each pinned Qt
minor from the KWin series' minimum gives this candidate grid for one x86_64
system-library baseline.

| KWin targets | Qt minor build variants | Candidate artifacts |
| --- | --- | ---: |
| 6.4.0 through 6.4.6 | 6.8, 6.9, 6.10, 6.11 | 28 |
| 6.5.0 through 6.5.6 | 6.9, 6.10, 6.11 | 21 |
| 6.6.0 through 6.6.7 | 6.10, 6.11 | 16 |
| 6.7.0 through 6.7.6 | 6.10, 6.11 | 14 |
| 6.8.0 through 6.8.3 | 6.11 | 4 |
| **Total** | | **83** |

Qt patch releases do not normally require separate artifacts: its
[plugin loader rules](https://doc.qt.io/qt-6/deployment-plugins.html) allow patches
within a minor and plugins built with an older minor, while refusing a plugin
built with a newer minor than the host. Qt's
[binary compatibility contract](https://doc.qt.io/qt-6/qt-releases.html#binary-compatibility)
also requires compatible toolchains, system environments and Qt configurations.
KWin's private interface and downstream builds still require actual loader tests.

The generated plugins are **31,192 bytes each**, or **6,516–6,608 bytes** when
individually XZ-compressed. At those measured sizes, 83 artifacts would be about
**2.47 MiB unpacked or at most 536 KiB individually compressed**, excluding
metadata/archive overhead. Two system-library baselines would budget 166
artifacts, about **4.94 MiB unpacked or 1.05 MiB individually compressed**. These
are estimates for the candidate grid, not measured expanded bundles. The count
is bounded by the chosen versions, architectures and library baselines; arbitrary
downstream builds use the existing fallback chain. New stable versions extend
the dated grid.

### Prebuilt variants

The build matrix covers each KWin version on its upstream minimum Qt minor and
adds a Qt 6.11 variant for KWin 6.7.

| KWin series | Pinned Qt versions | Artifacts |
| --- | --- | ---: |
| 6.4 | 6.8.3 | 7 |
| 6.5 | 6.9.2 | 7 |
| 6.6 | 6.10.2 | 8 |
| 6.7 | 6.10.2, 6.11.2 | 14 |
| 6.8 | 6.11.2 | 4 |
| **Total** | | **40** |

The initial 33 variants were built and loaded on 2026-09-13. Their plugin files total
**1,029,336 bytes (0.98 MiB)**; metadata adds 62,532 bytes. The complete collection
with metadata is **300,239 bytes (293 KiB) as tar.gz** or **21,040 bytes (21 KiB)
as tar.xz**. Compressing the collection together benefits from the plugins' shared
code. Release bundles retain their existing tar.gz format.

For example, Frameworks 6.15's KArchive does not compile with Qt 6.11's changed
`QString::arg` overloads. We do not carry unrelated historical KDE ports merely
to populate that theoretical grid. Older-Qt plugins remain candidates on newer
Qt; Fedora's loader checks exercise that reuse. Additional variants should answer
an observed compatibility gap. No KWin version in the stated range is dropped.

### Building and checking coverage

Nix supplies the pinned toolchains for portable prebuilts. Exported plugins contain no Nix
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

The [prebuilt repository](https://github.com/Staphylococcus/LG_Buddy-prebuilts)
indexes immutable release assets by each target's exact build and loader-test
inputs. Maintainers build any missing targets locally, then publish the verified
outputs using an authenticated GitHub CLI.

```sh
python3 scripts/kwin_prebuilts.py publish --directory target/kwin-probe
```

The reusable `kwin-bridge.yml` workflow restores a complete group from an Actions
cache or downloads individual targets from the prebuilt repository. It checks
the archive checksum, plugin checksum, and input identity. A missing target
prints its version and input hash so a maintainer can build it locally.
CI still verifies full coverage and runs the Fedora loader tests against the
assembled set.

CI and release bundling use `--require-kwin-matrix`; a partial collection cannot
produce their release bundle. The copied payload is checked again and includes
`docs/kwin/matrix.json`. Ad hoc local bundles may omit that flag and retain the
ordinary local-build/no-source fallbacks.

## Validation

Native provisioner tests exercise prebuilt selection, incompatible/corrupt/rejected
artifacts, local-build cache reuse, dependency consent, denied cleanup retry, and
read-only managed-host inspection. `tests/kwin_provisioning.rs` runs the actual
native worker against the authorization owner with mocked privilege tools and
checks exclusion after frontend disconnection. `scripts/test-kwin-setup.sh` runs
native inspection and removal with an empty `PATH`, proving that neither needs
the external `flock` command or shell filesystem utilities. Private D-Bus tests in `tests/inhibition.rs`
exercise the actual Rust adapter, independent PowerDevil/KWin Booleans, owner
replacement, delayed replies, cancellation, failure and recovery.

`tests/features/plasma_setup.feature` runs production discovery, the native
provisioner and the privileged installer/remover together in disposable mount
and user namespaces. KWin's D-Bus API and package/configuration tools are external
contract fixtures; privileged file operations are real. It covers `/usr/lib64 ->
lib`, real `/usr/lib64`, repeat setup, repair with legacy receipts, and rejection
of an unexpected Qt directory symlink without changing the redirected files.

The required CI `plasma-setup` job additionally runs
`scripts/test-kwin-native-setup.sh` on Arch and Fedora with real headless KWin,
an installed runtime and the normal system plugin directory. It exercises setup,
repeat setup, stale-artifact repair and cleanup without injecting
`QT_PLUGIN_PATH`. The script requires a disposable host and an ordinary test user
with passwordless sudo; never run it against a personal desktop installation.

`scripts/test-kwin-plugin.sh` loads each CI artifact into a disposable headless
KWin, calls its getter and build identity, unloads it and checks compositor survival.
The complete 33-artifact local run passed these checks. Fedora 43 additionally
loaded the 6.7.5 / Qt 6.10.2 plugin on Qt 6.10.3; Fedora 44 loaded both Qt 6.10.2
and Qt 6.11.2 variants on Qt 6.11.2, without Nix or a compiler. A bundle packaging
check using the previously verified, unchanged application binaries also proved
that the extracted archive retained all 33 artifacts and the exact matrix manifest;
an incomplete collection was rejected before an archive was created. These are
loader and packaging checks; desktop inhibition behavior is covered separately.

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
