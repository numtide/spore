{
  lib,
  stdenv,
  callPackage,
  runCommand,
  writeText,
  dosfstools,
  limine,
  mtools,
  util-linux,
  kernel,
  initrd,
  cmdline ? "quiet",
  # boot the bootstrap on UEFI as a UKI that Limine chainloads
  uki ? false,
  ukiKernel ? kernel,
}:
let
  inherit (stdenv.hostPlatform) isx86 efiArch;
  ukiImage = callPackage ./uki.nix {
    kernel = ukiKernel;
    inherit initrd cmdline;
  };
  # the kernel and initrd files serve the linux protocol entry, which a UKI
  # disk needs only for BIOS
  linuxEntry = !uki || isx86;
  linux = ''
    protocol: linux
    path: boot():/kernel
    module_path: boot():/initrd
    cmdline: ${cmdline}
    textmode: yes
  '';
  indent = s: lib.concatMapStrings (l: "    ${l}\n") (lib.init (lib.splitString "\n" s));
  # Limine starts the first entry it can boot: it hides efi entries on BIOS
  entries =
    if uki then
      "/bootstrap\n"
      + indent ''
        protocol: efi
        path: boot():/bootstrap.efi
      ''
      + lib.optionalString isx86 "\n/bootstrap-bios\n${indent "if_fw_type: BIOS\n${linux}"}"
    else
      "/bootstrap\n${indent linux}";
  limineConf = writeText "limine.conf" ''
    timeout: 0
    quiet: yes
    graphics: no

    ${entries}'';
in
# GPT: partition 1 is the ESP (FAT32, label ESP, 128 MiB: bootstrap and
# target kernel + initrd), partition 2 holds Limine's BIOS stage 2. The rest
# of the disk stays unpartitioned for the bootstrap to lay out. UEFI-only
# platforms keep partition 2 empty, so the system is partition 3 everywhere.
runCommand "spore-disk"
  {
    nativeBuildInputs = [
      dosfstools
      limine
      mtools
      util-linux
    ];
    passthru = {
      inherit kernel initrd limineConf;
    }
    // lib.optionalAttrs uki { uki = ukiImage; };
  }
  ''
    img=disk.img
    truncate -s 131M $img
    sfdisk --quiet $img <<EOF
    label: gpt
    label-id: 6E697874-7261-6D70-6F6C-696E65000000
    start=4096, size=262144, type=uefi, uuid=6E697874-7261-6D70-6F6C-696E65000001, name=ESP
    start=2048, size=2048, type=21686148-6449-6E6F-744E-656564454649, uuid=6E697874-7261-6D70-6F6C-696E65000002, name=bios
    EOF
    truncate -s 128M esp.img
    mkfs.vfat -F 32 -n ESP -i 6e747270 esp.img > /dev/null
    export MTOOLS_SKIP_CHECK=1
    mmd -i esp.img ::/EFI ::/EFI/BOOT
    ${lib.optionalString linuxEntry ''
      mcopy -i esp.img ${kernel}/${kernel.target} ::/kernel
      mcopy -i esp.img ${initrd}/initrd ::/initrd
    ''}
    ${lib.optionalString uki ''
      mcopy -i esp.img ${ukiImage}/spore.efi ::/bootstrap.efi
      # systemd-stub creates its random seed in /loader. Without the directory
      # it logs an error, and a logged error stalls the boot for 2.5 s
      # (log_wait in src/boot/efi-log.c).
      mmd -i esp.img ::/loader
    ''}
    mcopy -i esp.img ${limineConf} ::/limine.conf
    mcopy -i esp.img ${limine}/share/limine/BOOT${lib.toUpper efiArch}.EFI ::/EFI/BOOT/BOOT${lib.toUpper efiArch}.EFI
    ${lib.optionalString isx86 ''
      mcopy -i esp.img ${limine}/share/limine/limine-bios.sys ::/limine-bios.sys
    ''}
    dd if=esp.img of=$img bs=1M seek=2 conv=notrunc status=none
    ${lib.optionalString isx86 ''
      limine bios-install $img 2 > /dev/null
    ''}
    mkdir -p $out
    mv $img $out/disk.img
  ''
