# A cpio initrd with `init` as /init. `tools` go to /bin (name -> binary),
# `files` anywhere (path in the initrd -> source). Every ELF in it must be
# static: the initrd has no loader and no shared libraries.
{
  lib,
  runCommand,
  cpio,
  zstd,
  file,
  init,
  tools ? { },
  files ? { },
}:
let
  entries = {
    init = init;
  }
  // lib.mapAttrs' (n: lib.nameValuePair "bin/${n}") tools
  // files;
in
runCommand "spore-initrd"
  {
    nativeBuildInputs = [
      cpio
      zstd
      file
    ];
    passthru = { inherit init tools files; };
  }
  ''
    root=$PWD/root
    mkdir -p $root/{bin,dev,etc,mnt,proc,run,sys,tmp}
    ${lib.concatStrings (
      lib.mapAttrsToList (dst: src: ''
        install -D -m 755 ${src} $root/${dst}
        if file -L $root/${dst} | grep -q 'dynamically linked'; then
          echo "${dst} is not static" >&2
          exit 1
        fi
      '') entries
    )}
    ${lib.optionalString (tools ? busybox) ''
      for a in $($root/bin/busybox --list); do
        [ -e $root/bin/$a ] || ln -s busybox $root/bin/$a
      done
    ''}
    # mtools is one binary that dispatches on argv[0]
    ${lib.optionalString (tools ? mcopy) "for a in mmd mtype mdir; do ln -s mcopy $root/bin/$a; done"}
    ${lib.optionalString (tools ? nix) "for a in nix-store nix-env; do ln -s nix $root/bin/$a; done"}
    mkdir -p $out
    (cd $root && find . | sort | cpio -o -H newc --quiet -R 0:0) | zstd -19 -T0 -q > $out/initrd
  ''
