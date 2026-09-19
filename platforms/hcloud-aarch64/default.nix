# hcloud cax types boot UEFI only (EDK II with ACPI). Upload disk.img like
# the x86_64 image, from a cax helper server.
{ pkgs, common }:
let
  userdataUrl = "http://169.254.169.254/hetzner/v1/userdata";
in
rec {
  kernelConfig = common.kernelConfig {
    name = "hcloud-aarch64";
    defconfigs = [
      "defconfig"
      "virt.config"
    ];
    fragment = import ./kernel.nix;
  };
  kernel = common.kernel { configfile = ./kernel.config; };
  # the kernel of the UKI: 6.5 MB, the Image is 17 MB
  zbootKernel = common.kernel {
    configfile = ./kernel.config;
    zboot = true;
  };
  initrd = common.initrd { inherit userdataUrl; };
  debugInitrd = common.initrd {
    inherit userdataUrl;
    debug = true;
  };
  linuxDisk = common.disk {
    inherit kernel initrd;
    cmdline = "quiet";
  };
  ukiDisk = linuxDisk.override {
    uki = true;
    ukiKernel = zbootKernel;
  };
  # the UKI with zboot boots as fast and puts 8.6 MB on the ESP, not 19 MB
  disk = ukiDisk;
  debugDisk = disk.override { initrd = debugInitrd; };
  target =
    (pkgs.nixos {
      imports = [ ../../targets/hcloud ];
      boot.kernelPackages = pkgs.linuxPackagesFor kernel;
      # the kernel has no softdog yet: its kernel.config needs a native build
      spore.bootGood.deadline = null;
    }).config.system.build.toplevel;
  # TCG: the arm builders have no /dev/kvm
  test = {
    qemu = "qemu-system-aarch64 -machine virt -cpu max,pauth-impdef=on";
    console = "ttyAMA0";
    timeout = 900;
    firmwares.uefi = "-bios ${pkgs.qemu_kvm}/share/qemu/edk2-aarch64-code.fd";
  };
}
