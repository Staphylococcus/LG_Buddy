{ lib }:
{
  # Filesystem ownership and symlink checks belong to config-binding at activation.
  configPathType = lib.types.addCheck lib.types.str (
    path:
    lib.hasPrefix "/" path
    && !lib.hasPrefix "/nix/store/" path
    && builtins.all (part: part != "." && part != ".." && part != "") (
      lib.drop 1 (lib.splitString "/" path)
    )
    && !lib.hasInfix "\n" path
    && !lib.hasInfix "\r" path
  );

  systemdQuote =
    value: "\"" + lib.replaceStrings [ "\\" "\"" "%" ] [ "\\\\" "\\\"" "%%" ] value + "\"";
}
