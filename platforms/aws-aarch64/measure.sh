#!/usr/bin/env bash
# measure.sh AMI SYSTEM [TYPE] [OUTDIR]: boots one spot instance from the AMI
# with SYSTEM as the spore target, collects the boot timings of the first
# boot and of a warm reboot, terminates the instance and prints both.
#
# Needs AWS credentials in the environment, plus SPORE_SUBNET, SPORE_SG and
# SPORE_KEY (an EC2 key pair whose private key ssh offers by default).
# SPORE_MARKET=on-demand switches off the spot request. SPORE_LAYOUT is a
# JSON layout for the user-data (the bootstrap then skips the lookup in the
# system). SPORE_CLOSURE is the store path of the system's closureInfo (the
# flake's <platform>-target-closure). SPORE_EXTRA names a script to run on
# the target after the warm boot; its output lands in OUTDIR/extra.
set -euo pipefail

ami=$1
system=$2
type=${3:-t4g.small}
out=${4:-measurements/$(date -u +%Y%m%dT%H%M%SZ)-$type}
market=${SPORE_MARKET:-spot}
mkdir -p "$out"

ud=$(mktemp)
trap 'rm -f "$ud"' EXIT
# optional keys as whole lines: a heredoc strips the quotes inside ${var:+...}
layout_line=${SPORE_LAYOUT:+"\"layout\": $SPORE_LAYOUT,"}
closure_line=${SPORE_CLOSURE:+"\"closure\": \"$SPORE_CLOSURE\","}
cat >"$ud" <<JSON
{"spore": {"version": 1, "system": "$system",
  $layout_line
  $closure_line
  "substituters": ["${SPORE_SUBSTITUTER:-https://cache.numtide.com}"],
  "trusted-public-keys": ["${SPORE_KEY_PUB:-niks3.numtide.com-1:DTx8wZduET09hRmMtKdQDxNNthLQETkc/yaX7M4qK0g=}"]}}
JSON

opts=()
if [ "$market" = spot ]; then
  opts+=(--instance-market-options 'MarketType=spot,SpotOptions={SpotInstanceType=one-time}')
fi
# SPORE_SUBNET may list several subnets: spot capacity comes and goes per zone
id=
for subnet in $SPORE_SUBNET; do
  t_launch=$(date -u +%s.%N)
  id=$(aws ec2 run-instances \
    --image-id "$ami" --instance-type "$type" \
    --key-name "$SPORE_KEY" --subnet-id "$subnet" --security-group-ids "$SPORE_SG" \
    --associate-public-ip-address --user-data "file://$ud" "${opts[@]}" \
    --block-device-mappings "DeviceName=/dev/xvda,Ebs={VolumeSize=8,VolumeType=gp3,DeleteOnTermination=true${SPORE_EBS:+,$SPORE_EBS}}" \
    --metadata-options HttpTokens=required,HttpEndpoint=enabled \
    --instance-initiated-shutdown-behavior terminate \
    --tag-specifications 'ResourceType=instance,Tags=[{Key=Name,Value=spore-measure}]' \
    --query 'Instances[0].InstanceId' --output text 2>"$out/launch.err") && break
  grep -q InsufficientInstanceCapacity "$out/launch.err" || {
    cat "$out/launch.err" >&2
    exit 1
  }
  echo "no $market capacity in $subnet" >&2
  id=
done
[ -n "$id" ] || {
  echo "no capacity in any subnet" >&2
  exit 1
}
echo "instance $id ($type, $market)" | tee "$out/instance"
trap 'aws ec2 terminate-instances --instance-ids "$id" --query "TerminatingInstances[0].CurrentState.Name" --output text; rm -f "$ud"' EXIT

ip=
while [ -z "$ip" ] || [ "$ip" = None ]; do
  sleep 2
  ip=$(aws ec2 describe-instances --instance-ids "$id" --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)
done
ssh=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5 "root@$ip")

# waits for sshd, then prints the seconds since the launch call
wait_ssh() {
  local i
  for i in $(seq 1 240); do
    if "${ssh[@]}" true 2>/dev/null; then
      awk -v a="$t_launch" -v b="$(date -u +%s.%N)" 'BEGIN { printf "%.1f", b - a }'
      return
    fi
    sleep 1
  done
  echo "no ssh after 240 s" >&2
  return 1
}

# collect NAME: the timings of the running boot into $out/NAME.*
collect() {
  "${ssh[@]}" 'cat /var/log/spore.log' >"$out/$1.spore" 2>/dev/null || true
  "${ssh[@]}" 'systemd-analyze; systemd-analyze critical-chain --no-pager; systemd-analyze blame --no-pager | head -20' >"$out/$1.systemd" 2>/dev/null || true
  "${ssh[@]}" 'dmesg' >"$out/$1.dmesg" 2>/dev/null || true
  "${ssh[@]}" 'journalctl -b -o short-monotonic --no-pager | head -80; echo; systemctl list-units --all --no-legend | wc -l' >"$out/$1.journal" 2>/dev/null || true
  "${ssh[@]}" 'spore stamp 2>/dev/null || true; journalctl -b -u spore-boot-good --no-pager -o short-monotonic 2>/dev/null | tail -3' >"$out/$1.good" 2>/dev/null || true
}

echo "first boot: ssh after $(wait_ssh) s" | tee -a "$out/summary"
collect first
aws ec2 get-console-output --instance-id "$id" --latest --query Output --output text >"$out/first.console" 2>/dev/null || true

t_launch=$(date -u +%s.%N)
"${ssh[@]}" 'systemctl reboot' 2>/dev/null || true
sleep 5
echo "warm reboot: ssh after $(wait_ssh) s" | tee -a "$out/summary"
collect warm
# the ESP path logs to the console only, which the API serves with a delay
for i in $(seq 1 30); do
  aws ec2 get-console-output --instance-id "$id" --latest --query Output --output text >"$out/warm.console" 2>/dev/null || true
  grep -q 'on the ESP' "$out/warm.console" && break
  sleep 5
done
awk '/kexec into \/nix/ { n++ } n >= 1 && /spore:/' "$out/warm.console" | tail -n +2 >"$out/warm.spore"

if [ -n "${SPORE_EXTRA:-}" ]; then
  "${ssh[@]}" 'cat >/tmp/extra.sh && bash /tmp/extra.sh' <"$SPORE_EXTRA" >"$out/extra" 2>&1 || true
fi

for b in first warm; do
  echo "== $b" | tee -a "$out/summary"
  grep -E 'spore: (started|user-data|network|disk ready|fetched|boot entry|target read|kexec|synced)' "$out/$b.spore" | sed 's/^/  /' | tee -a "$out/summary"
  head -1 "$out/$b.systemd" | sed 's/^/  /' | tee -a "$out/summary"
  sed 's/^/  /' "$out/$b.good" | tee -a "$out/summary"
done
echo "written to $out"
