# sdsat-forge

面向软件定义卫星的软件栈构建框架，使用 Rust 实现，通过 just 提供统一的配置、构建与运行入口。

## 基本功能

- 通过配置组合板卡、软件组件和工具链。
- 管理固定版本的源码下载与校验，支持构建缓存和并行编译。
- 打包系统镜像，启动 QEMU 并执行启动验收。

## 快速上手

先安装 Rust（含 Cargo、rustup）、just、Git、curl 和 QEMU，并加入 `PATH`。

在仓库根目录执行，将 `<name>` 替换为列出的预设名称：

```sh
just list-defconfigs
just defconfig <name>
just setup
just doctor
just
just run
```

运行 `just test-boot` 可自动验证系统启动，`just help` 查看完整帮助。

## 命令列表

### 基础使用

| 命令 | 用途 |
| --- | --- |
| `just help` | 查看 Forge 帮助 |
| `just list-defconfigs` | 列出可用预设 |
| `just defconfig <name>` | 选择预设并重置本地配置 |
| `just setup` | 安装所选配置的 Rust 工具链与辅助工具 |
| `just` / `just build` | 构建完整系统镜像 |
| `just run` | 启动已有系统镜像 |

### 构建管理

| 命令 | 用途 |
| --- | --- |
| `just doctor` | 检查构建环境 |
| `just source` | 下载并校验源码归档 |
| `just build <package>` | 构建指定组件及其依赖 |
| `just rebuild <package>` | 重新执行指定组件的构建 |
| `just clean` | 清理当前配置的产物，保留配置与下载缓存 |

### 高级与开发

| 命令 | 用途 |
| --- | --- |
| `just plan` | 查看组件依赖顺序 |
| `just test-boot` | 构建并验证系统启动 |
| `just savedefconfig <name>` | 将当前配置保存为新预设 |
| `just check` | 检查代码格式与 Clippy |

使用 `just --jobs 8 build` 指定编译并行度。任务名后的参数转发给 Forge，例如 `just run --help`。

## 目录架构

- `src/`：Rust 构建框架。
- `configs/`：系统预设。
- `boards/`：板卡配置。
- `profiles/`：软件栈与工具链配置。
- `packages/`：组件配方与补丁。
- `locks/`：源码版本与校验信息。
- `support/`：构建与启动辅助文件。
- `justfile`：命令入口。

本地预设选择 `forge.toml`、下载缓存 `dl/` 和构建产物 `output/`、`target/` 均不纳入版本控制。

## 许可协议

sdsat-forge 的原创代码和配置采用 **MIT OR MulanPSL-2.0**，你可以任选以下一种许可证：

- [MIT 许可证](LICENSE-MIT)
- [木兰宽松许可证，第 2 版](LICENSE-MULAN)

下载的第三方源码、依赖、固件、内核和生成的镜像保留各自的上游许可证。使用 Forge 构建组件不会改变该组件的许可证。
