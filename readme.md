# RStarSleep是一个基于btrfs和写时复制的版本化操作系统管理器



RStarSleep的配置文件借鉴自mkosi，实现方式不同，同时仅保留生成目录所需的选项

工作目录、初始化方式和软件包配置读取接口见 [src/config/layout.md](src/config/layout.md)。

## 初始化工作目录

将 Btrfs 顶层（`subvolid=5`）挂载到一个现有绝对目录后，使用 `init` 子命令：

```sh
cargo run -- init --help
cargo run -- init /mnt/btrfs
```

该命令调用配置模块的初始化代码，在 `/mnt/btrfs/@rstarsleep` 创建或复用专用子卷，
生成管理器目录和默认全局配置，并将内嵌 Arch 插件写入 `plugins/arch.tar.gz`，
保留已有配置和插件文件。执行时需要 `btrfs` 工具及相应的
子卷创建和文件写入权限。成功后输出工作目录路径；失败时输出原因并返回非零退出码。

`src/main.rs` 只负责进程入口、错误输出和退出码。命令分派位于 `src/cli/mod.rs`，
各子命令分别实现在 `src/cli/init.rs`、`src/cli/init_system.rs` 和
`src/cli/packages.rs`、`src/cli/build.rs`。新增子命令时添加独立模块，并在分派模块中注册。

## 发行版插件

用户将插件压缩包放入工作目录的 `plugins/`。系统构建配置通过
`system_type = "arch"` 选择类型；控制器按插件元数据自动匹配，不需要注册路径。

项目根的 [presets/plugins/arch](presets/plugins/arch) 提供 Arch Linux 预设。
Cargo 构建时将该目录打包并嵌入二进制，`init` 时写出压缩包。Arch 插件 API 为版本 3，
每个操作使用根内同名 `.sh` 文件，调用形式为 `bash <插件根>/<操作名>.sh [参数...]`，
元数据不包含 `entry`。复杂任务可以随包携带二进制。加载时在临时目录解包，自动处理单层包装
目录，并保留全部资源。具体格式与流程见 [docs/build-design.md](docs/build-design.md)。

插件元数据通过 `[tools]` 声明宿主和系统内所需工具。程序在调用前检查实际环境，
缺失时返回错误，不执行操作文件；包内二进制可以使用相对路径声明。

API 1 插件需拆分操作文件并重新打包。API 3 新增 `template_clean.sh` 和
`system_init.sh`，分别清理模板身份和初始化系统副本。API 2 仍可维护已有缓存，
无缓存创建需要 API 3。初始化会保留已有插件包，升级时需主动替换旧包。

已实现预设嵌入、初始化分发、按系统类型查找/临时加载、调用前工具检查
和 Bash/nspawn 调用，以及版本构建、阶段缓存和软件维护。
预设 Arch 支持 `x86_64` 和官方仓库包。

## 构建系统

先初始化系统并编辑 `systems/<系统>/config/build/config.toml` 中的 `packages`：

```sh
RStarSleep init-system /rstarsleep desktop
RStarSleep build desktop
RStarSleep build --all
RStarSleep build desktop --root /mnt/btrfs/@rstarsleep
```

省略系统标识默认构建全部系统，逐个执行，个别失败不阻断其他系统。
临时工作目录默认是管理根下的 `work/`，可通过全局 `work_dir` 覆盖。
需要 Btrfs 工具、nspawn、插件声明的工具，以及安装系统所需权限。
Arch bootstrap 使用可信 Arch 签名密钥环；默认位置为
`/usr/share/pacman/keyrings/archlinux.gpg`，可用 `RSTAR_ARCH_KEYRING` 指定。

构建只从最新过往尝试读取已完成的只读阶段缓存。无基础缓存时，使用共享的
`templates/<完整插件SHA-256>/<架构>/completed/root` 创建可写 CoW 副本；模板不存在
才下载 bootstrap 并安装基础系统。完整归档中的脚本、辅助文件或元数据变化都会
产生新模板，系统名称和类型不作为模板索引。模板在独立目录中构建，不含系统配置
和用户软件清单；每个系统单独初始化 machine-id 和 pacman 本地密钥。
有缓存的维护流程保持不变，插件哈希变化不强制重建已有兼容缓存。软件维护执行一次完整更新，
补装缺失目标，将移除目标改为依赖并清理不再需要的软件。
最终只读系统根位于 `systems/<系统>/versions/<版本>/root`。
详细协议、步骤缓存动作和文件职责见 [构建设计](docs/build-design.md)。
当前已通过模拟流程测试；真实 bootstrap/nspawn 完整安装尚未验证。

## 计划-未完成

- 持久化的目录和文件
- 构建前目录和构建后脚本
- 将构建的系统放入systemd-boot
- 运行中的系统动态维护支持
- aur适配
- 可构建子系统，基于systemd-nspawn管理子系统

# 计划-已完成
- 配置模块：工作目录定义、Btrfs 专用子卷初始化、软件包配置读取接口
- 插件基础：Bash 压缩包格式、Arch 预设嵌入与分发、按系统类型自动查找和临时加载
- 构建维护：版本锁与配置留档、强制 CoW 阶段缓存、Arch 基础系统及官方仓库软件维护
