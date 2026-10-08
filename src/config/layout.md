# RStarSleep 工作目录

RStarSleep 的工作目录属于整个管理体系，不依附任何被管理系统的 `/etc`。
在 Btrfs 顶层（subvolid=5）创建专用子卷 `@RStarSleep`，建议运行时挂载到
`/rstarsleep`。存储路径与挂载路径指向同一份数据；初始化接口不执行挂载。

## 目录树

```text
Btrfs 顶层/
└── @RStarSleep/                         # 管理器专用子卷
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
    ├── work/                           # 默认构建临时目录，可由 work_dir 覆盖
    └── systems/
        └── <系统标识>/                  # 系统身份不随角色变更
            ├── config/                 # 当前可编辑配置
            │   ├── build/
            │   │   └── config.conf     # mkosi 风格 INI，读取 Packages 和发行版
            │   ├── script/            # 软件步骤后的构建脚本输入
            │   └── subsystem/
            │       └── config.toml     # 子系统运行配置，暂留模板
            ├── build.lock              # 每系统构建独占锁
            ├── identity.toml           # 独立安装时保存的稳定 machine-id
            ├── versions/
            │   └── <版本标识>/
            │       ├── state.toml      # building / failed / complete 状态
            │       ├── config/         # 构建时配置留档，结构同当前 config/
            │       └── root/           # 该版本系统根，由构建模块创建子卷
            ├── cache/
            │   └── <版本标识>/
            │       ├── base-system/    # 只读基础系统子卷快照
            │       ├── base-system.toml # 基础阶段完成、兼容性与来源
            │       ├── packages/       # 只读软件阶段子卷快照
            │       ├── packages.toml   # 软件阶段完成、目标与实际清单
            │       ├── script/         # 只读脚本阶段子卷快照，不作后续构建来源
            │       └── script.toml     # 脚本阶段完成与实际清单
            └── persistent/             # 此系统跨版本共享的持久化目录和文件
```

初始化管理器时创建 `@RStarSleep` 子卷、`config/`、`systems/`、`plugins/`、`predownload/`、`persistent/` 和全局配置，
并将内嵌的 Arch 预设压缩包写入 `plugins/arch.tar.gz`。已有同名普通文件不覆盖；
缺失的预设包会在再次初始化时补充。
初始化一个被管理系统时创建其当前配置（包括空 `script/`）、版本、缓存和持久化目录及默认配置。
这些系统目录目前是普通目录；具体版本根、阶段缓存是否使用子卷由对应功能实现。
初始化不会生成系统版本、配置留档或修改主/子系统角色，也不会覆盖已有配置。
子卷创建失败时返回错误，不使用普通目录代替；既有 `@RStarSleep` 必须为真实子卷。

## 系统归属与角色

任意被管理系统都可以由全局配置指定为主系统。全局配置单独指定子系统名单，
其余系统仅保留为被管理对象。子系统运行配置属于对应系统，只有它被显式指定为
子系统时才应用，包括向当前主系统导出应用入口。

角色变更不移动目录。移出子系统名单时，后续运行模块负责撤销已建立的集成。
持久化数据默认跨同一系统的版本共享，删除版本不应删除它；其快照与回滚策略
由持久化功能实现。缓存独立于已完成版本，可以按版本和构建流程清理。
每个系统独立安装和维护基础根，仅复用自己的最新过往阶段缓存。
下载的初始系统安装文件由插件保存到声明的全局持久目录，供后续独立安装使用；
管理器不创建跨系统共享的基础根。基础安装或恢复后均执行一次基础维护。

当前配置用于下一次构建。后续构建模块在开始构建时保存配置留档，并使用留档
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
```

未指定主系统时，省略 `main_system`。该文件目前仅生成，不实现角色配置读取。

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

已有系统或历史版本缺少 `build/config.conf` 时，继续读取旧 `build/config.toml`。
两种文件都存在时优先读取 INI，INI 无效不会回退到 TOML；初始化不以空模板覆盖旧 TOML 的设置。
迁移时把旧 `schema_version`、`system_type`（或 `distribution`）、`architecture` 和 `packages`
分别改写为 `[Config] SchemaVersion`、`[Distribution] Distribution`、`Architecture`
及 `[Content] Packages`，另存为 `build/config.conf`。其他自定义设置也应迁入相应分节。

系统运行配置 `systems/<系统标识>/config/subsystem/config.toml` 目前仅包含
`schema_version = 1` 和说明注释。其字段随子系统运行功能确定。

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
元数据必须包含 `[tools]` 的 `host` 和 `system` 工具列表；无额外工具时显式使用
空列表。裸工具名从实际执行环境的 PATH 查找，带 `/` 的相对路径从插件根定位。
工具需为可执行文件，不能使用绝对路径或父级跳转。

`LoadedPlugin::invoke()` 按操作名定位文件，只向 Bash 传递该操作的参数，调用形式
为 `bash <插件根>/<操作名>.sh [参数...]`。调用前检查文件和工具：bootstrap 检查宿主声明及 Bash；
其他任务先检查宿主 nspawn，再在目标系统中检查系统声明及 Bash。目标根需有
`/bin/sh` 供固定的工具检查脚本使用。缺少工具时返回插件、任务、执行环境及全部
缺失项，操作文件不运行。系统任务通过 nspawn 挂载完整插件及独立临时空间后执行。
版本和阶段缓存流程仍按构建设计接入，包格式和流程设计见 `docs/build-design.md`。

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

本模块不自动编辑 `/etc/fstab`、执行挂载或导出应用入口。

`init` 子命令直接调用 `config::initialize()`。目录参数缺失或过多时显示具体错误和
子命令用法；初始化失败时输出错误原因并返回非零退出码。

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
构建先冻结全部普通配置文件，后续只读冻结版本。插件必须声明 `[cache] base_system`、
`software`；支持 `RSTAR_裸参数名`、`RSTAR_节名`（整节）和 `RSTAR_*节名_参数名`（精确字段）。
基础字段变化清理历史基础及后续缓存；仅软件字段变化清理软件及后续缓存；空列表不按字段失效。
软件缓存还依赖基础字段，未选参数变化保留缓存。Arch 预设检测 Config、Distribution 整节，
不检测 Packages。完整留档摘要检测历史来源被修改的情况。
每次构建还固定删除历史 script 及后续阶段缓存；上述删除不影响已发布版本根及配置留档。

构建、插件和软件代码的文件职责见 [构建设计](../../docs/build-design.md)。

## 系统构建脚本

`systems/<系统>/config/script/` 由 `init-system` 创建，旧配置缺少此目录时视为空。
构建开始将脚本输入一并留档；script 步骤位于软件安装/维护后、版本发布前。

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

此步骤每次直接从本次软件缓存创建可写 CoW 工作根，执行成功后独立提交
`cache/<版本>/script` 与 `script.toml`，最终版本从 script 缓存发布。
旧 script 缓存永不用于构建，无有缓存/无缓存分支；删除或修改脚本不会继承旧脚本效果。
脚本失败阻止发布，但保留基础和软件缓存。全配置摘要包含脚本及权限，检测留档修改。


构建配置目录内所有普通 INI 文件按文件名排序独立解析后叠加，文件名和扩展名没有语义区别。
`config.toml` 仅作为没有 INI 文件时的历史格式回退；不参与混合叠加。重复单项核心设置
（SchemaVersion、Distribution、Architecture）报错，空赋值保留为列表成员，不清空之前的赋值。

插件可在 `[config.constraints.RSTAR_字段名]` 设置 `kind`：`free-single`、`free-list`、
`enum-single`、`enum-list`、`number`；枚举还需声明 `values=["候选值"]`。
单项和数字要求恰好一个赋值，数字必须有限；无约束字段默认为自由列表。
这些规则作用于原始赋值，而非拆分后的软件包名。插件应检查其余业务约束并明确报错。
缓存摘要包含每一个列表成员及其边界。


主系统由全局 `main_system` 指定。成功构建主系统时，从该版本 `/boot/vmlinuz-linux` 和
`/boot/initramfs-linux.img` 复制到 `/boot/RStarSleep/<版本>/vmlinuz` 和 `initramfs-linux.img`，
随后原子创建 `loader/entries/RStarSleep-<系统>-<版本>.conf`。
通过 `findmnt` 自动读取该版本 Btrfs UUID 和顶层子卷路径，生成 `root=`、`rootflags=`、`ro`。
全局可设置 `boot_dir="/boot"` 和 `kernel_options=["quiet"]`；启动目录应指向 systemd-boot
实际读取的 ESP/XBOOTLDR。额外参数不能覆盖程序生成的根选项。
缺少启动文件、UUID 或启动目标冲突均报错，不标记构建完成。
各版本的 `boot.toml` 保存启动文件所有权，清理不依赖之后的主系统角色变更。

新初始化使用 `@RStarSleep`，已有 `@rstarsleep` 会继续复用。
每次构建将当前可执行程序同步到管理根的 `RStarSleep`，内容一致时不改写。


`RStarSleep clean <系统> <版本> [--root <管理根>]` 删除该系统截止版本及之前的版本记录、
系统根、对应阶段缓存、启动目录和启动项，保留当前配置、身份及 persistent。
清理与构建共用独占锁，运行中的系统根不能被清理，检查失败不会开始删除。
`version-counter` 保留已分配的最高编号，清空历史后也不会复用旧版本号。
删除过程遇到错误会停止，可重试；版本所有权记录随版本保留到对应外部启动文件删除完成。
