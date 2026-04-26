{ pkgs, ... }:
let
  hermesWrapper = pkgs.writeShellScriptBin "hermes" ''
    exec /run/current-system/sw/bin/kley-hermes "$@"
  '';
in {
  imports = [ ./runtime-harness.nix ];

  environment.systemPackages = [ hermesWrapper ];

  kley.agentVm.harnesses = [ "hermes" ];
  kley.agentVm.runtimeHarnesses.hermes = {
    bindAddr = "127.0.0.1:3211";
    publicOrigin = null;
    stateRoot = "/var/lib/kley/hermes";
    wrapperName = "kley-hermes";
    serviceName = "kley-web-hermes";
    workingDirectory = "/var/lib/kley/hermes/home";
  };
}
