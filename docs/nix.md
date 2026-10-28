# NixOS and Home Manager

The flake exports `nixosModules.lg-buddy` and `homeManagerModules.lg-buddy`
(with `default` aliases), plus matching CLI/GTK packages for `x86_64-linux` and
`aarch64-linux`. These are the public declarations for the v2.1 NixOS route.
The native runner below exercises fresh x86_64 GNOME/Plasma installations and
generation changes in desktop VMs. Physical-TV and hardware lifecycle
qualification remains open in [#253](https://github.com/Staphylococcus/LG_Buddy/issues/253);
development module availability does not establish that final support claim.

## Install both layers

Use the NixOS module for boot/shutdown, sleep/wake, NetworkManager and machine
config discovery. Use Home Manager for the selected user's graphical-session
monitor, user discovery and read-only settings. Home Manager alone does not
install machine files or complete system integration. The machine supports one
selected desktop user's config; it does not install global per-user services.

The following flake fragment uses the unreleased `dev` branch, pinned by the
consumer's lock file. Choose an appropriate revision when adopting it.

```nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    home-manager = {
      url = "github:nix-community/home-manager/release-26.05";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    lg-buddy.url = "github:Staphylococcus/LG_Buddy/dev";
  };

  outputs = { nixpkgs, home-manager, lg-buddy, ... }: {
    nixosConfigurations.desktop = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        ./configuration.nix # Hardware, boot and the rest of your host configuration.
        lg-buddy.nixosModules.default
        home-manager.nixosModules.home-manager
        {
          networking.networkmanager.enable = true;
          users.users.alice.isNormalUser = true;
          services.lg-buddy = {
            enable = true;
            user = "alice";
            # Defaults to this user's home + /.config/lg-buddy/config.env.
            # configPath = "/home/alice/.config/lg-buddy/config.env";
          };
          home-manager.useGlobalPkgs = true;
          home-manager.useUserPackages = true;
          home-manager.users.alice = { osConfig, ... }: {
            imports = [ lg-buddy.homeManagerModules.default ];
            home.stateVersion = "26.05"; # Keep your existing value on an existing home.
            services.lg-buddy = {
              enable = true;
              package = osConfig.services.lg-buddy.package;
              configPath = osConfig.services.lg-buddy.configPath;
              settings = {
                screen.idle_timeout = 300;
                screen.honor_idle_inhibitors = "enabled";
                system.sleep_wake_policy = "enabled";
              };
            };
          };
        }
      ];
    };
  };
}
```

Run `sudo nixos-rebuild switch --flake .#desktop`, then enter a graphical session
and launch **LG Buddy** from the application menu, or run `lg-buddy`. Pair through
the normal GUI or `lg-buddy setup`. There is no config-restoration prerequisite:
activation publishes discovery before a config or native credentials exist.
The monitor serves setup before pairing; the lifecycle service waits for the
first writable config and then starts the real daemon without a restart or
another rebuild. The boot oneshot stays active without contacting
a TV when no config exists, allowing later shutdown to use newly paired state.

Both modules must select the same package and canonical absolute `configPath`.
Integrated Home Manager checks this and the selected username. The default
packages build this source with the consumer's package set. A custom XDG config
home or config location must also be selected by the machine declaration.
The runtime refuses symlinked/unsafe config paths and conflicting old selectors;
it never makes the writable config a store file. Preserve an existing valid
selection when migrating an installation.

For standalone Home Manager, import `lg-buddy.homeManagerModules.default` into
your existing `homeConfigurations.<name>` and set `services.lg-buddy.enable`,
`package` and `configPath` to match the separately activated NixOS module. Apply
with `home-manager switch --flake .#<name>` after the machine rebuild. Its user
activation never writes `/etc` or runs machine TV power commands.

## Declared settings and state

`services.lg-buddy.settings` contains only these optional keys. Every value
defaults to `null`, meaning the key remains editable and uses saved/default
resolution. Names and values match the settings CLI.

| Key | Values |
| --- | --- |
| `screen.backend` | `auto`, `gnome`, `wayland` (legacy preference; normally leave unmanaged) |
| `screen.idle_blank` | `enabled`, `disabled` |
| `screen.honor_idle_inhibitors` | `enabled`, `disabled` |
| `screen.idle_timeout` | Integer, 1–86400 seconds |
| `screen.restore_policy` | `conservative`, `aggressive` |
| `system.sleep_wake_policy` | `enabled`, `disabled` |
| `updates.auto_check` | `enabled`, `disabled` (retained preference; externally managed updates do not run checks) |
| `updates.channel` | `stable`, `prerelease` (retained preference; Nix chooses the installed version) |

Home Manager generates validated version-1 JSON with Nix attribution. It
publishes the immutable generation through the runtime's
[binding protocol](defaults-and-configuration.md#publication-and-service-activation),
then reloads the matching units and conditionally refreshes an active monitor.
Existing monitor changes use that conditional refresh rather than Home Manager's
ordinary restart. An inactive graphical session stays inactive. Offline
activation defers loading the policy until the next session; a reachable manager
whose reload/refresh fails makes activation fail and can be retried.

Declared rows are disabled in Settings with the conditional Nix notice; CLI
writes to those keys fail. Undeclared rows remain editable. The JSON contains no
TV profiles, addresses, MACs or credentials. Preferences and native tokens remain
user-owned writable files and are neither copied into Nix nor rolled back by it.

To remove a managed value, omit it or set it to `null` and activate again.
Removing all values removes the declaration binding, restoring saved/default
resolution. Reapplying an older declaration restores its policy while retaining
writable preferences and credentials.

## Optional Plasma bridge

On a host with `services.desktopManager.plasma6.enable = true`, set:

```nix
services.lg-buddy.plasma.enable = true;
```

The NixOS module builds the plugin with that host package set's KWin/Qt (KWin
6.4 or newer), using the exact unchanged source payload from the selected LG
Buddy package. It installs the plugin and a system `kwinrc` default enabling
`lg_buddy_inhibition`. An explicit per-user `kwinrc` override still wins. Remove
obsolete downstream bridge packages or user overrides before switching.

Start a new Plasma session after installing or updating KWin/the bridge. A
rebuild does not replace code inside an already-running compositor. Successful
live build-identity/getter checks establish setup readiness; build success alone
does not. The Nix loader check uses a private headless compositor; native Plasma
session and physical-TV qualification remain #253 work. Without the bridge,
KWin's native inhibition is absent and applicable setup remains incomplete;
GNOME does not need this plugin. See [KWin integration](kwin-integration.md).

## Existing installations and removal

Remove the old downstream LG Buddy declarations before enabling these modules.
Do not import both sets of service definitions. The machine module masks obsolete
global update units and clears their global enablement links. Home Manager masks
the update service and timer at user priority, covering shadowing user files, then
stops retained active instances. Other units and saved update preferences are
untouched. Existing unmanaged files with the same unit names use Home Manager's
normal collision/backup handling: inspect them, preserve a backup, and use your
usual `home-manager switch -b <backup-suffix>` or NixOS
`home-manager.backupFileExtension` policy. The module does not force-delete them.

To uninstall, keep the Home Manager module imported, set its `enable = false`
and activate once. The previous generation's receipt lets activation remove its
own declaration and stop its monitor while preserving the user selector,
preferences and credentials. Then remove the import and package input if desired.
Removing the import without this activation also removes the cleanup hook; use
the [publication protocol](defaults-and-configuration.md#publication-and-service-activation)
to clear any retained declaration before garbage collection removes its target.

Disable the NixOS module and rebuild to remove machine integration. Ordinary
rebuilds and removal do not stop/restart the boot/shutdown oneshot, avoiding an
unintended TV power action. An already-active oneshot retains its shutdown
behavior across ordinary rebuilds. Removal also removes its shutdown action;
the previous oneshot may remain active in systemd until it is stopped, without
controlling the TV. Never use the mutable-root installer/uninstaller to manage
these Nix declarations.

## Updates belong to Nix

LG Buddy recognizes a running executable in a root-owned, immutable Nix store
package, including launches through profile symlinks and the GTK wrapper. An OS
name, environment variable or writable store lookalike cannot establish that
ownership. An unverifiable store payload cannot use the application installer.

For these installations, Settings shows **Managed externally**, explains the
Nix update procedure and hides the inapplicable automatic-check and release-channel
controls. Saved preferences remain intact. CLI checks, background checks and
notification checks return the same guidance without fetching releases or
creating an update cache. `lg-buddy updates install` refuses installation before
downloading a payload or requesting authorization.

Update the LG Buddy flake input, then rebuild and activate the owning NixOS or
Home Manager configuration, or update the owning Nix profile. Updating
`flake.lock` alone does not replace the running application.

Setup does not require update-check units or activate a timer for a Nix-owned
runtime, even when automatic checks were previously enabled. If an old timer is
still enabled or running, setup asks you to disable and stop it through its
owning configuration. It does not rewrite managed units or clear preferences.
Home Manager masks both update units and stops any retained active instances. It preserves saved update preferences.

## Update and rollback

Update the consumer's LG Buddy flake input and activate its owning configuration:

```sh
nix flake update lg-buddy
sudo nixos-rebuild switch --flake .#desktop
# For separately managed homes, follow with:
home-manager switch --flake .#<name>
```

A lock update alone does not activate the installed package. Use the owning
NixOS/Home Manager generation rollback mechanism to return to a previous
package/declaration, then recheck setup; a changed Plasma bridge needs a new
session. User data is retained independently, so generation rollback is not a
backup or a promise of arbitrary writable-data downgrade compatibility. Keep
backups before migrating old user data. Incompatible data must be repaired or
restored through the application's documented recovery flow.

## Build and validation

```sh
nix build .#lg-buddy
nix flake check --no-build --all-systems
nix build --print-build-logs --no-link .#checks.x86_64-linux.package
nix build --print-build-logs --no-link .#checks.x86_64-linux.module-evaluation .#checks.x86_64-linux.modules
nix build --print-build-logs --no-link .#checks.x86_64-linux.kwin
bash scripts/test-nix-update-ownership.sh \
  "$(nix eval --raw .#packages.x86_64-linux.lg-buddy.outPath)"
python3 scripts/test-declarative-settings.py \
  "$(nix eval --raw .#packages.x86_64-linux.lg-buddy.outPath)/bin/lg-buddy" --nix
```

The package builds matching CLI/GUI binaries, wraps GTK and installs their
matching launchers, icon, GNOME brightness extension and KWin inspection payload.
The CLI remains an ELF for executable-ownership and companion discovery. Flake
builds embed their revision and retain the `dev` channel; packaging is not release
publication. `flake.lock` pins nixpkgs and the Home Manager test interface;
`Cargo.lock` pins Rust dependencies. Python is a test input, not a direct runtime
requirement; retired `swayidle`/dialog tools are not dependencies.

Module checks evaluate valid/invalid options and use real NixOS/Home Manager
activation and systemd managers in a disposable VM. Real CLI publication/settings
are paired with recording TV/session command peers. They cover absent initial
config, regular selectors, update masks, literal paths (including spaces and
systemd percent specifiers), active refresh, inactive-session preservation,
package/declaration switches, rollback, machine removal without a shutdown action,
installed startup/shutdown behavior, and preserved preferences/credentials without
contacting a physical TV. The separate KWin check loads and queries the compiled plugin in
the matching headless compositor. Existing package/GTK launch and external-update
ownership checks remain in CI. aarch64 outputs evaluate; native aarch64 execution
and full desktop/TV lifecycle qualification are separate acceptance evidence.

For public-module qualification on real disposable desktops, run each VM
separately (about 6 GiB guest RAM):

```sh
nix build --impure --no-link --print-build-logs \
  --file scripts/test-native-setup-nixos.nix --argstr desktop gnome
nix build --impure --no-link --print-build-logs \
  --file scripts/test-native-setup-nixos.nix --argstr desktop plasma
```

The runner uses the public modules, unchanged packaged runtime, real GTK,
systemd and compositor, replacing only the TV with the loopback WebOS fixture.
It begins without preferences or credentials and checks pairing, setup admission,
managed/editor state, CLI mutations, external update ownership, native idle
inhibition/restore, activation, bootloader power-cycle, generation rollback and
declaration removal with retained credentials. Plasma also checks the live bridge.
The resulting test output contains screenshots, component and service identities,
and transport observations. Physical TV approval, visible HDMI output and actual
hardware sleep/wake and power behavior still require a host pass.
