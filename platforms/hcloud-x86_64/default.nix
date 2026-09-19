# Upload disk.img as a snapshot: a helper server writes it to its own disk,
# then hcloud snapshots that disk. Servers from the snapshot need user-data.
{ pkgs, common }:
let
  userdataUrl = "http://169.254.169.254/hetzner/v1/userdata";
in
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
  initrd = common.initrd { inherit userdataUrl; };
  debugInitrd = common.initrd {
    inherit userdataUrl;
    debug = true;
  };
  linuxDisk = common.disk {
    inherit kernel initrd;
    cmdline = "quiet";
  };
  ukiDisk = linuxDisk.override { uki = true; };
  # the UKI goes through the kernel's EFI stub, 0.17 s slower on UEFI than
  # Limine's linux protocol
  disk = linuxDisk;
  debugDisk = disk.override { initrd = debugInitrd; };
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
