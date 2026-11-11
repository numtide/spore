# The lean target on hcloud: the root ssh keys and the host name come from
# the Hetzner metadata service.
{
  lib,
  pkgs,
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
  imports = [ ../lean.nix ];

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

  environment.etc."ssh/authorized-keys" = {
    source = authorizedKeys;
    mode = "0555";
  };
}
