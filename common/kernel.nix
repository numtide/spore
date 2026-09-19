# configfile is the platform's committed kernel.config, which kernel-config.nix
# resolves from the fragments; the kernel-config check keeps the two in sync.
# Everything the bootstrap needs is built in: it loads no modules, and the
# firmware reads a small bzImage (on BIOS int13 moves ~35 MB/s).
# Not callPackage: linuxPackagesFor overrides the kernel with features and
# kernelPatches, which only linuxManualConfig takes.
#
# zboot: the same config plus EFI_ZBOOT, a self-decompressing EFI
# application (vmlinuz.efi). A UKI boots it; Limine's linux protocol and
# arm64 kexec_file_load take only the plain Image.
{
  pkgs,
  configfile,
  zboot ? false,
}:
let
  inherit (pkgs) lib;
  zbootConfig = pkgs.stdenv.mkDerivation {
    name = "kernel-zboot-config";
    inherit (pkgs.linux) src;
    nativeBuildInputs = pkgs.linux.nativeBuildInputs ++ [
      pkgs.bc
      pkgs.flex
      pkgs.bison
      pkgs.perl
    ];
    buildPhase = ''
      cp ${configfile} .config
      patchShebangs scripts/config
      scripts/config --enable EFI_ZBOOT --enable KERNEL_ZSTD
      make ARCH=${pkgs.stdenv.hostPlatform.linuxArch} olddefconfig
      grep -q '^CONFIG_EFI_ZBOOT=y' .config
      grep -q '^CONFIG_KERNEL_ZSTD=y' .config
    '';
    installPhase = "cp .config $out";
  };
in
pkgs.linuxManualConfig (
  {
    inherit (pkgs.linux) src version modDirVersion;
    configfile = if zboot then zbootConfig else configfile;
  }
  // lib.optionalAttrs zboot { target = "vmlinuz.efi"; }
)
