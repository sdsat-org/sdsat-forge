# 天智软件栈（sdsat-forge）

面向软件定义卫星的软件栈构建框架，使用 Rust 实现，通过 just 统一配置、构建与运行，管理源码版本与构建缓存，并打包系统镜像。

## 快速入手

安装 Rust（含 Cargo、rustup）、just、Git、curl 和 QEMU，并加入 `PATH`。

> Windows 上构建 BusyBox 需使用 Linux SSH 构建机，先将 `FORGE_BUILD_SSH` 设置为 SSH 主机别名；Linux 可使用本机。

在仓库根目录执行以下命令，构建并启动 Asterinas QEMU 系统：

```sh
just defconfig qemu_riscv64_asterinas
just setup
just doctor
just build
just run
```

产物保存在 `output/qemu_riscv64_asterinas/images/`。其他预设通过 `just list-defconfigs` 查看；硬件预设需将镜像写入启动介质后上板运行。

`just test-boot` 自动验证 QEMU 启动，`just plan` 查看构建顺序，`just help` 查看完整帮助。使用 `just --jobs 8 build` 指定编译并行度。

## 组件与目录

组件配方支持 `cargo`（Rust 项目）、`kbuild`（标准 Make/Kbuild）和 `prebuilt`（预编译文件）三种后端。

- `configs/`、`boards/`、`profiles/`：系统预设、板卡与软件栈配置。
- `packages/`：组件配方与补丁。
- `locks/`：源码版本与校验信息。
- `src/`、`support/`：构建框架与辅助文件。
- `justfile`：统一命令入口。

本地配置 `forge.toml`、下载缓存 `dl/` 和构建产物 `output/`、`target/` 均不纳入版本控制。

## 许可协议

原创代码和配置采用 **MIT OR MulanPSL-2.0**，可任选 [MIT 许可证](LICENSE-MIT)或[木兰宽松许可证，第 2 版](LICENSE-MULAN)。

下载的第三方源码、依赖、固件、内核和生成的镜像保留各自的上游许可证。
