{ lib, pkgs, ... }:
let
  runtimeCliNpmPrefix = "/home/agent/.npm-global";
  runtimeCliNpmBin = "${runtimeCliNpmPrefix}/bin";
  runtimeCliOpencodePrefix = "/home/agent/.opencode";
  runtimeCliOpencodeBin = "/home/agent/.opencode/bin";
  codexNpmPackage = "@openai/codex";
in {
  system.activationScripts.kleyHermesRuntimeCleanup.text = ''
    rm -f /usr/local/bin/hermes
    rm -f /home/agent/.local/bin/hermes
    rm -rf /usr/local/lib/hermes-agent
  '';

  systemd.services."runtime-cli-tools-install" = {
    description = "Install or update runtime CLIs";
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    wantedBy = [ "multi-user.target" ];
    serviceConfig = {
      Type = "oneshot";
    };
    path = with pkgs; [
      bash
      coreutils
      curl
      gnutar
      gnugrep
      nodejs_22
      patchelf
      util-linux
    ];
    script = ''
      set -euo pipefail

      install -d -m 0755 /usr/local/bin
      install -d -m 0755 -o agent -g users /home/agent
      install -d -m 0755 -o agent -g users ${runtimeCliNpmPrefix}
      install -d -m 0755 -o agent -g users ${runtimeCliNpmBin}
      install -d -m 0755 -o agent -g users ${runtimeCliOpencodePrefix}
      install -d -m 0755 -o agent -g users ${runtimeCliOpencodeBin}
      install -d -m 0755 -o agent -g users /home/agent/.npm

      tmp_npmrc=$(mktemp)
      if [ -f /home/agent/.npmrc ]; then
        grep -v -E '^prefix=' /home/agent/.npmrc > "$tmp_npmrc" || true
      else
        : > "$tmp_npmrc"
      fi
      printf '%s\n' ${lib.escapeShellArg "prefix=${runtimeCliNpmPrefix}"} >> "$tmp_npmrc"
      install -m 0600 -o agent -g users "$tmp_npmrc" /home/agent/.npmrc
      rm -f "$tmp_npmrc"

      runuser -u agent -- env \
        HOME=/home/agent \
        NPM_CONFIG_PREFIX=${runtimeCliNpmPrefix} \
        PATH=${runtimeCliNpmBin}:${pkgs.nodejs_22}/bin:/run/current-system/sw/bin:/usr/bin:/bin \
        npm install -g ${lib.escapeShellArg codexNpmPackage}

      runuser -u agent -- env \
        HOME=/home/agent \
        NPM_CONFIG_PREFIX=${runtimeCliNpmPrefix} \
        PATH=${runtimeCliNpmBin}:${pkgs.nodejs_22}/bin:/run/current-system/sw/bin:/usr/bin:/bin \
        npm uninstall -g opencode-ai >/dev/null 2>&1 || true

      runuser -u agent -- env \
        HOME=/home/agent \
        PATH=${pkgs.coreutils}/bin:${pkgs.curl}/bin:${pkgs.gnutar}/bin:${pkgs.patchelf}/bin:/run/current-system/sw/bin:/usr/bin:/bin \
        bash -c '
          set -euo pipefail
          tmpdir=$(mktemp -d)
          trap "rm -rf \"$tmpdir\"" EXIT
          arch=$(uname -m)
          case "$arch" in
            x86_64) arch=x64 ;;
            aarch64|arm64) arch=arm64 ;;
            *)
              echo "Unsupported OpenCode arch: $arch" >&2
              exit 1
              ;;
          esac
          target="linux-$arch"
          if [ "$arch" = "x64" ] && ! grep -qwi avx2 /proc/cpuinfo 2>/dev/null; then
            target="$target-baseline"
          fi
          asset="opencode-''${target}-musl.tar.gz"
          curl -fsSL "https://github.com/anomalyco/opencode/releases/latest/download/''${asset}" -o "$tmpdir/opencode.tar.gz"
          tar -xzf "$tmpdir/opencode.tar.gz" -C "$tmpdir"
          install -m 0755 "$tmpdir/opencode" ${runtimeCliOpencodeBin}/opencode
          patchelf \
            --set-interpreter ${pkgs.musl}/lib/ld-musl-x86_64.so.1 \
            --set-rpath ${pkgs.musl}/lib:${pkgs.pkgsMusl.stdenv.cc.cc.lib}/lib \
            ${runtimeCliOpencodeBin}/opencode
        '

      ln -sfn ${runtimeCliNpmBin}/codex /usr/local/bin/codex
      ln -sfn ${runtimeCliOpencodeBin}/opencode /usr/local/bin/opencode

      ${runtimeCliNpmBin}/codex --version >/dev/null
      ${runtimeCliOpencodeBin}/opencode --version >/dev/null
    '';
  };
}
