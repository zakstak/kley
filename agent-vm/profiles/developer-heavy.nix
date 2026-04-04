{ pkgs, lib }:

let
  developerHeavyPackages = lib.unique ([
    pkgs.git
    pkgs.gh
    pkgs.rustc
    pkgs.cargo
    pkgs.rustfmt
    pkgs.clippy
    pkgs.gcc
    pkgs.gnumake
    pkgs.cmake
    pkgs.nodejs_22
    pkgs.go
    pkgs.python3
    pkgs.sqlite
    pkgs.shellcheck
    pkgs.tree
    pkgs.jq
    pkgs.fd
    pkgs.bat
    pkgs.rust-analyzer
    pkgs.gopls
    pkgs.golangci-lint
    pkgs.prettier
    pkgs.gitleaks
    pkgs.cargo-nextest
    pkgs.bash-language-server
    pkgs.yaml-language-server
    pkgs.nixd
    pkgs.pyright
  ]);
in {
  packages = developerHeavyPackages;
}
