#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Called inside fakeroot by pack-openruyi.sh, with a newly extracted Zero tree.
set -efu
export LC_ALL=C
[ "$#" -eq 7 ] || exit 2
tree=$1
image=$2
root_partuuid=$3
boot_partuuid=$4
filesystem_uuid=$5
root_bytes=$6
images=$7
support=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

for dir in etc usr usr/lib var; do
    [ -d "$tree/$dir" ] && [ ! -L "$tree/$dir" ] || {
        printf 'prepare-rootfs: unexpected directory: %s\n' "$tree/$dir" >&2
        exit 1
    }
done
# TMPDIR may be mounted noexec: inspect recorded permission bits rather than
# access(X_OK), since target programs are never executed on the build host.
executable_file() {
    [ -f "$1" ] || return 1
    mode=$(stat -Lc %a "$1")
    [ "$((0$mode & 0111))" -ne 0 ]
}
executable_file "$tree/usr/bin/busybox"
[ -f "$tree/etc/inittab" ]
for tool in sfdisk partx findmnt lsblk blockdev blkid resize2fs mount mdevd mdevd-coldplug; do
    found=
    for dir in usr/bin usr/sbin; do
        if executable_file "$tree/$dir/$tool"; then found=1; fi
    done
    [ -n "$found" ] || {
        printf 'prepare-rootfs: Zero image lacks required tool: %s\n' "$tool" >&2
        exit 1
    }
done
# The Forge kernel has its required drivers built in; no foreign modules belong
# in this rootfs. The release is assembled from headless Zero packages only.
rm -rf -- "$tree/usr/lib/modules"
mkdir -p "$tree/boot" "$tree/proc" "$tree/sys" "$tree/dev/pts" \
    "$tree/run" "$tree/tmp" "$tree/usr/sbin" "$tree/usr/libexec/tianzhi" \
    "$tree/var/lib/tianzhi"
sed -e "s/@ROOT_PARTUUID@/$root_partuuid/g" -e "s/@BOOT_PARTUUID@/$boot_partuuid/g" \
    "$support/fstab" > "$tree/etc/fstab"
printf 'tianzhi-node\n' > "$tree/etc/hostname"
# Match PID 1's built-in locale. Zero's empty /usr/lib64/locale does not
# support C.UTF-8; selecting it also triggers profile.d/lang.sh's bad fallback.
printf 'LANG=C\n' > "$tree/etc/locale.conf"
printf '127.0.0.1 localhost\n127.0.1.1 tianzhi-node\n::1 localhost\n' > "$tree/etc/hosts"
if [ -f "$tree/usr/share/zoneinfo/Asia/Shanghai" ]; then
    ln -snf /usr/share/zoneinfo/Asia/Shanghai "$tree/etc/localtime"
fi
# Preserve Zero's BusyBox init and askfirst console behavior. The kernel invokes
# /sbin/init, and board-specific startup helpers stay under /usr/libexec.
install -m 755 "$support/init.sh" "$tree/usr/libexec/tianzhi/init"
install -m 755 "$support/rcS.sh" "$tree/usr/libexec/tianzhi/rcS"
install -m 755 "$support/udhcpc.sh" "$tree/usr/libexec/tianzhi/udhcpc"
install -m 755 "$support/grow-rootfs.sh" "$tree/usr/libexec/tianzhi/grow-rootfs"
ln -snf /usr/libexec/tianzhi/init "$tree/usr/sbin/init"
sed -e 's|::sysinit:.*|::sysinit:/usr/libexec/tianzhi/rcS|' -e '/^tty1::/d' \
    "$tree/etc/inittab" > "$tree/etc/inittab.sd"
mv "$tree/etc/inittab.sd" "$tree/etc/inittab"
rm -f -- "$tree/init" "$tree/etc/init.d/rcS" "$tree/var/lib/tianzhi/rootfs-grown"
chmod 1777 "$tree/tmp"
if [ -f "$images/k3s" ]; then
    for file in k3s k3s-start.sh k3s-check.sh k3s-config.yaml k3s-airgap-images-riscv64.tar \
        k3s-LICENSE NOTICE.k3s THIRD_PARTY_LICENSES.k3s.txt; do
        [ -f "$images/$file" ] && [ -s "$images/$file" ] || {
            printf 'prepare-rootfs: missing K3s artifact: %s\n' "$file" >&2
            exit 1
        }
    done
    mkdir -p "$tree/etc/rancher/k3s" "$tree/var/lib/rancher/k3s/agent/images" \
        "$tree/usr/share/licenses/k3s"
    install -m 755 "$images/k3s" "$tree/usr/bin/k3s"
    install -m 755 "$images/k3s-start.sh" "$tree/usr/bin/k3s-start"
    install -m 755 "$images/k3s-check.sh" "$tree/usr/bin/k3s-check"
    install -m 644 "$images/k3s-config.yaml" "$tree/etc/rancher/k3s/config.yaml"
    install -m 644 "$images/k3s-airgap-images-riscv64.tar" \
        "$tree/var/lib/rancher/k3s/agent/images/k3s-airgap-images-riscv64.tar"
    for file in k3s-LICENSE NOTICE.k3s THIRD_PARTY_LICENSES.k3s.txt; do
        install -m 644 "$images/$file" "$tree/usr/share/licenses/k3s/$file"
    done
fi
truncate -s "$root_bytes" "$image"
mkfs.ext4 -q -F -m 1 -L rootfs -U "$filesystem_uuid" -d "$tree" "$image"
