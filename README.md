# 天智软件栈（sdsat-forge）

面向软件定义卫星的软件栈构建框架，使用 Rust 实现，通过 just 统一配置、构建与运行，管理源码版本与构建缓存，并打包系统镜像。

## 快速入手

安装 Rust（含 Cargo、rustup）、just、Git、curl 和 QEMU，并加入 `PATH`。`just setup` 安装所需的 Rust 工具链；Kubernetes 等 Go 程序直接下载固定版本的预编译包。

> Windows 需先在 PowerShell 执行 `$env:FORGE_BUILD_SSH = "user@linux-host"`（也可使用 SSH 主机别名）。源码编译在 Linux 编译机执行，产物取回本机运行 QEMU；`setup`、`doctor` 同样使用编译机。Linux 可使用本机。

在仓库根目录执行以下命令，构建并启动 Asterinas QEMU 系统：

```sh
just defconfig qemu_riscv64_asterinas
just setup
just doctor
just build
just run
```

产物保存在 `output/qemu_riscv64_asterinas/images/`。可用预设通过 `just list-defconfigs` 查看。

进入终端后按需执行 `kubernetes-start` 启动 Kubernetes，再用 `kubectl get nodes` 查看节点。`just test-boot --timeout 600` 会自动启动集群并验证一个实际 BusyBox Pod。当前 Asterinas 缺少独立网络命名空间和 CNI，节点显示 `NotReady`；示例使用主机网络、PID/IPC 命名空间及特权容器运行。

Linux/openRuyi QEMU 使用 `just defconfig qemu_riscv64_openruyi_zero`，其余命令相同，Kubernetes 同样在开机后手动启动。此预设使用 bridge CNI，验收节点 `Ready`、独立 Pod IP 和 Pod 间 HTTP 通信；尚未配置 Service 代理和集群 DNS。

MUSE-Card 使用持久化 SD 系统并自动启动 k3s，见 [openRuyi Zero 指南](boards/spacemit/muse-card/OPENRUYI.md)。

`just plan` 查看构建顺序，`just help` 查看完整帮助。使用 `just --jobs 8 build` 指定 Rust/Make 编译并行度。

## 组件与目录

组件配方支持 `cargo`（Rust）、`go`（Go）、`kbuild`（Make/Kbuild）和 `prebuilt`（预编译文件）四种后端。

`kbuild` 配方可用 `patches` 列出相对仓库根目录的补丁路径，也可在 `board.toml` 中覆盖该列表。补丁在源码解包后按顺序校验并应用；内容参与构建缓存，修改后会重新构建。本地和 SSH 编译机均支持，使用补丁时编译机需安装 Git。

需要将根文件系统安装到 SD 等外部存储时，可在 profile 的 `[rootfs]` 中设置 `kind = "external"` 和 `package`。Forge 保留该组件的产物供板卡部署流程使用，不生成 `initramfs.cpio`；此模式不支持 `rootfs_files`、启动脚本覆写或 `run` / `test-boot`，应通过板卡启动流程验证。

- `configs/`、`boards/`、`profiles/`：系统预设、板卡与软件栈配置。
- `packages/`：组件配方与补丁。
- `locks/`：源码版本与校验信息。
- `src/`、`support/`：构建框架与辅助文件。
- `justfile`：统一命令入口。

本地配置 `forge.toml`、下载缓存 `dl/` 和构建产物 `output/`、`target/` 均不纳入版本控制。

## 许可协议

原创代码和配置采用 **MIT OR MulanPSL-2.0**，可任选 [MIT 许可证](LICENSE-MIT)或[木兰宽松许可证，第 2 版](LICENSE-MULAN)。

下载的第三方源码、依赖、固件、内核和生成的镜像保留各自的上游许可证。
