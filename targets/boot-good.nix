# Boot counting for a target of the spore bootstrap. The bootstrap
# takes one try before each start of the target. This module gives the tries
# back once the boot is good, and makes a hang reset the machine, so that the
# next bootstrap counts it.
#
# A boot is good when spore-boot-good.service runs: after
# boot-complete.target, like systemd-bless-boot. A health check joins with
# `requiredBy = [ "boot-complete.target" ]` and
# `before = [ "boot-complete.target" ]`.
{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.spore.bootGood;
in
{
  options.spore.bootGood.deadline = lib.mkOption {
    type = lib.types.nullOr lib.types.ints.positive;
    default = 300;
    description = ''
      Seconds from the start of the softdog driver until the boot must be
      good. Then the software watchdog resets the machine. null disables the
      watchdog: a hang then waits for a reset from outside.
    '';
  };

  config = {
    systemd.services.spore-boot-good = {
      description = "Mark the boot good for the spore bootstrap";
      wantedBy = [ "multi-user.target" ];
      requires = [ "boot-complete.target" ];
      after = [ "boot-complete.target" ];
      path = [
        pkgs.mtools
        pkgs.util-linux
      ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${pkgs.runtimeShell} ${./boot-good.sh}";
      };
    };

    boot.kernelParams = lib.optionals (cfg.deadline != null) [
      "softdog.soft_active_on_boot=1"
      "softdog.soft_margin=${toString cfg.deadline}"
    ];
    boot.initrd.kernelModules = lib.optional (
      cfg.deadline != null && !(config.boot.kernelPackages.kernel.config.isYes "SOFT_WATCHDOG")
    ) "softdog";
  };
}
