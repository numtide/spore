# The lean target on EC2: the root ssh key and the host name come from the
# instance metadata service, with an IMDSv2 token.
{
  lib,
  pkgs,
  curlSlim,
  ...
}:
let
  imds = "http://169.254.169.254/latest";
  # curl -sf is quiet on a missing key pair (404)
  authorizedKeys = pkgs.writeShellScript "ec2-authorized-keys" ''
    [ "$1" = root ] || exit 0
    curl=${lib.getExe curlSlim}
    t=$($curl -sf --max-time 5 -X PUT -H 'X-aws-ec2-metadata-token-ttl-seconds: 60' ${imds}/api/token) || exit 0
    $curl -sf --max-time 5 -H "X-aws-ec2-metadata-token: $t" ${imds}/meta-data/public-keys/0/openssh-key
  '';
in
{
  imports = [ ../lean.nix ];

  spore.initrd = false;
  # advertised even when nix probes before /dev/kvm is there; the VM sizes
  # have no /dev/kvm, which the lean target does not mind
  nix.settings.system-features = [
    "nixos-test"
    "benchmark"
    "big-parallel"
    "kvm"
  ];
  boot.kernelParams = [
    "console=ttyS0,115200n8"
    # https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/nvme-ebs-volumes.html#timeout-nvme-ebs-volumes
    "nvme_core.io_timeout=4294967295"
  ];

  systemd.services.ec2-hostname = {
    wantedBy = [ "multi-user.target" ];
    script = ''
      curl=${lib.getExe curlSlim}
      t=$($curl -sf --retry 60 --retry-delay 1 --retry-all-errors --max-time 2 -X PUT -H 'X-aws-ec2-metadata-token-ttl-seconds: 60' ${imds}/api/token)
      h=$($curl -sf --max-time 2 -H "X-aws-ec2-metadata-token: $t" ${imds}/meta-data/local-hostname)
      echo -n "''${h%%.*}" > /proc/sys/kernel/hostname
    '';
  };

  environment.etc."ssh/authorized-keys" = {
    source = authorizedKeys;
    mode = "0555";
  };
}
