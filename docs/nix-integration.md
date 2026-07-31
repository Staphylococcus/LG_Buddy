# Experimental Nix Integration

LG Buddy exposes a flake package and a declarative module for users who want to
evaluate the project through Nix. This is deliberately scoped as packaging and
service wiring for the existing Linux runtime. It does not broaden LG Buddy's
desktop compatibility guarantees, and the release-bundle installer remains the
primary installation path.

The integration builds `lg-buddy` from `Cargo.lock`, packages the pinned
`bscpylgtvcommand` helper without a mutable pip environment, installs the
brightness desktop entry, and declares the existing system and user service
topology.

## Consumer flake

Until release-channel refs are published, use an explicit LG Buddy release tag
that contains the flake files:

```nix
{
  inputs.lg-buddy = {
    url = "github:Staphylococcus/LG_Buddy/vX.Y.Z";
    inputs.nixpkgs.follows = "nixpkgs";
  };
}
```

Import the module in the machine configuration and declare the runtime values:

```nix
{
  inputs,
  ...
}:

{
  imports = [ inputs.lg-buddy.nixosModules.default ];

  services.lg-buddy = {
    enable = true;
    user = "vas";

    tv = {
      ip = "192.168.1.100";
      mac = "aa:bb:cc:dd:ee:ff";
      input = "HDMI_2";
    };

    screen = {
      backend = "auto";
      idleBlank = true;
      idleTimeout = 300;
      restorePolicy = "conservative";
    };

    systemSleepWake = true;
    updates.autoCheck = false;
  };
}
```

The module creates root-owned startup, shutdown, and lifecycle services, plus a
user-session monitor and optional release-check timer. When NetworkManager is
enabled it also declares the pre-down dispatcher used by the lifecycle rail.

## Configuration and pairing state

Runtime settings are rendered into an immutable generated `config.env`.
Inspection commands such as `lg-buddy settings list`, `get`, and `describe`
remain useful, but declared values must be changed in the consumer configuration
and applied through its normal rebuild process. `settings set` and `unset`
cannot modify the generated file.

The webOS pairing database is not placed in the Nix store. It defaults to
`/var/lib/lg-buddy/<user>/client-key.sqlite`, in a directory owned by the
configured desktop user. Root services retain ownership of lifecycle behavior
while launching the Python TV helper with that user's identity.

Background release checks default to disabled because installed software is
advanced through the consumer's flake input and lock file. Enabling checks only
enables notifications; it does not update the locked input or rebuild the
machine.

## Repository checks

From this repository, evaluate and build the package and representative module
configuration with:

```bash
nix flake check
```

The check does not exercise a physical TV, firmware-specific webOS behavior, or
every desktop session.
