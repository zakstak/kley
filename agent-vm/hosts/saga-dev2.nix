{ ... }:
{
  # Machine facts + lane toggle only.
  networking.hostName = "saga-dev2";
  networking.useDHCP = false;
  networking.interfaces.eth0.ipv4.addresses = [ {
    address = "10.0.0.51";
    prefixLength = 24;
  } ];
  networking.defaultGateway = "10.0.0.1";
  networking.nameservers = [ "1.1.1.1" "8.8.8.8" ];
  networking.firewall.allowedTCPPorts = [ 3000 ];

  boot.loader.grub.enable = true;
  boot.loader.grub.devices = [ "/dev/disk/by-id/virtio-0" ];

  fileSystems."/" = {
    device = "/dev/disk/by-label/saga-dev2-root";
    fsType = "ext4";
  };

  kley.agentVm.promotionLane = "canary";
  system.stateVersion = "24.11";
}
