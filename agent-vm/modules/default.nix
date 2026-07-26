let
  nixosModules = {
    base = import ./base.nix;
    "developer-tools" = import ./developer-tools.nix;
    "runtime-cli-tools" = import ./runtime-cli-tools.nix;
    disko = import ./disko.nix;
    impermanence = import ./impermanence.nix;
  };
  baseModuleImports = [
    nixosModules.base
    nixosModules."developer-tools"
    nixosModules.disko
    nixosModules.impermanence
  ];
in {
  inherit nixosModules;

  # Keep the shared module order explicit so the single host consumes a stable baseline.
  inherit baseModuleImports;
  sharedModuleImports = baseModuleImports;
}
