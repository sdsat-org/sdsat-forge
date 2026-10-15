#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Ephemeral single-node demo for QEMU's restricted user network.
set -eu
umask 077

export PATH=/bin:/sbin:/usr/bin:/usr/sbin
config_dir=${FORGE_KUBERNETES_CONFIG:-/etc/kubernetes/forge}
state_dir=/run/forge-kubernetes
log_dir=/var/log/forge-kubernetes
node_ip=${FORGE_KUBERNETES_NODE_IP:-10.0.2.15}
retries=${FORGE_KUBERNETES_RETRIES:-120}
export KUBECONFIG=$state_dir/kubeconfig
mkdir -p "$state_dir/pki" "$log_dir" /var/lib/forge-kubernetes/etcd \
    /var/lib/kubelet /var/log/pods /var/log/containers /run/containerd /dev/shm

diagnostics() {
    echo 'Kubernetes startup failed; component logs follow.' >&2
    for log in "$log_dir"/*.log; do
        [ -f "$log" ] || continue
        printf '\n--- %s ---\n' "$log" >&2
        tail -n 50 "$log" >&2 || true
    done
}
trap 'status=$?; if [ "$status" -ne 0 ]; then diagnostics; fi; exit "$status"' 0

if [ -f "$state_dir/started" ] && [ -s "$KUBECONFIG" ]; then
    running=true
    for name in containerd etcd apiserver controller-manager scheduler kubelet; do
        if [ ! -f "$state_dir/$name.pid" ] || ! kill -0 "$(cat "$state_dir/$name.pid")" 2>/dev/null; then
            running=false
        fi
    done
    if [ "$running" = true ] && kubectl --request-timeout=5s get --raw=/readyz > /dev/null 2>&1; then
        mkdir -p /root/.kube
        cp "$KUBECONFIG" /root/.kube/config
        echo 'Kubernetes is already running.'
        exit 0
    fi
fi

# runc still tracks processes in cgroup2 when CRI resource controls are disabled.
# Asterinas implements recursive private mount propagation.
mount --make-rprivate /
mkdir -p /sys/fs/cgroup
if ! grep -q ' /sys/fs/cgroup cgroup2 ' /proc/mounts; then
    mount -t cgroup2 none /sys/fs/cgroup
fi
if ! grep -q ' /dev/shm tmpfs ' /proc/mounts; then
    mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs /dev/shm
fi
chmod 1777 /dev/shm

start_daemon() {
    name=$1
    shift
    if [ -f "$state_dir/$name.pid" ] && kill -0 "$(cat "$state_dir/$name.pid")" 2>/dev/null; then
        return
    fi
    printf 'Starting %s\n' "$name"
    # Keep components running independently of the interactive login session.
    "${FORGE_BUSYBOX:-/bin/busybox}" setsid "$@" < /dev/null > "$log_dir/$name.log" 2>&1 &
    echo "$!" > "$state_dir/$name.pid"
}

wait_for() {
    name=$1
    shift
    attempt=0
    printf 'Waiting for %s\n' "$name"
    until "$@" < /dev/null > "$log_dir/$name-ready.log" 2>&1; do
        if [ -f "$state_dir/$name.pid" ] && ! kill -0 "$(cat "$state_dir/$name.pid")" 2>/dev/null; then
            echo "$name exited before becoming ready" >&2
            return 1
        fi
        attempt=$((attempt + 1))
        if [ "$attempt" -ge "$retries" ]; then
            echo "Timed out waiting for $name" >&2
            return 1
        fi
        if [ "$((attempt % 10))" -eq 0 ]; then
            printf 'Waiting for %s (attempt %s/%s)\n' "$name" "$attempt" "$retries"
        fi
        sleep 1
    done
}

api_get() {
    # Match the local kubeconfig's TLS policy without starting kubectl each time.
    "${FORGE_BUSYBOX:-/bin/busybox}" wget -q -O - -T 5 -Y off --no-check-certificate \
        --header "Authorization: Bearer $token" "https://127.0.0.1:6443$1"
}

containerd_ready() {
    # Written after srv.Wait(): CRI Version rejects requests before initialization.
    grep -q 'containerd successfully booted' "$log_dir/containerd.log" &&
        ctr --timeout 5s --address /run/containerd/containerd.sock version
}

# Asterinas brings lo up itself and currently rejects RTM_NEWLINK.
hostname tianzhi-node
if ! ip link set lo up > "$log_dir/loopback.log" 2>&1; then
    ip link show dev lo | grep -q UP
fi
printf '127.0.0.1 localhost\n%s tianzhi-node\n' "$node_ip" > /etc/hosts
[ -f /etc/resolv.conf ] || : > /etc/resolv.conf

if [ ! -s "$state_dir/tokens.csv" ]; then
    token=$(od -An -N32 -tx1 /dev/urandom | tr -d ' \n')
    printf '%s,forge,forge,"system:masters"\n' "$token" > "$state_dir/tokens.csv"
fi
token=$(cut -d, -f1 "$state_dir/tokens.csv")
cat > "$KUBECONFIG" <<EOF
apiVersion: v1
kind: Config
clusters:
- name: forge
  cluster:
    server: https://127.0.0.1:6443
    insecure-skip-tls-verify: true
users:
- name: forge
  user:
    token: $token
contexts:
- name: forge
  context:
    cluster: forge
    user: forge
current-context: forge
EOF
mkdir -p /root/.kube
cp "$KUBECONFIG" /root/.kube/config

# These daemons initialize while kubeadm prepares the local certificates.
start_daemon containerd containerd --config "$config_dir/containerd.toml"
start_daemon etcd env ETCD_UNSUPPORTED_ARCH=riscv64 etcd \
    --name forge --data-dir /var/lib/forge-kubernetes/etcd \
    --listen-client-urls http://127.0.0.1:2379 \
    --advertise-client-urls http://127.0.0.1:2379 \
    --listen-peer-urls http://127.0.0.1:2380 \
    --initial-advertise-peer-urls http://127.0.0.1:2380 \
    --initial-cluster forge=http://127.0.0.1:2380

# Generate fresh EC keys in /run each boot; avoid repeated RSA generation in QEMU.
# Explicit version avoids kubeadm's online version discovery. This source is 1.38.
cat > "$state_dir/kubeadm.yaml" <<EOF
apiVersion: kubeadm.k8s.io/v1beta4
kind: InitConfiguration
localAPIEndpoint:
  advertiseAddress: "$node_ip"
---
apiVersion: kubeadm.k8s.io/v1beta4
kind: ClusterConfiguration
kubernetesVersion: v1.38.0-alpha.0
certificatesDir: "$state_dir/pki"
encryptionAlgorithm: ECDSA-P256
networking:
  serviceSubnet: 10.96.0.0/24
apiServer:
  certSANs:
  - 127.0.0.1
  - "$node_ip"
etcd:
  external:
    endpoints:
    - http://127.0.0.1:2379
EOF
kubeadm init phase certs all --config "$state_dir/kubeadm.yaml" \
    > "$log_dir/certificates.log" 2>&1
wait_for etcd etcdctl --endpoints=http://127.0.0.1:2379 \
    --dial-timeout=2s --command-timeout=3s endpoint health
# These root-owned demo services share one ephemeral local serving certificate.
start_daemon apiserver kube-apiserver \
    --bind-address=127.0.0.1 --advertise-address="$node_ip" --secure-port=6443 \
    --cert-dir="$state_dir/pki" --etcd-servers=http://127.0.0.1:2379 \
    --tls-cert-file="$state_dir/pki/apiserver.crt" \
    --tls-private-key-file="$state_dir/pki/apiserver.key" \
    --service-cluster-ip-range=10.96.0.0/24 \
    --anonymous-auth=false --authorization-mode=AlwaysAllow --allow-privileged=true \
    --token-auth-file="$state_dir/tokens.csv" \
    --service-account-issuer=https://kubernetes.default.svc \
    --service-account-key-file="$state_dir/pki/sa.pub" \
    --service-account-signing-key-file="$state_dir/pki/sa.key" \
    --kubelet-preferred-address-types=InternalIP

# Scheduler and kubelet retry API connections while the server initializes.
start_daemon scheduler kube-scheduler \
    --kubeconfig="$KUBECONFIG" --authentication-kubeconfig="$KUBECONFIG" \
    --authorization-kubeconfig="$KUBECONFIG" --bind-address=127.0.0.1 \
    --tls-cert-file="$state_dir/pki/apiserver.crt" \
    --tls-private-key-file="$state_dir/pki/apiserver.key" \
    --leader-elect=false
wait_for containerd containerd_ready
start_daemon kubelet kubelet --config="$config_dir/kubelet.yaml" \
    --kubeconfig="$KUBECONFIG" --hostname-override=tianzhi-node \
    --node-ip="$node_ip" --address="$node_ip" --root-dir=/var/lib/kubelet

# Prepare the workload image while the API server initializes.
if ! ctr --timeout 5s --address /run/containerd/containerd.sock --namespace k8s.io \
    images list --quiet | grep -qx localhost/forge/busybox:local; then
    sh "$config_dir/make-image.sh" "$state_dir/image" > "$log_dir/image.log" 2>&1
    ctr --timeout 120s --address /run/containerd/containerd.sock --namespace k8s.io \
        images import --local --snapshotter native --platform linux/riscv64 \
        --label io.cri-containerd.image=managed \
        "$state_dir/image/busybox.oci.tar" >> "$log_dir/image.log" 2>&1
fi
wait_for apiserver api_get /readyz

start_daemon controller-manager kube-controller-manager \
    --kubeconfig="$KUBECONFIG" --authentication-kubeconfig="$KUBECONFIG" \
    --authorization-kubeconfig="$KUBECONFIG" --bind-address=127.0.0.1 \
    --tls-cert-file="$state_dir/pki/apiserver.crt" \
    --tls-private-key-file="$state_dir/pki/apiserver.key" \
    --leader-elect=false --use-service-account-credentials=false \
    --service-account-private-key-file="$state_dir/pki/sa.key"
wait_for kubelet api_get /api/v1/nodes/tianzhi-node
wait_for controller-manager api_get /api/v1/namespaces/default/serviceaccounts/default
for name in containerd etcd apiserver controller-manager scheduler kubelet; do
    kill -0 "$(cat "$state_dir/$name.pid")"
done
touch "$state_dir/started"
echo 'Kubernetes components started. Run sh /etc/kubernetes/forge/check.sh to submit and verify the Pod.'
