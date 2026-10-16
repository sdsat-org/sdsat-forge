#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Expand only this installation's last rootfs partition on a physical SD card.
set -eu
export LC_ALL=C

fail() { printf 'grow-rootfs: %s\n' "$*" >&2; exit 1; }
for tool in findmnt readlink lsblk blkid blockdev sfdisk partx resize2fs; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing tool: $tool"
done
[ "$(findmnt -rn -o FSTYPE --target /)" = ext4 ] || fail "root filesystem is not ext4"
# A kernel-mounted root can be reported as /dev/root, which need not exist
# without udev. Resolve the mounted device's major/minor through sysfs instead.
root_number=$(findmnt -rn -o MAJ:MIN --target /)
case "$root_number" in ''|*[!0-9:]*) fail "invalid root device number" ;; esac
root_sys=$(readlink -f "/sys/dev/block/$root_number")
[ -d "$root_sys" ] || fail "root device is absent from sysfs"
rootdev="/dev/${root_sys##*/}"
[ -b "$rootdev" ] || fail "root is not a block device: $rootdev"
[ "$(lsblk -rdn -o TYPE "$rootdev")" = part ] || fail "root is not a partition"
diskname=$(lsblk -rdn -o PKNAME "$rootdev")
case "$diskname" in mmcblk[0-9]*) ;; *) fail "root is not on an SD controller" ;; esac
case "$diskname" in *[!a-z0-9]*) fail "invalid disk name" ;; esac
disk="/dev/$diskname"
sysdisk="/sys/class/block/$diskname"
[ "$(cat "$sysdisk/device/type")" = SD ] || fail "root device is not an SD card"
[ -b "$disk" ] && [ "$(lsblk -rdn -o TYPE "$disk")" = disk ] || fail "invalid SD disk"
[ "$(blockdev --getss "$disk")" = 512 ] || fail "expected 512-byte SD sectors"
# Zero uses mdevd/libudev-zero, so lsblk's udev-backed GPT fields can be empty.
# Probe the disk and partition table directly instead.
[ "$(blkid -p -s PTTYPE -o value "$disk")" = gpt ] || fail "SD does not have a GPT"
partname=${rootdev##*/}
partsys="$sysdisk/$partname"
partno=$(cat "$partsys/partition")
case "$partno" in ''|*[!0-9]*) fail "invalid partition number" ;; esac
[ "$rootdev" = "${disk}p${partno}" ] || fail "root partition does not belong to the SD disk"
[ "$(sfdisk --part-label "$disk" "$partno")" = rootfs ] || fail "root partition is not named rootfs"
root_uuid=$(sfdisk --part-uuid "$disk" "$partno" | tr 'A-F' 'a-f')
[ -n "$root_uuid" ] || fail "root has no GPT partition UUID"
expected=$(awk '$2 == "/" { print $1 }' /etc/fstab)
[ "$expected" = "PARTUUID=$root_uuid" ] || fail "root does not match this installation's fstab"
root_start=$(cat "$partsys/start")
case "$root_start" in ''|*[!0-9]*) fail "invalid root partition start" ;; esac

# Reject any layout with another partition at or after rootfs, including overlap.
for part in "$sysdisk"/"$diskname"p*; do
    [ -f "$part/partition" ] || continue
    [ "$part" = "$partsys" ] && continue
    start=$(cat "$part/start")
    size=$(cat "$part/size")
    [ "$start" -lt "$root_start" ] && [ "$((start + size))" -le "$root_start" ] ||
        fail "rootfs is not the last non-overlapping partition"
done

state=/var/lib/tianzhi
mkdir -p "$state"
sfdisk --dump "$disk" > "$state/partition-table.before"
# -N keeps this partition's start, type, UUID and name. '+' uses the remaining
# space; no other partition is changed. Update the live kernel table below.
printf ',+\n' | sfdisk --no-reread --no-tell-kernel -N "$partno" "$disk" ||
    fail "partition expansion failed"
[ "$(partx --raw --noheadings --nr "$partno" --output START "$disk")" = "$root_start" ] ||
    fail "partition start changed unexpectedly"
[ "$(sfdisk --part-uuid "$disk" "$partno" | tr 'A-F' 'a-f')" = "$root_uuid" ] ||
    fail "partition UUID changed unexpectedly"
new_size=$(partx --raw --noheadings --nr "$partno" --output SECTORS "$disk")
if [ "$(cat "$partsys/size")" != "$new_size" ]; then
    partx --update --nr "$partno" "$disk" || fail "kernel partition update failed; retry after reboot"
fi
[ "$(cat "$partsys/size")" = "$new_size" ] || fail "kernel still has the old partition size"
resize2fs "$rootdev"
printf 'PARTUUID=%s sectors=%s\n' "$root_uuid" "$new_size" > "$state/rootfs-grown"
printf 'grow-rootfs: %s now uses the available SD space\n' "$rootdev"
