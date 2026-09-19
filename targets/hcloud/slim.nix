{
  config,
  lib,
  pkgs,
  curlSlim,
  ...
}:
{
  # one curl for nix and the scripts: no GSS (krb5), scp
  # (libssh2), brotli, IDN, PSL or HTTP/3
  _module.args.curlSlim = pkgs.curlMinimal.override {
    gssSupport = false;
    scpSupport = false;
  };

  # boost only needs ICU for boost.locale, which nix does not use; icu4c is
  # 40 MiB of the closure. Scoped to nix: a global overlay would also
  # rebuild qemu and everything else the image builder pulls in.
  nix.package =
    (pkgs.nixVersions.nixComponents_2_34.overrideScope (
      _: prev: {
        # nix links container, context, coroutine, iostreams, random, regex
        # and url; the full set is 14 MiB. b2 cannot mix --with and the
        # --without-python the package passes, so name what to leave out.
        boost = pkgs.boost.override {
          enableIcu = false;
          extraB2Args = map (l: "--without-${l}") [
            "cobalt"
            "contract"
            "fiber"
            "filesystem"
            "graph"
            "graph_parallel"
            "json"
            "locale"
            "log"
            "math"
            "mpi"
            "nowide"
            "process"
            "program_options"
            "serialization"
            "stacktrace"
            "test"
            "timer"
            "type_erasure"
            "wave"
          ];
        };
        curl = curlSlim;
        # the target substitutes over https; S3 support is the aws-c-* stack
        nix-store = prev.nix-store.override { withAWS = false; };
      }
    )).nix-everything;

  # systemd is also most of the initrd the loader reads.
  # This rebuilds systemd.
  systemd.package =
    (pkgs.systemd.override {
      withApparmor = false;
      withBootloader = false;
      withCoredump = false;
      withCryptsetup = false;
      withDocumentation = false;
      withEfi = false;
      withFido2 = false;
      withFirstboot = false;
      withGcrypt = false;
      withHomed = false;
      withHostnamed = false;
      withImportd = false;
      withImds = false;
      withKexectools = false;
      withLibBPF = false;
      withLibarchive = false;
      withLibidn2 = false;
      withLocaled = false;
      withMachined = false;
      withNspawn = false;
      withOomd = false;
      withPasswordQuality = false;
      withPCRE2 = false;
      withPolkit = false;
      withPortabled = false;
      withQrencode = false;
      withRepart = false;
      withRemote = false;
      withShellCompletions = false;
      withSysupdate = false;
      withTimedated = false;
      withTpm2Tss = false;
      withVConsole = false;
      withVmspawn = false;
      # the hardware database sources (9 MiB); the image ships no hwdb.bin
      withHwdb = false;
    }).overrideAttrs
      (old: {
        # message catalogs for other languages (2 MiB)
        mesonFlags = old.mesonFlags ++ [ (lib.mesonBool "translations" false) ];
      });
  # no keymap or font setup: kbd and the vconsole units leave systemd and
  # the initrd, and the console keeps the kernel defaults
  console.enable = false;
  # bcache-tools and its udev rule are for block caching the VM never uses
  boot.bcache.enable = false;
  systemd.coredump.enable = false;
  # dbus-broker links the stock systemd, 62 MiB next to the one above, and
  # dbus-daemon systemd-minimal, another 22 MiB.
  services.dbus.implementation = "dbus";
  services.dbus.dbusPackage = pkgs.dbus.override {
    systemdMinimal = config.systemd.package;
    x11Support = false;
  };
  fonts.fontconfig.enable = false;
  # C.UTF-8 is built into glibc; the default also compiles en_US
  i18n.defaultLocale = "C.UTF-8";
  i18n.supportedLocales = [ "C.UTF-8/UTF-8" ];
  security.pam.services.su.forwardXAuth = lib.mkForce false;
  # root runs everything, and sshd without PAM needs no unix_chkpwd, so no
  # setuid wrapper is left (they pull in linux-headers-static, and their
  # setup service runs on every boot).
  security.enableWrappers = false;
  # the udev hardware database (13 MiB) names nothing a fixed VM needs
  environment.etc."udev/hwdb.bin".enable = false;
  # systemd-bsod needs qrencode, which the build above leaves out
  boot.initrd.systemd.suppressedUnits = [ "systemd-bsod.service" ];
  boot.initrd.systemd.suppressedStorePaths = [
    "${config.boot.initrd.systemd.package}/lib/systemd/systemd-bsod"
  ];

  # the default core set is sized for an interactive machine; the ones
  # gone are netcat, host, coreutils-full, cpio, patch, diff, bzip2, su,
  # mkpasswd, time, gawk, tar and the acl/attr/libcap tools.
  environment.corePackages = lib.mkForce (
    with pkgs;
    [
      bashInteractive
      coreutils
      curlSlim
      findutils
      getent
      gnugrep
      gnused
      gzip
      hostname
      # ip and ss only: arpd needs db, tc BPF/xt elfutils, libbpf, iptables
      (iproute2.overrideAttrs { buildInputs = [ libmnl ]; })
      iputils
      less
      procps
      stdenv.cc.libc
      util-linux
      which
      xz
      zstd
    ]
  );
}
