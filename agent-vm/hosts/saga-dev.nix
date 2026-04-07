{ ... }:
{
  # Machine facts + lane toggle only.
  networking.hostName = "saga-dev";
  networking.useDHCP = false;
  networking.interfaces.eth0.ipv4.addresses = [ {
    address = "10.0.0.50";
    prefixLength = 24;
  } ];
  networking.interfaces.eth0.ipv4.routes = [ {
    address = "10.0.1.0";
    prefixLength = 24;
    via = "10.0.0.254";
  } ];
  networking.defaultGateway = "10.0.0.1";
  networking.nameservers = [ "1.1.1.1" "8.8.8.8" ];
  networking.firewall.allowedTCPPorts = [ 3000 3210 ];

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
  kley.agentVm.webPublicOrigin = "http://10.0.0.50:3210";
  system.stateVersion = "24.11";
}
