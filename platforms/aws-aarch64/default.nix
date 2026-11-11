# EC2 Graviton sizes, VM and .metal: Nitro boots the AMI with UEFI from
# EFI/BOOT/BOOTAA64.EFI. ./upload.sh makes the AMI from disk.img.
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
  # mem=2G: arm64 kexec_file_load copies the linear map one 4 KiB page at a
  # time with a barrier each, 12.3 s for the 128 GiB of a c7g.metal (97 % in
  # trans_pgd_create_copy) and 6 ms for 2 GiB. The target gets its own command
  # line and all the memory.
  linuxDisk = common.disk {
    inherit kernel initrd;
    # the ACPI SPCR table does not make ttyS0 the console by itself
    cmdline = "quiet console=ttyS0,115200n8 mem=2G";
  };
  ukiDisk = linuxDisk.override {
    uki = true;
    ukiKernel = zbootKernel;
  };
  # UEFI only: no Limine hop
  disk = ukiDisk.override { efiDirect = true; };
  debugDisk = disk.override { initrd = debugInitrd; };
  target =
    (pkgs.nixos {
      imports = [ ../../targets/aws ];
      # systemd brings util-linux-minimal, and the getty, fsck and activation
      # units pull the full one next to it: 28 MiB. These are the flags of
      # util-linuxMinimal, whose definition goes through the final util-linux
      # and would recurse.
      nixpkgs.pkgs = pkgs.extend (
        _: prev: {
          util-linux = prev.util-linux.override {
            cryptsetupSupport = false;
            nlsSupport = false;
            ncursesSupport = false;
            pamSupport = false;
            shadowSupport = false;
            systemdSupport = false;
            translateManpages = false;
            withLastlog = false;
          };
        }
      );
      boot.kernelPackages = pkgs.linuxPackagesFor kernel;
      spore.bootGood.stamp = common.spore;
    }).config.system.build.toplevel;
  # TCG: the arm builders have no /dev/kvm
  test = {
    qemu = "qemu-system-aarch64 -machine virt -cpu max,pauth-impdef=on";
    console = "ttyAMA0";
    timeout = 900;
    firmwares.uefi = "-bios ${pkgs.qemu_kvm}/share/qemu/edk2-aarch64-code.fd";
  };
}
