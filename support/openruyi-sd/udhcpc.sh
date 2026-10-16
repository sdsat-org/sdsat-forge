#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
set -efu
case "${interface:-}" in ''|*[!a-zA-Z0-9_.:-]*) exit 1 ;; esac
case "${1:-}" in
    deconfig)
        busybox ip address flush dev "$interface"
        ;;
    bound|renew)
        busybox ifconfig "$interface" "$ip" netmask "${subnet:-255.255.255.0}" up
        for gateway in ${router:-}; do
            busybox ip route replace default via "$gateway" dev "$interface"
            break
        done
        : > /run/resolv.conf
        for server in ${dns:-}; do
            printf 'nameserver %s\n' "$server" >> /run/resolv.conf
        done
        ln -snf /run/resolv.conf /etc/resolv.conf
        ;;
esac
