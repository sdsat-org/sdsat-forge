#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
set -eu

export PATH=/bin:/sbin:/usr/bin:/usr/sbin
config_dir=${FORGE_KUBERNETES_CONFIG:-/etc/kubernetes/forge}
log_dir=/var/log/forge-kubernetes
export KUBECONFIG=/run/forge-kubernetes/kubeconfig
kubernetes-start

diagnostics() {
    kubectl --request-timeout=5s get nodes,pods -o wide >&2 || true
    kubectl --request-timeout=5s describe pod forge-busybox >&2 || true
    kubectl --request-timeout=5s get events --sort-by=.metadata.creationTimestamp >&2 || true
    for log in "$log_dir"/*.log; do
        [ -f "$log" ] || continue
        printf '\n--- %s ---\n' "$log" >&2
        tail -n 50 "$log" >&2 || true
    done
}
trap 'status=$?; if [ "$status" -ne 0 ]; then diagnostics; fi; exit "$status"' 0

kubectl --request-timeout=10s apply -f "$config_dir/pod.yaml"
attempt=0
while [ "$attempt" -lt "${FORGE_KUBERNETES_RETRIES:-120}" ]; do
    phase=$(kubectl --request-timeout=5s get pod forge-busybox \
        -o 'jsonpath={.status.phase}' 2>/dev/null || true)
    if [ "$phase" = Failed ]; then
        echo 'The Kubernetes workload failed.' >&2
        exit 1
    fi
    if [ "$phase" = Succeeded ]; then
        exit_code=$(kubectl --request-timeout=5s get pod forge-busybox \
            -o 'jsonpath={.status.containerStatuses[0].state.terminated.exitCode}')
        [ "$exit_code" = 0 ]
        kubectl --request-timeout=10s logs forge-busybox -c busybox > "$log_dir/pod.log"
        grep -qx FORGE_KUBERNETES_POD_OK "$log_dir/pod.log"
        grep -qx architecture=riscv64 "$log_dir/pod.log"
        cat "$log_dir/pod.log"
        echo FORGE_KUBERNETES_OK
        exit 0
    fi
    for name in containerd etcd apiserver controller-manager scheduler kubelet; do
        kill -0 "$(cat "/run/forge-kubernetes/$name.pid")"
    done
    attempt=$((attempt + 1))
    sleep 2
done
echo 'Timed out waiting for the Kubernetes workload to succeed.' >&2
exit 1
