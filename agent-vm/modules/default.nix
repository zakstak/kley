let
  nixosModules = {
    base = import ./base.nix;
    "opencode-harness" = import ./opencode-harness.nix;
    disko = import ./disko.nix;
    impermanence = import ./impermanence.nix;
  };
in {
  inherit nixosModules;

  # Keep the shared module order explicit so hosts consume a stable contract.
  sharedModuleImports = [
    nixosModules.base
    nixosModules."opencode-harness"
    nixosModules.disko
    nixosModules.impermanence
  ];
}
