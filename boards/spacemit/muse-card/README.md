# MUSE-Card

openRuyi Zero 的 SD 卡系统构建与安装见 [openRuyi Zero 指南](OPENRUYI.md)。以下为 Asterinas 方案。

此预设使用官方 SDK 稳定版 `k1-bl-v2.2.11-release`，回移 DDR 训练和 QSPI 修复后编译 `FSBL.bin` 和精简 U-Boot，搭配 RustSBI、Asterinas 和 BusyBox，不含 Kubernetes。U-Boot 保留 SD、NOR、串口、USB Fastboot 和看门狗。

```sh
just defconfig spacemit_muse_card_asterinas
just setup
just build
sh support/spacemit/pack.sh output/spacemit_muse_card_asterinas/images
```

打包脚本在 Linux 上运行，需要 `u-boot-tools`、`device-tree-compiler` 和 `binutils-riscv64-linux-gnu`。Windows 构建后，将仓库脚本、模板、profile 和 images 目录同步到编译机再打包。可用 `OBJCOPY`、`READELF` 指定工具，`BOOTARGS` 覆盖 minimal profile 的启动参数。

脚本生成 RustSBI 固件 `fw_dynamic.itb`、`u-boot-muse-card.itb`、USB 恢复用 `rustsbi-usb-muse-card.itb` 和压缩内核的 `asterinas.itb`，不会写入设备。U-Boot FIT 使用源码构建的精简程序，并通过设备树补全 CPU 缓存属性和自动启动命令。

首次安装或更新 NOR 中的 FSBL、RustSBI、U-Boot、Asterinas，请按[烧录指南](FLASH.md)操作，包含 USB 恢复、备份、分区写入和回读命令。

已安装本项目启动固件后，日常更新 Asterinas 只需将 `asterinas.itb` 复制到 TF 卡。在项目根目录执行（将 `E:` 替换为 TF 卡的实际盘符）：

```powershell
Copy-Item output/spacemit_muse_card_asterinas/images/asterinas.itb E:\asterinas.itb
```

安全弹出 TF 卡，插回板子后按 RESET；若板子仍在运行，也可执行 `reboot`。无需重烧 NOR。

8 MiB NOR 布局：`bootinfo_spinor.bin` 位于 `0`，`FSBL.bin` 位于 `0x20000`，`fw_dynamic.itb` 位于 `0x70000`（192 KiB），`u-boot-muse-card.itb` 位于 `0xa0000`，`asterinas.itb` 位于 `0x280000`（最多 5.5 MiB）。保留板上 `0x10000` 的 private 区域；`0x60000` 为 env。

镜像写入后自动启动：倒计时 3 秒，按 `s` 可进入 U-Boot。优先扫描 TF 卡（`mmc 0`）前 16 个分区的 `/asterinas.itb` 和 `/boot/asterinas.itb`，找不到可用镜像则回退 NOR。支持 FAT/ext4，无需 `saveenv` 或调整分区；更新 TF 卡上的文件即可更新优先启动的系统。

需要手动从 NOR 启动时：

```text
sf probe
sf read 0x4000000 0x280000 0x580000
setenv bootargs
setenv fdt_high 0xffffffffffffffff
setenv initrd_high 0xffffffffffffffff
bootm 0x4000000#muse-card
```

内核、initramfs、设备树分别加载到 4、32、48 MiB；64 MiB 暂存 FIT。设备树保留低 2 MiB 给 SBI/RCPU，描述 `svade` 并启用 XScale UART。

实板已运行 8 核、8 GiB 内存的 BusyBox，系统 FIT 约 3.34 MiB。默认 `loglevel=warn`，逐核确认上线避免静默启动时的停顿；在 shell 中执行 `reboot` 可通过 K1 看门狗复位整板。
