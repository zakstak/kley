{ config, lib, pkgs, kleyPackage, sourceResolution, ... }:
let
  inherit (lib) mkIf mkOption types;
  hostName = config.networking.hostName or "unknown";
  operatorAuthorizedKeyRuntimePath = "/var/lib/kley/operator-authorized-key.pub";
  githubMachineUser = "saga-agent";
  githubTokenRuntimePath = "/var/lib/kley/github-token";
  tailscaleAuthKeyRuntimePath = "/var/lib/kley/tailscale-auth-key";
  vaultEnvironment = lib.filterAttrs
    (name: value:
      builtins.elem name [ "VAULT_ADDR" ]
      && builtins.isString value
      && value != "")
    {
      VAULT_ADDR = builtins.getEnv "VAULT_ADDR";
    };
  runtimeHarnesses = config.kley.agentVm.runtimeHarnesses;
  publicRuntimeName = config.kley.agentVm.publicRuntime;
  publicRuntime =
    if publicRuntimeName == null then null else runtimeHarnesses.${publicRuntimeName} or null;
  uniqueHarnesses = lib.unique config.kley.agentVm.harnesses;
  harnessRoots = builtins.map (name: "/var/lib/kley/${name}") uniqueHarnesses;
  githubBootstrapTargets = [
    {
      name = "baseline";
      home = "/home/agent";
      configHome = "/home/agent/.config";
      keyTitle = "saga-agent-${hostName}-baseline";
    }
  ] ++ builtins.map (name: {
    inherit name;
    home = "/var/lib/kley/${name}/home";
    configHome = "/var/lib/kley/${name}/config";
    keyTitle = "saga-agent-${hostName}-${name}";
  }) uniqueHarnesses;
  githubBootstrapCommands = lib.concatStringsSep "\n" (builtins.map (target: ''
    bootstrap_target ${lib.escapeShellArg target.name} ${lib.escapeShellArg target.home} ${lib.escapeShellArg target.configHome} ${lib.escapeShellArg target.keyTitle}
  '') githubBootstrapTargets);
  githubKnownHostsCommands = lib.concatStringsSep "\n" (builtins.map (target: ''
    install -d -m 0700 -o agent -g users ${lib.escapeShellArg target.home}
    install -d -m 0700 -o agent -g users ${lib.escapeShellArg "${target.home}/.ssh"}
    touch ${lib.escapeShellArg "${target.home}/.ssh/known_hosts"}
    cat ${lib.escapeShellArg githubKnownHostsFile} >> ${lib.escapeShellArg "${target.home}/.ssh/known_hosts"}
    sort -u ${lib.escapeShellArg "${target.home}/.ssh/known_hosts"} -o ${lib.escapeShellArg "${target.home}/.ssh/known_hosts"}
    chown agent:users ${lib.escapeShellArg "${target.home}/.ssh/known_hosts"}
    chmod 600 ${lib.escapeShellArg "${target.home}/.ssh/known_hosts"}
  '') githubBootstrapTargets);
  portableZshrc = pkgs.writeText "agent-zshrc" ''
    # Portable agent shell profile derived from Zack's desktop zsh setup.
    export XDG_CONFIG_HOME="''${XDG_CONFIG_HOME:-$HOME/.config}"
    export XDG_CACHE_HOME="''${XDG_CACHE_HOME:-$HOME/.cache}"

    export ZSH="${pkgs.oh-my-zsh}/share/oh-my-zsh"
    export ZSH_CACHE_DIR="$XDG_CACHE_HOME/oh-my-zsh"
    export ZSH_THEME=""

    DISABLE_MAGIC_FUNCTIONS="true"
    ENABLE_CORRECTION="false"
    COMPLETION_WAITING_DOTS="true"

    if [[ $options[zle] = on ]]; then
      plugins=(git extract)
      source "$ZSH/oh-my-zsh.sh"
    fi

    HISTFILE="$HOME/.zsh_history"
    HISTSIZE=50000
    SAVEHIST=50000
    setopt APPEND_HISTORY
    setopt EXTENDED_HISTORY
    setopt HIST_EXPIRE_DUPS_FIRST
    setopt HIST_IGNORE_ALL_DUPS
    setopt HIST_IGNORE_SPACE
    setopt HIST_VERIFY
    setopt INC_APPEND_HISTORY
    setopt SHARE_HISTORY
    export HISTIGNORE="&:[bf]g:c:clear:history:exit:q:pwd:* --help"

    export LESS_TERMCAP_md="$(tput bold 2> /dev/null; tput setaf 2 2> /dev/null)"
    export LESS_TERMCAP_me="$(tput sgr0 2> /dev/null)"

    export PATH="$HOME/.npm-global/bin:$HOME/.local/bin:$HOME/go/bin:$HOME/.pragma/bin:$PATH"
    export LS_COLORS="di=38;5;141:ln=38;5;110:so=38;5;179:pi=38;5;179:ex=38;5;108:bd=38;5;137:cd=38;5;137:su=38;5;167:sg=38;5;167:tw=38;5;141:ow=38;5;141"

    if [[ $options[zle] = on ]]; then
      [[ -r ${pkgs.fzf}/share/fzf/completion.zsh ]] && source ${pkgs.fzf}/share/fzf/completion.zsh
      [[ -r ${pkgs.fzf}/share/fzf/key-bindings.zsh ]] && source ${pkgs.fzf}/share/fzf/key-bindings.zsh
      [[ -r ${pkgs.zsh-autosuggestions}/share/zsh-autosuggestions/zsh-autosuggestions.zsh ]] && source ${pkgs.zsh-autosuggestions}/share/zsh-autosuggestions/zsh-autosuggestions.zsh
      [[ -r ${pkgs.zsh-syntax-highlighting}/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh ]] && source ${pkgs.zsh-syntax-highlighting}/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh
    fi

    if [[ $options[zle] = on ]] && command -v starship >/dev/null 2>&1; then
      eval "$(${pkgs.starship}/bin/starship init zsh)"
    fi

    typeset -gA ZSH_HIGHLIGHT_STYLES
    ZSH_HIGHLIGHT_STYLES[command]='none'
    ZSH_HIGHLIGHT_STYLES[alias]='none'
    ZSH_HIGHLIGHT_STYLES[builtin]='none'
    ZSH_HIGHLIGHT_STYLES[function]='none'
    ZSH_HIGHLIGHT_STYLES[precommand]='none'
    ZSH_HIGHLIGHT_STYLES[unknown-token]='fg=red'
  '';
  portableStarshipToml = pkgs.writeText "agent-starship.toml" ''
    add_newline = false
    command_timeout = 500
    format = "$character"
    right_format = "$directory$git_branch''${custom.git_main_branch}$git_status"

    [character]
    error_symbol = "[❯ ](bold red)"
    success_symbol = "[❯ ](#6e6a86)"

    [directory]
    truncation_length = 1
    truncation_symbol = ""
    style = "#6e6a86"
    format = "[$path]($style)"

    [git_branch]
    style = "bold #a277ff"
    format = " [$branch]($style)"
    ignore_branches = ["main", "master"]

    [custom.git_main_branch]
    command = "git branch --show-current"
    when = 'git branch --show-current | grep -qE "^(main|master)$"'
    style = "bold #a277ff"
    format = " [$output]($style)"

    [git_status]
    style = "#6e6a86"
    format = ' [$all_status$ahead_behind]($style)'
    conflicted = "⌁''${count} "
    untracked  = "?''${count} "
    modified   = "~''${count} "
    stashed    = "≡''${count} "
    staged     = "+''${count} "
    renamed    = "»''${count} "
    deleted    = "-''${count} "
    ahead      = "⇡''${count} "
    diverged   = "⇕⇡''${ahead_count}⇣''${behind_count} "
    behind     = "⇣''${count} "
  '';
  portableTmuxConf = pkgs.writeText "agent-tmux.conf" ''
    set -g extended-keys on
  '';
  shellBootstrapCommands = lib.concatStringsSep "\n" (builtins.map (target: ''
    install -d -m 0700 -o agent -g users ${lib.escapeShellArg target.home}
    install -d -m 0700 -o agent -g users ${lib.escapeShellArg target.configHome}
    install -m 0600 -o agent -g users ${lib.escapeShellArg "${portableZshrc}"} ${lib.escapeShellArg "${target.home}/.zshrc"}
    install -m 0600 -o agent -g users ${lib.escapeShellArg "${portableStarshipToml}"} ${lib.escapeShellArg "${target.configHome}/starship.toml"}
    install -m 0600 -o agent -g users ${lib.escapeShellArg "${portableTmuxConf}"} ${lib.escapeShellArg "${target.home}/.tmux.conf"}
  '') githubBootstrapTargets);
  piNpmPackage = "@earendil-works/pi-coding-agent";
  piNpmPrefix = "/home/agent/.npm-global";
  piNpmBin = "${piNpmPrefix}/bin";
  piCodingAgentWrapper = pkgs.writeShellScriptBin "pi" ''
    exec ${piNpmBin}/pi "$@"
  '';
  sourceMetadata = {
    exactRevision = sourceResolution.kley.exactRevision;
    shortRevision = sourceResolution.kley.shortRevision;
    lastModified = sourceResolution.kley.lastModified;
  };
  resolvedInputs = {
    nixpkgs = sourceResolution.nixpkgs;
  };
  buildMetadata = {
    hostName = hostName;
    harnesses = uniqueHarnesses;
    publicRuntime = publicRuntimeName;
    runtimeHarnesses = lib.mapAttrs (_: runtime: {
      bindAddr = runtime.bindAddr;
      publicOrigin = runtime.publicOrigin;
      stateRoot = runtime.stateRoot;
      wrapperName = runtime.wrapperName;
      serviceName = runtime.serviceName;
    }) runtimeHarnesses;
    source = sourceMetadata;
    resolvedInputs = resolvedInputs;
  };
  githubKnownHostsFile = pkgs.writeText "github-known-hosts" ''
    github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl
    github.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=
    github.com ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=
  '';
  runtimeWrapper = name: runtime:
    pkgs.writeShellScriptBin runtime.wrapperName ''
      export HOME=${lib.escapeShellArg "${runtime.stateRoot}/home"}
      export XDG_CONFIG_HOME=${lib.escapeShellArg "${runtime.stateRoot}/config"}
      export XDG_STATE_HOME=${lib.escapeShellArg "${runtime.stateRoot}/state"}
      export XDG_CACHE_HOME=${lib.escapeShellArg "${runtime.stateRoot}/cache"}
      export KLEY_HARNESS=${lib.escapeShellArg name}
      ${lib.optionalString (runtime.publicOrigin != null) "export KLEY_WEB_PUBLIC_ORIGIN=${lib.escapeShellArg runtime.publicOrigin}"}
      exec ${kleyPackage}/bin/kley "$@"
    '';
in {
  options.kley.agentVm = {
    harnesses = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = ''
        Harness identities layered onto this shared agent VM environment.
      '';
    };

    publicRuntime = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = ''
        Name of the runtime harness exposed through the shared public nginx vhost.
      '';
    };

    runtimeHarnesses = mkOption {
      type = types.attrsOf (types.submodule ({ ... }: {
        options = {
          bindAddr = mkOption {
            type = types.str;
          };

          publicOrigin = mkOption {
            type = types.nullOr types.str;
            default = null;
          };

          stateRoot = mkOption {
            type = types.str;
          };

          wrapperName = mkOption {
            type = types.str;
          };

          serviceName = mkOption {
            type = types.str;
          };

          workingDirectory = mkOption {
            type = types.str;
          };
        };
      }));
      default = { };
      description = ''
        Runtime harness definitions that co-exist on the shared agent VM.
      '';
    };

    buildMetadata = mkOption {
      type = types.attrsOf types.anything;
      readOnly = true;
      description = ''
        Build-time metadata for the shared host baseline plus additive harnesses.
      '';
    };
  };

  config = lib.mkMerge [
    {
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
        shell = pkgs.zsh;
      };

      programs.zsh.enable = true;

      services.openssh.enable = true;
      services.openssh.settings = {
        PasswordAuthentication = false;
        KbdInteractiveAuthentication = false;
      };
      services.tailscale.enable = true;

      system.activationScripts.kleyOperatorAuthorizedKeys.text = ''
        install -d -m 0755 /var/lib/kley
        install -d -m 0755 /etc/ssh/authorized_keys.d

        maybe_seed_runtime_key() {
          local source_path="$1"
          if [ ! -s "${operatorAuthorizedKeyRuntimePath}" ] && [ -s "$source_path" ]; then
            install -m 0644 "$source_path" "${operatorAuthorizedKeyRuntimePath}"
          fi
        }

        maybe_seed_runtime_key /etc/ssh/authorized_keys.d/root
        maybe_seed_runtime_key /etc/ssh/authorized_keys.d/agent
        maybe_seed_runtime_key /root/.ssh/authorized_keys
        maybe_seed_runtime_key /home/agent/.ssh/authorized_keys

        if [ -s "${operatorAuthorizedKeyRuntimePath}" ]; then
          install -m 0644 "${operatorAuthorizedKeyRuntimePath}" /etc/ssh/authorized_keys.d/root
          install -m 0644 "${operatorAuthorizedKeyRuntimePath}" /etc/ssh/authorized_keys.d/agent
        else
          echo "[kley] operator SSH key missing at ${operatorAuthorizedKeyRuntimePath}; preserving existing SSH access only" >&2
        fi
      '';

      system.activationScripts.kleySharedRootPermissions.text = ''
        install -d -m 0755 /var/lib/kley
        chmod 0755 /var/lib/kley
      '';

      system.activationScripts.kleyGithubKnownHosts.text = ''
        ${githubKnownHostsCommands}
      '';

      system.activationScripts.kleyShellExperience.text = ''
        ${shellBootstrapCommands}
      '';

      security.sudo.wheelNeedsPassword = lib.mkDefault false;

      assertions = [
        {
          assertion = publicRuntimeName == null || lib.hasAttr publicRuntimeName runtimeHarnesses;
          message = "Configured public runtime must exist in kley.agentVm.runtimeHarnesses.";
        }
      ];

      kley.agentVm.buildMetadata = buildMetadata;
      system.configurationRevision = sourceMetadata.exactRevision;
      environment.variables = vaultEnvironment;
      environment.etc."kley-agent-vm-build.json".text = builtins.toJSON buildMetadata;
      systemd.tmpfiles.rules =
        [
          "d /var/lib/kley 0755 root root -"
        ]
        ++ builtins.concatLists (builtins.map (root: [
          "d ${root} 0700 agent users -"
          "d ${root}/home 0700 agent users -"
          "d ${root}/config 0700 agent users -"
          "d ${root}/state 0700 agent users -"
            "d ${root}/cache 0700 agent users -"
        ]) harnessRoots);
      environment.systemPackages =
        [ pkgs.gh pkgs.git pkgs.openssh pkgs.tmux piCodingAgentWrapper ]
        ++ builtins.attrValues (lib.mapAttrs runtimeWrapper runtimeHarnesses);
    }
    (mkIf (publicRuntime != null) {
      services.nginx.enable = true;
      services.nginx.recommendedProxySettings = true;
      services.nginx.virtualHosts.${hostName} = {
        locations."/".proxyPass = "http://${publicRuntime.bindAddr}";
        locations."/ws" = {
          proxyPass = "http://${publicRuntime.bindAddr}";
          proxyWebsockets = true;
        };
      };
    })
    {
      systemd.services =
        (lib.mapAttrs' (name: runtime: lib.nameValuePair runtime.serviceName {
          description = "Kley web UI (${name})";
          after = [ "network-online.target" ];
          wants = [ "network-online.target" ];
          wantedBy = [ "multi-user.target" ];
          serviceConfig = {
            Type = "simple";
            ExecStartPre = "${pkgs.writeShellScript "kley-web-pre-start-${name}" ''
              ${pkgs.procps}/bin/pkill -f -- ${lib.escapeShellArg "kley web --bind ${runtime.bindAddr}"} || true
            ''}";
            ExecStart = "${pkgs.bash}/bin/bash -lc ${lib.escapeShellArg (lib.concatStringsSep "; " ([
              "export HOME=${runtime.stateRoot}/home"
              "export XDG_CONFIG_HOME=${runtime.stateRoot}/config"
              "export XDG_STATE_HOME=${runtime.stateRoot}/state"
              "export XDG_CACHE_HOME=${runtime.stateRoot}/cache"
              "export KLEY_HARNESS=${name}"
            ] ++ lib.optional (runtime.publicOrigin != null) "export KLEY_WEB_PUBLIC_ORIGIN=${runtime.publicOrigin}" ++ [
              "exec ${kleyPackage}/bin/kley web --bind ${runtime.bindAddr}"
            ]))}";
            Restart = "on-failure";
            RestartSec = "2s";
            User = "agent";
            Group = "users";
            WorkingDirectory = runtime.workingDirectory;
          };
          environment = vaultEnvironment;
        }) runtimeHarnesses)
        // {
          agentGithubBootstrap = {
            description = "Bootstrap shared GitHub access for host and harnesses";
            after = [ "network-online.target" ];
            wants = [ "network-online.target" ];
            wantedBy = [ "multi-user.target" ];
            serviceConfig = {
              Type = "oneshot";
            };
            path = with pkgs; [
              bash
              coreutils
              gh
              git
              gnugrep
              gnused
              openssh
              python3
              shadow
            ];
            script = ''
              set -euo pipefail

              token_file=${githubTokenRuntimePath}
              expected_user=${githubMachineUser}

              if [ ! -s "$token_file" ]; then
                  echo "[agent-github] no GitHub token at $token_file; skipping" >&2
                exit 0
              fi

              token=$(tr -d '\r\n' < "$token_file")
              if [ -z "$token" ] || printf %s "$token" | grep -q placeholder; then
                  echo "[agent-github] GitHub token missing or placeholder; skipping" >&2
                exit 0
              fi

              login=$(GH_TOKEN="$token" GH_PROMPT_DISABLED=1 gh api user --jq .login 2>/dev/null || true)
              if [ "$login" != "$expected_user" ]; then
                  echo "[agent-github] token login '$login' does not match expected '$expected_user'" >&2
                exit 1
              fi

              bootstrap_target() {
                local target_name="$1"
                local target_home="$2"
                local target_config_home="$3"
                local key_title="$4"

                install -d -m 0700 -o agent -g users "$target_home"
                install -d -m 0700 -o agent -g users "$target_home/.ssh"
                install -d -m 0700 -o agent -g users "$target_config_home"
                install -d -m 0700 -o agent -g users "$target_config_home/gh"

                if [ ! -f "$target_home/.ssh/id_ed25519" ]; then
                  HOME="$target_home" ssh-keygen -t ed25519 -N "" -f "$target_home/.ssh/id_ed25519" -C "$key_title" >/dev/null 2>&1
                  chown agent:users "$target_home/.ssh/id_ed25519" "$target_home/.ssh/id_ed25519.pub"
                  chmod 600 "$target_home/.ssh/id_ed25519"
                  chmod 644 "$target_home/.ssh/id_ed25519.pub"
                fi

                printf '%s\n' \
                  'github.com:' \
                  "    user: $login" \
                  "    oauth_token: $token" \
                  '    git_protocol: ssh' \
                  > "$target_config_home/gh/hosts.yml"
                chown agent:users "$target_config_home/gh/hosts.yml"
                chmod 600 "$target_config_home/gh/hosts.yml"

                HOME="$target_home" XDG_CONFIG_HOME="$target_config_home" git config --global --unset-all url."git@github.com:".insteadOf >/dev/null 2>&1 || true
                HOME="$target_home" XDG_CONFIG_HOME="$target_config_home" git config --global --add url."git@github.com:".insteadOf "https://github.com/"
                HOME="$target_home" XDG_CONFIG_HOME="$target_config_home" git config --global --add url."git@github.com:".insteadOf "ssh://git@github.com/"
                chown agent:users "$target_home/.gitconfig"
                chmod 600 "$target_home/.gitconfig"

                pubkey=$(cut -d ' ' -f 1-2 "$target_home/.ssh/id_ed25519.pub")
                keys_json=$(GH_TOKEN="$token" GH_PROMPT_DISABLED=1 gh api user/keys 2>/dev/null || true)
                export GH_KEYS_JSON="$keys_json" GH_EXPECTED_PUBKEY="$pubkey"
                if ! python3 -c 'import json, os, sys; payload=os.environ.get("GH_KEYS_JSON", ""); needle=os.environ.get("GH_EXPECTED_PUBKEY", "").strip();
if not payload or not needle: sys.exit(1)
try: entries=json.loads(payload)
except json.JSONDecodeError: sys.exit(1)
sys.exit(0 if any(entry.get("key", "").strip() == needle for entry in entries) else 1)'
                then
                  GH_TOKEN="$token" GH_PROMPT_DISABLED=1 gh api -X POST user/keys -f title="$key_title" -f key="$pubkey" >/dev/null
                fi
              }

              ${githubBootstrapCommands}
            '';
          };
          tailscaleBootstrap = {
            description = "Authenticate Tailscale on first boot when a staged key is present";
            after = [ "network-online.target" "tailscaled.service" ];
            wants = [ "network-online.target" "tailscaled.service" ];
            wantedBy = [ "multi-user.target" ];
            serviceConfig = {
              Type = "oneshot";
              User = "root";
              Group = "root";
            };
            path = with pkgs; [
              bash
              coreutils
              gnugrep
              jq
              tailscale
            ];
            script = ''
              set -euo pipefail

              auth_key_file=${tailscaleAuthKeyRuntimePath}

              tailscale_running() {
                tailscale status --json | jq -e '.BackendState == "Running"' >/dev/null
              }

              if tailscale_running; then
                rm -f "$auth_key_file"
                exit 0
              fi

              if [ ! -s "$auth_key_file" ]; then
                echo "[tailscale-bootstrap] missing staged auth key at $auth_key_file" >&2
                exit 1
              fi

              auth_key=$(tr -d '\r\n' < "$auth_key_file")
              if [ -z "$auth_key" ] || printf %s "$auth_key" | grep -qi placeholder; then
                echo "[tailscale-bootstrap] staged auth key is empty or placeholder" >&2
                exit 1
              fi

              tailscale up --auth-key="$auth_key"

              if ! tailscale_running; then
                echo "[tailscale-bootstrap] tailscale did not reach BackendState=Running after bootstrap" >&2
                exit 1
              fi

              rm -f "$auth_key_file"
            '';
          };
          "pi-coding-agent-npm-install" = {
            description = "Install or update Pi Coding Agent with npm";
            after = [ "network-online.target" ];
            wants = [ "network-online.target" ];
            wantedBy = [ "multi-user.target" ];
            serviceConfig = {
              Type = "oneshot";
            };
            path = with pkgs; [
              bash
              coreutils
              gnugrep
              nodejs_22
              util-linux
            ];
            script = ''
              set -euo pipefail

              install -d -m 0755 /usr/local/bin
              install -d -m 0755 -o agent -g users /home/agent
              install -d -m 0755 -o agent -g users ${piNpmPrefix}
              install -d -m 0755 -o agent -g users ${piNpmBin}
              install -d -m 0755 -o agent -g users /home/agent/.npm

              runuser -u agent -- env \
                HOME=/home/agent \
                NPM_CONFIG_PREFIX=${piNpmPrefix} \
                PATH=${piNpmBin}:${pkgs.nodejs_22}/bin:/run/current-system/sw/bin:/usr/bin:/bin \
                npm install -g ${lib.escapeShellArg piNpmPackage}

              ln -sfn ${piNpmBin}/pi /usr/local/bin/pi
              ${piNpmBin}/pi --version >/dev/null
            '';
          };
        };
    }
  ];
}
