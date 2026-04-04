{ lib, pkgs, ... }:

let
  developerHeavyProfile = import ../profiles/developer-heavy.nix { inherit pkgs lib; };
in {
  # Shared agent runtime layer. Keep the developer-heavy inventory here so the
  # Task 3 package contract remains centralized instead of drifting into hosts.
  environment.systemPackages = developerHeavyProfile.packages;
}
