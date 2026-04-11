{ lib, pkgs, ... }:
let
  developerHeavyProfile = import ../profiles/developer-heavy.nix { inherit pkgs lib; };
in {
  environment.systemPackages = developerHeavyProfile.packages;
}
