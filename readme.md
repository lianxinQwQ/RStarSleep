# RStarSleep是一个基于btrfs和写时复制的版本化操作系统管理器



RStarSleep 的系统构建配置采用 mkosi 风格的 INI 分节格式，仅保留生成目录所需的选项。
软件包可以在 `Packages=` 下缩进列出，每行一个，便于维护大量软件包。

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

用户将插件压缩包或目录放入工作目录的 `plugins/`。系统构建配置通过
在 `[Distribution]` 中用 `Distribution=arch` 选择类型；控制器按插件元数据自动匹配，不需要注册路径。

项目根的 [presets/plugins/arch](presets/plugins/arch) 提供 Arch Linux 预设。
Cargo 构建时将该目录打包并嵌入二进制，`init` 时写出压缩包。Arch 插件 API 为版本 3，
每个操作使用根内同名 `.sh` 文件，调用形式为 `bash <插件根>/<操作名>.sh [参数...]`。
插件操作也支持无后缀同名可执行文件，直接以 `<插件根>/<操作名> [参数...]` 调用；
两种入口同时存在时优先 `.sh`。参数、环境变量、工作目录及执行位置遵循相同协议。
预下载生命周期入口同样支持 `predownload/install` 和 `predownload/update`；
启用预下载时，根内 `predownload` 名称已被资源目录占用，入口仍使用 `predownload.sh`。
元数据不包含 `entry`。复杂任务可以随包携带二进制。加载时在临时目录解包或复制目录，自动处理单层包装
目录，并保留全部资源。具体格式与流程见 [docs/build-design.md](docs/build-design.md)。

插件元数据通过 `[tools]` 声明宿主和系统内所需工具。程序在调用前检查实际环境，
缺失时返回错误，不执行操作文件；包内二进制可以使用相对路径声明。

API 1 插件需拆分操作文件并重新打包。API 3 要求 `system_init.sh`，用于初始化独立安装系统的稳定身份。API 2 仍可维护已有缓存，
无缓存创建需要 API 3。初始化会保留已有插件包，升级时需主动替换旧包。

已实现预设嵌入、初始化分发、按系统类型查找/临时加载、调用前工具检查
和 Bash/nspawn 调用，以及版本构建、阶段缓存和软件维护。
预设 Arch 支持 `x86_64` 和官方仓库包。

## 构建系统

先初始化系统并编辑 `systems/<系统>/config/build/config.conf` 中的 `[Content] Packages`：

```ini
[Config]
SchemaVersion=1

[Distribution]
Distribution=arch
Architecture=x86_64

[Content]
Packages=
    linux
    systemd
    vim
```

也可以用空格或逗号分隔包名。重复 `Packages=` 会追加，空值赋值会清空之前的列表；
缩进续行和 `#` 注释采用 mkosi 的写法。文件必须显式提供 `Packages=`，空列表有效。
新系统生成 INI 模板；已有 `build/config.toml` 和旧版本留档仍可读取。
同一配置目录中同时存在两种格式时，优先读取 `config.conf`；迁移时将旧文件中的字段
改写为上述分节格式并保存为 `config.conf`。全局配置、运行配置及插件元数据继续使用 TOML。

`[Distribution]` 中除 `Distribution` 外的设置保留原名并转大写，作为 `RSTAR_<键名>`
环境变量传入插件和构建脚本，例如 `Architecture` → `RSTAR_ARCHITECTURE`。
插件必须在 `plugin.toml` 的 `[config]` 显式提供 `required`、`optional` 两个列表，
使用原始键名。匹配成功后，必选项缺失或为空、配置包含未声明的键时均报错退出，
不运行插件脚本。Arch 的可选字段见 [插件说明](presets/plugins/arch/readme.md)。
旧插件（包括 API 2）也必须补充这一声明。

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
`/usr/share/pacman/keyrings/archlinux.gpg`，可在 `[Distribution]` 设置 `Keyring`。

构建只从最新过往尝试读取已完成的只读阶段缓存。每个系统独立创建和维护自己的
基础根，无基础缓存时直接执行 bootstrap、基础安装及身份初始化。插件必须把下载的
初始安装文件保存到声明的持久目录备用；Arch 将归档及签名保存到
`persistent/arch-bootstrap/`，每次使用重新验签。新安装或恢复基础根后都执行一次基础维护，
成功后再提交基础缓存；软件有缓存时仍单独执行软件维护。
同一系统保持稳定的 machine-id，不同系统拥有独立身份。

构建开始固定删除本系统所有历史 `script` 及后续阶段缓存；基础配置与最新过往留档变化时，
删除本系统历史基础及后续阶段缓存后继续。比较排除 `Packages` 和 `script/`，因此软件包增删
和脚本变化保留基础缓存。已发布版本根和配置留档保留。插件哈希变化不会强制重建兼容缓存。
软件维护执行一次完整更新，补装缺失目标，将移除目标改为依赖并清理不再需要的软件。
软件步骤完成后执行 `systems/<系统>/config/script/`：目录或压缩包运行 `main.sh`，
独立 `*.sh` 复制进空目录后运行。按文件名顺序，在系统内部以各资源目录为工作目录执行。
script 每次从本次软件缓存创建可写 CoW 副本，执行成功后提交独立 `script` 阶段缓存，
不复用旧 script 缓存；失败时保留已完成的软件缓存并停止发布。
最终只读系统根位于 `systems/<系统>/versions/<版本>/root`。
详细协议、步骤缓存动作和文件职责见 [构建设计](docs/build-design.md)。
当前已通过模拟流程测试；真实 bootstrap/nspawn 完整安装尚未验证。

## 计划-未完成

- 持久化的目录和文件
- 构建前目录
- 将构建的系统放入systemd-boot
- 运行中的系统动态维护支持
- 可构建子系统，基于systemd-nspawn管理子系统

# 计划-已完成
- 配置模块：工作目录定义、Btrfs 专用子卷初始化、软件包配置读取接口
- 插件基础：Bash 压缩包/目录格式、Arch 预设嵌入与分发、按系统类型自动查找和临时加载
- 构建维护：版本锁与配置留档、强制 CoW 阶段缓存、Arch 基础系统及官方仓库软件维护

- 构建脚本：配置 script 目录、压缩包/目录/独立 .sh、系统内执行和独立 CoW 阶段提交

- 预下载：单一可选插件接口、批次需求合并、独立环境初始化/维护与 CoW 缓存
- 全局持久数据：直属子卷申请与目标映射、事务快照、失败回滚及中断恢复、嵌套子卷检查
- Arch AUR：非 root 构建、共享源码/包缓存及 rstarsleep 本地二进制仓库（实际安装链路待集成验证）
