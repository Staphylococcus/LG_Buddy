# Nix package

The flake provides the package foundation for the v2.1.0 NixOS integration in
[issue #253](https://github.com/Staphylococcus/LG_Buddy/issues/253). It does not yet
provide the NixOS or Home Manager modules, external update ownership (#254), or
read-only declaratively managed settings (#255). Installing this package alone
does not complete LG Buddy setup or establish supported NixOS integration.

## Build and inspect

With Nix's `nix-command` and `flakes` features enabled, run from this checkout:

```bash
nix build .#lg-buddy
./result/bin/lg-buddy --version
./result/bin/lg-buddy-gui --version
```

`packages.<system>.lg-buddy` and `packages.<system>.default` expose the same
package for `x86_64-linux` and `aarch64-linux`. CI builds and launches the x86_64
package; it evaluates the aarch64 output without claiming a native aarch64 test.
`flake.lock` pins nixpkgs and `Cargo.lock` pins Rust dependencies. The package can
also be called with a consumer's package set through `nix/package.nix`, supplying
`src` and optionally `buildCommit`.

Both executables are built from the same source. The package version comes from
Cargo, and flake builds embed the source revision (including a dirty revision
when available). These development builds retain the `dev` release channel;
packaging does not turn a checkout into an official release.

## Package contents and boundaries

The output contains the CLI, a GTK-wrapped GUI companion, desktop entry, icon,
GNOME brightness extension and KWin setup inspection payload. Launchers and
helpers point at their matching store package. The CLI remains an ELF so its
companion discovery and executable ownership checks see the installed runtime.
GTK resources are supplied by the GUI wrapper.

The KWin source payload is preserved unchanged, including its build fingerprint.
This package does not build, activate or load a native KWin plugin. Optional
Plasma integration and its matching KWin/Qt build belong to the later module
slice; the downstream metadata patch must not be copied without keeping the
runtime and bridge identities consistent.

There are no system/user unit activations, NetworkManager hooks, update timers,
configuration selectors, generated settings or pairing credentials in this
package slice. User settings and native TV credentials remain writable user
state, outside the Nix store. Do not use the mutable-root shell installer to
integrate this package into NixOS. Final module installation, rebuild/activation,
rollback and removal instructions will accompany the corresponding integration.

LG Buddy's direct Python dependency is a test input; the GTK dependency closure
can include Python transitively. Retired `swayidle` and dialog tools are not
runtime dependencies.

## Validation

```bash
nix flake check --no-build --all-systems
nix build --print-build-logs --no-link .#checks.x86_64-linux.package
```

The package build runs the Rust runtime library suite. The installed-package
check verifies matching CLI/GUI identities, store-bound launch assets and an
unchanged KWin source payload. It then reuses the GUI launch smoke on a private
D-Bus session and virtual X11 display: once through a profile symlink to the CLI,
and once through the GTK wrapper. Both paths exercise activation, the brightness
deep link, single-instance replacement and clean application exit.

These checks verify packaging and installed launch behavior. Native NixOS
pairing, lifecycle, Plasma integration and generation rollback remain part of
the full #253 acceptance, after #254 and #255.
