{ pkgs }:
let
  inherit (pkgs) lib;
  mkInitrd = args: pkgs.callPackage ./initrd.nix args;
  spore = pkgs.callPackage ./spore.nix { };
  rescueBusybox = pkgs.callPackage ./rescue-busybox.nix { };
  # libarchive (for `mke2fs -d`) makes the static mke2fs 9.3 MB instead of 1 MB
  mke2fs = (pkgs.pkgsStatic.e2fsprogs.override { withFuse = false; }).overrideAttrs (o: {
    configureFlags = map (
      f: if f == "--with-libarchive=direct" then "--with-libarchive=no" else f
    ) o.configureFlags;
    buildInputs = lib.filter (p: (p.pname or "") != "libarchive") o.buildInputs;
    doCheck = false;
    env = (o.env or { }) // {
      NIX_CFLAGS_COMPILE = "-Os -ffunction-sections -fdata-sections";
      NIX_LDFLAGS = "--gc-sections";
    };
  });
in
{
  inherit spore;
  kernel = args: import ./kernel.nix ({ inherit pkgs; } // args);
  kernelConfig = args: pkgs.callPackage ./kernel-config.nix args;
  disk = args: pkgs.callPackage ./disk.nix args;

  # spore as /init. It has the CA roots built in (webpki-roots), so the
  # initrd has no CA bundle. Only the debug variant has busybox, for the
  # emergency and rescue shell.
  initrd =
    {
      userdataUrl,
      debug ? false,
    }:
    mkInitrd {
      init = "${spore}/bin/spore";
      tools = {
        mke2fs = "${mke2fs.bin}/bin/mke2fs";
      }
      // lib.optionalAttrs debug {
        busybox = "${rescueBusybox}/bin/busybox";
      };
      files."etc/spore/userdata-url" = pkgs.writeText "userdata-url" userdataUrl;
    };
}
