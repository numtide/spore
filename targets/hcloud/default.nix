# A lean NixOS target for hcloud, the system the benchmarks provision. It
# boots the platform's kernel, which has every driver built in, so the
# target initrd loads no modules. The bootstrap owns the disk layout: no
# growpart or repart here.
{
  lib,
  pkgs,
  modulesPath,
  curlSlim,
  ...
}:
let
  metadata = "http://169.254.169.254/hetzner/v1/metadata";
  authorizedKeys = pkgs.writeShellScript "hcloud-authorized-keys" ''
    [ "$1" = root ] || exit 0
    ${lib.getExe curlSlim} -sf --max-time 5 ${metadata}/public-keys | ${lib.getExe pkgs.jq} -r '.[]'
  '';
in
{
  imports = [
    "${modulesPath}/profiles/minimal.nix"
    "${modulesPath}/profiles/perlless.nix"
    ./slim.nix
    ../boot-good.nix
  ];

  system.stateVersion = "26.11";
  # the bootstrap writes the boot entry; switch-to-configuration would pull
  # the bootloader installer (and perl) into the closure
  system.switch.enable = false;
  boot.loader.grub.enable = false;

  boot.kernelParams = [
    "quiet"
    "udev.log_level=3"
    # without a configured size, PID 1 queries the console with ANSI
    # sequences that never get an answer and waits 333 ms, in the initrd and
    # again after switch-root
    "systemd.tty.rows.console=25"
    "systemd.tty.columns.console=80"
    # the kernel hands this to PID 1 as an environment variable. The default
    # (5 mount events per second) parks every initrd mount behind the
    # credential mounts for up to a second.
    "SYSTEMD_DEFAULT_MOUNT_RATE_LIMIT_BURST=100"
    # the system never mounts the ESP; gpt-auto would automount it on /boot
    "systemd.gpt_auto=0"
  ];
  # the bpf LSM only backs RestrictFileSystems=; attaching it costs PID 1
  # ~0.2 s on each start
  security.lsm = lib.mkForce [
    "landlock"
    "yama"
  ];
  boot.consoleLogLevel = 3;
  boot.initrd.verbose = false;
  boot.initrd.systemd.enable = true;
  boot.initrd.includeDefaultModules = false;
  boot.initrd.systemd.tpm2.enable = false;
  systemd.tpm2.enable = false;
  boot.initrd.systemd.fido2.enable = false;

  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    fsType = "ext4";
    options = [ "noatime" ];
    noCheck = true;
  };

  system.etc.overlay.enable = true;
  services.userborn.enable = true;
  system.nixos-init.enable = true;

  networking.hostName = "";
  networking.useDHCP = false;
  networking.useNetworkd = true;
  # hcloud firewalls filter before the guest, and the kernel has no netfilter
  networking.firewall.enable = false;
  systemd.network.networks."10-uplink" = {
    matchConfig.Type = "ether";
    networkConfig.DHCP = "ipv4";
  };
  # hcloud DHCP sends no hostname option. A oneshot ordered after
  # network-online.target would hold multi-user.target for DHCP plus a
  # request; curl retries until the uplink is there instead.
  systemd.services.hcloud-hostname = {
    wantedBy = [ "multi-user.target" ];
    script = ''
      h=$(${lib.getExe curlSlim} -sf --retry 60 --retry-delay 1 --retry-all-errors --max-time 2 ${metadata}/hostname)
      echo -n "$h" > /proc/sys/kernel/hostname
    '';
  };

  services.openssh = {
    enable = true;
    startWhenNeeded = true;
    hostKeys = [
      {
        type = "ed25519";
        # the /etc overlay upper dir is recreated on every boot
        path = "/var/lib/ssh/ssh_host_ed25519_key";
      }
    ];
    # sshd refuses a command below a group-writable directory, and the first
    # store write makes /nix/store 1775 root:nixbld: run it from /etc
    authorizedKeysCommand = "/etc/ssh/hcloud-authorized-keys %u";
    authorizedKeysCommandUser = "nobody";
    settings.PasswordAuthentication = false;
    # keys only; PAM would need the unix_chkpwd setuid helper
    settings.UsePAM = false;
  };
  # without PAM, sshd treats a "!" password as a locked account, even for keys
  users.users.root.hashedPassword = "*";
  environment.etc."ssh/hcloud-authorized-keys" = {
    source = authorizedKeys;
    mode = "0555";
  };

  nixpkgs.flake.setFlakeRegistry = false;
  nixpkgs.flake.setNixPath = false;
  nix.channel.enable = false;
  nix.settings.experimental-features = [
    "nix-command"
    "flakes"
  ];

  # PID 1 parses every unit on each start, at ~1.5 ms each on a shared vCPU
  systemd.oomd.enable = false;
  systemd.shutdownRamfs.enable = false;
  systemd.suppressedSystemUnits = [
    "factory-reset.target"
    "systemd-bootctl.socket"
    "systemd-bootctl@.service"
    "systemd-boot-random-seed.service"
    "systemd-factory-reset-reboot.service"
    "systemd-factory-reset-request.service"
    "systemd-factory-reset.socket"
    "systemd-factory-reset@.service"
    "systemd-hibernate-clear.service"
    "systemd-importd.service"
    "systemd-importd.socket"
    "systemd-journal-catalog-update.service"
    "systemd-machined.service"
    "systemd-machined.socket"
    "systemd-mute-console.socket"
    "systemd-mute-console@.service"
    "systemd-pcrextend.socket"
    "systemd-pcrextend@.service"
    "systemd-pcrlock.socket"
    "systemd-pcrlock@.service"
    "systemd-pcrlogin@.service"
    "systemd-pstore.service"
    "systemd-repart.socket"
    "systemd-repart@.service"
    "systemd-tpm2-clear.service"
    "systemd-tpm2-setup-early.service"
    "systemd-tpm2-setup.service"
  ];
  users.manageLingering = false;
  boot.kexec.enable = false;
  powerManagement.enable = false;
  services.fstrim.enable = false;
  programs.nano.enable = false;
  security.sudo.enable = false;
  system.disableInstallerTools = true;
  services.lvm.enable = false;
}
