{ config, lib, pkgs, ... }:
let
  rootLabel = config.kley.agentVm.rootFsLabel;
  grubDevices = config.boot.loader.grub.devices or [ ];
  diskDevice = if builtins.length grubDevices > 0 then builtins.head grubDevices else null;
in
{
  options.kley.agentVm.rootFsLabel = lib.mkOption {
    type = lib.types.str;
    default = "${config.networking.hostName}-root";
    description = ''
      Ext4 filesystem label for the agent VM root partition. Keep this at 16
      characters or fewer so the declared mount path matches the on-disk label.
    '';
  };

  config = {
    assertions = [
      {
        assertion = diskDevice != null;
        message = ''
          agent-vm/modules/disko.nix requires a host-scoped disk device via
          boot.loader.grub.devices so shared storage logic never hardcodes
          machine-specific disk paths.
        '';
      }
      {
        assertion = builtins.stringLength rootLabel <= 16;
        message = ''
          agent-vm/modules/disko.nix requires kley.agentVm.rootFsLabel to fit in
          the ext4 16-character label limit so boot-time by-label mounts resolve
          exactly.
        '';
      }
    ];

    # Minimal disk provisioning script used by nixos-anywhere.
    # We keep a single ext4 root partition labeled per-host so host files can keep
    # machine facts declarative via fileSystems."/" label references.
    system.build.diskoScript = pkgs.writeShellScript "disko-${config.networking.hostName}" ''
      set -euo pipefail

      mode="disko"
      while [ "$#" -gt 0 ]; do
        case "$1" in
          --mode)
            mode="$2"
            shift 2
            ;;
          --mode=*)
            mode="''${1#*=}"
            shift
            ;;
          *)
            shift
            ;;
        esac
      done

      disk="${diskDevice}"
      part="''${disk}1"

      if [ "$mode" = "mount" ]; then
        mkdir -p /mnt
        mount "$part" /mnt 2>/dev/null || mount "LABEL=${rootLabel}" /mnt
        exit 0
      fi

      umount -R /mnt 2>/dev/null || true
      swapoff -a 2>/dev/null || true

      wipefs -af "$disk"
      parted -s "$disk" mklabel msdos
      parted -s "$disk" mkpart primary ext4 1MiB 100%
      partprobe "$disk"
      udevadm settle

      mkfs.ext4 -F -L "${rootLabel}" "$part"
      mkdir -p /mnt
      mount "$part" /mnt
    '';
  };
}
