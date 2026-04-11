{
  description = "Kley development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs, ... }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
      pkgsFor = system: import nixpkgs { inherit system; };
      packageFor = system:
        let
          pkgs = pkgsFor system;
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "kley";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "--bin" "kley" ];
          doCheck = false;
          meta.mainProgram = "kley";
        };
      resolveFlakeSource = flake: {
        exactRevision =
          if flake ? rev then flake.rev
          else if flake ? dirtyRev then flake.dirtyRev
          else "unknown";
        shortRevision =
          if flake ? shortRev then flake.shortRev
          else if flake ? dirtyShortRev then flake.dirtyShortRev
          else null;
        lastModified = if flake ? lastModified then flake.lastModified else null;
      };
      sourceResolution = {
        kley = resolveFlakeSource self;
        nixpkgs = resolveFlakeSource nixpkgs;
      };
      agentVm = import ./agent-vm {
        inherit nixpkgs sourceResolution;
        kleyPackage = packageFor "x86_64-linux";
      };
      # Hostname strings kept explicit for saga deploy preflight grep checks:
      # "saga-dev" "saga-dev2" "agent-pi"
    in {
      packages = forAllSystems (system: {
        default = packageFor system;
        kley = packageFor system;
      });

      devShells = forAllSystems (system:
        let
          pkgs = pkgsFor system;
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustc
              cargo
              rustfmt
              clippy
              git
              gh
              jq
              go
              gitleaks
              nodejs_22
              python3
              sqlite
              shellcheck
              tree
              fd
              bat
              rust-analyzer
              gopls
              bash-language-server
              yaml-language-server
              nixd
              pyright
            ];

            shellHook = ''
              echo "Kley dev shell ready"
              echo "Run: ./preflight.sh && ./kley-run.sh chat"
            '';
          };
        });

      checks.x86_64-linux =
        let
          checkPkgs = import nixpkgs { system = "x86_64-linux"; };
        in {
          "vm-baseline-host-builds" = checkPkgs.runCommand "agent-vm-host-builds" {
            src = ./.;
            buildInputs = [
              agentVm.nixosConfigurations.saga-dev.config.system.build.toplevel
              agentVm.nixosConfigurations.saga-dev2.config.system.build.toplevel
              agentVm.nixosConfigurations.agent-pi.config.system.build.toplevel
            ];
          } ''
            echo "saga-dev, saga-dev2, and agent-pi host toplevels built"
            mkdir -p "$out"
            touch "$out/hosts-built"
          '';

          "vm-baseline-manifest" = checkPkgs.runCommand "agent-vm-manifest-check" {
            src = ./.;
            buildInputs = [ checkPkgs.python3 ];
          } ''
            set -euo pipefail
            bash "$src/tests/vm-baseline-check.sh"
          '';
        };

      nixosConfigurations = agentVm.nixosConfigurations;
      nixosModules = agentVm.nixosModules;
    };
}
