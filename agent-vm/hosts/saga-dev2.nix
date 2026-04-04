{ ... }:
{
  # Machine facts + lane toggle only.
  networking.hostName = "saga-dev2";
  networking.useDHCP = true;
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
