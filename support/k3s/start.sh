#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Called in the background by rcS; all state and logs persist on the SD rootfs.
set -eu
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
umask 077

[ "$(id -u)" -eq 0 ] || { echo 'k3s-start: root is required' >&2; exit 1; }
mkdir -p /run/k3s /var/log /var/lib/rancher/k3s/agent/images
if ! mkdir /run/k3s/start.lock 2>/dev/null; then
    echo 'k3s-start: another start is in progress' >&2
    exit 1
fi
trap 'rmdir /run/k3s/start.lock' 0
trap 'exit 1' HUP INT TERM
if [ -r /run/k3s/server.pid ]; then
    pid=$(cat /run/k3s/server.pid)
    case "$pid" in
        ''|*[!0-9]*) ;;
        *)
            if kill -0 "$pid" 2>/dev/null &&
                tr '\000' ' ' < "/proc/$pid/cmdline" | grep -q 'k3s.*server'; then
                echo "k3s-start: already running (PID $pid)"
                exit 0
            fi
            ;;
    esac
fi

for tool in k3s ip nohup; do command -v "$tool" >/dev/null; done
[ -f /sys/fs/cgroup/cgroup.controllers ] || {
    echo 'k3s-start: a mounted cgroup v2 hierarchy is required' >&2
    exit 1
}
[ -s /var/lib/rancher/k3s/agent/images/k3s-airgap-images-riscv64.tar ] || {
    echo 'k3s-start: the offline image archive is missing' >&2
    exit 1
}
touch /var/lib/rancher/k3s/agent/images/.cache.json

# Give the asynchronous DHCP client a bounded chance; no cable is required.
iface=${K3S_NETWORK_INTERFACE:-eth0}
ip link set "$iface" up 2>/dev/null || true
attempt=0
node_ip=
while [ "$attempt" -lt 8 ]; do
    node_ip=$(ip -o -4 addr show dev "$iface" scope global 2>/dev/null |
        awk 'NR == 1 { split($4, a, "/"); print a[1] }')
    [ -z "$node_ip" ] || break
    attempt=$((attempt + 1))
    sleep 1
done

# K3s/kube-proxy require a default route even for an isolated single node.
# This is the documentation-only subnet used in the upstream air-gap guide.
if [ -z "$node_ip" ] || [ -z "$(ip -4 route show default)" ]; then
    if ! ip link show tianzhi0 >/dev/null 2>&1; then
        ip link add tianzhi0 type dummy
    fi
    ip link set tianzhi0 up
    ip addr replace 203.0.113.254/31 dev tianzhi0
    ip route replace default via 203.0.113.255 dev tianzhi0 metric 1000
fi
if [ -z "$node_ip" ]; then
    iface=tianzhi0
    node_ip=203.0.113.254
    echo 'k3s-start: isolated single-node network on tianzhi0'
fi

# Keep cluster DNS working without forwarding to a loopback resolver. The
# unreachable fallback has no effect on Kubernetes service-name resolution.
resolver=/etc/resolv.conf
[ -r "$resolver" ] || resolver=/dev/null
awk '$1 == "nameserver" && $2 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ &&
    $2 !~ /^(127|0)\./ { print; found=1 }
    END { if (!found) print "nameserver 203.0.113.255" }' \
    "$resolver" > /run/k3s/resolv.conf
printf '1\n' > /proc/sys/net/ipv4/ip_forward
if [ -e /proc/sys/net/bridge/bridge-nf-call-iptables ]; then
    printf '1\n' > /proc/sys/net/bridge/bridge-nf-call-iptables
fi

nohup k3s server --config /etc/rancher/k3s/config.yaml \
    --node-ip "$node_ip" --advertise-address "$node_ip" --flannel-iface "$iface" \
    </dev/null >>/var/log/k3s.log 2>&1 &
pid=$!
printf '%s\n' "$pid" > /run/k3s/server.pid
sleep 1
kill -0 "$pid" 2>/dev/null || {
    echo 'k3s-start: server exited; inspect /var/log/k3s.log' >&2
    exit 1
}
printf 'k3s-start: PID %s, interface %s, node IP %s; log /var/log/k3s.log\n' \
    "$pid" "$iface" "$node_ip"
