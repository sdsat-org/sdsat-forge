#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Build the small offline OCI image set from pinned upstream inputs.
set -efu
export LC_ALL=C
[ "$#" -eq 3 ] || {
    printf 'usage: sh support/k3s/build-airgap.sh OUTPUT_DIR MUSL_RISCV64_GCC BUSYBOX_ELF\n' >&2
    exit 2
}
mkdir -p "$1"
out=$(cd "$1" && pwd)
compiler=$2
busybox=$3
qemu=${QEMU_RISCV64:-qemu-riscv64}
project=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
for tool in curl sha256sum tar gzip "$qemu" "$compiler"; do
    command -v "$tool" >/dev/null
done
stage=$(mktemp -d "$out/.airgap.XXXXXXXX")
trap 'rm -rf -- "$stage"' 0
trap 'exit 1' HUP INT TERM
sources="$out/airgap-sources"
mkdir -p "$sources" "$stage/oci/blobs/sha256"

fetch() {
    name=$1; digest=$2; url=$3
    if ! printf '%s  %s\n' "$digest" "$sources/$name" | sha256sum -c --status 2>/dev/null; then
        curl --fail --location --retry 3 --output "$sources/$name" "$url"
    fi
    printf '%s  %s\n' "$digest" "$sources/$name" | sha256sum -c
}
fetch pause.c 14a3fe21ab5f8e62937a2b453e76f3b94b0d3c7c04c3d09bc37195f54c1695ea \
    https://raw.githubusercontent.com/kubernetes/kubernetes/v1.34.1/build/pause/linux/pause.c
fetch coredns.tgz d751f467d70cebbf56656455265939b2ae84323b6ee6d0cc2ce92c69d1eef918 \
    https://github.com/coredns/coredns/releases/download/v1.14.7/coredns_1.14.7_linux_riscv64.tgz
fetch busybox-1.38.0.tar.bz2 34f9ea6ff8636f2c9241153b9114eefa9e65674a45318ae1ef95bb5f31c53bb2 \
    https://busybox.net/downloads/busybox-1.38.0.tar.bz2
fetch LICENSE.pause cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30 \
    https://raw.githubusercontent.com/kubernetes/kubernetes/v1.34.1/LICENSE
fetch LICENSE.coredns f0530760b1e2ffcf23df322db638ee06d06fc07fead0ed3ddd710f2e224698a9 \
    https://raw.githubusercontent.com/coredns/coredns/v1.14.7/LICENSE
cp "$project/packages/busybox/defconfig" "$sources/busybox.defconfig"
cp "$project/packages/busybox/package.toml" "$sources/busybox.package.toml"
mkdir -p "$stage/pause" "$stage/busybox/bin" "$stage/coredns"
for name in pause busybox coredns; do mkdir -p "$stage/$name/usr/share/licenses/$name"; done
cp "$sources/LICENSE.pause" "$stage/pause/usr/share/licenses/pause/LICENSE"
cp "$sources/LICENSE.coredns" "$stage/coredns/usr/share/licenses/coredns/LICENSE"
tar -xOf "$sources/busybox-1.38.0.tar.bz2" busybox-1.38.0/LICENSE \
    > "$stage/busybox/usr/share/licenses/busybox/LICENSE"
"$compiler" -Os -s -Wall -Werror -static -march=rv64gc -mabi=lp64d \
    -DVERSION=v3.10.1-v1.34.1 -o "$stage/pause/pause" "$sources/pause.c"
cp "$busybox" "$stage/busybox/bin/busybox"
chmod 755 "$stage/busybox/bin/busybox"
tar -xzf "$sources/coredns.tgz" -C "$stage/coredns" coredns
chmod 755 "$stage/coredns/coredns"
"$qemu" -cpu rv64 "$stage/pause/pause" -v
"$qemu" -cpu rv64 "$stage/busybox/bin/busybox" --help > "$sources/busybox.version" 2>&1
grep -q '^BusyBox v1\.38\.0 ' "$sources/busybox.version" || {
    printf 'build-airgap: the input must be the Forge BusyBox 1.38.0 binary\n' >&2
    exit 1
}
"$qemu" -cpu rv64 "$stage/busybox/bin/busybox" echo BUSYBOX_RVA20_OK
"$qemu" -cpu rv64 "$stage/busybox/bin/busybox" --list > "$stage/applets"
while IFS= read -r applet; do
    [ "$applet" = busybox ] || ln -s busybox "$stage/busybox/bin/$applet"
done < "$stage/applets"
"$qemu" -cpu rv64 "$stage/coredns/coredns" -version

hash() { sha256sum "$1" | cut -d ' ' -f 1; }
blob() {
    digest=$(hash "$1")
    cp "$1" "$stage/oci/blobs/sha256/$digest"
    printf '%s' "$digest"
}
descriptor() {
    printf '{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:%s","size":%s,"platform":{"architecture":"riscv64","os":"linux"},"annotations":{"org.opencontainers.image.ref.name":"%s","io.containerd.image.name":"%s"}}' \
        "$manifest_digest" "$manifest_size" "$1" "$1"
}
make_image() {
    name=$1; entrypoint=$2; tag=$3
    tar --sort=name --format=ustar --mtime=@0 --owner=0 --group=0 --numeric-owner \
        -cf "$stage/layer.tar" -C "$stage/$name" .
    diff_id=$(hash "$stage/layer.tar")
    gzip -1n -c "$stage/layer.tar" > "$stage/layer.tar.gz"
    layer_digest=$(blob "$stage/layer.tar.gz")
    layer_size=$(wc -c < "$stage/layer.tar.gz")
    printf '{"created":"1970-01-01T00:00:00Z","architecture":"riscv64","os":"linux","config":{"Env":["PATH=/bin:/usr/bin"],"Entrypoint":["%s"],"WorkingDir":"/"},"rootfs":{"type":"layers","diff_ids":["sha256:%s"]}}' \
        "$entrypoint" "$diff_id" > "$stage/config.json"
    config_digest=$(blob "$stage/config.json")
    config_size=$(wc -c < "$stage/config.json")
    printf '{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:%s","size":%s},"layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha256:%s","size":%s}]}' \
        "$config_digest" "$config_size" "$layer_digest" "$layer_size" > "$stage/manifest.json"
    manifest_digest=$(blob "$stage/manifest.json")
    manifest_size=$(wc -c < "$stage/manifest.json")
    descriptor "$tag"
}
{
    printf '{"schemaVersion":2,"manifests":['
    make_image pause /pause registry.local/tianzhi/pause:3.10.1-riscv64
    printf ','
    make_image busybox /bin/sh registry.local/tianzhi/busybox:1.38.0-riscv64
    printf ','
    make_image coredns /coredns registry.local/tianzhi/coredns:1.14.7-riscv64
    printf ','
    # K3s's bundled DNS manifest uses this name. It aliases our upstream binary
    # image; this is not a Rancher-built container image.
    descriptor docker.io/rancher/mirrored-coredns-coredns:1.14.7
    printf ']}\n'
} > "$stage/oci/index.json"
printf '{"imageLayoutVersion":"1.0.0"}\n' > "$stage/oci/oci-layout"
tar --sort=name --format=ustar --mtime=@0 --owner=0 --group=0 --numeric-owner \
    -cf "$out/k3s-airgap-images-riscv64.tar" -C "$stage/oci" .
sha256sum "$busybox" "$sources/pause.c" "$sources/coredns.tgz" \
    "$out/k3s-airgap-images-riscv64.tar" > "$out/airgap-inputs.sha256"
printf 'Created %s/k3s-airgap-images-riscv64.tar\n' "$out"
