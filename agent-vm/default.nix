{ nixpkgs, sourceResolution, kleyPackage }:
let
  moduleGraph = import ./modules/default.nix;
  mkHost = {
    hostModule,
    extraModules ? [ ],
  }:
    nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      specialArgs = {
        inherit sourceResolution kleyPackage;
      };
      modules = moduleGraph.baseModuleImports ++ extraModules ++ [ hostModule ];
    };
in
{
  inherit (moduleGraph) nixosModules;

  nixosConfigurations = {
    saga-runtime = mkHost {
      hostModule = ./hosts/saga-runtime.nix;
      extraModules = [
        moduleGraph.nixosModules."runtime-cli-tools"
      ];
    };
  };
}
