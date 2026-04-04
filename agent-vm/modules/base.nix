{ config, lib, promotionContract, ... }:
let
  hostName = config.networking.hostName or "unknown";
  expectedLane =
    if hostName == promotionContract.canaryHost then "canary"
    else if hostName == promotionContract.baselineHost then "baseline"
    else null;
  buildMetadata = {
    hostName = hostName;
    promotionLane = config.kley.agentVm.promotionLane;
    promotion = {
      canaryHost = promotionContract.canaryHost;
      baselineHost = promotionContract.baselineHost;
      updateDriver = promotionContract.updateDriver;
      sharedModuleGraph = promotionContract.sharedModuleGraph;
      defaultCheckoutRef = promotionContract.defaultCheckoutRef;
    };
    source = promotionContract.source;
    resolvedInputs = promotionContract.resolvedInputs;
  };
in {
  options.kley.agentVm = {
    promotionLane = lib.mkOption {
      type = lib.types.enum [ "baseline" "canary" ];
      default = "baseline";
      description = "Promotion lane for this host (baseline or canary).";
    };

    buildMetadata = lib.mkOption {
      type = lib.types.attrsOf lib.types.anything;
      readOnly = true;
      description = ''
        Build-time metadata for the lockfile-driven promotion contract, including
        the exact checkout revision and resolved shared inputs.
      '';
    };
  };

  config = {
    # Shared OS/runtime contract for every agent VM. Host files stay limited to
    # machine facts like hostname, boot targets, filesystems, and network values.
    nix.settings.experimental-features = [ "nix-command" "flakes" ];
    nix.settings.auto-optimise-store = lib.mkDefault true;

    time.timeZone = lib.mkDefault "UTC";
    i18n.defaultLocale = lib.mkDefault "en_US.UTF-8";

    users.users.agent = {
      isNormalUser = true;
      description = "Agent VM machine user";
      extraGroups = [ "wheel" ];
      openssh.authorizedKeys.keys = lib.mkDefault [ ];
    };

    services.openssh.enable = lib.mkDefault true;
    services.openssh.settings = {
      PasswordAuthentication = false;
      KbdInteractiveAuthentication = false;
    };

    security.sudo.wheelNeedsPassword = lib.mkDefault false;

    assertions = lib.optional (expectedLane != null) {
      assertion = config.kley.agentVm.promotionLane == expectedLane;
      message = ''
        Host `${hostName}` must stay on promotion lane `${expectedLane}` so the
        canary-to-baseline contract cannot drift via host-local edits.
      '';
    };

    kley.agentVm.buildMetadata = buildMetadata;
    system.configurationRevision = promotionContract.source.exactRevision;
    environment.etc."kley-agent-vm-build.json".text = builtins.toJSON buildMetadata;
  };
}
