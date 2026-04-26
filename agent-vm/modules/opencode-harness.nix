{ pkgs, ... }:
let
  opencodeWrapper = pkgs.writeShellScriptBin "opencode" ''
    exec /run/current-system/sw/bin/kley-opencode "$@"
  '';
in {
  imports = [ ./runtime-harness.nix ];

  environment.systemPackages = [ opencodeWrapper ];

  kley.agentVm.harnesses = [ "opencode" ];
  kley.agentVm.publicRuntime = "opencode";
  kley.agentVm.runtimeHarnesses.opencode = {
    bindAddr = "127.0.0.1:3210";
    publicOrigin = "http://saga-dev";
    stateRoot = "/var/lib/kley/opencode";
    wrapperName = "kley-opencode";
    serviceName = "kley-web-opencode";
    workingDirectory = "/var/lib/kley/opencode/home";
  };
}
