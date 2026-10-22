# RStarSleep 是一个基于 Btrfs 和写时复制的版本化操作系统管理器
RStarSleep 的系统构建配置采用 mkosi 风格的 INI 分节格式，仅保留生成目录所需的选项。
软件包可以在 `Packages=` 下缩进列出，每行一个，便于维护大量软件包。

工作目录、初始化方式和软件包配置读取接口见 [src/config/layout.md](src/config/layout.md)。
账户继承、漂移策略和集中预检见 [账户继承说明](docs/account-inheritance.md)。

## 初始化工作目录

使用 `init` 自动识别当前系统 `/` 所在的 Btrfs 文件系统及其顶层（`subvolid=5`）：

```sh
cargo run -- init --help
cargo run -- init
# 也可显式指定已经挂载的顶层目录：
cargo run -- init /mnt/btrfs
```

省略路径时，即使系统运行于快照中，也会通过文件系统 UUID 定位实际 Btrfs 顶层，
优先复用已有可写顶层挂载；若没有，则在 `/run/rstarsleep/init-top-*` 自动挂载顶层。
自动创建的挂载成功后保留，工作目录可通过输出路径访问；重启后需重新挂载，命令不修改 `/etc/fstab`。
自动识别需要 `findmnt`，自动挂载还需要 `mount` 及挂载权限。

该命令调用配置模块的初始化代码，在顶层下创建或复用专用子卷 `@RStarSleep`（显式示例中为 `/mnt/btrfs/@RStarSleep`），
生成管理器目录和默认全局配置，并将内嵌 Arch 插件写入 `plugins/arch.tar.gz`，
默认保留已有配置和插件文件。已有小写 `@rstarsleep` 子卷会继续复用。
执行时需要 `btrfs` 工具及相应的
子卷创建和文件写入权限。成功后输出工作目录路径；失败时输出原因并返回非零退出码。

升级程序后，可显式更新目标工作目录中的预制插件：

```sh
RStarSleep init --update-presets
# 也可显式指定 Btrfs 顶层目录：
RStarSleep init /mnt/btrfs --update-presets
```

该选项用当前程序内嵌的插件原子替换 `plugins/arch.tar.gz`，包括用户修改过的同名包；
保留已有配置及其他路径的插件。缺失的预制插件会自动补齐。
预制插件在编译时嵌入；修改 `presets/plugins/arch/` 后需重新编译，再运行更新命令。

`init-system` 也可省略工作目录，复用同一套顶层识别逻辑，定位当前系统所在文件系统上的
`@RStarSleep`（或已有 `@rstarsleep`）工作目录。需要先执行 `init`：

```sh
RStarSleep init
RStarSleep init-system desktop
# 显式工作目录仍然可用：
RStarSleep init-system /rstarsleep desktop
```

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

插件可在 `plugin.toml` 顶层声明 `post_maintenance = true`，提供无参数的
`post_maintenance.sh` 或同名可执行文件，执行通用的“软件维护后行为”。未声明时跳过。
每次构建在软件阶段完成后、单系统用户脚本之前调用，复用软件缓存时也执行。
默认 Arch 插件用此接口设置可选的 root 密码，在系统构建配置中填写：

```ini
[Content]
RootPassword=replace-with-your-password
```

未配置或为空时不修改软件阶段输出中的密码；这是 Arch 插件的具体行为，通用接口不规定密码语义。
该值按现有 INI 规则解析，首尾空白会去除，`#` 开始注释；明文随构建配置一并留档。
配置值及参数 JSON 通过环境传递，不放入 nspawn 命令行；Arch 通过 stdin 调用 `chpasswd`，不输出密码。

API 1 插件需拆分操作文件并重新打包。API 3 要求 `system_init.sh`，用于初始化独立安装系统的稳定身份。API 2 仍可维护已有缓存，
无缓存创建需要 API 3。默认初始化保留已有插件包，升级内嵌预制插件使用 `init --update-presets`。

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
Arch 支持混合包名和软件组（例如 `linux gnome`），组会展开为全部成员并去重，
展开后的成员作为显式安装目标；包与组同名时优先选择包。
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

初始化系统还会生成 `build/environment.conf`，使用同一 INI 机制新增 `[Environment]` 组：

```ini
[Environment]
# Assign a literal value, or inherit a variable from the RStarSleep process.
# 直接赋值，或继承启动 RStarSleep 的进程环境变量。
http_proxy=http://127.0.0.1:10808
https_proxy=$https_proxy
no_proxy=$no_proxy
```

该组可写在任意构建 INI 文件中，沿用文件排序合并、`#` 注释和缩进续行规则。
变量名保留大小写，每个变量只能赋值一次；`NAME=` 设置空值，`NAME=$SOURCE` 的来源
未设置时不传入该变量，`NAME=$$value` 设置字面值 `$value`，不执行 shell 展开。
变量名允许 ASCII 字母、数字及下划线，不能以数字开头；`RSTAR_`、`RSTAT_` 为保留前缀。
该组由控制器处理，无需插件声明，作用于宿主 bootstrap、容器插件任务、预下载和构建脚本。
准备构建时解析一次并随配置文件留档，后续编辑只影响下次构建。通过 `sudo` 启动时，
继承来源是 `sudo` 保留后的进程环境。默认模板只提供注释示例，不启用代理。
旧 TOML 可在 `build/config.toml` 中使用 `[Environment]` 字符串表；空模板不影响旧格式回退。

插件可在 `[config.constraints.RSTAR_字段名]` 设置 `kind`：`free-single`、`free-list`、
`enum-single`、`enum-list`、`number`；枚举还需声明 `values=["候选值"]`。
单项和数字要求恰好一个赋值，数字必须有限；无约束字段默认为自由列表。
这些规则作用于原始赋值，而非拆分后的软件包名；缓存摘要保留列表成员及其边界。

```sh
RStarSleep init-system /rstarsleep desktop
RStarSleep build desktop
RStarSleep build --all
RStarSleep build desktop --root /mnt/btrfs/@RStarSleep
RStarSleep build desktop --inheritance-root /mnt/source-system
```

省略系统标识默认构建全部系统，逐个执行，个别失败不阻断其他系统。
`--root` 选择管理存储；`--inheritance-root <绝对路径>` 选择本次构建的继承来源系统根，
默认 `/`。两个参数互相独立，指定的来源根同时用于构建继承、首次启动来源身份、
Mount 首次初始化和账户继承。来源无效或读取失败时不回退当前系统根。
可在各参与系统的 `build/config.conf` 中设置 `[Config] DeduplicatePackages=yes`，手动开启
本批系统间的软件包文件去重，默认关闭。开启要求插件声明 `query_package_files`，否则
预检查报错；软件维护后、二阶段快照前共享内核校验一致的 Btrfs 数据块，保留独立文件
及写时复制行为。该功能增加文件查询和比较开销，第三阶段应用导出清单保持原有行为。
详情见[构建期软件包去重](docs/build-design.md#可选构建期软件包去重)。

临时工作目录默认是管理根下的 `work/`，可通过全局 `work_dir` 覆盖。
需要 Btrfs 工具、nspawn、插件声明的工具，以及安装系统所需权限。
插件任务和构建脚本的诊断即时输出到终端；连接终端时通过伪终端保留下载进度条，并传入窗口尺寸。
查询和共享预下载的 stdout 协议结果仍通过管道由管理器解析；stderr 连接终端时保留下载进度条，
诊断重定向到文件或管道时使用普通日志输出。
每个已分配版本自动将构建进度、插件与脚本的终端输出保存到
`systems/<系统>/versions/<版本>/install.log`，失败版本也保留错误及清理过程。
准备阶段的输出随版本一同归档，共享预下载的输出写入各参与版本；查询操作的 stdout 协议数据不写入日志。
日志保留原始字节，包括颜色转义、回车和进度条控制字符，可用
`less -R /rstarsleep/systems/desktop/versions/v00000001/install.log` 查看。
后续构建及阶段缓存清理保留历史日志；使用 `clean` 删除版本时一并删除其日志。
每次构建将当前可执行程序同步到管理根的 `RStarSleep`，内容一致时不改写。
Arch bootstrap 使用可信 Arch 签名密钥环；默认位置为
`/usr/share/pacman/keyrings/archlinux.gpg`，可在 `[Distribution]` 设置 `Keyring`。
同时支持二进制密钥环、GnuPG keybox 和 ASCII 装甲公钥；装甲格式由宿主 `gpg`
转换到临时文件后使用 `gpgv` 验签，不修改系统密钥环。

Arch 插件通过既有持久目录机制，将全局 `persistent/arch-data/` 的事务副本挂到 `data/`。
使用前从包内 `default-data/` 补齐缺失文件，保留已有配置，首次构建无需手动初始化。
软件源配置方法见 [Arch 插件说明](presets/plugins/arch/readme.md#软件源配置模板)。

插件可在完成有价值且一致的操作后主动保存本次调用的全部持久卷。Shell 调用
`"$RSTAR_SAVE"`；Python 将 `RSTAR_SDK_DIR` 加入模块搜索路径后调用 `rstar.save()`。
核心注入的客户端需要 Python 3，插件应声明相应工具依赖；也可自行实现
[控制协议](docs/build-design.md#插件主动保存)。保存同步完成并持久化后才返回，
不移动当前工作卷，允许重复保存。后续失败只丢弃最近一次保存之后的修改，成功退出仍自动提交最终修改。
系统根和临时编译目录不在保存范围内。调用前须刷新用户态缓冲，并暂停持久卷的并发写入直到返回。
不调用保存接口的插件保持原有事务行为。

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
软件步骤完成并提交 `packages` 缓存后，从该缓存创建可写 `script-root`，
先应用 `CopyOnWriteBuild`，再调用插件声明的 `post_maintenance`，最后执行
`systems/<系统>/config/script/`：目录或压缩包运行 `main.sh`，
独立 `*.sh` 复制进空目录后运行。按文件名顺序，在系统内部以各资源目录为工作目录执行。
构建继承、插件后处理和用户脚本共用此可写 CoW 副本，执行成功后提交独立 `script` 阶段缓存，
不复用旧 script 缓存，也不将后处理结果写回软件缓存；任一执行失败时保留已完成的软件缓存并停止发布。
script 及后续阶段的快照用于调试和备份，不作为下次构建的复用来源。
随后处理 `Mount`、启动继承准备及版本发布，最终系统根位于 `systems/<系统>/versions/<版本>/root`。
详细协议、步骤缓存动作和文件职责见 [构建设计](docs/build-design.md)。
当前已通过模拟流程和真实 Bash 测试；真实 Btrfs、bootstrap/nspawn 完整安装尚未验证。

## 从指定系统根继承数据

新系统生成空的 `systems/<系统>/config/build/inherit.conf` 模板，由控制器独立读取，
接受 `[Inheritance] CopyOnWriteBuild`、`CopyOnWriteBoot`、`Mount` 和 `DirectorySubvolume`。文件与其他 INI 一同叠加，名称没有特殊含义：

```ini
[Inheritance]
# Create an independent subvolume for each inherited directory (default yes).
DirectorySubvolume=yes
# Copy after package cache commit, before post_maintenance and scripts.
# 软件缓存提交后、插件后处理及脚本执行前继承。
CopyOnWriteBuild=/etc/hostname
# Capture independently at the first boot of each version.
# 每个版本首次启动时独立继承。
CopyOnWriteBoot=
    /home/alice/
    /var/lib/myapp/
# Initialize once in this system's persistent storage, then reuse it.
# 在本系统 persistent 中首次初始化，之后复用。
Mount=/srv/data/
Mount=/etc/myapp.conf
```

每行登记一个规范绝对路径，支持重复赋值和多行值。路径必须以 `/` 开头：目录必须以 `/`
结尾，普通文件和符号链接不能以 `/` 结尾；执行继承时校验实际类型。路径不能为 `/`，不能包含
`//`、`.` 或 `..` 路径段，也不能重复、相互包含，或去掉目录末尾 `/` 后将其余 `/`
替换成 `-` 时发生名称冲突；这些检查同时覆盖两份 CoW 列表和 Mount。
路径始终是所选来源系统内的绝对路径。例如使用 `--inheritance-root /mnt/source-system`
并配置 `/etc/hostname`，读取的是 `/mnt/source-system/etc/hostname`，目标仍为 `/etc/hostname`。
可选择已有版本的 `versions/<版本>/root`；本次参与构建系统的 `cache/` 目录可能被清理，
不能作为继承来源根。

两份 CoW 列表直接复制符号链接自身，原样保留链接文本，不解析或修正目标；断链、循环链接也可继承。
来源路径的祖先不能经过符号链接。`Mount` 直接配置符号链接会在预检时报错，应改用 CoW 列表。

`CopyOnWriteBuild` 在软件安装/维护及 `packages` 缓存提交后、`post_maintenance` 和用户脚本
执行前从所选来源根继承，结果随脚本输出保存到 `script` 缓存；`CopyOnWriteBoot` 在版本首次
启动时继承。两份列表可同时配置、分别执行，均要求来源与目标的 Btrfs UUID 相同。
文件强制 reflink；目录默认在目标位置创建独立子卷，来源已经是子卷时直接递归快照。`DirectorySubvolume=no` 可让最外层目标保持普通目录，原有内部子卷仍保留。
缓存保存、恢复和版本发布都会递归快照整个系统根中的子卷，包括脚本创建的子卷。
阶段缓存及其内部子卷只读；最终版本根及其内部子卷可写，主系统以 `rw` 挂载根。
直接写入版本根的修改随该版本保留；需要跨版本共享的数据使用 Mount。

`CopyOnWriteBoot` 非空时，构建保存包含构建时继承结果的独立 `prepared-root`，来源清单只记录
启动列表，并固定所选来源根的 Btrfs 文件系统及根子卷身份。首次启动按配置路径读取该根子卷，
不采用构建时覆盖对应路径的外部挂载内容。主系统首次选择该版本时，在正式根挂载前完成启动列表的继承并发布最终根；
子系统在启动准备阶段完成。失败可重试，完成后不再继承。当前支持 Arch mkinitcpio
的传统和 systemd initramfs。继承路径不设黑名单，CLI 在任何构建修改前提示并等待 3 秒，
可按 Ctrl+C 取消。内核和 initramfs 固定使用构建时产物，继承后的兼容性由用户负责。
启动继承检查的进度、原生命令输出及错误追加到管理子卷的
`systems/<系统>/versions/<版本>/inheritance.log`；最近一次错误保存在同目录的
`inheritance-error.txt`，包含前置校验失败。重试成功后保留日志历史并删除错误摘要。
配置、依赖、提交恢复及验证范围见 [首次启动继承说明](docs/initramfs-inheritance.md)。

`Mount` 使用本系统 `persistent/<编码路径>`，编码前去掉目录末尾 `/`，例如 `/home/alice/`
对应 `-home-alice`。复用时按持久对象校验路径类型，不要求当前来源仍存在。
项目不存在时从所选来源根完整复制，存在时直接复用，即使更换来源根或当前来源已不存在也不覆盖。
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
若版本的 `/boot` 中存在 `intel-ucode.img` 或 `amd-ucode.img`，一并复制到该版本的启动目录，
并在启动项中将其 `initrd` 行放在主 initramfs 之前；微码镜像缺失时跳过，存在时必须为普通文件。
通过 `findmnt` 自动读取该版本 Btrfs UUID 和顶层子卷路径，生成 `root=`、`rootflags=`、`rw`。
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

已实现配置与插件分发、独立系统安装、CoW 阶段缓存、构建脚本、预下载与 Arch AUR 增量构建、
全局持久子卷事务、当前系统数据继承、主系统 systemd-boot 发布、版本清理及插件子命令。
真实 Btrfs/nspawn、官方镜像与 AUR 完整安装链路仍待集成验证。
Arch AUR 默认复用未变化的构建产物并检查 Git 上游更新，配置与限制见
[Arch 插件说明](presets/plugins/arch/readme.md#aur-增量构建)和
[增量构建方案](docs/aur-cache-plan.md)。
已增加基于 systemd-nspawn 的子系统守护进程、启停/执行/终端、按需回收、图形会话、
用户级应用入口及容器内管理。子系统消费当前成功构建，在开机/重启前完成延后继承；
没有运行修改比较或版本选择。真实子系统引导与 GUI/音频完整链路仍需特权环境验收。

参考软链接中的 meowSub 制定的功能对照、运行架构与分阶段实施方案见
[子系统运行与应用集成计划](docs/subsystem-plan.md)，实际命令、依赖与验证范围见
[子系统使用说明](docs/subsystem-usage.md)。
计划采用版本化启动：子系统直接使用当前构建产物，在启动/重启时完成其待办继承再引导，
不比较运行修改，不提供版本选择。
