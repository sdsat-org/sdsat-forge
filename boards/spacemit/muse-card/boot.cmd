# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# U-Boot hush commands embedded into its control DT by pack.sh.
# The official binary's part list searches the first 16 partitions, including
# partitions without a bootable flag. Partition numbers and filesize are hex.
setenv bootargs;
setenv fdt_high 0xffffffffffffffff;
setenv initrd_high 0xffffffffffffffff;
if mmc dev 0 && mmc rescan; then
    setenv tianzhi_parts;
    if part list mmc 0 tianzhi_parts; then
        for tianzhi_part in ${tianzhi_parts}; do
            if fstype mmc 0:${tianzhi_part} tianzhi_fstype; then
                for tianzhi_path in /asterinas.itb /boot/asterinas.itb; do
                    if size mmc 0:${tianzhi_part} ${tianzhi_path}; then
                        if test 0x${filesize} -gt 0 && test 0x${filesize} -le 0x4000000; then
                            setenv tianzhi_size ${filesize};
                            if load mmc 0:${tianzhi_part} 0x4000000 ${tianzhi_path} ${tianzhi_size}; then
                                if test 0x${filesize} -eq 0x${tianzhi_size}; then
                                    echo Booting TF mmc 0:${tianzhi_part} ${tianzhi_path};
                                    setenv bootargs;
                                    bootm 0x4000000#muse-card;
                                fi;
                            fi;
                        else
                            echo Skipping empty or oversized TF image;
                        fi;
                    fi;
                done;
            fi;
        done;
    fi;
fi;
echo Booting Asterinas from SPI NOR;
if sf probe && sf read 0x4000000 0x280000 0x580000; then
    setenv bootargs;
    bootm 0x4000000#muse-card;
fi;
echo No bootable Asterinas image found;
