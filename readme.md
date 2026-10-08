# RStarSleep 是一个基于 Btrfs 和写时复制的版本化操作系统管理器
RStarSleep 的系统构建配置采用 mkosi 风格的 INI 分节格式，仅保留生成目录所需的选项。
软件包可以在 `Packages=` 下缩进列出，每行一个，便于维护大量软件包。

工作目录、初始化方式和软件包配置读取接口见 [src/config/layout.md](src/config/layout.md)。

## 初始化工作目录

将 Btrfs 顶层（`subvolid=5`）挂载到一个现有绝对目录后，使用 `init` 子命令：

```sh
cargo run -- init --help
cargo run -- init /mnt/btrfs
```

该命令调用配置模块的初始化代码，在 `/mnt/btrfs/@RStarSleep` 创建或复用专用子卷，
生成管理器目录和默认全局配置，并将内嵌 Arch 插件写入 `plugins/arch.tar.gz`，
保留已有配置和插件文件。已有小写 `@rstarsleep` 子卷会继续复用。
执行时需要 `btrfs` 工具及相应的
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

也可以用空格或逗号分隔包名。重复 `Packages=` 会追加，空值赋值不清空之前的列表；
缩进续行和 `#` 注释采用 mkosi 的写法。文件必须显式提供 `Packages=`，空列表有效。
新系统生成 INI 模板；已有 `build/config.toml` 和旧版本留档仍可读取。
迁移时将旧文件中的字段改写为上述分节格式并保存为 `config.conf`。
全局配置、运行配置及插件元数据继续使用 TOML。

单系统插件和构建脚本接收全部配置的统一大写名称：裸参数 `Channel` 为 `RSTAR_CHANNEL`，
`[Distribution] Architecture` 为 `RSTAR_DISTRIBUTION_ARCHITECTURE`。
插件在 `[config] required`、`optional` 中声明这些同名参数；必选值缺失或为空、
配置包含未声明参数时退出。还须显式声明 `[cache] base_system`、`software` 两组检测字段，
可选裸参数、整节或 `RSTAR_*节名_参数名`；空列表表示不按配置字段失效。
旧插件（包括 API 2）须改名并补齐声明。统一预下载接口保留原来的环境名及包需求参数。
Arch 配置与迁移说明见 [插件说明](presets/plugins/arch/readme.md)。

构建配置目录内所有普通 INI 文件按文件名排序独立解析后叠加，文件名和扩展名没有语义区别。
`config.toml` 仅在没有实际 INI 赋值时作为历史格式回退，不参与混合叠加。
每个设置是字符串列表，重复赋值追加，空赋值保留为成员，多行值保留为一个成员。
核心单项 SchemaVersion、Distribution、Architecture 重复时报错。
插件和脚本通过 `RSTAR_PARAMETERS_JSON` 获得原始列表边界；传统环境变量以换行连接值。

插件可在 `[config.constraints.RSTAR_字段名]` 设置 `kind`：`free-single`、`free-list`、
`enum-single`、`enum-list`、`number`；枚举还需声明 `values=["候选值"]`。
单项和数字要求恰好一个赋值，数字必须有限；无约束字段默认为自由列表。
这些规则作用于原始赋值，而非拆分后的软件包名；缓存摘要保留列表成员及其边界。

```sh
RStarSleep init-system /rstarsleep desktop
RStarSleep build desktop
RStarSleep build --all
RStarSleep build desktop --root /mnt/btrfs/@RStarSleep
```

省略系统标识默认构建全部系统，逐个执行，个别失败不阻断其他系统。
临时工作目录默认是管理根下的 `work/`，可通过全局 `work_dir` 覆盖。
需要 Btrfs 工具、nspawn、插件声明的工具，以及安装系统所需权限。
每次构建将当前可执行程序同步到管理根的 `RStarSleep`，内容一致时不改写。
Arch bootstrap 使用可信 Arch 签名密钥环；默认位置为
`/usr/share/pacman/keyrings/archlinux.gpg`，可在 `[Distribution]` 设置 `Keyring`。

构建只从最新过往尝试读取已完成的只读阶段缓存。每个系统独立创建和维护自己的
基础根，无基础缓存时直接执行 bootstrap、基础安装及身份初始化。插件必须把下载的
初始安装文件保存到声明的持久目录备用；Arch 将归档及签名保存到
`persistent/arch-bootstrap/`，每次使用重新验签。新安装或恢复基础根后都执行一次基础维护，
成功后再提交基础缓存；软件有缓存时仍单独执行软件维护。
同一系统保持稳定的 machine-id，不同系统拥有独立身份。

构建开始固定删除本系统所有历史 `script` 及后续阶段缓存。按插件声明比较当前与最新过往留档：
基础字段变化删除历史基础及后续缓存；仅软件字段变化从软件缓存开始清理。
Arch 预设检测 Config、Distribution 整节，软件包增删和脚本变化保留基础缓存。
已发布版本根和配置留档保留。插件哈希变化不会强制重建兼容缓存。
软件维护执行一次完整更新，补装缺失目标，将移除目标改为依赖并清理不再需要的软件。
软件步骤完成后执行 `systems/<系统>/config/script/`：目录或压缩包运行 `main.sh`，
独立 `*.sh` 复制进空目录后运行。按文件名顺序，在系统内部以各资源目录为工作目录执行。
script 每次从本次软件缓存创建可写 CoW 副本，执行成功后提交独立 `script` 阶段缓存，
不复用旧 script 缓存；失败时保留已完成的软件缓存并停止发布。
随后执行配置的继承，最终系统根位于 `systems/<系统>/versions/<版本>/root`。
详细协议、步骤缓存动作和文件职责见 [构建设计](docs/build-design.md)。
当前已通过模拟流程和真实 Bash 测试；真实 Btrfs、bootstrap/nspawn 完整安装尚未验证。

## 从当前运行系统继承数据

新系统生成空的 `systems/<系统>/config/build/inherit.conf` 模板，由控制器独立读取，
只接受 `[Inheritance] CopyOnWrite` 和 `Mount`。文件与其他 INI 一同叠加，名称没有特殊含义：

```ini
[Inheritance]
# Copy from the currently running system into each new version.
# 每次从当前运行系统复制到新版本。
CopyOnWrite=/etc/hostname
CopyOnWrite=
    /home/alice
    /var/lib/myapp
# Initialize once in this system's persistent storage, then reuse it.
# 在本系统 persistent 中首次初始化，之后复用。
Mount=/srv/data
Mount=/etc/myapp.conf
```

每行登记一个规范绝对路径，支持重复赋值和多行值；路径不能为 `/`、重复、相互包含，
或在 `/` 全部替换成 `-` 后发生名称冲突。

`CopyOnWrite` 在软件和 script 之后从当前运行系统复制，要求来源与构建目录的 Btrfs UUID
相同，并拒绝来源内部的挂载点和子卷。普通文件强制 reflink；目录先填充独立临时子卷，
再建立独立快照，不写入 persistent。继承缓存为 `cache/<版本>/inherit` 和 `inherit.toml`，
不复用旧缓存，下次构建与 script 缓存一同清理。缓存和最终版本的父根、继承子卷均只读，
发布时显式快照子卷，保留继承内容；需要持续写入的数据使用 Mount。

`Mount` 使用本系统 `persistent/<编码路径>`，例如 `/home/alice` 对应 `-home-alice`。
项目不存在时从当前系统完整复制，存在时直接复用，即使当前来源已不存在也不重新复制。
允许来源跨分区、包含挂载点或子卷，并输出 Warning；跨分区首次复制允许普通复制。
目录保存为可写 Btrfs 子卷，文件保存为普通文件。新根的底层目录内容保留到实际挂载，
使 `/usr` 等目标仍可提供系统工具。控制器创建目标路径，在插件调用和包查询期间绑定
persistent；继承 `/etc` 时把最终 fstab 同步回新根。Arch 在专用标记块中写入 fstab：
目录以 UUID/subvol 挂载，文件通过 `/rstarsleep` 绑定挂载，必要时自动添加管理子卷挂载。
重复调用替换标记块，保留块外配置。Arch 还接受自由列表 `[Content] FstabLines`
写入完整 fstab 行；此字段无需配合 Mount 使用。

## 主系统启动项

主系统由全局 `main_system` 指定。成功构建主系统时，从该版本 `/boot/vmlinuz-linux` 和
`/boot/initramfs-linux.img` 复制到 `/boot/RStarSleep/<版本>/vmlinuz` 和 `initramfs-linux.img`，
随后原子创建 `loader/entries/RStarSleep-<系统>-<版本>.conf`。
通过 `findmnt` 自动读取该版本 Btrfs UUID 和顶层子卷路径，生成 `root=`、`rootflags=`、`ro`。
全局可设置 `boot_dir="/boot"` 和 `kernel_options=["quiet"]`；启动目录应指向 systemd-boot
实际读取的 ESP/XBOOTLDR。额外参数不能覆盖程序生成的根选项。
缺少启动文件、UUID 或启动目标冲突均报错，不标记构建完成。
各版本的 `boot.toml` 保存启动文件所有权，清理不依赖之后的主系统角色变更。

## 插件子命令

插件可在 `plugin.toml` 顶层声明 `commands=["configure:tools/setup", "install-app"]`。
省略入口时使用同名入口，支持 `.sh` 或无后缀可执行文件；不能覆盖内置子命令，
多个插件也不能声明同一命令。宿主执行插件命令，stdin/stdout/stderr 直接连接终端，
所有参数原样透传（包括 `--root`）。管理根由 `RSTAR_MANAGER_ROOT` 环境变量选择，默认 `/rstarsleep`；
插件获得 `RSTAR_MANAGER_ROOT`、`RSTAR_PLUGIN_ROOT` 和 `RSTAR_PLUGIN_TMP`。
插件子命令必须自行申请 root（例如适当使用 sudo），管理器不自动提权。
这类入口可用于交互式安装软件并写入系统配置；应自行检查输入并传播失败。

## 清理版本

`RStarSleep clean <系统> <版本> [--root <管理根>]` 删除该系统截止版本及之前的版本记录、
系统根、对应阶段缓存、启动目录和启动项，保留当前配置、身份及 persistent。
清理与构建共用独占锁，拒绝删除运行中的系统根或跨越挂载边界，检查失败不会开始删除。
`version-counter` 保留已分配的最高编号，清空历史后也不会复用旧版本号。
删除过程遇到错误会停止，可重试；版本所有权记录随版本保留到对应外部启动文件删除完成。

## 已实现与后续工作

已实现配置与插件分发、独立系统安装、CoW 阶段缓存、构建脚本、预下载与 Arch AUR、
全局持久子卷事务、当前系统数据继承、主系统 systemd-boot 发布、版本清理及插件子命令。
真实 Btrfs/nspawn、官方镜像与 AUR 完整安装链路仍待集成验证。
运行中的系统动态维护、基于 systemd-nspawn 的子系统运行与应用集成仍待实现。
