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
  # arm64 kexec copies the linear map for its relocation code, one entry per
  # 4 KiB page with a barrier each: 12.3 s for the 128 GiB of a c7g.metal
  # (perf: 97 % in trans_pgd_create_copy), 6 ms for the 2 GiB of a t4g.small.
  # The bootstrap needs no more than 2 GiB; the target gets its own command
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
  # UEFI only, so the UKI boots directly: Limine would be one hop more
  disk = ukiDisk.override { efiDirect = true; };
  debugDisk = disk.override { initrd = debugInitrd; };
  target =
    (pkgs.nixos {
      imports = [ ../../targets/aws ];
      # systemd brings util-linux-minimal; the getty, fsck and activation
      # units named the full one next to it, 28 MiB more in the closure
      nixpkgs.pkgs = pkgs.extend (
        _: prev: {
          # the same build as util-linuxMinimal (whose definition goes through
          # the final util-linux and would recurse)
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
