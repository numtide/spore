{
  pkgs,
  platform,
  disk,
  firmwares,
  qemu,
  console,
  features ? [ ],
  timeout ? 180,
  # the initrd has busybox: an emergency or rescue drops to a shell
  shell ? false,
}:
let
  inherit (pkgs) lib;

  # A tiny "system" with the layout the bootstrap reads: kernel, initrd,
  # init and kernel-params. Its initrd runs `body` and has busybox, mtools
  # and the boot-good script of targets/.
  mkInit =
    body:
    pkgs.writeScript "init" ''
      #!/bin/busybox sh
      export PATH=/bin
      mount -t devtmpfs devtmpfs /dev
      mount -t proc proc /proc
      mount -t sysfs sysfs /sys
      ${body}
    '';
  mkTarget =
    name: init: extra:
    let
      initrd = pkgs.runCommand "${name}-initrd" { nativeBuildInputs = [ pkgs.cpio ]; } ''
        mkdir -p root/bin root/dev root/proc root/sys root/tmp root/data
        cp ${pkgs.pkgsStatic.busybox}/bin/busybox root/bin/busybox
        for a in $(root/bin/busybox --list); do ln -s busybox root/bin/$a; done
        cp ${pkgs.pkgsStatic.mtools}/bin/mtools root/bin/mtools
        for a in mcopy mtype; do ln -s mtools root/bin/$a; done
        cp ${../targets/boot-good.sh} root/bin/boot-good
        cp ${init} root/init
        mkdir $out
        (cd root && find . | cpio -o -H newc --quiet -R 0:0) > $out/initrd
      '';
    in
    pkgs.runCommand name { } ''
      mkdir $out
      ln -s ${disk.kernel}/${disk.kernel.target} $out/kernel
      ln -s ${initrd}/initrd $out/initrd
      ln -s ${init} $out/init
      echo "console=${console}" > $out/kernel-params
      ${extra}
    '';

  # Marks the boot good and powers off. With a partition labelled `data`, it
  # also reports whether a marker file on it survived from an earlier boot.
  goodInit = mkInit ''
    if data=$(findfs LABEL=data 2>/dev/null); then
      mount -t ext4 "$data" /data
      if [ -e /data/marker ]; then
        echo DATA-KEPT > /dev/console
      else
        echo marker > /data/marker
        echo DATA-NEW > /dev/console
      fi
      umount /data
    fi
    sh /bin/boot-good > /dev/console 2>&1
    echo SPORE-TARGET-OK > /dev/console
    poweroff -f
  '';
  target = mkTarget "spore-test-target" goodInit "";
  # The kernel panics. The sleep keeps the marker apart from the panic text.
  panicTarget = mkTarget "spore-test-target-panic" (mkInit ''
    echo SPORE-TARGET-PANIC > /dev/console
    sleep 1
    echo c > /proc/sysrq-trigger
  '') "";
  # Boots, but never marks the boot good.
  silentTarget = mkTarget "spore-test-target-silent" (mkInit ''
    echo SPORE-TARGET-SILENT > /dev/console
    poweroff -f
  '') "";

  # Like a NixOS toplevel: etc/repart.d reaches the definitions through
  # three store paths (system -> etc -> repart.d -> each file).
  repartD = pkgs.runCommand "repart.d" { } ''
    mkdir $out
    ln -s ${pkgs.writeText "00-esp.conf" ''
      [Partition]
      Type=esp
      Format=vfat
      SizeMinBytes=128M
      MountPoint=/boot
    ''} $out/00-esp.conf
    ln -s ${pkgs.writeText "10-root.conf" ''
      [Partition]
      Type=root
      Label=nixos
      Format=ext4
      SizeMaxBytes=1G
      MountPoint=/
    ''} $out/10-root.conf
    ln -s ${pkgs.writeText "20-data.conf" ''
      [Partition]
      Type=linux-generic
      Label=data
      Format=ext4
      MountPoint=/var
    ''} $out/20-data.conf
  '';
  etc = pkgs.runCommand "etc" { } ''
    mkdir -p $out/etc
    ln -s ${repartD} $out/etc/repart.d
  '';
  repartTarget = mkTarget "spore-test-target-repart" goodInit "ln -s ${etc}/etc $out/etc";

  targets = [
    target
    repartTarget
    panicTarget
    silentTarget
  ];
  # the rest of the document belongs to the target; `other` checks that
  # the bootstrap leaves it alone
  userdata =
    system: extra:
    builtins.toJSON {
      spore = {
        version = 1;
        system.${pkgs.stdenv.hostPlatform.system} = system;
        substituters = [ "http://10.0.2.2:8080" ];
        trusted-public-keys = [ (lib.fileContents ./test-key.pub) ];
      }
      // extra;
      other.key = "value";
    };
  cache = pkgs.runCommand "spore-test-cache" { nativeBuildInputs = [ pkgs.nix ]; } ''
    cp -r ${pkgs.mkBinaryCache { rootPaths = targets; }} $out
    chmod -R u+w $out
    export HOME=$TMPDIR NIX_STATE_DIR=$TMPDIR/state NIX_CONF_DIR=$TMPDIR
    nix --extra-experimental-features nix-command store sign \
      --store "file://$out" --key-file ${./test-key.sec} --recursive ${toString targets}
    cp ${pkgs.writeText "ud" (userdata target { })} $out/userdata.json
    cp ${pkgs.writeText "ud" (userdata repartTarget { })} $out/userdata-repart.json
    cp ${pkgs.writeText "ud" (userdata panicTarget { boot-tries = 2; })} $out/userdata-panic.json
    cp ${
      pkgs.writeText "ud" (
        userdata silentTarget {
          boot-tries = 1;
          fallback = "rescue";
        }
      )
    } $out/userdata-rescue.json
  '';

  testDisk =
    ud:
    disk.override {
      cmdline = "console=${console} spore.userdata=http://10.0.2.2:8080/${ud}";
    };

  # `boot LOG [PATTERN]` runs one boot: QEMU exits on a reset or a power
  # off. With PATTERN, it stops QEMU once the log has it.
  run =
    name: firmware: ud: script:
    pkgs.runCommand "spore-boot-${platform}-${name}"
      {
        nativeBuildInputs = [
          pkgs.qemu_kvm
          pkgs.busybox
          pkgs.mtools
        ];
        requiredSystemFeatures = features;
      }
      ''
        cp ${testDisk ud}/disk.img disk.img
        chmod u+w disk.img
        truncate -s 2G disk.img
        busybox httpd -p 127.0.0.1:8080 -h ${cache}
        boot() {
          timeout ${toString timeout} ${qemu} ${firmware} \
            -m 2048 -smp 2 -drive file=disk.img,if=virtio,format=raw \
            -nic user,model=virtio-net-pci -display none -serial file:$1 -no-reboot &
          q=$!
          if [ -n "''${2-}" ]; then
            while kill -0 $q 2>/dev/null && ! grep -q "$2" $1; do sleep 0.2; done
            kill $q 2>/dev/null || true
          fi
          wait $q || true
        }
        # the ESP starts at sector 4096 (disk.nix)
        export MTOOLS_SKIP_CHECK=1
        esp=disk.img@@2097152
        # the serial log ends lines in CR LF
        show() { tr -d '\r' < $1; }
        need() {
          grep -q "$1" $2 || {
            echo "$2 lacks: $1" >&2
            exit 1
          }
        }
        tries() {
          test "$(mtype -i $esp ::/target/tries)" = "$1" || {
            echo "tries: want $1, have $(mtype -i $esp ::/target/tries)" >&2
            exit 1
          }
        }
        ${script}
        mkdir $out
        cp *.log $out/
      '';

  # The bootstrap starts every boot. The first boot provisions and kexecs
  # into the target, which marks the boot good. The second boot takes a try
  # and kexecs the target from the ESP without the network; the good mark
  # gives the try back.
  goodLane =
    name: firmware:
    run name firmware "userdata.json" ''
      boot first.log
      show first.log
      need '^spore: boot entry written after .* s: 3 tries' first.log
      need '^spore: kexec into ${target}' first.log
      need 'boot good: 3 of 3 tries left' first.log
      need SPORE-TARGET-OK first.log
      tries "3 3"
      boot second.log
      show second.log
      need '^spore: kexec into the target on the ESP after .* s: 2 of 3 tries left' second.log
      need 'boot good: 3 of 3 tries left' second.log
      need SPORE-TARGET-OK second.log
      if grep -q '^spore: network up' second.log; then
        echo "second boot provisioned again" >&2
        exit 1
      fi
      tries "3 3"
    '';

  # The target panics on each boot. With 2 tries, the third boot finds no
  # tries left and provisions the same user-data again.
  panicLane =
    name: firmware:
    run "${name}-panic" firmware "userdata-panic.json" ''
      boot first.log
      show first.log
      need '^spore: kexec into ${panicTarget}' first.log
      need 'Kernel panic' first.log
      tries "1 2"
      boot second.log
      show second.log
      need '^spore: kexec into the target on the ESP after .* s: 0 of 2 tries left' second.log
      need 'Kernel panic' second.log
      tries "0 2"
      boot third.log
      show third.log
      need '^spore: the target used all 2 tries without a good boot' third.log
      need '^spore: fetched ${panicTarget} after .* s: 0 paths' third.log
      need '^spore: kexec into ${panicTarget}' third.log
      tries "1 2"
    '';

  # The target never marks the boot good. With 1 try and "fallback":
  # "rescue", the second boot stays in the bootstrap and keeps the disk.
  rescueLane =
    name: firmware:
    run "${name}-rescue" firmware "userdata-rescue.json" ''
      boot first.log
      show first.log
      need SPORE-TARGET-SILENT first.log
      tries "0 1"
      boot second.log '${if shell then "dropping to a shell" else "no shell in this build"}'
      show second.log
      need '^spore: the target used all 1 tries without a good boot' second.log
      need '^spore: rescue: the user-data asks for it' second.log
      if grep -q '^spore: kexec' second.log; then
        echo "rescue started the target" >&2
        exit 1
      fi
      tries "0 1"
    '';

  # The layout comes from the target's repart.d, with the ESP of the image
  # as its vfat ESP definition. After the first boot, the test removes the
  # boot counter, so the second boot provisions again: it must keep both
  # partitions and the data on them.
  repartLane = run "repart" (firmwares.bios or firmwares.uefi) "userdata-repart.json" ''
    boot first.log
    show first.log
    need '^spore: layout from the repart.d of the system after .* s: 3 partitions' first.log
    need '^spore: not mounting /boot: no ext4 on it' first.log
    need '^spore: disk ready after .* s: 2 new partitions' first.log
    need DATA-NEW first.log
    mdel -i $esp ::/target/tries
    boot second.log
    show second.log
    need '^spore: disk ready after .* s: 0 new partitions' second.log
    need '^spore: fetched ${repartTarget} after .* s: 0 paths' second.log
    need DATA-KEPT second.log
    boot third.log
    need DATA-KEPT third.log
    need '^spore: kexec into the target on the ESP' third.log
  '';
in
lib.mapAttrs goodLane firmwares
// lib.mapAttrs' (n: f: lib.nameValuePair "${n}-panic" (panicLane n f)) firmwares
// lib.mapAttrs' (n: f: lib.nameValuePair "${n}-rescue" (rescueLane n f)) firmwares
// {
  repart = repartLane;
}
