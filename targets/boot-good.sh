#!/bin/sh
# Marks this boot good for the spore bootstrap: sets the tries left
# in target/tries on the ESP back to the total. Then it stops the software
# watchdog that boot-good.nix arms. Needs findfs and mtools.
set -eu
export MTOOLS_SKIP_CHECK=1

esp=$(findfs LABEL=ESP)
mnt=
while read -r dev dir _; do
  if [ "$dev" = "$esp" ]; then mnt=$dir && break; fi
done </proc/mounts
if [ -n "$mnt" ]; then
  tries=$(cat "$mnt/target/tries" 2>/dev/null) || tries=
else
  tries=$(mtype -i "$esp" ::/target/tries 2>/dev/null) || tries=
fi
# shellcheck disable=SC2086
set -- $tries
if [ $# != 2 ]; then
  echo "no boot counter on $esp"
elif [ "$1" != "$2" ]; then
  if [ -n "$mnt" ]; then
    echo "$2 $2" >"$mnt/target/tries"
    sync -f "$mnt/target/tries"
  else
    tmp=$(mktemp)
    echo "$2 $2" >"$tmp"
    mcopy -o -i "$esp" "$tmp" ::/target/tries
    rm "$tmp"
    sync
  fi
  echo "boot good: $2 of $2 tries left"
fi

for w in /sys/class/watchdog/watchdog*; do
  if [ "$(cat "$w/identity" 2>/dev/null)" = "Software Watchdog" ]; then
    # the magic close character stops it
    printf V >"/dev/${w##*/}"
  fi
done
