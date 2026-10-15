#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
set -eu

export PATH=/usr/sbin:/usr/bin:/sbin:/bin
# The vendor BusyBox is dynamically linked; the OCI image needs this static copy.
export FORGE_BUSYBOX=/usr/libexec/tianzhi/busybox
node_ip=${FORGE_KUBERNETES_NODE_IP:-10.0.2.15}

# QEMU's restricted user network supplies no kernel-side address configuration.
"$FORGE_BUSYBOX" ip link set eth0 up
"$FORGE_BUSYBOX" ip addr replace "$node_ip/24" dev eth0
"$FORGE_BUSYBOX" ip route replace default via 10.0.2.2 dev eth0

/bin/sh "${FORGE_KUBERNETES_CONFIG:-/etc/kubernetes/forge}/start.sh"
export KUBECONFIG=/run/forge-kubernetes/kubeconfig
kubectl --request-timeout=190s wait --for=condition=Ready node/tianzhi-node --timeout=180s
