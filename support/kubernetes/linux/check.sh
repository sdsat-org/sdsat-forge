#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
set -eu

export PATH=/usr/bin:/usr/sbin:/bin:/sbin
config_dir=${FORGE_KUBERNETES_CONFIG:-/etc/kubernetes/forge}
state_dir=/run/forge-kubernetes
log_dir=/var/log/forge-kubernetes
export KUBECONFIG=$state_dir/kubeconfig

diagnostics() {
    kubectl --request-timeout=5s get nodes,pods -o wide >&2 || true
    kubectl --request-timeout=5s describe pod forge-network-server forge-busybox >&2 || true
    kubectl --request-timeout=5s get events --sort-by=.metadata.creationTimestamp >&2 || true
    for log in "$log_dir"/*.log; do
        [ -f "$log" ] || continue
        printf '\n--- %s ---\n' "$log" >&2
        tail -n 50 "$log" >&2 || true
    done
}

finish() {
    status=$?
    if [ "$status" -ne 0 ]; then diagnostics; fi
    kubectl --request-timeout=5s delete pod forge-network-server \
        --ignore-not-found --wait=false > /dev/null 2>&1 || true
    exit "$status"
}
trap finish 0

kubernetes-start
kubectl --request-timeout=130s wait --for=condition=Ready node/tianzhi-node --timeout=120s
kubectl --request-timeout=70s delete pod forge-network-server forge-busybox \
    --ignore-not-found --wait=true --timeout=60s
kubectl --request-timeout=10s apply -f "$config_dir/server.yaml"
kubectl --request-timeout=130s wait --for=condition=Ready pod/forge-network-server --timeout=120s
server_ip=$(kubectl --request-timeout=5s get pod forge-network-server \
    -o 'jsonpath={.status.podIP}')
case "$server_ip" in
    10.244.0.*) ;;
    *) echo "Unexpected server Pod IP: $server_ip" >&2; exit 1 ;;
esac
sed "s/__SERVER_IP__/$server_ip/g" "$config_dir/pod.yaml" > "$state_dir/client.yaml"
kubectl --request-timeout=10s apply -f "$state_dir/client.yaml"

attempt=0
while [ "$attempt" -lt "${FORGE_KUBERNETES_RETRIES:-120}" ]; do
    phase=$(kubectl --request-timeout=5s get pod forge-busybox \
        -o 'jsonpath={.status.phase}' 2>/dev/null || true)
    if [ "$phase" = Failed ]; then
        kubectl --request-timeout=5s logs forge-busybox -c busybox >&2 || true
        echo 'The Kubernetes network workload failed.' >&2
        exit 1
    fi
    if [ "$phase" = Succeeded ]; then
        exit_code=$(kubectl --request-timeout=5s get pod forge-busybox \
            -o 'jsonpath={.status.containerStatuses[0].state.terminated.exitCode}')
        [ "$exit_code" = 0 ]
        client_ip=$(kubectl --request-timeout=5s get pod forge-busybox \
            -o 'jsonpath={.status.podIP}')
        case "$client_ip" in
            10.244.0.*) ;;
            *) echo "Unexpected client Pod IP: $client_ip" >&2; exit 1 ;;
        esac
        [ "$client_ip" != "$server_ip" ]
        kubectl --request-timeout=10s logs forge-busybox -c busybox > "$log_dir/pod.log"
        grep -qx FORGE_KUBERNETES_POD_OK "$log_dir/pod.log"
        grep -qx architecture=riscv64 "$log_dir/pod.log"
        grep -qx FORGE_CNI_NETWORK_OK "$log_dir/pod.log"
        kubectl --request-timeout=10s get nodes,pods -o wide
        cat "$log_dir/pod.log"
        echo FORGE_KUBERNETES_OK
        exit 0
    fi
    for name in containerd etcd apiserver controller-manager scheduler kubelet; do
        kill -0 "$(cat "$state_dir/$name.pid")"
    done
    attempt=$((attempt + 1))
    sleep 2
done

echo 'Timed out waiting for the Kubernetes network workload to succeed.' >&2
exit 1
