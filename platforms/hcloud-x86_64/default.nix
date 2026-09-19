# Upload disk.img as a snapshot: a helper server writes it to its own disk,
# then hcloud snapshots that disk. Servers from the snapshot need user-data.
{ pkgs, common }:
rec {
  kernelConfig = common.kernelConfig {
    name = "hcloud-x86_64";
    defconfigs = [
      "x86_64_defconfig"
      "kvm_guest.config"
    ];
    fragment = import ./kernel.nix;
  };
  kernel = common.kernel { configfile = ./kernel.config; };
  initrd = common.shellInitrd { userdata = ./userdata.sh; };
  disk = common.disk {
    inherit kernel initrd;
    cmdline = "quiet";
  };
  rustInitrd = common.rustInitrd {
    userdataUrl = "http://169.254.169.254/hetzner/v1/userdata";
  };
  rustDisk = common.disk {
    inherit kernel;
    initrd = rustInitrd;
    cmdline = "quiet";
  };
  ukiDisk = rustDisk.override { uki = true; };
  debugInitrd = common.rustInitrd {
    userdataUrl = "http://169.254.169.254/hetzner/v1/userdata";
    debug = true;
  };
  releaseDisk = rustDisk;
  debugDisk = releaseDisk.override { initrd = debugInitrd; };
  target =
    (pkgs.nixos {
      imports = [ ../../targets/hcloud ];
      boot.kernelPackages = pkgs.linuxPackagesFor kernel;
    }).config.system.build.toplevel;
  test = {
    qemu = "qemu-system-x86_64 -enable-kvm -machine q35 -cpu host";
    console = "ttyS0";
    features = [ "kvm" ];
    # cx types boot SeaBIOS, cpx types boot UEFI
    firmwares = {
      bios = "";
      uefi = "-bios ${pkgs.OVMF.fd}/FV/OVMF.fd";
    };
  };
}
