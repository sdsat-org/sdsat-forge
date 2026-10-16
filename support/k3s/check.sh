#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Leave the named acceptance resources available for inspection and reboot checks.
set -eu
export PATH=/usr/sbin:/usr/bin:/sbin:/bin
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
k() { k3s kubectl --request-timeout=30s "$@"; }

attempt=0
until k get node tianzhi-node >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 120 ] || { echo 'k3s-check: node registration timed out' >&2; exit 1; }
    sleep 5
done
k wait --for=condition=Ready node/tianzhi-node --timeout=600s
k -n kube-system rollout status deployment/coredns --timeout=300s
k apply -f - <<'YAML'
apiVersion: v1
kind: Namespace
metadata:
  name: tianzhi-check
---
apiVersion: v1
kind: Pod
metadata:
  name: server
  namespace: tianzhi-check
  labels:
    app: tianzhi-check-server
spec:
  hostNetwork: false
  containers:
    - name: server
      image: registry.local/tianzhi/busybox:1.38.0-riscv64
      imagePullPolicy: Never
      command: ["/bin/sh", "-ec"]
      args: ["mkdir -p /www; echo tianzhi-pod-network-ok > /www/index.html; exec httpd -f -p 8080 -h /www"]
      readinessProbe:
        httpGet:
          path: /
          port: 8080
---
apiVersion: v1
kind: Pod
metadata:
  name: client
  namespace: tianzhi-check
spec:
  hostNetwork: false
  containers:
    - name: client
      image: registry.local/tianzhi/busybox:1.38.0-riscv64
      imagePullPolicy: Never
      command: ["/bin/sh", "-ec", "while :; do sleep 3600; done"]
---
apiVersion: v1
kind: Service
metadata:
  name: server
  namespace: tianzhi-check
spec:
  selector:
    app: tianzhi-check-server
  ports:
    - port: 8080
      targetPort: 8080
YAML
k -n tianzhi-check wait --for=condition=Ready pod/server pod/client --timeout=300s
pod_ip=$(k -n tianzhi-check get pod server -o jsonpath='{.status.podIP}')
service_ip=$(k -n tianzhi-check get service server -o jsonpath='{.spec.clusterIP}')
case "$service_ip" in
    ''|None) echo 'k3s-check: server must have a ClusterIP' >&2; exit 1 ;;
esac
[ -n "$pod_ip" ] || { echo 'k3s-check: server has no Pod IP' >&2; exit 1; }
for destination in "$pod_ip" "$service_ip" server.tianzhi-check.svc.cluster.local; do
    reply=$(k -n tianzhi-check exec client -- wget -T 20 -qO- "http://$destination:8080/")
    [ "$reply" = tianzhi-pod-network-ok ] || { echo "k3s-check: wrong reply from $destination" >&2; exit 1; }
done
k get nodes -o wide
k -n tianzhi-check get pods,services -o wide
echo 'k3s-check: PASS — Node Ready, two Pods, Pod IP HTTP, ClusterIP and cluster DNS'
