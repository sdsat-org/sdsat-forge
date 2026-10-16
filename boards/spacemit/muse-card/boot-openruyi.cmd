# SPDX-License-Identifier: MIT OR MulanPSL-2.0
# Read the official bootfs layout on TF; provisioning owns partition changes.
setenv bootargs;
setenv fdt_high 0xffffffffffffffff;
setenv initrd_high 0xffffffffffffffff;
setenv tianzhi_bootpart;
if mmc dev 0 && mmc rescan && part number mmc 0 bootfs tianzhi_bootpart; then
    if size mmc 0:${tianzhi_bootpart} /env_k1-x.txt; then
        if test 0x${filesize} -gt 0 && test 0x${filesize} -le 0x4000; then
            setenv tianzhi_size ${filesize};
            if load mmc 0:${tianzhi_bootpart} 0x4000000 /env_k1-x.txt ${tianzhi_size}; then
                if test 0x${filesize} -eq 0x${tianzhi_size}; then
                    env import -t 0x4000000 ${filesize} bootargs;
                fi;
            fi;
        fi;
    fi;
    if test -n "${bootargs}" && size mmc 0:${tianzhi_bootpart} /Image; then
        if test 0x${filesize} -gt 0 && test 0x${filesize} -le 0x8000000; then
            setenv tianzhi_size ${filesize};
            if load mmc 0:${tianzhi_bootpart} 0x600000 /Image ${tianzhi_size}; then
                if test 0x${filesize} -eq 0x${tianzhi_size} && size mmc 0:${tianzhi_bootpart} /k1-x_MUSE-Card.dtb; then
                    if test 0x${filesize} -gt 0 && test 0x${filesize} -le 0x200000; then
                        setenv tianzhi_size ${filesize};
                        if load mmc 0:${tianzhi_bootpart} 0x18000000 /k1-x_MUSE-Card.dtb ${tianzhi_size}; then
                            if test 0x${filesize} -eq 0x${tianzhi_size}; then
                                echo Booting OpenRuyi from TF mmc 0:${tianzhi_bootpart};
                                booti 0x600000 - 0x18000000;
                            fi;
                        fi;
                    fi;
                fi;
            fi;
        fi;
    fi;
fi;
echo OpenRuyi boot failed - staying in U-Boot;
