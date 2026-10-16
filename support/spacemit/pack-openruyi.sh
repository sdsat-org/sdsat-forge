#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Create ordinary image files only; this script never opens a physical device.
set -efu
export LC_ALL=C

fail() { printf 'pack-openruyi: %s\n' "$*" >&2; exit 1; }
[ "$#" -eq 1 ] || fail "usage: sh support/spacemit/pack-openruyi.sh IMAGES_DIR"
images=$(cd "$1" && pwd)
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
for tool in fakeroot cpio sfdisk partx mkfs.ext4 mkfs.vfat mcopy \
    e2fsck blkid truncate dd awk sed find install gzip; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing tool: $tool"
done
for file in openruyi-zero.cpio.gz Image k1-x_MUSE-Card.dtb FSBL.bin rustsbi.bin u-boot.itb \
    bootinfo_spinor-fsbl.bin bootinfo_sd-fsbl.bin; do
    [ -f "$images/$file" ] && [ -s "$images/$file" ] || fail "missing regular input file: $images/$file"
done
for file in sdcard.img sdcard.img.gz bootfs.img rootfs.ext4 env_k1-x.txt \
    bootinfo_spinor.bin bootinfo_sd.bin fw_dynamic.itb \
    u-boot-muse-card-original.dtb u-boot-muse-card.dtb u-boot-muse-card.itb \
    rustsbi-usb-muse-card.itb; do
    [ ! -L "$images/$file" ] || fail "output must not be a symbolic link: $images/$file"
    [ ! -e "$images/$file" ] || [ -f "$images/$file" ] || fail "output is not a regular file: $images/$file"
done
if [ "${BOOTARGS+x}" = x ]; then
    bootargs=$BOOTARGS
else
    bootargs=$(awk '
        { sub(/\r$/, "") }
        /^\[/ { boot=($0 == "[boot]") }
        boot && /^append = "[^"\\]*"$/ {
            sub(/^append = "/, ""); sub(/"$/, ""); print; found++
        }
        END { if (found != 1) exit 1 }
    ' "$root/profiles/linux-openruyi-zero-sd/profile.toml") ||
        fail "cannot read profile boot.append; set BOOTARGS explicitly"
fi
case "$bootargs" in
    *"
"*) fail "BOOTARGS must be a single line" ;;
esac
case " $bootargs " in *" root="*) fail "root is supplied by the SD image packer" ;; esac
UBOOT_LINUX_ISA=1 sh "$root/support/spacemit/pack-firmware.sh" \
    "$images" "$root/boards/spacemit/muse-card/boot-openruyi.cmd"

# All filesystem assembly stays in this private directory.
work=$(mktemp -d "${TMPDIR:-/tmp}/forge-openruyi.XXXXXXXX")
trap 'rm -rf -- "$work"' 0
trap 'exit 1' HUP INT TERM
printf 'Extracting the openRuyi Zero RVA20 filesystem...\n'
gzip -dc "$images/openruyi-zero.cpio.gz" > "$work/rootfs.cpio"

new_uuid() { cat /proc/sys/kernel/random/uuid; }
disk_uuid=$(new_uuid)
boot_uuid=$(new_uuid)
root_uuid=$(new_uuid)
filesystem_uuid=$(new_uuid)
truncate -s 4G "$work/sdcard.img"
# Official partition_universal.json: bootfs at 4 MiB, rootfs at 260 MiB.
sfdisk "$work/sdcard.img" > "$work/partition.log" <<EOF
label: gpt
label-id: $disk_uuid
unit: sectors
first-lba: 34
start=256, size=512, name="fsbl"
start=768, size=128, name="env"
start=896, size=768, name="opensbi"
start=1664, size=4096, name="uboot"
start=8192, size=524288, name="bootfs", uuid=$boot_uuid
start=532480, name="rootfs", uuid=$root_uuid
EOF
root_sectors=$(partx --raw --noheadings --nr 6 --output SECTORS "$work/sdcard.img")
printf 'Creating persistent ext4 rootfs...\n'
FAKEROOTDONTTRYCHOWN=1 fakeroot sh -eu -c '
    work=$1; support=$2; root_uuid=$3; boot_uuid=$4; filesystem_uuid=$5; root_bytes=$6; images=$7
    mkdir "$work/rootfs"
    (cd "$work/rootfs" && cpio -idmu --no-absolute-filenames < "$work/rootfs.cpio")
    sh "$support/prepare-rootfs.sh" "$work/rootfs" "$work/rootfs.ext4" \
        "$root_uuid" "$boot_uuid" "$filesystem_uuid" "$root_bytes" "$images"
' sh "$work" "$root/support/openruyi-sd" "$root_uuid" "$boot_uuid" \
    "$filesystem_uuid" "$((root_sectors * 512))" "$images"
e2fsck -fn "$work/rootfs.ext4"

printf 'bootargs=%s root=PARTUUID=%s\n' "$bootargs" "$root_uuid" > "$work/env_k1-x.txt"
# The vendor boot path appends DT arguments missing from the environment.
# Remove the SDK's rdinit=/init and keep env_k1-x.txt as the only argument source.
cp "$images/k1-x_MUSE-Card.dtb" "$work/k1-x_MUSE-Card.dtb"
if fdtget -t s "$work/k1-x_MUSE-Card.dtb" /chosen bootargs >/dev/null 2>&1; then
    fdtput -d "$work/k1-x_MUSE-Card.dtb" /chosen bootargs
fi
truncate -s 256M "$work/bootfs.img"
mkfs.vfat -F 32 -n bootfs "$work/bootfs.img" >/dev/null
mcopy -i "$work/bootfs.img" "$images/Image" "$work/k1-x_MUSE-Card.dtb" "$work/env_k1-x.txt" ::/

write_region() {
    file=$1; offset=$2; capacity=$3
    bytes=$(wc -c < "$file")
    [ "$bytes" -gt 0 ] && [ "$bytes" -le "$capacity" ] || fail "$(basename -- "$file") exceeds its SD region"
    dd if="$file" of="$work/sdcard.img" bs=4M oflag=seek_bytes seek="$offset" conv=notrunc,sparse status=none
}
# Copy only the 80-byte bootinfo, preserving the PMBR partition entry/signature.
[ "$(wc -c < "$images/bootinfo_sd-fsbl.bin")" -eq 80 ] || fail "expected an 80-byte SD bootinfo"
write_region "$images/bootinfo_sd-fsbl.bin" 0 80
write_region "$images/FSBL.bin" "$((128 * 1024))" "$((256 * 1024))"
write_region "$images/fw_dynamic.itb" "$((448 * 1024))" "$((384 * 1024))"
write_region "$images/u-boot-muse-card.itb" "$((832 * 1024))" "$((2 * 1024 * 1024))"
write_region "$work/bootfs.img" "$((4 * 1024 * 1024))" "$((256 * 1024 * 1024))"
write_region "$work/rootfs.ext4" "$((260 * 1024 * 1024))" "$((root_sectors * 512))"
sfdisk --verify "$work/sdcard.img"
printf 'Compressing SD image for transfer...\n'
gzip -1 -c "$work/sdcard.img" > "$work/sdcard.img.gz"
for file in sdcard.img sdcard.img.gz bootfs.img rootfs.ext4 env_k1-x.txt; do
    cp --sparse=always "$work/$file" "$images/$file"
done
cp "$images/bootinfo_spinor-fsbl.bin" "$images/bootinfo_spinor.bin"
cp "$images/bootinfo_sd-fsbl.bin" "$images/bootinfo_sd.bin"
printf 'Created 4 GiB sdcard.img, sdcard.img.gz, bootfs.img and rootfs.ext4 in %s\n' "$images"
