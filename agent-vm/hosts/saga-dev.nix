{ ... }:
{
  # Machine facts + lane toggle only.
  networking.hostName = "saga-dev";
  networking.useDHCP = true;
  networking.firewall.allowedTCPPorts = [ 3000 ];

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
    device = "/dev/disk/by-label/saga-dev-root";
    fsType = "ext4";
  };

  kley.agentVm.promotionLane = "baseline";
  system.stateVersion = "24.11";
}
