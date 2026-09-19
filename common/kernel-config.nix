# Resolves a platform's .config: the arch defconfig plus the platform's
# config targets, then the base fragment, then the platform fragment. Later
# entries win, so a platform can re-enable what the base turns off. Fails if
# an enable did not land (a missing dependency).
{
  lib,
  stdenv,
  linux,
  bc,
  flex,
  bison,
  perl,
  name,
  defconfigs,
  fragment,
}:
let
  base = import ./kernel-base.nix;
  arch = stdenv.hostPlatform.linuxArch;
  flags = f: lib.concatMapStringsSep " " (o: "--${f} ${o}");
  wanted = lib.subtractLists (base.disable ++ fragment.disable) (base.enable ++ fragment.enable);
in
stdenv.mkDerivation {
  name = "${name}-kernel-config";
  inherit (linux) src;
  nativeBuildInputs = linux.nativeBuildInputs ++ [
    bc
    flex
    bison
    perl
  ];
  buildPhase = ''
    patchShebangs scripts/config
    make ARCH=${arch} ${toString defconfigs}
    scripts/config ${flags "enable" base.enable} ${flags "disable" base.disable} \
      ${flags "enable" fragment.enable} ${flags "disable" fragment.disable}
    make ARCH=${arch} olddefconfig
    for o in ${toString wanted}; do
      grep -q "^CONFIG_$o=y" .config || echo "not set: $o"
    done > missing
    if [ -s missing ]; then cat missing >&2; exit 1; fi
  '';
  installPhase = "cp .config $out";
}
