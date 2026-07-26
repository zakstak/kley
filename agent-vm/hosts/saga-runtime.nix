{ ... }:
let
  rootFsLabel = "saga-runtime-roo";
in {
  networking.hostName = "saga-runtime";
  kley.agentVm.rootFsLabel = rootFsLabel;
  networking.usePredictableInterfaceNames = false;
  networking.useDHCP = false;
  systemd.network.links."10-primary-virtio" = {
    matchConfig.Driver = "virtio_net";
    linkConfig.Name = "eth0";
  };
  networking.interfaces.eth0.ipv4.addresses = [ {
    address = "10.0.0.51";
    prefixLength = 24;
  } ];
  networking.interfaces.eth0.ipv4.routes = [ {
    address = "10.0.1.0";
    prefixLength = 24;
    via = "10.0.0.1";
  } ];
  networking.defaultGateway = "10.0.0.1";
  networking.nameservers = [ "1.1.1.1" "8.8.8.8" ];
  networking.hosts."192.168.4.42" = [ "executor.home.zakstak.com" ];

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
    "net.ifnames=0"
    "biosdevname=0"
    "console=tty0"
    "console=ttyS0,115200n8"
    "systemd.log_level=debug"
    "systemd.log_target=console"
  ];

  fileSystems."/" = {
    device = "/dev/disk/by-label/${rootFsLabel}";
    fsType = "ext4";
  };

  system.stateVersion = "24.11";
}
