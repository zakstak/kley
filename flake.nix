{
  description = "Kley development environment";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);
    in {
      devShells = forAllSystems (system:
        let
          pkgs = import nixpkgs { inherit system; };
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
            ];

            shellHook = ''
              echo "Kley dev shell ready"
              echo "Run: ./preflight.sh && ./kley-run.sh chat"
            '';
          };
        });
    };
}
