# The files of a GitHub release. `platforms` is a list of
# { name, arch, value, binary }, value being the platform's default.nix.
{
  pkgs,
  version,
  platforms,
}:
let
  inherit (pkgs) lib;
  files =
    p:
    let
      stem = "spore-${version}-${p.name}-${p.arch}";
    in
    ''
      zstd -q -19 -T0 ${p.value.disk}/disk.img -o ${stem}.img.zst
      zstd -q -19 -T0 ${p.value.debugDisk}/disk.img -o ${stem}-debug.img.zst
      cp ${p.value.ukiDisk.uki}/spore.efi ${stem}.efi
      cp ${p.binary}/bin/spore spore-${version}-${p.arch}-linux
    '';
in
pkgs.runCommand "spore-${version}-release" { nativeBuildInputs = [ pkgs.zstd ]; } ''
  mkdir $out
  cd $out
  ${lib.concatMapStrings files platforms}
  sha256sum * > SHA256SUMS
''
