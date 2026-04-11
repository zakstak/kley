let
  nixosModules = {
    base = import ./base.nix;
    "developer-tools" = import ./developer-tools.nix;
    "opencode-harness" = import ./opencode-harness.nix;
    disko = import ./disko.nix;
    impermanence = import ./impermanence.nix;
  };
  baseModuleImports = [
    nixosModules.base
    nixosModules."developer-tools"
    nixosModules.disko
    nixosModules.impermanence
  ];
  agentModuleImports = baseModuleImports ++ [ nixosModules."opencode-harness" ];
in {
  inherit nixosModules;

  # Keep the shared module order explicit so hosts consume a stable contract.
  inherit baseModuleImports agentModuleImports;
  sharedModuleImports = baseModuleImports;
}
