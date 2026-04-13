{ pkgs, ... }:
{
  # Machine facts only. This host is an operator SSH box, not an agent runtime.
  networking.hostName = "agent-pi";
  networking.useDHCP = false;
  networking.interfaces.eth0.ipv4.addresses = [ {
    address = "10.0.0.52";
    prefixLength = 24;
  } ];
  networking.interfaces.eth0.ipv4.routes = [ {
    address = "10.0.1.0";
    prefixLength = 24;
    via = "10.0.0.254";
  } ];
  networking.defaultGateway = "10.0.0.1";
  networking.nameservers = [ "1.1.1.1" "8.8.8.8" ];

  services.qemuGuest.enable = true;

  boot.loader.grub.enable = true;
  boot.loader.grub.devices = [ "/dev/sda" ];
  boot.initrd.availableKernelModules = [
    "virtio_pci"
    "virtio_scsi"
    "sd_mod"
    "sr_mod"
  ];
  boot.kernelParams = [
    "console=tty0"
    "console=ttyS0,115200n8"
    "systemd.log_level=debug"
    "systemd.log_target=console"
  ];

  fileSystems."/" = {
    device = "/dev/disk/by-label/agent-pi-root";
    fsType = "ext4";
  };

  users.users.agent.shell = pkgs.zsh;

  programs.zsh = {
    enable = true;
    histSize = 10000;
    shellInit = ''
      zsh-newuser-install() { :; }
    '';
    shellAliases = {
      ls = "eza --group-directories-first";
      la = "eza -a --group-directories-first";
      ll = "eza -lah --group-directories-first --git";
      cat = "bat --style=plain --paging=never";
    };
    promptInit = ''
      autoload -Uz colors && colors
      autoload -Uz vcs_info
      precmd_functions+=(vcs_info)
      zstyle ':vcs_info:git:*' formats ' %F{magenta}[%b]%f'
      PROMPT='%F{cyan}%n@%m%f %F{yellow}%~%f''${vcs_info_msg_0_} %# '
    '';
    interactiveShellInit = ''
      if [[ "$USER" != "agent" ]]; then
        return
      fi

      source ${pkgs.zsh-autosuggestions}/share/zsh-autosuggestions/zsh-autosuggestions.zsh
      source ${pkgs.zsh-syntax-highlighting}/share/zsh-syntax-highlighting/zsh-syntax-highlighting.zsh
      eval "$(${pkgs.zoxide}/bin/zoxide init zsh)"

      export FZF_DEFAULT_COMMAND='${pkgs.fd}/bin/fd --type f --hidden --follow --exclude .git'
      export FZF_CTRL_T_COMMAND="$FZF_DEFAULT_COMMAND"
      export ZELLIJ_CONFIG_FILE="$HOME/.config/zellij/config.kdl"
      export ZELLIJ_AUTO_EXIT=true

      if [[ -o interactive ]] \
        && [[ -z "''${ZELLIJ:-}" ]] \
        && [[ -z "''${TMUX:-}" ]] \
        && [[ "''${TERM:-}" != "dumb" ]] \
        && [[ "''${DISABLE_AUTO_ZELLIJ:-0}" != "1" ]]; then
        exec ${pkgs.zellij}/bin/zellij attach -c main
      fi
    '';
  };

  environment.systemPackages = with pkgs; [
    eza
    fzf
    pi-coding-agent
    zoxide
    zsh-autosuggestions
    zsh-syntax-highlighting
  ];

  system.activationScripts.agentInteractiveLoginBootstrap.text = ''
    install -d -m 700 -o agent -g users /home/agent
    install -d -m 700 -o agent -g users /home/agent/.config
    install -d -m 700 -o agent -g users /home/agent/.config/zellij

    cat > /home/agent/.bash_profile <<'EOF'
    if [ -x /run/current-system/sw/bin/zsh ]; then
      exec /run/current-system/sw/bin/zsh -l
    fi
    EOF

    cat > /home/agent/.profile <<'EOF'
    if [ -x /run/current-system/sw/bin/zsh ]; then
      exec /run/current-system/sw/bin/zsh -l
    fi
    EOF

    cat > /home/agent/.zshrc <<'EOF'
    # Managed by NixOS so zsh skips the first-run wizard and keeps using /etc/zshrc.
    EOF

    cat > /home/agent/.config/zellij/config.kdl <<'EOF'
    show_startup_tips false
    show_release_notes false
    simplified_ui true
    mouse_mode false
    on_force_close "detach"
    default_shell "/run/current-system/sw/bin/zsh"
    EOF

    zellij_version="$(${pkgs.zellij}/bin/zellij --version | ${pkgs.coreutils}/bin/cut -d ' ' -f 2)"
    install -d -m 700 -o agent -g users "/home/agent/.cache/zellij/$zellij_version"
    : > "/home/agent/.cache/zellij/$zellij_version/seen_release_notes"

    chown agent:users /home/agent/.bash_profile /home/agent/.profile /home/agent/.zshrc /home/agent/.config/zellij/config.kdl "/home/agent/.cache/zellij/$zellij_version/seen_release_notes"
    chmod 644 /home/agent/.bash_profile /home/agent/.profile /home/agent/.zshrc /home/agent/.config/zellij/config.kdl "/home/agent/.cache/zellij/$zellij_version/seen_release_notes"
  '';

  kley.agentVm.promotionLane = "standalone";
  kley.agentVm.enableKleyRuntime = false;
  system.stateVersion = "24.11";
}
