#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
printf 'Welcome to openRuyi Zero\n'
hostname "$(cat /etc/hostname)"
# -C performs coldplug after the event listener is ready, as in upstream Zero.
mdevd -C -O 4 &
if [ ! -f /var/lib/tianzhi/rootfs-grown ]; then
    /usr/libexec/tianzhi/grow-rootfs ||
        printf 'SD root expansion failed; it will retry at the next boot.\n' >&2
fi
busybox ip link set lo up
# Bring up physical Ethernet asynchronously so an unplugged cable never delays
# the console. udhcpc stays alive to renew its lease.
for device in /sys/class/net/*; do
    [ -e "$device/device" ] && [ "$(cat "$device/type")" = 1 ] || continue
    interface=${device##*/}
    busybox ip link set "$interface" up
    busybox udhcpc -b -i "$interface" -p "/run/udhcpc-$interface.pid" \
        -s /usr/libexec/tianzhi/udhcpc > "/run/udhcpc-$interface.log" 2>&1 &
done
if [ -x /usr/bin/k3s-start ]; then
    mkdir -p /var/log
    /usr/bin/k3s-start </dev/null >>/var/log/k3s-start.log 2>&1 &
fi
