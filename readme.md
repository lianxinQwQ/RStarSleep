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
生成管理器目录和默认全局配置，并保留已有文件。执行时需要 `btrfs` 工具及相应的
子卷创建和文件写入权限。成功后输出工作目录路径；失败时输出原因并返回非零退出码。

`src/main.rs` 只负责进程入口、错误输出和退出码。命令分派位于 `src/cli/mod.rs`，
各子命令分别实现在 `src/cli/init.rs`、`src/cli/init_system.rs` 和
`src/cli/packages.rs`。新增子命令时添加独立模块，并在分派模块中注册。

## 计划-未完成

- 基础系统构建和维护
- 自定义软件包
- 持久化的目录和文件
- 构建前目录和构建后脚本
- 将构建的系统放入systemd-boot
- 运行中的系统动态维护支持
- aur适配
- 可构建子系统，基于systemd-nspawn管理子系统

# 计划-已完成
- 配置模块：工作目录定义、Btrfs 专用子卷初始化、软件包配置读取接口
