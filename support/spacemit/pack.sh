#!/bin/sh
# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Package MUSE-Card firmware and a NOR/SD-loadable Asterinas FIT; never flash a device.
set -efu
export LC_ALL=C

fail() { printf 'pack: %s\n' "$*" >&2; exit 1; }
[ "$#" -eq 1 ] || fail "usage: sh support/spacemit/pack.sh IMAGES_DIR"
images=$(cd "$1" && pwd)
root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
board="$root/boards/spacemit/muse-card"
objcopy=${OBJCOPY:-riscv64-linux-gnu-objcopy}
readelf=${READELF:-riscv64-linux-gnu-readelf}
for tool in mkimage dumpimage dtc fdtget fdtput gzip "$objcopy" "$readelf"; do
    command -v "$tool" >/dev/null 2>&1 || fail "missing tool: $tool"
done
for file in rustsbi.bin asterinas.elf initramfs.cpio u-boot.itb \
    bootinfo_spinor-fsbl.bin bootinfo_sd-fsbl.bin; do
    [ -s "$images/$file" ] || fail "missing or empty input: $images/$file"
done

# Keep timestamps reproducible, including when building from an exported source tree.
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
stage=$(mktemp -d "$images/.muse-card.XXXXXXXX")
trap 'rm -rf -- "$stage"' 0
trap 'exit 1' HUP INT TERM
cp "$images/rustsbi.bin" "$images/initramfs.cpio" "$images/u-boot.itb" "$stage/"
cp "$board/asterinas.its" "$stage/"
for medium in spinor sd; do
    [ "$(wc -c < "$images/bootinfo_$medium-fsbl.bin")" -eq 80 ] ||
        fail "expected an 80-byte $medium bootinfo from spacemit-fsbl"
    cp "$images/bootinfo_$medium-fsbl.bin" "$stage/bootinfo_$medium.bin"
done

within() {
    bytes=$(wc -c < "$1")
    [ "$bytes" -gt 0 ] && [ "$bytes" -le "$2" ] ||
        fail "$(basename -- "$1") is $bytes bytes; allowed range is 1..$2"
}

# PR #3872 permits loading this image at 4 MiB while retaining its ELF link address.
# Check the entire in-memory kernel, including BSS, against the ramdisk at 32 MiB.
"$readelf" -W -l "$images/asterinas.elf" > "$stage/program-headers"
awk '$1 == "LOAD" { print $4, $6 }' "$stage/program-headers" > "$stage/load-segments"
[ -s "$stage/load-segments" ] || fail "Asterinas ELF has no LOAD segments"
read -r first_address first_size < "$stage/load-segments"
[ "$((first_address))" -eq "$((0x80200000))" ] || fail "unexpected Asterinas ELF load base"
while read -r address memory_size; do
    [ "$((address))" -ge "$((0x80200000))" ] &&
        [ "$((address - 0x80200000 + memory_size + 0x400000))" -le "$((0x2000000))" ] ||
        fail "Asterinas kernel memory overlaps the initramfs at 32 MiB"
done < "$stage/load-segments"
"$objcopy" -O binary "$images/asterinas.elf" "$stage/asterinas.bin"
within "$stage/asterinas.bin" "$((0x2000000 - 0x400000))"
# U-Boot expands the kernel into its checked load range. Omit gzip timestamps
# and filenames so that the same inputs produce the same FIT.
gzip -n -9 -c "$stage/asterinas.bin" > "$stage/asterinas.bin.gz"
within "$stage/initramfs.cpio" "$((0x3000000 - 0x2000000))"

sh "$root/support/spacemit/pack-firmware.sh" "$stage" "$board/boot.cmd"
uboot_dtb="$stage/u-boot-muse-card.dtb"
# Preserve the Asterinas preset's existing artifact name without overwriting a
# Linux DT when the firmware helper is used on its own.
cp "$stage/u-boot-muse-card-original.dtb" "$stage/k1-x_MUSE-Card.dtb"

if [ "${BOOTARGS+x}" = x ]; then
    bootargs=$BOOTARGS
else
    # The minimal profile deliberately uses one unescaped TOML string. Reject
    # unsupported syntax instead of evaluating profile text as shell commands.
    bootargs=$(awk '
        { sub(/\r$/, "") }
        /^\[/ { boot = ($0 == "[boot]") }
        boot && /^append = "[^"\\]*"$/ {
            sub(/^append = "/, ""); sub(/"$/, ""); print; found++
        }
        END { if (found != 1) exit 1 }
    ' "$root/profiles/asterinas-busybox-minimal/profile.toml") ||
        fail "cannot read profile boot.append; set BOOTARGS explicitly"
fi
dtb="$stage/asterinas.dtb"
cp "$uboot_dtb" "$dtb"
# U-Boot control properties are not part of the operating system's DT.
fdtput -r "$dtb" /config
# The current K1 firmware leaves S-mode with software-managed A/D bits.
# Describe that behavior so Asterinas sets them before using new mappings.
for cpu in $(fdtget -l "$dtb" /cpus); do
    case "$cpu" in cpu@*) ;; *) continue ;; esac
    isa=$(fdtget -t s "$dtb" "/cpus/$cpu" riscv,isa)
    case "${isa}_" in *_svade_*) ;; *) isa="${isa}_svade" ;; esac
    fdtput -t s "$dtb" "/cpus/$cpu" riscv,isa "$isa"
    if extensions=$(fdtget -t s "$dtb" "/cpus/$cpu" riscv,isa-extensions 2>/dev/null); then
        case " $extensions " in *" svade "*) ;; *) extensions="$extensions svade" ;; esac
        fdtput -t s "$dtb" "/cpus/$cpu" riscv,isa-extensions $extensions
    fi
done
fdtput -t s "$dtb" /soc/uart@d4017000 compatible intel,xscale-uart
fdtput -t x "$dtb" /soc/uart@d4017000 reg-shift 2
fdtput -t x "$dtb" /soc/uart@d4017000 reg-io-width 4
# U-Boot polls this UART and omits IRQ properties. Match the official Linux DTS:
# k1-x.dtsi serial@d4017000 uses PLIC interrupt 42.
plic=$(fdtget -t x "$dtb" /soc/interrupt-controller@e0000000 phandle)
fdtput -t x "$dtb" /soc/uart@d4017000 interrupt-parent "$plic"
fdtput -t i "$dtb" /soc/uart@d4017000 interrupts 42
fdtput -p -t x "$dtb" /reserved-memory '#address-cells' 2
fdtput -t x "$dtb" /reserved-memory '#size-cells' 2
fdtput "$dtb" /reserved-memory ranges
fdtput -p -t x "$dtb" /reserved-memory/sbi@0 reg 0 0 0 200000
fdtput "$dtb" /reserved-memory/sbi@0 no-map
fdtput -p -t s "$dtb" /chosen bootargs "$bootargs"
# bootm supplies the final linux,initrd-start/end values after loading the FIT.
for property in linux,initrd-start linux,initrd-end; do
    if fdtget "$dtb" /chosen "$property" >/dev/null 2>&1; then
        fdtput -d "$dtb" /chosen "$property"
    fi
done
# Leave padding for U-Boot's final memory, initrd and chosen-node fixups.
within "$dtb" "$((0x4000000 - 0x3000000 - 0x10000))"
(cd "$stage" && mkimage -f asterinas.its asterinas.itb)
# Load the FIT at 64 MiB; all destinations are below it. Stay below 128 MiB.
within "$stage/asterinas.itb" "$((0x8000000 - 0x4000000))"
within "$stage/asterinas.itb" "$((0x800000 - 0x280000))"
for file in bootinfo_spinor.bin bootinfo_sd.bin fw_dynamic.itb asterinas.itb asterinas.bin asterinas.bin.gz asterinas.dtb k1-x_MUSE-Card.dtb \
    u-boot-muse-card.itb u-boot-muse-card.dtb rustsbi-usb-muse-card.itb; do
    cp "$stage/$file" "$images/$file"
done
printf 'Created firmware, U-Boot, USB recovery and Asterinas FITs in %s\n' "$images"
