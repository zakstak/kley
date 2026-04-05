{ nixpkgs, sourceResolution, kleyPackage }:
let
  moduleGraph = import ./modules/default.nix;
  promotionContract = import ./promotion-contract.nix { inherit sourceResolution; };
  mkHost = hostModule:
    nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      specialArgs = {
        inherit promotionContract kleyPackage;
      };
      modules = moduleGraph.sharedModuleImports ++ [ hostModule ];
    };
in
{
  inherit (moduleGraph) nixosModules;

  nixosConfigurations = {
    saga-dev = mkHost ./hosts/saga-dev.nix;
    saga-dev2 = mkHost ./hosts/saga-dev2.nix;
  };
}
