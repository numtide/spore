# A unified kernel image: systemd-stub with the kernel, the initrd and the
# command line in one PE. It boots from UEFI directly, and Limine chainloads
# it with the `efi` protocol.
{
  lib,
  stdenv,
  buildPackages,
  systemdUkify,
  runCommand,
  writeText,
  kernel,
  initrd,
  cmdline,
}:
let
  osRelease = writeText "os-release" ''
    ID=spore
    NAME=spore
  '';
in
runCommand "spore-uki"
  {
    nativeBuildInputs = [ buildPackages.systemdUkify ];
    passthru = { inherit kernel initrd cmdline; };
  }
  ''
    mkdir $out
    ukify build \
      --linux=${kernel}/${kernel.target} \
      --initrd=${initrd}/initrd \
      --cmdline=${lib.escapeShellArg cmdline} \
      --os-release=@${osRelease} \
      --uname=${kernel.modDirVersion} \
      --stub=${systemdUkify}/lib/systemd/boot/efi/linux${stdenv.hostPlatform.efiArch}.efi.stub \
      --output=$out/spore.efi
  ''
