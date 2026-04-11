{ nixpkgs, sourceResolution, kleyPackage }:
let
  moduleGraph = import ./modules/default.nix;
  promotionContract = import ./promotion-contract.nix { inherit sourceResolution; };
  mkHost = {
    hostModule,
    extraModules ? [ ],
  }:
    nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      specialArgs = {
        inherit promotionContract kleyPackage;
      };
      modules = moduleGraph.baseModuleImports ++ extraModules ++ [ hostModule ];
    };
in
{
  inherit (moduleGraph) nixosModules;

  nixosConfigurations = {
    saga-dev = mkHost {
      hostModule = ./hosts/saga-dev.nix;
      extraModules = [ moduleGraph.nixosModules."opencode-harness" ];
    };
    saga-dev2 = mkHost {
      hostModule = ./hosts/saga-dev2.nix;
      extraModules = [ moduleGraph.nixosModules."opencode-harness" ];
    };
    agent-pi = mkHost { hostModule = ./hosts/agent-pi.nix; };
  };
}
