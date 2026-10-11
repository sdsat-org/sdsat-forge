#!/bin/sh
set -eu
trap 'status=$?; if [ "$status" -ne 0 ]; then echo "BusyBox boot check failed ($status)." >&2; fi; exit "$status"' EXIT

[ "$(uname -m)" = riscv64 ]
mkdir -p /tmp/forge-check
printf 'hello\n' > /tmp/forge-check/original
cp /tmp/forge-check/original /tmp/forge-check/copy
[ "$(cat /tmp/forge-check/copy)" = hello ]
[ "$(printf 'alpha\nbeta\n' | grep beta | wc -l)" -eq 1 ]

sleep 1 &
wait "$!"
/bin/sh -c 'exit 7' &
if wait "$!"; then
    echo 'Child exit status was lost.' >&2
    exit 1
else
    [ "$?" -eq 7 ]
fi

signal_seen=no
trap 'signal_seen=yes' USR1
kill -USR1 "$$"
[ "$signal_seen" = yes ]
trap - USR1
[ "$(printf '%.1f' 1.5)" = 1.5 ]
grep -q '^MemTotal:' /proc/meminfo
grep -q 'proc /proc proc' /proc/mounts
rm /tmp/forge-check/original /tmp/forge-check/copy
rmdir /tmp/forge-check
echo 'BusyBox checks passed.'
