# MUSE-Card openRuyi Zero

启动链为 NOR 中的 SpacemiT FSBL → RustSBI → 精简 U-Boot → SD 卡中的 Linux/openRuyi Zero。U-Boot 保留 SD、SPI NOR、USB Fastboot 恢复和看门狗；系统使用 openRuyi Zero 用户态及 K1 官方 Linux 6.6。

## 构建与打包

```sh
just defconfig spacemit_muse_card_openruyi_zero
just setup
just build
sh support/spacemit/pack-openruyi.sh output/spacemit_muse_card_openruyi_zero/images
```

打包在 Linux 上执行，需要 `u-boot-tools`、`device-tree-compiler`、`cpio`、`fakeroot`、`fdisk`、`e2fsprogs`、`dosfstools`、`mtools`。Windows 构建后，将仓库配置、脚本和 images 目录同步到编译机打包。脚本只生成文件，不写入设备。

产物包含启动固件、`sdcard.img`（4 GiB）及其 gzip 压缩版。Zero 用户态安装在持久化的 ext4 根分区中。

## 安装

按[烧录指南](FLASH.md)将本预设 images 目录中的 `bootinfo_spinor.bin`、`FSBL.bin`、`fw_dynamic.itb`、`u-boot-muse-card.itb` 写入 NOR 对应区域。只写这四项，跳过指南中的 `asterinas.itb`。

将 SD 卡连接 Linux 读卡器，确认设备并卸载卡上的分区。以下 `/dev/sdX` 必须替换为 **SD 卡整盘设备**；写入会覆盖卡上原有分区和数据。

```sh
lsblk -o NAME,SIZE,MODEL,MOUNTPOINTS
sudo dd if=output/spacemit_muse_card_openruyi_zero/images/sdcard.img of=/dev/sdX bs=4M conv=fsync status=progress
sync
```

插回 SD 卡后上电即可自动启动，串口为 115200、8N1，按 Enter 进入 root 命令行。倒计时按 `s` 可进入 U-Boot。

分区遵循[官方 K1 布局](https://github.com/spacemit-com/buildroot-ext/blob/855fba3e9458d69d08f71bad66e42e87979e6885/board/spacemit/k1/partition_universal.json)：`bootfs` 从 4 MiB 开始、大小 256 MiB，保存 `Image`、`k1-x_MUSE-Card.dtb` 和 `env_k1-x.txt`；`rootfs` 从 260 MiB 开始，首次启动自动扩展至卡尾。系统按 PARTUUID 挂载 `/` 和 `/boot`，主机名为 `tianzhi-node`。

内核与 QEMU 共用 `packages/linux/defconfig`，关闭显示、音频和输入设备驱动，保留相机采集及传感器接口，启用容器所需的 cgroups、namespaces、seccomp、overlayfs、veth、bridge 和 VXLAN。`packages/k3s` 提供 k3s 及离线镜像，开机后在后台自动启动；集群数据持久保存在 SD 卡的 `/var/lib/rancher/k3s`。

没有网线时使用内部网络运行单节点，支持本板 Pod 通信。进入终端后查看状态：

```sh
k3s kubectl get nodes
k3s kubectl get pods -A
k3s-check
```

`k3s-check` 验证节点 Ready、两个 Pod、Pod IP、Service 和集群 DNS；K3s 服务日志在 `/var/log/k3s.log`。

已在 8 GiB MUSE-Card、128 GB SD 卡上验证自动启动及首启扩容，根分区扩至 118.83 GiB。未接网线时上述集群检查全部通过，重启后 Pod、Service 与持久化文件保留。
