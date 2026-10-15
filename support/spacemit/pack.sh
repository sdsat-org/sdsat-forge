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
for file in rustsbi.bin asterinas.elf initramfs.cpio u-boot.itb; do
    [ -s "$images/$file" ] || fail "missing or empty input: $images/$file"
done

# Keep timestamps reproducible, including when building from an exported source tree.
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-0}
stage=$(mktemp -d "$images/.muse-card.XXXXXXXX")
trap 'rm -rf -- "$stage"' 0
trap 'exit 1' HUP INT TERM
cp "$images/rustsbi.bin" "$images/initramfs.cpio" "$stage/"
cp "$board/fw_dynamic.its" "$board/asterinas.its" "$board/rustsbi-usb-muse-card.its" "$stage/"

within() {
    bytes=$(wc -c < "$1")
    [ "$bytes" -gt 0 ] && [ "$bytes" -le "$2" ] ||
        fail "$(basename -- "$1") is $bytes bytes; allowed range is 1..$2"
}

(cd "$stage" && mkimage -f fw_dynamic.its fw_dynamic.itb)
# Official SPI NOR layout: SBI at 0x70000, U-Boot at 0xa0000.
within "$stage/fw_dynamic.itb" "$((0x30000))"

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

# The official U-Boot FIT contains multiple board DTBs; select by description,
# not by an image index that may change in a later release.
original="$stage/k1-x_MUSE-Card.dtb"
fit="$images/u-boot.itb"
fdt_node=
config_node=
for config in $(fdtget -l "$fit" /configurations); do
    if [ "$(fdtget -t s "$fit" "/configurations/$config" description)" = "k1-x_MUSE-Card" ]; then
        config_node=$config
        fdt_node=$(fdtget -t s "$fit" "/configurations/$config" fdt)
        uboot_node=$(fdtget -t s "$fit" "/configurations/$config" loadables)
        break
    fi
done
[ -n "$fdt_node" ] || fail "official U-Boot FIT has no MUSE-Card configuration"
index=0
for node in $(fdtget -l "$fit" /images); do
    if [ "$node" = "$fdt_node" ]; then
        dumpimage -T flat_dt -p "$index" -o "$original" "$fit"
    fi
    if [ "$node" = "$uboot_node" ]; then
        dumpimage -T flat_dt -p "$index" -o "$stage/u-boot-nodtb.bin" "$fit"
    fi
    index=$((index + 1))
done
[ -s "$original" ] && [ -s "$stage/u-boot-nodtb.bin" ] || fail "MUSE-Card FIT payloads were not found"
case "$(fdtget -t s "$original" / model)" in
    *MUSE-Card*) ;;
    *) fail "input DTB is not for MUSE-Card" ;;
esac
case " $(fdtget -t s "$original" / compatible) " in
    *" spacemit,k1x "*) ;;
    *) fail "input DTB lacks spacemit,k1x compatibility" ;;
esac
[ "$(fdtget -t x "$original" / '#address-cells')" = 2 ] &&
    [ "$(fdtget -t x "$original" / '#size-cells')" = 2 ] || fail "expected 64-bit DTB address and size cells"

# RustSBI uses these properties to enable S-mode cache-block operations. The
# official K1 U-Boot executes cbo.flush with a 64-byte stride, but its supplied
# DTB omits zicbom. Fix the DTB passed by SPL, before U-Boot itself can run.
uboot_dtb="$stage/u-boot-muse-card.dtb"
cp "$original" "$uboot_dtb"
for cpu in $(fdtget -l "$uboot_dtb" /cpus); do
    case "$cpu" in cpu@*) ;; *) continue ;; esac
    isa=$(fdtget -t s "$uboot_dtb" "/cpus/$cpu" riscv,isa)
    case "${isa}_" in *_zicbom_*) ;; *) isa="${isa}_zicbom" ;; esac
    fdtput -t s "$uboot_dtb" "/cpus/$cpu" riscv,isa "$isa"
    fdtput -t x "$uboot_dtb" "/cpus/$cpu" riscv,cbom-block-size 40
    if extensions=$(fdtget -t s "$uboot_dtb" "/cpus/$cpu" riscv,isa-extensions 2>/dev/null); then
        case " $extensions " in *" zicbom "*) ;; *) extensions="$extensions zicbom" ;; esac
        # Split the device-tree string list into fdtput arguments; globbing is disabled.
        fdtput -t s "$uboot_dtb" "/cpus/$cpu" riscv,isa-extensions $extensions
    fi
done
bootcmd=$(awk '
    { sub(/\r$/, "") }
    /^[[:space:]]*#/ { next }
    { sub(/^[[:space:]]+/, ""); if (length) printf "%s ", $0 }
' "$board/boot.cmd")
[ -n "$bootcmd" ] || fail "empty MUSE-Card boot command"
fdtput -p -t s "$uboot_dtb" /config bootcmd "$bootcmd"
fdtput -t i "$uboot_dtb" /config bootdelay 3
adapted="$stage/u-boot-muse-card.itb"
cp "$fit" "$adapted"
# od emits one hexadecimal byte per argument, as required by fdtput's bx type.
fdtput -t bx "$adapted" "/images/$fdt_node" data $(od -An -v -t x1 "$uboot_dtb")
# The official multi-board FIT leaves only 206 bytes before the OS partition.
# Keep this board's configuration and its two referenced payloads so its boot
# command fits, without changing the official U-Boot executable.
for property in $(fdtget -p "$fit" "/configurations/$config_node"); do
    case "$property" in description|loadables|fdt) ;;
        *) fail "unsupported MUSE-Card FIT configuration property: $property" ;;
    esac
done
for config in $(fdtget -l "$fit" /configurations); do
    [ "$config" = "$config_node" ] || fdtput -r "$adapted" "/configurations/$config"
done
for node in $(fdtget -l "$fit" /images); do
    if [ "$node" != "$fdt_node" ] && [ "$node" != "$uboot_node" ]; then
        fdtput -r "$adapted" "/images/$node"
    fi
done
fdtput -t s "$adapted" /configurations default "$config_node"
mkimage -F "$adapted"
# Keep the original U-Boot start at 0xa0000 and reserve 0x280000 onwards for
# the compressed OS FIT in this board's 8 MiB NOR.
within "$adapted" "$((0x280000 - 0xa0000))"
# Re-extract both retained payloads after rehashing. The executable must match
# the original archive exactly, and the DTB must contain our board settings.
index=0
for node in $(fdtget -l "$adapted" /images); do
    dumpimage -T flat_dt -p "$index" -o "$stage/adapted-payload" "$adapted" >/dev/null
    if [ "$node" = "$fdt_node" ]; then
        cmp "$uboot_dtb" "$stage/adapted-payload"
    else
        [ "$node" = "$uboot_node" ] || fail "unexpected adapted FIT payload: $node"
        cmp "$stage/u-boot-nodtb.bin" "$stage/adapted-payload"
    fi
    index=$((index + 1))
done
(cd "$stage" && mkimage -f rustsbi-usb-muse-card.its rustsbi-usb-muse-card.itb)

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
for file in fw_dynamic.itb asterinas.itb asterinas.bin asterinas.bin.gz asterinas.dtb k1-x_MUSE-Card.dtb \
    u-boot-muse-card.itb u-boot-muse-card.dtb rustsbi-usb-muse-card.itb; do
    cp "$stage/$file" "$images/$file"
done
printf 'Created firmware, U-Boot, USB recovery and Asterinas FITs in %s\n' "$images"
