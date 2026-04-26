{ lib, pkgs, ... }:
let
  agentHome = "/home/agent";
  hermesHome = "${agentHome}/.hermes";
  localBinDir = "${agentHome}/.local/bin";
  hermesInstallDir = "/usr/local/lib/hermes-agent";
  hermesInstallRef = "v2026.4.23";
  hermesInstallerUrl = "https://raw.githubusercontent.com/NousResearch/hermes-agent/${hermesInstallRef}/scripts/install.sh";
  hermesInstallerSha256 = "251c1b97dda5db092d152d34afa315612fe27329e821c5414130f2a7e0c011e2";
  hermesEntrypoint = "${hermesInstallDir}/venv/bin/hermes";
  hermesUserCommand = "${localBinDir}/hermes";
  hermesCommand = "/usr/local/bin/hermes";
  hermesInstaller = pkgs.writeShellScript "install-hermes-runtime" ''
    set -euo pipefail

    export HERMES_HOME=${lib.escapeShellArg hermesHome}
    export HERMES_INSTALL_DIR=${lib.escapeShellArg hermesInstallDir}
    export HERMES_INSTALL_REF=${lib.escapeShellArg hermesInstallRef}
    export HERMES_INSTALLER_URL=${lib.escapeShellArg hermesInstallerUrl}
    export HERMES_INSTALLER_SHA256=${lib.escapeShellArg hermesInstallerSha256}
    export TMPDIR=/tmp
    export PATH=${lib.escapeShellArg (lib.makeBinPath [
      pkgs.bash
      pkgs.coreutils
      pkgs.curl
      pkgs.ffmpeg
      pkgs.gawk
      pkgs.git
      pkgs.gnugrep
      pkgs.gnused
      pkgs.nodejs_22
      pkgs.openssh
      pkgs.python311
      pkgs.python3
      pkgs.ripgrep
      pkgs.uv
    ])}:$PATH

    mkdir -p ${lib.escapeShellArg hermesHome}
    installer_script=$(mktemp)
    trap 'rm -f "$installer_script"' EXIT

    if [ ! -x ${lib.escapeShellArg hermesEntrypoint} ]; then
      ${pkgs.curl}/bin/curl -fsSL "$HERMES_INSTALLER_URL" -o "$installer_script"
      installer_sha="$(${pkgs.coreutils}/bin/sha256sum "$installer_script" | ${pkgs.gawk}/bin/awk '{print $1}')"
      if [ "$installer_sha" != "$HERMES_INSTALLER_SHA256" ]; then
        echo "[kley] Hermes installer hash mismatch for $HERMES_INSTALLER_URL" >&2
        exit 1
      fi
      ${pkgs.bash}/bin/bash "$installer_script" --skip-setup --branch "$HERMES_INSTALL_REF" --dir "$HERMES_INSTALL_DIR" --hermes-home "$HERMES_HOME"
      chown -R agent:users ${lib.escapeShellArg hermesHome}
    fi

    if [ -x ${lib.escapeShellArg hermesEntrypoint} ]; then
      install -d -m 0755 /usr/local/bin
      ln -sf ${lib.escapeShellArg hermesEntrypoint} ${lib.escapeShellArg hermesCommand}
      install -d -m 0700 -o agent -g users ${lib.escapeShellArg localBinDir}
      ln -sf ${lib.escapeShellArg hermesEntrypoint} ${lib.escapeShellArg hermesUserCommand}
      chown -h agent:users ${lib.escapeShellArg hermesUserCommand}
    fi
  '';
in {
  environment.localBinInPath = true;
  environment.systemPackages = [ pkgs.opencode ];

  systemd.tmpfiles.rules = [
    "d ${agentHome}/.local 0700 agent users -"
    "d ${localBinDir} 0700 agent users -"
    "d ${hermesHome} 0700 agent users -"
  ];

  system.activationScripts.kleyHermesRuntimeInstall.text = ''
    if [ ! -x ${lib.escapeShellArg hermesEntrypoint} ]; then
      ${hermesInstaller}
    fi

    if [ -x ${lib.escapeShellArg hermesEntrypoint} ]; then
      install -d -m 0755 /usr/local/bin
      ln -sf ${lib.escapeShellArg hermesEntrypoint} ${lib.escapeShellArg hermesCommand}
      install -d -m 0700 -o agent -g users ${lib.escapeShellArg localBinDir}
      ln -sf ${lib.escapeShellArg hermesEntrypoint} ${lib.escapeShellArg hermesUserCommand}
      chown -h agent:users ${lib.escapeShellArg hermesUserCommand}
    fi
  '';
}
