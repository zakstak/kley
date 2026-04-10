{ config, lib, pkgs, promotionContract, kleyPackage, ... }:
let
  hostName = config.networking.hostName or "unknown";
  operatorAuthorizedKeyPath = ../.generated/operator-authorized-key.pub;
  operatorAuthorizedKeys =
    if builtins.pathExists operatorAuthorizedKeyPath then
      let
        key = lib.strings.removeSuffix "\n" (builtins.readFile operatorAuthorizedKeyPath);
      in
      lib.optional (key != "") key
    else
      [ ];
  vaultEnvironment = lib.filterAttrs
    (name: value:
      builtins.elem name [ "VAULT_ADDR" "VAULT_TOKEN" ]
      && builtins.isString value
      && value != "")
    {
      VAULT_ADDR = builtins.getEnv "VAULT_ADDR";
      VAULT_TOKEN = builtins.getEnv "VAULT_TOKEN";
    };
  expectedLane =
    if hostName == promotionContract.canaryHost then "canary"
    else if hostName == promotionContract.baselineHost then "baseline"
    else null;
  buildMetadata = {
    hostName = hostName;
    promotionLane = config.kley.agentVm.promotionLane;
    webBindAddr = config.kley.agentVm.webBindAddr;
    webPublicOrigin = config.kley.agentVm.webPublicOrigin;
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

    webPublicOrigin = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        Public origin that Kley web should use when constructing OpenAI browser
        login redirect URIs on this host.
      '';
    };

    webBindAddr = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1:3210";
      description = ''
        Internal bind address for the persistent Kley web service.
      '';
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
      openssh.authorizedKeys.keys = lib.mkDefault operatorAuthorizedKeys;
    };

    users.users.root.openssh.authorizedKeys.keys = lib.mkDefault operatorAuthorizedKeys;

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

    services.nginx.enable = true;
    services.nginx.recommendedProxySettings = true;
    services.nginx.virtualHosts.${hostName} = {
      locations."/".proxyPass = "http://${config.kley.agentVm.webBindAddr}";
      locations."/ws" = {
        proxyPass = "http://${config.kley.agentVm.webBindAddr}";
        proxyWebsockets = true;
      };
    };

    systemd.services.kley-web = {
      description = "Kley web UI";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        Type = "simple";
        ExecStartPre = "${pkgs.writeShellScript "kley-web-pre-start" ''
          ${pkgs.procps}/bin/pkill -f -- ${lib.escapeShellArg "kley web --bind"} || true
        ''}";
        ExecStart = "${kleyPackage}/bin/kley web --bind ${lib.escapeShellArg config.kley.agentVm.webBindAddr}";
        Restart = "on-failure";
        RestartSec = "2s";
        User = "agent";
        Group = "users";
        WorkingDirectory = "/home/agent";
      };
      environment = lib.optionalAttrs (config.kley.agentVm.webPublicOrigin != null) {
        KLEY_WEB_PUBLIC_ORIGIN = config.kley.agentVm.webPublicOrigin;
      } // vaultEnvironment;
    };

    kley.agentVm.buildMetadata = buildMetadata;
    system.configurationRevision = promotionContract.source.exactRevision;
    environment.variables = vaultEnvironment // lib.optionalAttrs (config.kley.agentVm.webPublicOrigin != null) {
      KLEY_WEB_PUBLIC_ORIGIN = config.kley.agentVm.webPublicOrigin;
    };
    environment.etc."kley-agent-vm-build.json".text = builtins.toJSON buildMetadata;
  };
}
