#!/bin/sh
set -eu
trap 'status=$?; if [ "$status" -ne 0 ]; then echo "openRuyi Zero boot check failed ($status)." >&2; /bin/busybox poweroff -f; fi' EXIT

export PATH=/usr/sbin:/usr/bin:/sbin:/bin
/bin/busybox --install -s /usr/bin
mkdir -p /dev /proc /sys /tmp
mount -t devtmpfs devtmpfs /dev
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t tmpfs tmpfs /tmp
exec </dev/console >/dev/console 2>/dev/console

. /usr/lib/os-release
[ "$ID" = openruyi ]
[ -x /bin/bash ]
[ -x /bin/mdevd ]
/bin/bash -c '[ "$BASH_VERSION" ]'
/bin/sh /forge-userspace-check.sh
printf 'openRuyi %s checks passed.\n' "$VERSION_ID"
echo FORGE_BOOT_OK
/bin/busybox poweroff -f
