# RStarSleep 工作目录

账户声明、插件协议和统一预检见 [账户继承说明](../../docs/account-inheritance.md)。

RStarSleep 的工作目录属于整个管理体系，不依附任何被管理系统的 `/etc`。
在 Btrfs 顶层（subvolid=5）创建专用子卷 `@RStarSleep`，建议运行时挂载到
`/rstarsleep`。存储路径与挂载路径指向同一份数据；显式路径初始化接口不执行挂载。
无参数 `init` 自动识别当前系统所在的 Btrfs 文件系统，即使 `/` 位于快照内，
也会定位文件系统顶层，而不是在快照内创建管理器子卷。
优先复用同一文件系统已有可写顶层挂载；否则在 `/run/rstarsleep/init-top-*` 挂载 `subvolid=5`。
自动创建的挂载成功后保留供后续访问，失败时尝试解除；重启后需重新挂载。
新初始化使用 `@RStarSleep`，已有小写 `@rstarsleep` 子卷会继续复用。

## 目录树

```text
Btrfs 顶层/
└── @RStarSleep/                         # 管理器专用子卷
    ├── RStarSleep                      # 每次构建同步的当前可执行程序
    ├── config/
    │   └── rstarsleep.toml              # 全局配置
    ├── plugins/                        # 用户存放的插件压缩包或目录
    │   └── arch.tar.gz                 # init 安装的内嵌 Arch 预设
    ├── predownload/                    # 独立下载环境缓存
    │   └── <完整插件SHA-256>/<架构>/<配置SHA-256>/
    │       └── completed/              # root 子卷及 environment.toml 清单
    ├── persistent/                     # 全局共享，不按插件划分
    │   └── <直属名称>/                 # 首次申请创建的独立子卷
    ├── persistent-state/               # 全局锁、事务副本及恢复日志
    │   ├── transaction/                # 本次调用的工作卷及最终提交日志
    │   └── save/                       # 显式保存期间的快照及提交日志
    ├── work/                           # 默认构建临时目录，可由 work_dir 覆盖
    └── systems/
        └── <系统标识>/                  # 系统身份不随角色变更
            ├── config/                 # 当前可编辑配置
            │   ├── build/
            │   │   ├── config.conf     # mkosi 风格 INI，读取 Packages 和发行版
            │   │   ├── environment.conf # [Environment] 执行环境，默认只有注释示例
            │   │   ├── inherit.conf    # 空继承模板，与其他 INI 一同叠加
            │   │   └── guest-packages.conf # 可选，guest install 的受控包请求
            │   ├── script/            # 软件步骤后的构建脚本输入
            │   └── subsystem/
            │       └── config.toml     # 当前子系统授权、启动、会话及导出配置
            ├── build.lock              # 每系统构建独占锁
            ├── build-current.toml      # 当前成功交付产物，失败构建不替换
            ├── identity.toml           # 独立安装时保存的稳定 machine-id
            ├── version-counter         # 最高已分配版本号，清理历史后仍保留
            ├── versions/
            │   └── <版本标识>/
            │       ├── state.toml      # building / pending-inheritance / failed / complete / cancelled
            │       ├── boot.toml       # 主系统启动文件的所有权记录
            │       ├── config/         # 构建时配置留档，结构同当前 config/
            │       ├── install.log     # 构建过程及错误日志
            │       ├── inheritance.log # 可选，启动继承检查的追加日志及重试历史
            │       ├── inheritance-error.txt # 可选，最近一次继承失败摘要，成功后删除
            │       ├── application-files.tsv # 可选，包所属文件候选索引
            │       ├── prepared-root/  # 可选，首次启动继承前的独立只读根
            │       ├── inheritance-pending.toml # 可选，原配置路径、来源根及目标身份
            │       ├── inheritance-commit.toml  # 可选，最终根及全部子卷提交记录
            │       └── root/           # 可写系统父根及全部内部子卷
            ├── cache/
            │   └── <版本标识>/
            │       ├── base-system/    # 只读基础系统子卷快照
            │       ├── base-system.toml # 基础阶段完成、兼容性与来源
            │       ├── packages/       # 只读软件阶段子卷快照
            │       ├── packages.toml   # 软件阶段完成、目标与实际清单
            │       ├── script/         # 只读脚本阶段子卷快照，不作后续构建来源
            │       ├── script.toml     # 脚本阶段完成与实际清单
            │       ├── inherit/        # 可选继承阶段，父根及子卷均只读，不复用
            │       └── inherit.toml    # 继承阶段完成与实际清单
            ├── runtime/                # 子系统运行存储和恢复记录，按需创建
            │   ├── activity.lock       # 应用/启停共享，捕获/迁移排他
            │   ├── current.toml        # 已同步运行代、版本、机器名及启动配置摘要
            │   ├── transaction.toml    # 未完成存储准备时存在
            │   ├── fstab               # 单独注入容器的只读适配配置
            │   ├── exports-users.toml  # 已主动登记用户入口集成的用户
            │   ├── generations/<代>/root/ # 可写父根及全部内部子卷
            │   └── guest/
            │       ├── mount-<摘要>.toml # 待迁移/已发布持久卷收据
            │       ├── build.lock      # guest 异步构建任务锁
            │       ├── build-current.toml # 当前 guest 构建任务状态
            │       └── build-*.log     # guest 构建输出
            └── persistent/             # Mount 跨版本共享数据，如 -home-alice
                └── guest/<摘要>/data   # guest migrate 发布的持久对象
```

初始化管理器时创建 `@RStarSleep` 子卷、`config/`、`systems/`、`plugins/`、`predownload/`、`persistent/` 和全局配置，
并将内嵌的 Arch 预设压缩包写入 `plugins/arch.tar.gz`。默认不覆盖已有同名普通文件；
缺失的预设包会在再次初始化时补充。显式运行 `init --update-presets` 时，用当前程序
内嵌的预设包原子替换同名文件，包括用户修改过的包；配置和其他路径的插件均保留。
预设目标若为符号链接、目录或其他非普通文件则报错。
初始化一个被管理系统时创建其当前配置（包括空 `script/` 和 INI 系统的空 `inherit.conf`）、
版本、缓存和持久化目录及默认配置。
这些系统目录目前是普通目录；具体版本根、阶段缓存是否使用子卷由对应功能实现。
初始化不会生成系统版本、配置留档或修改主/子系统角色，也不会覆盖已有配置。
子卷创建失败时返回错误，不使用普通目录代替；既有 `@RStarSleep` 必须为真实子卷。

## 系统归属与角色

任意被管理系统都可以由全局配置指定为主系统。全局配置单独指定子系统名单，
其余系统仅保留为被管理对象。子系统运行配置属于对应系统，只有它被显式指定为
子系统时才应用，包括向当前主系统导出应用入口。

角色变更不移动目录。同一系统不能同时承担主/子系统角色；移出子系统名单后拒绝新请求，
daemon 后台协调停止机器并撤销用户未修改的自有入口，保留版本与持久数据。
持久化数据默认跨同一系统的版本共享，删除版本不应删除它；其快照与回滚策略
由具体持久化功能管理。Mount 数据首次复制后持续复用，不随重建覆盖；两种 CoW 继承
数据属于各版本独立子卷，不放入 persistent。缓存独立于已完成版本，可以清理。
每个系统独立安装和维护基础根，仅复用自己的最新过往阶段缓存。
下载的初始系统安装文件由插件保存到声明的全局持久目录，供后续独立安装使用；
管理器不创建跨系统共享的基础根。基础安装或恢复后均执行一次基础维护。

当前配置用于下一次构建。构建模块在开始构建时保存配置留档，并使用留档
完成构建，避免期间编辑当前配置影响版本归属。既有版本留档不应覆盖。
全局角色配置不归属于某个系统版本。

## 默认配置

全局配置 `config/rstarsleep.toml`：

```toml
schema_version = 1
# Optional main system ID; omit this setting until a main system is selected.
# 可选的主系统标识；选定主系统前省略此项。
# main_system = "desktop"
# Only explicitly listed systems receive the subsystem role.
# 只有显式列出的系统才具有子系统角色。
subsystems = []
# Type #1 systemd-boot destination and optional extra kernel arguments.
# systemd-boot Type #1 启动目标及可选额外内核参数。
# boot_dir = "/boot"
# kernel_options = ["quiet"]
```

未指定主系统时，省略 `main_system`。构建和清理会读取全局配置；指定的主系统构建成功后
发布启动文件，`boot_dir` 默认 `/boot`。子系统名单不能重复，运行配置与命令见
[子系统使用说明](../../docs/subsystem-usage.md)。

系统构建配置 `systems/<系统标识>/config/build/config.conf`：

```ini
[Config]
SchemaVersion=1

[Distribution]
Distribution=arch
# Optional target architecture; omission uses the host architecture.
# 可选目标架构；省略时使用宿主架构。
# Architecture=x86_64

[Content]
Packages=
```

软件包配置示例：

```ini
[Config]
SchemaVersion=1

[Distribution]
Distribution=arch

[Content]
Packages=
    linux
    systemd
    vim
```

构建配置采用 mkosi 风格的 INI：分节名与键名区分大小写，首节之前允许裸参数。
缩进续行延续上一设置的值，空行和 `#` 注释不打断续行；支持行内 `#` 注释。
软件包值按逗号或空白分隔，重复 `[Content] Packages=` 按顺序追加，空值不清空先前列表。
无需 TOML 的引号、方括号或逗号列表语法；要求显式声明 `Packages=`，允许空列表。
Arch 允许混合具体包名和软件组，例如 `linux gnome`。软件维护阶段由插件展开组，
控制器合并重叠成员并作为显式包管理；配置读取阶段保留原始目标名称。
全部节和裸参数都原样留档，并作为插件参数读取。匹配插件后根据 `[config] required`、
`optional` 校验全部生效值：必选参数缺失或为空、显式参数未声明时退出。
声明和环境变量使用同一规范大写名称：裸参数 `Channel` 为 `RSTAR_CHANNEL`，
`[Distribution] Architecture` 为 `RSTAR_DISTRIBUTION_ARCHITECTURE`。
全部 INI 赋值均为字符串列表，重复项追加，多行赋值作为一个含换行的成员；
插件及脚本通过 `RSTAR_PARAMETERS_JSON` 读取准确的列表边界，传统 `RSTAR_*` 值以换行连接。
Packages 的传统变量保留按逗号/空白拆分后的包列表。
变量值不作为 shell 源码执行。名称组件以 ASCII 字母开头，后续只能含字母、数字、下划线；
不能发生大写或节/键拼接冲突，不能占用控制器变量。
Architecture 若声明为必选则必须显式提供；若声明为可选并省略，则传入用于插件匹配的默认架构。

`config/build/` 内除历史 `config.toml` 外的普通文件均作为 INI 按文件名排序独立解析后叠加，
文件名和扩展名没有配置职责区别。核心单项 SchemaVersion、Distribution、Architecture
重复时报错，空赋值不清空先前的成员。未知继承键由控制器报错；其余参数由插件声明校验。
插件可在 `[config.constraints.RSTAR_字段名]` 声明 `kind`：`free-single`、`free-list`、
`enum-single`、`enum-list`、`number`；枚举还需 `values`。单项和数字要求恰好一个赋值，
数字必须有限，未约束字段为自由列表。缓存摘要包含原始列表成员及边界。

已有系统或历史版本没有实际 INI 赋值时，继续读取旧 `build/config.toml`，空继承及环境模板不阻止回退。
有实际赋值时 INI 与 TOML 不混合，INI 无效不会回退；初始化不以空模板覆盖旧 TOML 的设置。
迁移时把旧 `schema_version`、`system_type`（或 `distribution`）、`architecture` 和 `packages`
分别改写为 `[Config] SchemaVersion`、`[Distribution] Distribution`、`Architecture`
及 `[Content] Packages`，另存为 `build/config.conf`。其他自定义设置也应迁入相应分节。

初始化系统生成 `build/environment.conf`，其 `[Environment]` 组与其他 INI 文件一起合并，
可移入任意构建配置文件。`http_proxy=http://127.0.0.1:10808` 直接赋值，
`https_proxy=$https_proxy` 继承 RStarSleep 进程的对应变量；来源未设置时不显式传入。
`NAME=` 设置空值，`NAME=$$value` 设置字面值 `$value`。变量名保留大小写，同名变量
只允许赋值一次；值沿用 `#` 注释和缩进续行规则，不经过 shell 展开。
该组由控制器解析，现有插件不必新增字段声明。宿主 bootstrap、容器任务、预下载和
构建脚本共用准备阶段解析的环境。配置文件随版本留档，运行中编辑不影响当前构建。
旧 TOML 支持对应 `[Environment]` 字符串表。默认环境模板不启用任何代理，初始化保留已有文件。

系统运行配置 `systems/<系统标识>/config/subsystem/config.toml` 保留
`schema_version = 1` 的旧模板兼容性，其他字段有默认值。支持授权 UID、自启、闲置时长、
会话模式、显式挂载、tmpfs 大小、GPU 及入口导出；完整示例见
[子系统使用说明](../../docs/subsystem-usage.md)。运行请求使用当前有效配置，构建留档
不锁定后续运行策略；配置无效时报错，不回退到旧配置。

## 当前接口

仓库内的 `src/config/` 是 Rust 源码模块，由 `src/lib.rs` 导出为
`rstarsleep::config`，与运行时工作目录内的 `config/` 区分。

源码注释使用英文在上、中文在下的对应段落。模块使用 `//!`，类型、字段和接口
使用 `///`，关键实现理由使用 `//`。所有函数（包括私有辅助函数、trait 声明及其
实现、测试函数）都必须写明函数作用，并在 `# Arguments` 章节逐项解释参数。
方法还需解释 `self`，未使用的参数需注明，无参数函数显式写明“无参数”。参数名称
应与签名一致，并说明路径、标识等参数的具体含义和约束。

公开接口另需说明前置条件、读写行为及错误条件；Rustdoc 使用标准的 `# Errors`
章节。默认配置模板和本文的代码示例采用相同的双语注释顺序。

```rust,no_run
use rstarsleep::config::{ConfigStore, initialize};

# /// Demonstrate workspace initialization and package configuration access.
# /// 演示工作目录初始化和软件包配置访问。
# ///
# /// # Arguments
# ///
# /// No parameters.
# /// 无参数。
# fn main() -> Result<(), Box<dyn std::error::Error>> {
// Use the mounted Btrfs top level with permission to create subvolumes and write files.
// 使用已挂载的 Btrfs 顶层；调用者应具有创建子卷、写文件的权限。
let store = initialize("/mnt/btrfs")?;
store.initialize_system("desktop")?;

// The runtime mount point can also be used directly for reading.
// 挂载后的入口也可直接用于读取。
let store = ConfigStore::new("/rstarsleep")?;
let config = store.read_packages("desktop")?;
assert!(config.packages.is_empty());

// Read a version's package configuration after the build module creates its record.
// 后续构建模块创建留档后，可读取某个版本的软件包配置。
let recorded = store.read_version_packages("desktop", "v1")?;
let cache = store.build_cache_path("desktop", "v1", "packages")?;
# Ok(())
# }
```

软件包读取接口只解释 `[Content] Packages`，保留列表顺序和重复项，不查询软件包仓库或
安装软件。系统类型使用独立的 `read_system_type()` 接口，读取 `[Distribution] Distribution`
和可选 `Architecture`；不设置架构时使用宿主架构。旧 TOML 继续接受 `distribution` 别名。
全局配置和子系统运行配置不会被这两个接口读取。
缺失文件、缺失 `Packages`、无效 INI 或无效包名均返回错误，不静默替换成空列表。
INI 错误包含文件路径及行号，缺失设置的行号为 0。完整构建读取要求 `[Config] SchemaVersion=1`。

## 插件预设与自动选择

项目根的 `presets/plugins/arch/` 包含 Arch 插件的元数据、各操作文件和资源。
Cargo 的 `build.rs` 按稳定顺序打包为 gzip 压缩的 tar，并通过 `include_bytes!`
嵌入程序；初始化不要求安装时还能访问项目源码或外部预设目录。
修改预设源码后需重新编译，再使用 `init [<Btrfs 顶层绝对路径>] --update-presets`
分发更新。库调用者可在初始化后调用 `ConfigStore::update_presets()` 执行同样的替换。

用户将插件压缩包或目录放入工作根的 `plugins/`，无需在全局配置注册路径。
`rstarsleep::controller::plugins::for_system(&store, "desktop")` 从系统构建配置
读取系统类型及架构，按包内 `plugin.toml` 的 `system_types` 和 `architectures`
自动选择。省略 `system_types` 时使用元数据的 `id`。匹配不依赖文件名；没有
匹配或存在多个有效匹配时返回明确错误。无效插件包跳过，其诊断保存在加载结果的
`discovery_warnings`；没有匹配时，错误同时包含这些诊断。

插件目录也会完整复制到独立会话中，源码不作为执行环境；摘要涵盖内容、权限及链接。
支持 `.tar`、`.tar.gz`、`.tgz`、`.tar.xz` 和 `.tar.zst`。解包根直接有
`plugin.toml` 时直接使用；否则只有一个顶层目录，且其中直接有该文件时自动
提升为插件根。目录计数包含隐藏项，多层包装不自动剥离。条目及链接目标不能
包含绝对路径或 `..`。支持插件 API 2 和 3；无基础缓存的系统创建需要 API 3 的
`system_init.sh`，API 2 仍可维护已有缓存。根内必须提供所有必需操作的同名
带 `.sh` 后缀的普通文件或无后缀的可执行文件；两者同时存在时优先 `.sh`，元数据不包含 `entry`。操作文件不允许使用目录或符号链接。

加载结果保留整个临时环境，释放时自动清理；查找和解包不自动执行插件。
插件可声明顶层 `post_maintenance = true`，加载时要求存在同名 `.sh` 或无后缀可执行入口。
未声明时关闭；接口无参数，表示通用的软件维护后行为，具体职责由发行版插件决定。
元数据必须包含 `[tools]` 的 `host` 和 `system` 工具列表；无额外工具时显式使用
空列表。裸工具名从实际执行环境的 PATH 查找，带 `/` 的相对路径从插件根定位。
工具需为可执行文件，不能使用绝对路径或父级跳转。

`LoadedPlugin::invoke()` 按操作名定位文件，只向 Bash 传递该操作的参数，调用形式
为 `bash <插件根>/<操作名>.sh [参数...]`。调用前检查文件和工具：bootstrap 检查宿主声明及 Bash；
其他任务先检查宿主 nspawn，再在目标系统中检查系统声明及 Bash。目标根需有
`/bin/sh` 供固定的工具检查脚本使用。缺少工具时返回插件、任务、执行环境及全部
缺失项，操作文件不运行。系统任务通过 nspawn 挂载完整插件及独立临时空间后执行。
版本和阶段缓存流程由构建控制器执行，包格式和流程见 [构建设计](../../docs/build-design.md)。

有持久卷的构建插件调用可通过 `RSTAR_CONTROL_SOCKET` 发送同步 `save` 请求，
将全部持久卷当前状态作为正式缓存保存。`RSTAR_SDK_DIR` 提供 Python 模块 `rstar.py`，
`RSTAR_SAVE` 指向 `rstar-save` 客户端（需要 Python 3）。控制通道只在本次调用内有效，
工具探测不挂载该通道。保存不改变当前工作挂载；后续失败保留最近一次保存，
下一次 build 仍正常维护缓存，不恢复上次执行位置。详见[保存协议](../../docs/build-design.md#插件主动保存)。

系统、版本和流程标识必须以 ASCII 字母或数字开头，其余字符只能使用字母、数字、
点、下划线和连字符，避免标识被当成路径使用。

## 命令行

```sh
# Show the init subcommand's argument requirements and usage.
# 查看 init 子命令的参数要求和使用方式。
cargo run -- init --help

# /mnt/btrfs must be the mounted Btrfs top level; the command requires write permission.
# /mnt/btrfs 必须是已挂载的 Btrfs 顶层，命令需要相应写权限。
cargo run -- init /mnt/btrfs

# Access the created subvolume directly or use the mounted /rstarsleep workspace.
# 可直接访问已创建子卷，也可使用已挂载的 /rstarsleep。
cargo run -- init-system /mnt/btrfs/@RStarSleep desktop
cargo run -- packages /mnt/btrfs/@RStarSleep desktop
```

初始化和配置读取不自动编辑 `/etc/fstab` 或导出应用入口。
显式路径初始化不执行挂载；省略路径时，自动识别需要 `findmnt`，自动挂载还需要 `mount` 及挂载权限。
构建中的 Mount 继承通过发行版插件生成挂载配置。

`init` 子命令显式传入路径时调用 `config::initialize()`，省略路径时调用
`config::initialize_current_system()` 自动定位当前系统所在文件系统的顶层。
目录参数过多时显示具体错误和子命令用法；初始化失败时输出错误原因并返回非零退出码。

`init-system [<工作目录绝对路径>] <系统标识>` 同样支持省略工作目录，使用
`config::current_system_workspace()` 查找当前系统所在文件系统上的已初始化工作目录，
兼容已有小写子卷；工作目录不存在时提示先运行 `init`。
两个命令通过独立的 `config::with_current_system_top_level()` 接口复用识别及挂载逻辑，
实现位于 `src/config/filesystem.rs`。

命令分派位于 `src/cli/mod.rs`，初始化命令的参数处理和执行位于 `src/cli/init.rs`。
其他子命令也使用独立模块；`src/main.rs` 仅保留进程入口、错误输出和退出码处理。


## 构建目录与配置留档

`ConfigStore::build_work_dir()` 读取可选全局 `work_dir`，默认使用 `<管理根>/work`。
相对路径基于管理根解析，绝对路径直接使用；拒绝父级跳转及与永久目录重叠。
该选项只影响临时构建根，不改变配置、插件、版本和缓存位置。

`ConfigStore::read_build_config()` 读取 `[Config] SchemaVersion`、`[Distribution] Distribution`、
可选 `Architecture` 和必需 `[Content] Packages`，返回现有 `BuildConfig` 数据结构。
`read_plugin_parameters_at()` 读取所有节和裸参数，返回规范环境名、生效值、节名和字段所属节。
旧 TOML 已知顶层字段映射到对应节；`distribution_options` 字符串表映射到 Distribution 节，
其他顶层字段保留为裸参数，其他表作为同名节。`read_distribution_options_at()` 仅为统一预下载
保留原来的 Distribution 环境和分组方式。
构建先在 `std::env::temp_dir()` 所选目录下的私有 `rstarsleep-preflight-*` 会话内冻结配置并暂存预检日志；
预检通过后，跨文件系统复制同一份冻结配置和日志，在版本目录内完成发布。
本次调用正常返回前显式清理预检会话，清理失败会报错；强制终止可能留下系统临时目录。
后续只读冻结版本。插件必须声明 `[cache] base_system`、
`software`；支持 `RSTAR_裸参数名`、`RSTAR_节名`（整节）和 `RSTAR_*节名_参数名`（精确字段）。
基础字段变化清理历史基础及后续缓存；仅软件字段变化清理软件及后续缓存；空列表不按字段失效。
软件缓存还依赖基础字段，未选参数变化保留缓存。Arch 预设检测 Config、Distribution 整节，
不检测 Packages。完整留档摘要检测历史来源被修改的情况。
每次构建还固定删除历史 script 及后续阶段缓存；上述删除不影响已发布版本根及配置留档。
构建开始将当前可执行程序同步到管理根的 `RStarSleep`，内容相同时跳过改写。

构建、插件和软件代码的文件职责见 [构建设计](../../docs/build-design.md)。

## 系统构建脚本

`systems/<系统>/config/script/` 由 `init-system` 创建，旧配置缺少此目录时视为空。
构建开始将脚本输入一并留档；script 步骤位于软件安装/维护后、版本发布前。
此步骤从已提交的 `packages` 缓存创建可写 `script-root`，先应用 `CopyOnWriteBuild`，
再执行可选插件 `post_maintenance`，最后执行单系统脚本。
Arch 预设在后处理接口中读取可选 `[Content] RootPassword` 设置 root 密码，未配置或为空时不修改当前工作根中的密码；
修改该字段不使基础或软件缓存失效。配置值按现有 INI 规则解析，并随配置以明文留档。

```text
config/script/
├── 01-configure.sh         # 放入独立空目录，保留文件名执行
├── 02-resources/           # 以自身为工作目录，在系统内部运行 main.sh
│   ├── main.sh
│   └── data/
└── 03-settings.tar.gz      # 解包后以资源根为工作目录运行 main.sh
```

压缩格式、单层包装根和链接边界与插件相同；入口必须是普通文件，不能是符号链接。
按顶层文件名顺序串行执行，每项在系统内获得独立可写资源目录，完成后清理。
输入来自冻结配置，资源写入不修改当前配置或留档。未知文件及不安全条目报错。

此步骤每次直接从本次软件缓存创建可写 CoW 工作根，构建继承和插件后处理也每次重新执行，
不会修改基础或软件缓存。构建继承、插件后处理和用户脚本均执行成功后独立提交
`cache/<版本>/script` 与 `script.toml`，随后处理 Mount、启动继承准备及版本发布。
没有 Mount、启动继承及额外 fstab 行时直接从 script 发布。
旧 script 缓存永不用于构建，无有缓存/无缓存分支；删除或修改脚本不会继承旧脚本效果。
script 及后续阶段快照只用于调试和备份，不作为下次构建的复用来源。
构建继承、插件后处理或脚本失败阻止发布，但保留基础和软件缓存。全配置摘要包含脚本及权限，检测留档修改。

## 继承配置与版本数据

新初始化的 INI 系统生成空的 `systems/<系统>/config/build/inherit.conf` 模板，已有文件保留。
可将继承设置写入该文件，例如：

```ini
[Inheritance]
# Capture after package cache commit, before post_maintenance and scripts.
# 软件缓存提交后、插件后处理及脚本执行前继承。
CopyOnWriteBuild=/etc/hostname
# Capture at the first start of this version.
# 每个版本首次启动时继承。
CopyOnWriteBoot=
    /home/alice/
    /var/lib/myapp/
# Seed once in this system's persistent directory, then reuse it.
# 在本系统 persistent 中首次初始化，之后复用。
Mount=/srv/data/
Mount=/etc/myapp.conf
```

控制器以 `InheritanceConfig::read()` 从合并参数独立读取这些设置，接受 CopyOnWriteBuild、CopyOnWriteBoot、Mount、DirectorySubvolume、NocowHandling，
不要求插件在 `[config]` 声明控制器所属继承参数。每行一个规范绝对路径，必须以 `/` 开头，
目录必须以 `/` 结尾，普通文件和符号链接不能以 `/` 结尾；执行继承时核验类型。不接受 `/`、
`//`、`.` 或 `..` 路径段、重复、相互包含或持久名称冲突；两份 CoW 列表和 Mount 统一校验，
来源还不能包含目标管理存储。
两份 CoW 列表原样复制符号链接自身，不解析或改写链接文本，支持断链及循环链接。
来源祖先链接会被拒绝；Mount 直接配置符号链接同样在预检时报错，目录内部链接仍可保留。
Mount 路径还不能遮盖 `/rstarsleep`、`/run/rstarsleep` 或包含 nspawn 绑定分隔字符 `:`、`\`。

CopyOnWriteBuild 在软件安装/维护及 `packages` 缓存提交后，在新建的 `script-root` 中继承当前运行系统，
随后才执行 `post_maintenance` 和用户脚本，结果保存在 `script` 缓存；CopyOnWriteBoot 在版本首次启动时继承。
两份列表可同时配置，分别执行，按 Btrfs UUID 检查同一文件系统。
构建开始先按冻结配置检查：CopyOnWriteBuild 失败立即退出；CopyOnWriteBoot 的显式路径在
当前视图中不存在、类型不符或 UUID 不一致仅警告。继承提示共用一次三秒取消等待，
不在后续构建阶段重复当前视图检查。
目录默认子卷化，已有来源子卷直接递归快照；`DirectorySubvolume=no` 可关闭最外层子卷化。
保存阶段缓存、恢复及发布根均显式处理全部内部子卷；缓存封存为只读，工作根与最终版本根可写。
`NocowHandling=error|reflink|copy|move-snapshot` 是统一枚举，默认 `error`，策略之间不会自动降级。
`reflink` 先匹配空目标 inode 的 CoW/NOCOW 属性再强制克隆；`copy` 仅对 NOCOW 文件明确完整复制。
`move-snapshot` 要求 `DirectorySubvolume=yes`，授权将实际离线普通来源目录迁成独立子卷并以快照恢复原位，
可能发生完整复制。构建迁移发生在冻结来源和计算继承指纹之前；启动迁移使用绑定待办摘要的持久化记录恢复，
未提交迁移保护来源免于清理。不支持的单文件、活动来源、目录外硬链接或重叠待办引用会报错。
策略固定在构建输入和启动清单中，详情见 [首次启动继承](../../docs/initramfs-inheritance.md)。

CopyOnWriteBoot 非空时，构建继承结果保存在准备根中，启动列表单独写入清单，在主系统首次
启动的 initramfs 或子系统启动准备阶段完成继承。新清单逐项保留配置原路径并固定当前来源根身份，
生成时不扫描来源目录或展开后代挂载；实际启动才在来源根的底层视图解析路径，发现并递归快照
嵌套子卷，子卷无需分别挂载。准备根与清单随版本保存，
不依赖后续会清理的 script 缓存。详细配置、依赖和恢复见
[首次启动继承](../../docs/initramfs-inheritance.md)。

Mount 先去掉目录路径末尾的 `/`，再将其余 `/` 替换为 `-`，包括开头斜杠：`/home/alice/` 保存为
本系统 `persistent/-home-alice`。不存在时首次复制当前运行系统的数据；已存在时复用，
不再读取或覆盖当前来源，按持久对象校验路径类型。允许跨分区、嵌套挂载及子卷，并报告 Warning；首次复制
允许普通复制。目录为可写 Btrfs 子卷，文件为普通文件，`clean` 保留这些数据。
新根保留挂载点底层内容以便系统工具可用；插件调用和查询期间绑定 persistent，
继承 `/etc` 时将生成的 fstab 同步到新根。

控制器创建目标路径后调用可选插件接口 `mount_inheritance`，每组参数为
`source target filesystem options`。Arch 把目录以 UUID/subvol 挂载，把文件通过
`/rstarsleep` 绑定挂载，必要时生成管理子卷挂载。`[Content] FstabLines` 是自由列表，
可重复赋值或使用多行值追加完整 fstab 行；该字段须在插件中声明，可独立使用。
Arch 用专用标记块保存生成行，重复调用替换该块并保留其他配置。

## 主系统启动发布

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

## 子系统产物、运行代与用户入口

构建最后原子发布 `systems/<系统>/build-current.toml`，指向完整 root 或有效的
pending-inheritance 产物；失败构建保持上一引用。冷启动/重启只读取这个引用，
不扫描历史版本，也没有版本选择入口。子系统在 nspawn 引导前完成延后继承，
再递归生成可写运行代。同产物和启动配置复用现有根，换产物或挂载/tmpfs/GPU 配置时
生成新代，不比较运行修改。授权和入口配置变化不触发根重建。

`runtime/current.toml` 表示可写根已同步，机器就绪另存于
`/run/rstarsleep/<规范管理根SHA-256>/<系统>.origin.toml`；同目录保存 daemon 锁、
`manager.sock` 和按系统隔离的 guest socket。daemon 恢复时核对实际 leader/根身份，
并修复 guest socket 绑定。新机器就绪后回收旧代，挂载或待办来源仍引用的根保留。

声明可选 `query_package_files` 的插件在构建 script 阶段后提供包所属文件清单，
保存为版本外层 `application-files.tsv`。该文件只是候选索引，继承可能改变实际文件；
导出和执行仍校验客体根内实际对象。运行机器使用其运行版本索引，冷机器使用当前产物。

`sync-exports` 将系统前缀命令写入用户 `~/.local/bin`，desktop/图标写入用户 XDG
数据目录。`runtime/exports-users.toml` 登记主动集成用户，用户自己的
`~/.local/state/rstarsleep/<管理根摘要>/<系统>.toml` 保存生成物内容所有权。
只有未被用户修改的自有文件会更新/撤销；产物或运行态切换会刷新已登记用户。

## 版本清理

`RStarSleep clean <系统> <版本> [--root <管理根>]` 删除该系统截止版本及之前的版本记录、
系统根、对应阶段缓存、启动目录和启动项，保留当前配置、身份及 persistent。
清理与构建共用独占锁，保护当前构建、运行及事务引用，并拒绝删除运行中的系统根、
待办来源或跨越挂载边界；检查失败不会开始删除。
`version-counter` 保留已分配的最高编号，清空历史后也不会复用旧版本号。
删除过程遇到错误会停止，可重试；版本所有权记录随版本保留到对应外部启动文件删除完成。

配置解析、模拟构建和真实 Bash 接口已测试；真实 Btrfs/nspawn 完整链路仍待集成验证。


`cache_reuse_minutes` 是可选全局非负整数，默认 `0`；系统 INI 的 `[Config] CacheReuseMinutes`（旧 TOML 顶层 `cache_reuse_minutes`）可覆盖，包括显式 `0`。
`ConfigStore::cache_reuse_minutes_at()` 在冻结配置上解析该策略，拒绝负数、多项及换算秒数溢出。策略无需在插件中声明。
正值允许前三个阶段在输入未变且未到期时跳过全部执行；执行时间不因缓存命中刷新。构建继承同时校验冻结来源的内容及元数据摘要。
详细阶段规则见 [构建设计](../../docs/build-design.md#限时直接复用前三个阶段)。


`[Config] DeduplicatePackages=yes/no` 是系统级可选布尔策略，默认 `no`，旧 TOML 对应
顶层 `deduplicate_packages = true/false`，无全局开关。此项由核心解析，不要求插件声明
配置字段；开启要求插件支持 `query_package_files`，否则版本分配、缓存清理及下载前报错。
软件维护后保存 `cache/<版本>/packages-files.tsv`，快照前与本批此前成功且开启的系统
二阶段只读缓存执行内核校验的 Btrfs 文件去重；第三阶段 `application-files.tsv` 保持现有行为。
二阶段直接缓存命中不执行新增去重，复用其索引；旧缓存缺索引时从可写副本补查。
索引随二阶段缓存失效或版本清理一并删除。开关不使软件内容缓存失效，不改变执行时间凭据。
功能增加查询和读取比较开销，共享字节数不等于实际释放空间。详见
[构建期软件包去重](../../docs/build-design.md#可选构建期软件包去重)。
