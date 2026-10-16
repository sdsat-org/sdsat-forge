# MUSE-Card 烧录指南

先完成 [README](README.md) 中的编译、打包，并将 images 产物放到连接主板的电脑上。主机安装 `fastboot`，连接板子的 USB 下载口；串口终端使用 115200、8N1、无流控（已验证的接线为 Windows COM5）。保持板子的 NOR 启动拨码不变。

以下标为“主机”的命令在电脑终端运行，“串口”的命令在 U-Boot `=>` 提示符运行。主机先进入产物目录：

```powershell
cd output/spacemit_muse_card_asterinas/images
```

## 进入 U-Boot

已安装本项目固件时，按 RESET 或在 Asterinas 中执行 `reboot`，在串口的 3 秒倒计时中按 `s`。若已有可用的官方 U-Boot，也可直接使用它的 `=>` 提示符。

无法进入 U-Boot 时，按住 FEL/下载键，再按 RESET 后松开。主机执行：

```text
fastboot devices
fastboot -s dfu-device stage FSBL.bin
fastboot -s dfu-device continue
```

等待串口显示 `Trying to boot from FASTBOOT`、USB 重新枚举，再执行：

```text
fastboot stage rustsbi-usb-muse-card.itb
fastboot continue
```

等待 U-Boot 启动并再次枚举为 Fastboot，主机再执行一次 `fastboot continue`，随后在串口倒计时中按 `s`。这里使用本项目编译的 `FSBL.bin`；以上步骤只加载到 RAM。

## 备份 NOR

串口执行，确认 `sf probe` 显示 8 MiB NOR：

```text
sf probe
fastboot -l 0x04000000 -s 0x800000 usb 0
```

主机执行，保存的备份应为 8,388,608 字节：

```text
fastboot oem read:mtd 0
fastboot get_staged muse-card-nor-backup.bin
fastboot continue
```

保留该板自己的备份，重复操作时使用新文件名。

## 写入镜像

首次安装按下表逐项写入；以后可仅更新需要的项目。不要在全部所需项目写完之前复位。所有地址和长度均为十六进制；private（`0x10000`）和 env（`0x60000`）不在写入范围内。

| 主机文件 | `flash_offset` | `flash_limit`（最大长度） |
| --- | --- | --- |
| `bootinfo_spinor.bin` | `0x0` | `0x10000` |
| `FSBL.bin` | `0x20000` | `0x40000` |
| `fw_dynamic.itb` | `0x70000` | `0x30000` |
| `u-boot-muse-card.itb` | `0xa0000` | `0x1e0000` |
| `asterinas.itb` | `0x280000` | `0x580000` |

每个文件重复以下三步，以 `FSBL.bin` 为例；处理其他文件时，替换偏移、最大长度和主机文件名。

1. 串口设置目标区域，并开启 USB 传输：

   ```text
   setenv flash_offset 0x20000
   setenv flash_limit 0x40000
   setenv filesize
   fastboot -l 0x04000000 -s ${flash_limit} usb 0
   ```

2. 主机传入对应文件；确认 `stage` 返回 `OKAY` 后再 `continue`：

   ```text
   fastboot stage FSBL.bin
   fastboot continue
   ```

3. 返回串口，检查长度、写入并回读比较（整行执行）：

   ```text
   test 0x${filesize} -gt 0 && test 0x${filesize} -le ${flash_limit} && sf probe && sf update 0x04000000 ${flash_offset} ${filesize} && sf read 0x08000000 ${flash_offset} ${filesize} && cmp.b 0x04000000 0x08000000 ${filesize}
   ```

`filesize` 由 Fastboot 下载完成后设置；看到 `Total of ... byte(s) were the same` 才表示该文件写入并验证成功。有任何失败则先处理错误，不执行下一项或复位。`sf update` 保留最后一个擦除扇区中未覆盖的尾部，不需要手工补齐镜像或整片擦除。

全部完成后，在串口执行 `reset`。板子优先启动 TF 卡上的 `/asterinas.itb` 或 `/boot/asterinas.itb`，否则启动 NOR 中的镜像；验证新烧录的 NOR 内核时先取出 TF 卡。正常启动后会出现 `tianzhi:~#`。
