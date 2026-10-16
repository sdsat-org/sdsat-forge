#!/bin/bash
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Rebuild the prebuilt K3s executable on Linux with a RISC-V musl toolchain.
set -euo pipefail
if [ "$#" -ne 2 ]; then
    echo 'usage: build-riscv64.sh WORK_DIRECTORY RISCV64_MUSL_GCC' >&2
    exit 2
fi
mkdir -p "$1"
base=$(cd "$1" && pwd)
CC=$(realpath "$2")
export CC
export PATH="$(dirname "$CC"):$base/go/bin:$PATH"
for tool in curl sha256sum git make gperf pkg-config tar zstd file "$CC"; do
    command -v "$tool" >/dev/null
done

fetch() {
    local name=$1 digest=$2 url=$3
    if ! printf '%s  %s\n' "$digest" "$base/$name" | sha256sum -c --status 2>/dev/null; then
        curl --fail --location --retry 3 "$url" -o "$base/$name"
    fi
    printf '%s  %s\n' "$digest" "$base/$name" | sha256sum -c
}
clone() {
    local directory=$1 url=$2 tag=$3 commit=$4
    if [ ! -d "$directory/.git" ]; then
        git clone --depth 1 --branch "$tag" "$url" "$directory"
    fi
    [ "$(git -C "$directory" rev-parse HEAD)" = "$commit" ] || {
        echo "unexpected revision in $directory" >&2
        exit 1
    }
    git -C "$directory" diff --quiet
    git -C "$directory" diff --cached --quiet
}

fetch go.tar.gz d0f743b33e8d8945e6b1f432edd15785c70507121d6e2a723b21285eddf8b57b \
    https://go.dev/dl/go1.26.8.linux-amd64.tar.gz
tar -xzf "$base/go.tar.gz" -C "$base"
fetch libseccomp.tar.gz 501f66c667225d53791b97e1d7cf85ab764c297d04881f60f38f451c4b0ee1be \
    https://github.com/seccomp/libseccomp/releases/download/v2.6.1/libseccomp-2.6.1.tar.gz
tar -xzf "$base/libseccomp.tar.gz" -C "$base"
(
    cd "$base/libseccomp-2.6.1"
    ./configure --host=riscv64-buildroot-linux-musl --prefix="$base/sysroot" \
        --disable-shared --enable-static
    make -j"${JOBS:-8}"
    make install
)
clone "$base/source" https://github.com/k3s-io/k3s.git v1.37.1+k3s1 \
    356c025475bbefaa09cf0d7e48e8e2adbbde57db
clone "$base/source/build/src/github.com/opencontainers/runc" \
    https://github.com/opencontainers/runc.git v1.4.2 \
    c241c0bb5e60a8e8c1b2e53d4eca8d0068d8d57e
clone "$base/source/build/src/github.com/containerd/containerd" \
    https://github.com/k3s-io/containerd.git v2.3.4-k3s1 \
    7f3710923bfb9ca8f437c7cf1c55c74037018168
fetch k3s-root-riscv64.tar 3b76a4a5bfc5c8623702a3b99e3015cd36b0336dd73c7ba4a765d018dc5a9685 \
    https://github.com/k3s-io/k3s-root/releases/download/v0.15.2/k3s-root-riscv64.tar
tar -xf "$base/k3s-root-riscv64.tar" -C "$base/source"
mkdir -p "$base/source/build/static/charts"
for chart in gateway-api-crd:gateway-api-crd-1.6.103.tgz \
    traefik-crd:traefik-crd-41.4.2+up41.4.0.tgz traefik:traefik-41.4.2+up41.4.0.tgz; do
    name=${chart%%:*}
    filename=${chart#*:}
    case "$name" in
        gateway-api-crd) digest=968f8bcf3ec81f9a0b8a686336b999c43df145d87d29c305a714c1ff0402687c ;;
        traefik-crd) digest=ce6acfefa2c9a991ac7906c3d8164be0b28721c00fa9e8d9f884d3f88a81db35 ;;
        traefik) digest=b78f0b7d76c88daf9b8cdf6993bde76a4e93b90980dbfb75209e44764971e98c ;;
    esac
    fetch "$filename" "$digest" "https://k3s.io/k3s-charts/assets/$name/$filename"
    cp "$base/$filename" "$base/source/build/static/charts/$filename"
done

export GOOS=linux GOARCH=riscv64 STATIC_BUILD=false SELINUX=false
export GOMODCACHE="$base/gomodcache" GOCACHE="$base/gocache" GOFLAGS="-p=${JOBS:-8}"
# Use the bundled SQLite implementation and a static musl link. The official
# STATIC_BUILD switch instead requires a separately installed libsqlite3.a.
export CGO_CPPFLAGS="-I$base/sysroot/include" CGO_LDFLAGS="-L$base/sysroot/lib -static"
export PKG_CONFIG_LIBDIR="$base/sysroot/lib/pkgconfig"
unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY
cd "$base/source"
go mod download
./scripts/build
./scripts/package-cli
file dist/artifacts/k3s-riscv64 bin/k3s bin/runc bin/containerd-shim-runc-v2
sha256sum dist/artifacts/k3s-riscv64
