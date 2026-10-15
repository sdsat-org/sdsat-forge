#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Build an OCI image from the guest's static BusyBox; no registry is contacted.
set -eu

image_dir=${1:-/run/forge-kubernetes/image}
mkdir -p "$image_dir/rootfs/bin" "$image_dir/rootfs/etc" \
    "$image_dir/rootfs/dev" "$image_dir/rootfs/proc" \
    "$image_dir/rootfs/sys" "$image_dir/rootfs/tmp" \
    "$image_dir/oci/blobs/sha256"
cp "${FORGE_BUSYBOX:-/bin/busybox}" "$image_dir/rootfs/bin/busybox"
chmod 755 "$image_dir/rootfs/bin/busybox"
chmod 1777 "$image_dir/rootfs/tmp"
tar -C "$image_dir/rootfs" -cf "$image_dir/layer.tar" .
layer_hash=$(sha256sum "$image_dir/layer.tar" | cut -d ' ' -f 1)
layer_size=$(wc -c < "$image_dir/layer.tar" | tr -d ' ')
mv "$image_dir/layer.tar" "$image_dir/oci/blobs/sha256/$layer_hash"

cat > "$image_dir/config.json" <<EOF
{"architecture":"riscv64","os":"linux","config":{"Env":["PATH=/bin"],"Entrypoint":["/bin/busybox","sh","-c"],"Cmd":["while :; do /bin/busybox sleep 3600; done"],"WorkingDir":"/"},"rootfs":{"type":"layers","diff_ids":["sha256:$layer_hash"]}}
EOF
config_hash=$(sha256sum "$image_dir/config.json" | cut -d ' ' -f 1)
config_size=$(wc -c < "$image_dir/config.json" | tr -d ' ')
mv "$image_dir/config.json" "$image_dir/oci/blobs/sha256/$config_hash"

cat > "$image_dir/manifest.json" <<EOF
{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:$config_hash","size":$config_size},"layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"sha256:$layer_hash","size":$layer_size}]}
EOF
manifest_hash=$(sha256sum "$image_dir/manifest.json" | cut -d ' ' -f 1)
manifest_size=$(wc -c < "$image_dir/manifest.json" | tr -d ' ')
mv "$image_dir/manifest.json" "$image_dir/oci/blobs/sha256/$manifest_hash"
printf '{"imageLayoutVersion":"1.0.0"}\n' > "$image_dir/oci/oci-layout"
cat > "$image_dir/oci/index.json" <<EOF
{"schemaVersion":2,"manifests":[{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:$manifest_hash","size":$manifest_size,"platform":{"architecture":"riscv64","os":"linux"},"annotations":{"org.opencontainers.image.ref.name":"localhost/forge/busybox:local"}}]}
EOF
tar -C "$image_dir/oci" -cf "$image_dir/busybox.oci.tar" oci-layout index.json blobs
printf '%s\n' "$image_dir/busybox.oci.tar"
