# EC2 Graviton sizes, VM and .metal: Nitro boots the AMI with UEFI from
# EFI/BOOT/BOOTAA64.EFI. Upload disk.img with ./upload.sh, which makes
# the EBS snapshot and registers the AMI. Instances need user-data.
{ pkgs, common }:
let
  userdataUrl = "imdsv2://169.254.169.254/latest/user-data";
in
rec {
  kernelConfig = common.kernelConfig {
    name = "aws-aarch64";
    defconfigs = [ "defconfig" ];
    fragment = import ./kernel.nix;
  };
  kernel = common.kernel { configfile = ./kernel.config; };
  zbootKernel = common.kernel {
    configfile = ./kernel.config;
    zboot = true;
  };
  initrd = common.initrd { inherit userdataUrl; };
  debugInitrd = common.initrd {
    inherit userdataUrl;
    debug = true;
  };
  # the EBS volume is longer than the image; the bootstrap moves the backup
  # GPT to its end on the first boot
  linuxDisk = common.disk {
    inherit kernel initrd;
    # the ACPI SPCR table does not make ttyS0 the console by itself
    cmdline = "quiet console=ttyS0,115200n8";
  };
  ukiDisk = linuxDisk.override {
    uki = true;
    ukiKernel = zbootKernel;
  };
  # UEFI only, so the UKI boots directly: Limine would be one hop more
  disk = ukiDisk.override { efiDirect = true; };
  debugDisk = disk.override { initrd = debugInitrd; };
  target =
    (pkgs.nixos {
      imports = [ ../../targets/aws ];
      boot.kernelPackages = pkgs.linuxPackagesFor kernel;
    }).config.system.build.toplevel;
  # TCG: the arm builders have no /dev/kvm
  test = {
    qemu = "qemu-system-aarch64 -machine virt -cpu max,pauth-impdef=on";
    console = "ttyAMA0";
    timeout = 900;
    firmwares.uefi = "-bios ${pkgs.qemu_kvm}/share/qemu/edk2-aarch64-code.fd";
  };
}
