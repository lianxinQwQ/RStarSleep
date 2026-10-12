# 构建、维护与发行版插件

本文说明已实现的 `build` 接口、文件职责、缓存状态和发行版插件协议。真实 bootstrap、nspawn 和完整系统安装链路仍需在具有相应权限的环境验证。

## 已确定的要求

- `build` 可以指定一个系统；省略系统或使用 `--all` 时构建全部系统。
- 工作目录默认由配置模块提供，使用其创建的专用根下独立目录；路径可以配置，
  不是命令行必需参数。
- `src/` 下新增构建维护控制器、基础系统、软件管理三个模块。
- 系统内操作统一通过 `systemd-nspawn` 执行。
- 各阶段通过强制 CoW 保存系统根缓存，不允许退回普通复制。
- 首版支持 Arch Linux / pacman，首次安装从官方 bootstrap rootfs 开始。
- 发行版插件以压缩包或目录分发，包含元数据、Bash 脚本及其他资源；复杂任务可以携带
  辅助二进制。新增发行版不要求重新编译主程序。
- 每个操作使用插件根内的同名 `.sh` 文件；调用参数只包含操作参数。
- 用户将插件包存放在工作根的 `plugins/`；系统配置通过 `[Distribution] Distribution` 自动选择
  匹配插件，不需要在全局配置注册每个包的路径。
- 项目根的 `presets/plugins/arch/` 提供 Arch 预设，编译时打包并嵌入程序，
  `init` 时写入 `plugins/arch.tar.gz`，保留用户已有的同名普通文件。
- 插件元数据显式声明需要的工具，程序在调用前检查对应执行环境；缺失时返回
  错误，不执行插件任务。
- 插件使用时在临时目录内解包或复制目录；仅有一层顶层包装目录且其中直接包含插件时，
  自动修正为插件根目录。
- 最新过往构建可以是失败构建，但只能复用其中已成功完成的阶段缓存。
- 软件维护先更新系统，再根据当前和缓存来源版本的 pkg 表调整软件，最后清理孤儿包。

以下规则对应当前实现；Arch 预设插件 API 为版本 3，元数据 schema 为版本 1。
API 2 仍支持已有缓存的维护；无基础缓存的创建需要 API 3。

## 命令行

工作目录不占用位置参数。`build` 从配置模块读取工作目录；未设置 `work_dir` 时，
使用配置模块创建的专用根下默认独立目录，不以当前 shell 的目录作为默认值。
默认管理根为 `/rstarsleep`，可通过 `--root <绝对路径>` 选择其他管理根。
默认构建工作目录为 `<管理根>/work`；全局 `work_dir` 的相对路径相对于管理根解析。
该目录必须独立于配置、插件和系统记录目录，修改它不会迁移永久数据。

```text
RStarSleep build
RStarSleep build --all
RStarSleep build <系统标识>
RStarSleep build desktop --root /mnt/btrfs/@RStarSleep
```

单个系统与 `--all` 不能同时指定。全部系统指 `systems/` 下已初始化的被管理系统，
不受主系统和子系统角色限制。按系统标识顺序串行构建；一个系统失败后继续
处理其他系统，最后汇报失败列表并返回非零退出码。

## 模块职责

| 模块 | 职责 |
| --- | --- |
| `src/controller/` | 系统选择、构建锁、版本分配、配置留档、缓存选择与校验、Btrfs CoW、插件解包及运行、nspawn 执行、完成状态 |
| `src/base_system/` | 调用插件完成 bootstrap、基础安装或基础维护，交由控制器提交基础缓存 |
| `src/software/` | 读取配置和包清单、计算包集合差异、调用插件调整软件及清理孤儿包，交由控制器提交软件缓存 |

参数解析放在 `src/cli/build.rs`，插件加载与执行接口放在控制器内部。
Arch 插件的源代码放在 `presets/plugins/arch/`，打包后嵌入程序并在初始化时分发。
核心模块中不直接拼接 pacman 命令。

文件按职责拆分；模块入口只声明模块、导出接口，执行流程与模型分别维护：

| 文件 | 单一职责 |
| --- | --- |
| `src/cli/build.rs` | 解析系统选择、`--all`、`--root` 和帮助 |
| `src/config/build.rs` | 完整构建配置、工作目录和系统枚举 |
| `src/config/ini.rs` | mkosi 风格 INI 解析、文件叠加、包列表追加及规范化摘要输入 |
| `src/config/inheritance.rs` | 独立读取控制器所属继承列表、路径及持久名称校验 |
| `src/config/runtime.rs` | 全局主系统、启动目录与额外内核参数校验 |
| `src/controller/model.rs` | 后端契约、冻结上下文、构建结果和错误 |
| `src/controller/workflow.rs` | 系统批次、一次构建生命周期、状态迁移和临时根清理 |
| `src/controller/stages.rs` | 按步骤执行基础系统、软件包、script、inherit 及最终根发布 |
| `src/controller/inheritance.rs` | 当前系统 CoW 复制、系统级持久挂载及显式子卷发布 |
| `src/controller/mounts.rs` | 文件系统 UUID、顶层子卷路径及挂载边界检查 |
| `src/controller/boot.rs` | 主系统 Type #1 启动发布与管理器可执行程序同步 |
| `src/controller/clean.rs` | 版本及缓存清理、启动文件所有权与运行系统保护 |
| `src/controller/records.rs` | 独占锁、单调版本分配和构建状态记录 |
| `src/controller/frozen.rs` | 配置及脚本输入留档、阶段兼容性摘要、完整来源摘要 |
| `src/controller/cache.rs` | 阶段旁记录校验、兼容性判断和快照原子提交 |
| `src/controller/predownload.rs` | 独立预下载环境初始化、维护、缓存发布与工作根清理 |
| `src/controller/persistent.rs` | 全局持久子卷事务、日志提交及中断恢复 |
| `src/controller/scoped_backend.rs` | 为每次插件调用分配并提交持久副本 |
| `src/controller/plugins/persistent.rs` | 持久映射校验和宿主/容器资源工作副本 |
| `src/controller/identity.rs` | 每系统 machine-id 的持久保存、旧身份接纳和首次生成 |
| `src/controller/backend.rs` | 原生 Btrfs 子卷/快照命令、插件及脚本执行适配 |
| `src/controller/fs.rs` | 路径检查、目录同步和 INI/TOML 文本原子写入 |
| `src/controller/plugins/model.rs` | 插件元数据、配置声明、加载会话与插件错误 |
| `src/controller/plugins/configuration.rs` | 必选/可选键校验、未声明键拒绝及 RSTAR_ 环境名转换 |
| `src/controller/resources/session.rs` | 共用资源冻结会话、目录/归档选择和根归一化 |
| `src/controller/resources/archive.rs` | 压缩格式识别、解码、安全 tar 解包 |
| `src/controller/resources/directory.rs` | 不跟随链接的目录复制、稳定完整树摘要 |
| `src/controller/container.rs` | 共用 nspawn 隔离参数及原生绑定参数编码 |
| `src/controller/plugins/loader.rs` | 共用资源准备后的插件元数据及操作校验 |
| `src/controller/scripts/inputs.rs` | 按文件名排序发现 script 条目，归一化目录、归档及独立 .sh |
| `src/controller/scripts/workflow.rs` | 复制资源到系统工作根、调用后端执行并清理资源 |
| `src/controller/scripts/runner.rs` | 以资源目录为容器工作目录运行 Bash，传播执行失败 |
| `src/controller/plugins/validation.rs` | 元数据、工具声明和资源路径校验 |
| `src/controller/plugins/discovery.rs` | 按类型/架构查找唯一插件，保留被跳过包的诊断 |
| `src/controller/plugins/commands.rs` | 插件宿主子命令声明、冲突检查与参数透传 |
| `src/controller/plugins/runner.rs` | 宿主或容器任务调用顺序及上下文传递 |
| `src/controller/plugins/tools.rs` | 可执行工具查找、固定探测和调用错误 |
| `src/controller/plugins/container.rs` | nspawn 参数、只读资源/配置与可写临时目录绑定 |
| `src/base_system/mod.rs` | bootstrap、基础安装与一次系统更新 |
| `src/software/model.rs` | 实际清单、目标状态和查询输出校验 |
| `src/software/session.rs` | 单个根上的包解析、查询与阶段状态捕获 |
| `src/software/reconcile.rs` | 首次安装、维护增删、安装原因和最终孤儿清理 |

插件归档测试、调用测试和上下文测试分别放入独立文件；软件测试与模拟包管理器
分别存放；`tests/build_workflow.rs` 验证完整流程，模拟存储后端放在 `tests/support/`。


## 插件压缩包与目录

支持直接存放在 `plugins/` 中的插件目录，以及 tar 系列压缩包，例如 `.tar.gz`、`.tar.xz` 和 `.tar.zst`。
插件根目录固定包含 `plugin.toml` 和各操作的同名文件，辅助资源由脚本相对引用：

```text
arch-plugin.tar.zst
├── plugin.toml
├── bootstrap.sh
├── base_install.sh
├── system_init.sh           # API 3 初始化独立安装系统的稳定身份
├── mount_inheritance.sh     # 声明能力后提供的可选挂载配置接口
├── update.sh
├── resolve_packages.sh
├── query_packages.sh
├── install.sh
├── remove.sh
├── mark_reason.sh
├── list_orphans.sh
├── remove_orphans.sh
├── clean_orphans.sh            # Arch 额外提供的操作
├── lib/common.sh            # 可选公共函数
├── bin/                     # 可选辅助二进制，可以按架构组织
└── data/                    # 配置模板、软件源和其他资源
```

元数据包含格式版本、插件身份和版本、支持的目标架构及缓存
格式标识。插件版本用于留档，缓存格式标识用于兼容性判断，两者分别记录。

```toml
# Metadata at the archive's plugin root.
# 压缩包插件根目录中的元数据。
schema_version = 1
api_version = 3
id = "arch"
version = "0.7.3"
system_types = ["arch"]
architectures = ["x86_64"]
cache_format = "arch-root-v1"
# Protected explicit base targets.
# 受保护的显式基础目标。
protected_packages = ["base"]
mount_inheritance = true

# Initial installation downloads must be retained for later independent installations.
# 初始安装下载文件必须保留，供后续独立安装使用。
persistent_dirs = ["arch-bootstrap:state/bootstrap"]

# Unified effective parameters accepted by single-system operations.
# 单系统操作接受的统一生效参数。
[config]
required = []
optional = [
    "RSTAR_CONFIG_SCHEMAVERSION", "RSTAR_DISTRIBUTION_DISTRIBUTION", "RSTAR_CONTENT_PACKAGES",
    "RSTAR_DISTRIBUTION_ARCHITECTURE", "RSTAR_DISTRIBUTION_BOOTSTRAPURL",
    "RSTAR_DISTRIBUTION_BOOTSTRAPSIGNATUREURL", "RSTAR_DISTRIBUTION_KEYRING",
    "RSTAR_DISTRIBUTION_MIRROR", "RSTAR_DISTRIBUTION_AURKEYSERVER", "RSTAR_CONTENT_FSTABLINES",
]

[config.constraints.RSTAR_CONTENT_FSTABLINES]
kind = "free-list"

# Base changes invalidate downstream caches; package targets are reconciled during maintenance.
# 基础变化使后续缓存失效；软件目标在维护中协调。
[cache]
base_system = ["RSTAR_CONFIG", "RSTAR_DISTRIBUTION"]
software = []

# External executables required by each execution environment.
# 各执行环境所需的外部可执行工具。
[tools]
host = ["bash", "curl", "tar", "zstd", "gpgv", "cp", "rm", "mktemp", "cat", "dirname", "mkdir", "sha256sum"]
system = ["bash", "pacman", "pacman-key", "awk", "cat", "rm", "mktemp", "dirname", "cp", "mkdir"]
```

操作文件优先使用 `<操作名>.sh`，例如 `install.sh`，也支持无后缀同名可执行文件，
不通过元数据配置入口。API 2 加载时要求原十个必需操作，API 3 额外要求系统身份初始化
`system_init`；声明 `mount_inheritance = true` 时还必须提供同名操作。
所有声明的操作文件必须存在且为普通文件，不接受目录或
符号链接。每次调用前再次校验当前操作文件。公共函数可以放在 `lib/common.sh`
供各操作脚本引用。

API 2 删除了 `entry` 字段，不接受 API 1 插件或旧入口声明。已有插件需拆分操作
文件、移除 `entry`、更新 `api_version` 并重新打包。`init` 保留已有同名插件包，
所以升级程序后需主动替换旧的 `plugins/arch.tar.gz`。
API 3 要求 `system_init`，未升级的 API 2 插件仍可维护已有
兼容缓存，但不能进入无基础缓存的创建分支。

辅助二进制由操作脚本选择和调用，可以分别提供宿主和目标系统所需的架构版本。
其动态库或解释器依赖需在
对应运行环境中具备，或者随插件提供；打包成压缩包本身不使二进制跨架构运行。

## 临时解包与目录修正

每次系统构建使用一个独立的临时会话目录，先将压缩包解包或将目录完整复制到 staging，再按以下
规则确定插件根：

1. 解包根直接有 `plugin.toml` 时，直接校验并使用该根。
2. 否则，解包根必须恰好只有一个普通目录，且该目录直接有 `plugin.toml`，
   才将该目录提升为会话中的插件根，去掉一层包装目录。
3. 其余情况报插件格式错误，不搜索任意深层目录，也不递归去除多层包装。

目录计数包含隐藏项；目录提升使用目录移动，不遗漏隐藏文件。根元数据存在但
内容无效时直接报错，不改用其他位置的元数据。元数据解析后继续校验 API 版本、
插件身份、目标架构和全部必需操作文件。

归档解包需拒绝绝对路径、逃出临时根的路径和设备节点；链接不得造成越界写入。
不应用归档携带的宿主所有者和特殊权限。解包及校验失败时不运行操作脚本。

插件根作为操作入口的资源根和默认工作目录。单独提供临时写入目录，构建完成或
失败后，在插件进程退出和容器挂载释放后清理整个临时会话。持久数据、系统根和
阶段缓存仍位于管理器的工作目录，由控制器管理，不写入插件临时目录。

插件运行环境包含资源根内全部文件；入口所需解释器和系统工具由对应执行环境提供。
构建记录保存插件身份、版本及归档摘要，同一次构建使用同一份已加载插件。

## 插件配置声明与环境变量

`plugin.toml` 必须提供 `[config] required`、`optional` 和 `[cache] base_system`、
`software` 四个字符串列表，即使为空。配置声明和单系统插件接口共用规范的大写名称：
首节之前的裸参数 `Channel` 为 `RSTAR_CHANNEL`；`[Distribution] Architecture` 为
`RSTAR_DISTRIBUTION_ARCHITECTURE`。读取全部节，包括控制器使用的 SchemaVersion、
Distribution 和 Packages；同名参数位于不同节时独立传入。名称组件以 ASCII 字母开头，
后续仅允许字母、数字、下划线；拒绝大写转换冲突以及节/键拼接冲突。

插件匹配成功后、预下载及任意插件脚本运行前，检查所有显式生效参数：必选值须存在且非空，
除控制器独立读取的 `[Inheritance]`、`[Environment]` 外，每个参数须在必选或可选列表中声明。
声明重复、包含无效名称或占用 `RSTAR_ROOT`、`RSTAR_CONFIG_DIR` 等控制器变量时加载失败。
值保留空格、换行和 `$`；每个设置都是字符串列表，重复赋值追加，空值不清空先前成员。
多行赋值保留为一个字符串，Packages 的传统变量提供按逗号/空白拆分后的包名列表。
`RSTAR_PARAMETERS_JSON` 提供完整参数名到原始字符串列表的映射，传统变量以换行连接值。
配置值不拆成位置参数，也不交给 shell 求值。
bootstrap 通过进程环境接收，系统内任务通过 nspawn `--setenv` 接收，构建脚本接收同一组冻结参数。
操作自身的位置参数（如包目标、安装原因和 machine-id）继续独立传递。

`[Environment]` 使用同一 INI 文件发现、排序合并、注释及续行机制，默认模板为
`build/environment.conf`，也可放入其他构建 INI 文件。该组保持原始环境变量名大小写，
不导出为 `RSTAR_ENVIRONMENT_*` 参数，也不要求发行版插件声明。变量名须符合
`[A-Za-z_][A-Za-z0-9_]*`，禁止 `RSTAR_`、`RSTAT_` 前缀；每个变量必须恰好一次赋值。
`NAME=value` 为字面值（允许空值和续行），`NAME=$SOURCE` 从 RStarSleep 进程环境继承，
来源未设置时移除宿主目标变量，并不向容器显式传入；`NAME=$$value` 为字面值 `$value`。
解析结果在 prepare 阶段固定于 BuildContext，原始配置随版本留档，继承值不另行持久化。
宿主使用原生进程环境，容器工具检查、插件及构建脚本使用独立的原生 `--setenv` 参数，
不经过 shell 求值，继承值保留非 UTF-8 字节。不同有效环境分别执行预下载批次；
环境组不改变插件声明的基础/软件缓存字段，已有下载根仍按插件、架构和发行版配置维护。
旧 TOML 的 `[Environment]` 字符串表使用相同解析规则；空 INI 模板继续允许旧格式回退。

`RSTAR_DISTRIBUTION_ARCHITECTURE` 若为必选，省略不会被默认值补齐；若为可选，
省略时传入用于匹配插件的默认架构。其他可选项省略时不传入，也不继承宿主的同名配置变量。
统一多系统 `predownload` 接口保留包需求参数以及原来的 Distribution 环境名（如
`RSTAR_ARCHITECTURE`、`RSTAR_AURKEYSERVER`）；环境初始化、维护和其他接口使用新名称。
旧 TOML 的顶层已知字段映射到对应节，`distribution_options` 映射到 Distribution 节，
其他顶层字段作为裸参数，其他表作为同名节。旧插件须修改名称、补充缓存声明并重新打包。

插件在 `[config.constraints.RSTAR_字段名]` 声明 `kind`，支持 `free-single`、`free-list`、
`enum-single`、`enum-list` 和 `number`；枚举用 `values` 声明允许值。
单项和数字拒绝重复赋值，数字拒绝 NaN/Infinity。未约束字段保持自由列表，
插件负责检查其余业务约束并报错。约束作用于原始赋值而非拆分后的包名；缓存摘要保留列表边界。

缓存列表使用以下选择器（名称均大写）：

| 选择器 | 检测范围 |
| --- | --- |
| `RSTAR_CHANNEL` | 裸参数 Channel 的生效值、存在或缺失 |
| `RSTAR_DISTRIBUTION` | Distribution 整节的存在状态及全部生效参数 |
| `RSTAR_*DISTRIBUTION_MIRROR` | 仅 Distribution 节的 Mirror 参数 |
| `RSTAR_DISTRIBUTION_MIRROR` | 已声明的精确参数名，也可以直接使用 |

被选字段须在 config 中声明；整节选择器的字段前缀须对应已声明的配置。
`*` 只出现在精确字段选择器中，不出现在实际环境变量中，不是通配符。
若裸参数与整节同名，选择器报歧义错误，应调整配置名称。
基础字段变化清理全部历史基础及后续缓存；仅软件字段变化清理软件及后续缓存。
软件兼容性同时依赖基础和软件两组字段。空列表不按配置字段使对应阶段失效；
插件身份、缓存格式、系统类型、架构和受保护基础目标仍须兼容。
未选字段、其他普通配置文件和 script 不参与这些字段摘要；完整留档摘要仍检测来源是否被修改。
Arch 预设检测 Config 和 Distribution 整节，software 列表为空，因此 Packages 变化由维护协调。

## 工具声明与调用前检查

元数据必须提供 `[tools]`，同时显式列出 `host` 与 `system`；没有额外需求时
对应列表写为 `[]`。工具名从实际执行环境的 PATH 查找，插件自带工具使用
`./bin/helper` 等相对路径，不能声明绝对路径或 `..`。

检查不仅要求文件存在，还要求它是可执行文件。bootstrap 检查宿主的声明工具，
系统任务通过 nspawn 中的固定 POSIX 探测检查目标系统的声明工具，不使用宿主
PATH 代替。操作执行器调用 `.sh` 入口时检查 Bash；无后缀入口按声明检查依赖，
不强制要求 Bash。系统任务还检查宿主 systemd-nspawn，
目标根需有 `/bin/sh` 用于探测。宿主依赖不会在 bootstrap 后错误要求存在于
目标系统，目标依赖也不会阻止在空根目录上进行 bootstrap。

所有缺失项在一次错误中报告，并包含插件身份、操作及宿主/目标环境。
检查脚本不运行插件或声明的工具。检查失败或工具缺失时，不执行操作文件；
程序不自动安装缺失工具。

## 插件任务接口

每个发行版提供一个插件，同时负责基础系统和软件包操作，避免两个插件对
发行版、包名和包数据库产生不同理解。插件提供以下能力：

| 能力 | 含义 |
| --- | --- |
| 元数据 | 接口版本、发行版身份、支持架构、缓存格式及能力声明 |
| `bootstrap` | 在空系统根上下载、校验并解包官方 rootfs |
| `base_install` | 定义并安装基础包，完成 bootstrap 包状态的整理 |
| `system_init`（API 3） | 在独立安装根中初始化稳定身份和系统特定配置 |
| `update` | 执行该发行版的完整系统更新 |
| `resolve_packages` / `query_packages` | 将配置解析为实际包名，查询已安装包及显式/依赖安装原因 |
| `install` / `remove` / `mark_reason` | 安装、依赖检查下删除、调整安装原因 |
| `list_orphans` / `remove_orphans` | 查找并清理孤儿包 |
| `mount_inheritance`（可选） | 在软件与脚本完成后生成系统所需的持久挂载配置 |

主程序只理解这些操作的语义；pacman、apt 等具体命令及输出解释属于发行版插件。
简单任务直接在脚本内完成，复杂任务调用随包携带的二进制，不改变核心接口。

使用短生命周期的任务调用，控制器按操作名优先选择对应的 `.sh` 文件，由 Bash 执行；
不存在 `.sh` 文件时选择无后缀同名可执行普通文件，直接执行。
每个文件只接收该操作的独立参数：

```text
bash <插件根>/<操作名>.sh [参数...]
<插件根>/<操作名> [参数...]
```

例如安装调用为 `bash <插件根>/install.sh vim git`，查询调用为
`bash <插件根>/query_packages.sh`。插件没有二级操作命令或统一分派入口。

| 必需操作文件 | 位置参数 | 查询 stdout / 行为 |
| --- | --- | --- |
| `bootstrap.sh` | 无 | 在空系统根上下载、验签并解包 rootfs |
| `base_install.sh` | 无 | 安装基础包并整理 bootstrap 包状态 |
| `system_init.sh`（API 3） | `machine-id` | 初始化副本，接收非零 32 位小写十六进制身份 |
| `update.sh` | 无 | 完整系统更新 |
| `resolve_packages.sh` | `包或组目标...` | 每行一个实际包名，一个目标可解析为多个包 |
| `query_packages.sh` | 无 | 每行 `包名<TAB>explicit` 或 `包名<TAB>dependency` |
| `install.sh` | `包名...` | 显式安装指定包 |
| `remove.sh` | `包名...` | 检查依赖后删除指定包 |
| `mark_reason.sh` | `explicit\|dependency 包名...` | 调整安装原因 |
| `list_orphans.sh` | 无 | 每行一个孤儿包名 |
| `remove_orphans.sh` | `包名...` | 删除指定孤儿包 |

Arch 还提供无参数的 `clean_orphans.sh`，用于自动查询并清理孤儿依赖。
控制器逐个解析唯一目标，要求每次返回至少一个合法、不重复的具体包名，
再合并不同目标的重叠结果。Arch 先用 `pacman -Si` 查询具体包，明确缺包时才用
`pacman -Sgq` 展开软件组；具体包与组同名时优先选择包。展开成员全部作为显式目标，
阶段记录保存具体包名；后续维护重新解析，按现有安装原因和孤儿清理规则处理成员增减。
`.sh` 操作文件通过 Bash 读取，无后缀入口和随包辅助二进制必须具备可执行权限。
两种入口使用相同参数、环境变量、工作目录、挂载、输出和退出码协议。

退出码 0 表示成功，非零表示失败。修改操作输出执行日志；查询操作的 stdout
只返回约定数据，日志走 stderr。简单包集合逐行返回实际包名；安装原因可以使用
约定的制表符分隔字段。查询数据的具体格式随 API 版本固定，主程序进行校验。
首次实现不要求插件提供双向 JSON 回调协议。

控制器的构建进度写入 stderr，使用 `==> [RStarSleep][系统标识] 中文 / English`
格式，覆盖配置准备、预下载、基础系统、软件、脚本、继承、快照保存、工作根清理和版本发布。
步骤执行前提示开始，结束后报告完成或失败及实际耗时；缓存复用与跳过执行也明确提示。
共享预下载批次会列出包含的系统，插件修改操作报告共享缓存工作副本的准备、提交或回滚。
查询操作不逐条打印额外操作提示，其 stdout 继续仅包含协议数据。

控制器传递明确的上下文变量，例如 `RSTAR_PLUGIN_ROOT`、`RSTAR_PLUGIN_TMP`、
`RSTAR_ROOT`、`RSTAR_CONFIG_DIR`、`RSTAR_SYSTEM_ID` 和 `RSTAR_DISTRIBUTION_ARCHITECTURE`。
这些路径必须对应当前宿主或容器内的实际可见路径。包名作为独立参数传递，
脚本使用 Bash 数组及正确引用，不将参数拼接后交给 `eval`。

执行位置由控制器根据任务确定：

- `bootstrap` 在宿主的插件临时环境运行，写入控制器预先创建的系统根子卷。
- 基础安装、更新、包查询、安装、删除和孤儿清理通过 nspawn 执行。
- 在 nspawn 中将整个插件根只读绑定到 `/run/rstarsleep/plugin`，包括脚本、
  辅助二进制和资源；临时写入目录与留档配置使用各自明确的容器路径。
- 系统内操作文件的工作目录为该插件根；插件临时写入目录单独提供。
- 运行插件前检查入口所需工具；bootstrap 得到的根必须能执行后续任务。
- CoW、缓存提交和版本完成标记始终由控制器负责。

用户将插件压缩包或目录放入工作根的 `plugins/` 即可备用。控制器读取每个系统的
`[Distribution] Distribution` 和可选 `Architecture`，按包内 `system_types` 及 `architectures` 自动匹配。
`system_types` 省略时使用插件 `id`；文件名不参与身份匹配。

没有有效匹配时报告缺失；多个有效匹配时报冲突，不按目录顺序任意选择。
损坏或不兼容的插件包跳过，诊断保留在加载结果中；无匹配时一并报告。
需要更换 Arch 插件时，可以替换默认包，或移出默认包后放入自己的压缩包或目录。

全局配置不注册插件路径，系统构建配置仅选择系统类型，例如：

```toml
# Optional workspace override; omission uses the configuration module's default.
# 可选的工作目录覆盖；省略时使用配置模块的默认目录。
# work_dir = "/mnt/rstarsleep-work"
```

```ini
# Editable per-system build configuration.
# 每个系统的当前可编辑构建配置。
[Config]
SchemaVersion=1

[Distribution]
Distribution=arch
Architecture=x86_64

[Content]
Packages=
    linux
    vim
```

`config/build/` 的全部普通 INI 文件按文件名顺序独立解析后叠加，名称和扩展名没有配置职责区别。
`config.conf` 是初始化模板，INI 系统还生成空的 `inherit.conf` 模板；文件名没有解析上的特殊含义。
`Packages=` 支持缩进续行、逗号/空白分隔和重复赋值追加，空值不清空列表；
`#` 注释和空行不打断续行。核心单项 SchemaVersion、Distribution、Architecture 重复时报错。
`config.toml` 仅在没有实际 INI 赋值时回退读取，空继承模板不阻止回退。
有实际赋值时不与 INI 混合，也不掩盖 INI 解析错误。

当前 `packages` 读取接口继续保留；新增完整构建配置读取接口。
软件流程将配置列表规范化为包集合，并保存插件解析后的实际包清单，供维护对比。

## 预设分发与调用

`build.rs` 将 `presets/plugins/arch/` 按稳定顺序打包为 `arch.tar.gz`，固定归档
时间和所有者，保留辅助工具的可执行位。Rust 程序使用 `include_bytes!` 嵌入
生成的包，初始化时原子写出，安装不依赖项目源码目录或外部预设文件。

`ConfigStore::plugins_path()` 定位共享目录，`read_system_type()` 读取类型和
可选架构（省略时用宿主架构，旧 TOML 兼容 `distribution` 写法）。
`controller::plugins::for_system()` 自动匹配，并返回管理临时环境生命周期的
加载结果。当前支持 tar、gzip、xz 和 zstd；解包和操作文件检查不执行脚本。
链接目标也必须为不含 `..` 的相对路径。

`LoadedPlugin::invoke()` 提供检查后的 Bash 任务调用，bootstrap 在宿主运行，
其他任务通过 nspawn 执行，所需工具检查与实际调用采用同一环境。
`LoadedPlugin::invoke_for_build()` 另接收冻结配置和系统标识，传入 `RSTAR_CONFIG_DIR`
和 `RSTAR_SYSTEM_ID`。配置在容器内只读绑定到 `/run/rstarsleep/config`。
插件、临时目录和配置的 bind 源路径保留原生字节；路径含冒号或反斜杠时明确报错，
避免 nspawn 将其误解为绑定选项分隔符。
Arch 预设提供上述包管理操作及挂载配置；构建控制器完成基础、软件、script、inherit 阶段编排和缓存提交。

## 版本与缓存选择

版本使用每个系统独立的递增序号，例如 `v00000001`，在系统构建锁内分配，
不通过目录修改时间推断先后。取得系统构建锁后，固定删除本系统所有历史版本的 script、inherit 缓存，
再完成配置留档；后续只读取冻结留档。`cache::STAGES` 保持阶段顺序，新增 script 后阶段
也须加入以纳入固定清理。缓存删除不影响已发布版本根、配置留档或持久数据。

“最新过往版本”是当前构建开始前序号最大的构建记录，可以包含失败构建。
基础和软件阶段均只检查该版本的缓存，不分别向更早版本搜索，以维持明确的来源关系。
软件维护使用软件缓存来源版本的配置和实际包清单，不使用任意其他成功版本。

缓存沿用现有路径接口：

```text
systems/<系统>/cache/<版本>/base-system
systems/<系统>/cache/<版本>/packages
systems/<系统>/cache/<版本>/script
systems/<系统>/cache/<版本>/inherit
systems/<系统>/versions/<版本>/config/
systems/<系统>/versions/<版本>/root/
```

阶段缓存都是系统根的只读 Btrfs 子卷快照，继承阶段含独立只读目录子卷，发布时显式快照。
没有继承和额外 fstab 行时不创建 inherit 阶段，直接从 script 发布。每个缓存有位于系统根
之外的完成记录，保存阶段、来源版本、插件身份及版本、归档摘要、缓存格式、
架构和实际包清单。
控制器按插件 ID、cache_format、系统类型、架构、受保护目标以及配置摘要判断兼容性。
插件发布版本与归档摘要保留为来源信息；同一 cache_format 下允许发布版本变化。
阶段兼容性摘要仅涵盖插件 `[cache]` 声明选定的生效参数，并包含选择器本身，
因此检测字段的声明变化也会阻止复用旧摘要。完整配置摘要另存，涵盖全部普通配置文件及 script，
规范化 INI 包列表、注释及排版，保留未知设置；旧 TOML 的完整摘要规则保持兼容。
选择创建或维护前，比较当前冻结配置与最新过往配置的阶段字段摘要。
基础字段变化时实际删除本系统所有历史基础及后续缓存根和完成旁记录；仅软件字段变化时
从软件阶段起删除。清理先撤销完成标记，再通过 Btrfs 递归删除子卷；存储删除失败报错退出。
新摘要与旧硬编码摘要不相容，旧阶段缓存会重新建立；配置留档与已发布版本根保留。

阶段旁记录为 `base-system.toml`、`packages.toml`、`script.toml` 与可选 `inherit.toml`，记录所在版本、实际来源版本、
最初内容版本、插件版本和摘要、兼容性身份、完整配置摘要及实际软件清单。
全新安装不记录缓存来源；每次基础维护后的内容版本及插件来源记为本次构建。
版本目录另有 `state.toml`，保存 `building`、`failed` 或 `complete` 状态及失败原因。

只有完成记录与实际快照均有效且兼容的缓存才属于“存在”。缺失或不兼容时进入
首次安装分支；损坏的记录、非子卷替代物和读取错误应报错，不静默作为缓存缺失。

## 按步骤处理缓存

构建按基础系统、软件包、script、可选 inherit、版本根发布的顺序执行，随后发布主系统启动文件。
基础和软件步骤独立判断自身是否有最新过往版本的有效缓存；script、inherit 不读取旧缓存。
每步提交输出供后续使用。

步骤 1：基础系统。

- 无缓存动作：创建本系统独立的可写 `base-root`，执行 bootstrap、基础安装和
  `system_init`，随后始终执行一次基础维护；成功后提交本系统基础缓存。
- 有缓存动作：从此系统自己的基础缓存恢复可写根，始终执行一次基础维护并提交新缓存。
- 是否存在软件缓存不影响基础维护。软件缓存有自己的独立维护流程。

步骤 2：软件包。

- 无缓存动作：从本次基础系统步骤已经提交的缓存恢复可写 `root`，执行首次
  软件安装，不重复系统更新，成功后提交软件缓存。
- 有缓存动作：从软件包步骤自己的过往缓存恢复可写 `root`，执行系统更新、
  软件目标调整和孤儿清理，成功后提交软件缓存。

步骤 3：script。

- 每次从本次已提交的软件缓存创建可写 CoW `script-root`，没有有缓存/无缓存分支，
  从不读取旧 script 缓存或最终版本根。即使 script 目录为空，也提交此步骤输出。
- 按文件名顺序执行冻结配置的 `script/` 中全部条目；目录和压缩包要求根内普通
  `main.sh`，允许与插件相同的单层包装目录。独立 `*.sh` 保留原名并置于空目录。
- 每项资源实际复制到系统根内一个独立可写临时目录，避免被 nspawn 的 `/run`、
  `/tmp` 挂载遮蔽；在系统内部以该目录为 cwd 执行 `bash -- <入口>`，执行结束清理。
- 传入 `RSTAR_SYSTEM_ID`、`RSTAR_DISTRIBUTION_ARCHITECTURE`、`RSTAR_ROOT=/`，冻结配置只读绑定，
  `RSTAR_CONFIG_DIR=/run/rstarsleep/config`。未知文件、特殊条目及越界链接直接报错。
- 执行成功后捕获实际包清单，并提交 `cache/<版本>/script` 只读快照和 `script.toml`。
  脚本失败时不提交 script 缓存、不发布版本，已完成的基础和软件缓存仍可复用。

步骤 4：inherit。

- 没有 CopyOnWrite、Mount 和 FstabLines 时直接从 script 缓存发布版本根。
- 否则从本次 script 缓存创建可写根，继承当前运行系统的数据，并在需要时调用挂载插件接口。
- 构建时继承完成后捕获实际包清单，保存 `inherit` 与 `inherit.toml`，随后递归快照父根和全部内部子卷
  作为版本系统根。继承缓存与发布的父根、CoW 目录子卷均只读。
- 继承失败阻止发布，已成功提交的基础、软件和 script 缓存仍保留；下次构建重新执行 script、inherit。

最终从本次最后完成的阶段创建只读版本根。各步骤始终使用独立工作根；
下一步骤使用已提交快照，不拼接文件树。一个步骤失败便不执行
后续步骤，先前已经提交的步骤缓存继续保留。

`stages.rs` 的主流程按顺序调用 `base_step → packages_step → script_step → inheritance_step`，
继承步骤负责从 script 或 inherit 输出发布版本根。
有无缓存的动作在各步骤内部处理，基础阶段两条路径均维护一次。
script 只消费本次软件输出，inherit 只消费本次 script 输出，两者每次重新执行。

## 当前运行系统的数据继承

控制器以 `InheritanceConfig::read()` 从合并 INI 参数独立读取 `[Inheritance]`，
不把继承作为 mkosi 构建选项或要求插件声明的配置。接受 CopyOnWrite、Mount、Timing 和 DirectorySubvolume：

```ini
# Suggested systems/<system>/config/build/inherit.conf; its name has no parsing semantics.
# 推荐 systems/<系统>/config/build/inherit.conf；文件名无解析语义。
[Inheritance]
CopyOnWrite=/etc/hostname
CopyOnWrite=
    /home/alice
    /var/lib/myapp
Mount=/srv/data
Mount=/etc/myapp.conf
```

重复赋值和多行值均支持，每行一个规范绝对路径。不接受 `/`、重复、相互包含、
控制字符、父级跳转或编码名称冲突；来源不能包含目标管理存储，顶层来源解析为普通文件或目录。
Mount 还不能遮盖 `/rstarsleep`、`/run/rstarsleep`，路径不得包含 nspawn 绑定分隔字符 `:`、`\`。

CopyOnWrite 默认在构建时从当前运行系统继承；`Timing=initramfs` 可延后到目标版本首次启动。
来源与目标必须具有相同 Btrfs UUID。普通文件使用强制 reflink；目录默认创建独立目标子卷，
来源已有子卷时直接递归快照。`DirectorySubvolume=no` 只关闭最外层目标子卷化，内部原有
子卷仍保留。阶段缓存保存、恢复及最终发布均处理完整子卷树，全部封存为只读。
构建时继承的来源不允许尚未解析的内部挂载；initramfs 模式将内部挂载固定为独立来源映射。

initramfs 模式完成软件、脚本及 Mount 配置后，重新生成包含早期执行器的 mkinitcpio 镜像，
保存独立准备根及待办清单，预先发布候选启动项并标记 `pending-inheritance`。首次启动在
正式根挂载前提交最终根，成功后标记 `complete`；不运行插件或重新捕获包清单。
路径不设黑名单，构建前提供 3 秒 Ctrl+C 取消时间；启动内核及 initramfs 保持构建时产物，
兼容性由用户负责。详细协议见 [首次启动继承](initramfs-inheritance.md)。

Mount 的路径把所有 `/` 替换为 `-`，包括开头斜杠，例如 `/home/alice` 对应
`systems/<系统>/persistent/-home-alice`。不存在时从当前运行系统首次复制；存在时直接复用，
即使当前来源已经不存在也不复制。来源可以跨分区、包含内部挂载点或子卷，输出 Warning，
首次复制允许普通复制。目录初始化为可写 Btrfs 子卷，文件为普通文件，不随版本清理删除。
新根保留挂载点的底层内容，确保 `/usr` 等路径仍能提供工具。在挂载插件和包查询期间，
这些持久项目绑定到目标系统；继承 `/etc` 时把最终 fstab 同步到新根。

控制器先确认目标路径存在，再以 nspawn 调用声明 `mount_inheritance = true` 的插件。
参数按四元组传入：`source target filesystem options`。目录使用 `UUID=...`、`btrfs`、
`rw,subvol=...`；文件使用 `/rstarsleep/systems/.../persistent/...`、`none`、
`bind,x-systemd.requires-mounts-for=/rstarsleep`，并在需要时先添加管理子卷挂载。
插件负责按发行版生成挂载配置，无需缓存维护接口。

Arch 的可选自由列表 `[Content] FstabLines` 支持重复赋值和多行值，提供完整 fstab 行，
即使没有 Mount 项也可使用。自动挂载行按 fstab 规则转义，额外行保持原文；两者写入
`# BEGIN RStarSleep inherited mounts` 与 `# END RStarSleep inherited mounts` 间的受管理块。
重复执行替换该块，保留块外用户行。此字段变化不使基础或软件缓存失效。

## 基础系统流程

Arch 的受保护基础包入口为 `base`；内核、桌面和其他系统用途相关
软件写入 `packages`，因此主系统和子系统可以共用流程而不强制安装内核。

每个系统首次安装时独立创建空 Btrfs 子卷，插件获取、校验并解包初始安装文件，
配置软件源，通过 nspawn 初始化包管理器并安装基础目标。不从跨系统共享的已安装根
创建新系统，预下载环境也独立执行 bootstrap 和基础安装。
bootstrap 自带但不属于基础目标的工具应整理为依赖并清理，避免永久混入基础根。

插件必须把下载的初始系统安装文件保存到 `persistent_dirs` 声明的持久目录备用，
包括验证所需的签名等文件；不要只放进调用后清理的临时目录。复用前重新验证安装文件，
区分来源或版本以免错误复用。Arch 的 `arch-bootstrap:state/bootstrap` 保存 rootfs 归档和
签名，以下载及签名 URL 的 SHA-256 索引；同 URL 可复用旧文件，更换 URL 创建新条目。
配置 `Keyring`、`BootstrapUrl`、`BootstrapSignatureUrl`、`Mirror` 通过新环境变量传入。

基础安装后调用 `system_init <machine-id>`。每系统身份保存在
`systems/<系统>/identity.toml`，首次从安全随机源生成，重建仍复用；旧系统可从自身
之前的根接纳有效 ID。Arch 写入 machine-id 并初始化 pacman 本地密钥，SSH 密钥留待服务生成。

初始安装文件可能来自较早时期，版本名称不变也不代表内容无需维护。因此无论新建还是
从此系统的旧基础缓存恢复，都执行一次完整基础维护，成功后才查询清单并提交基础缓存。
Arch 使用 `pacman -Syu`，不采用独立 `-Sy` 的部分升级。后续软件有缓存时仍执行自己的更新。
插件归档哈希变化本身不使兼容的每系统缓存失效。

## 软件首次安装与维护

首次安装从已经完成基础流程的根开始，安装当前 pkg 表中尚未安装的软件，确保
这些软件和受保护基础入口为显式安装，再清理孤儿包，最后保存软件缓存。
这一步不重复执行基础流程已经完成的系统更新。

软件维护从旧软件缓存开始，顺序为：

1. 在 nspawn 中执行一次完整系统更新。
2. 读取缓存来源版本和本次留档的 pkg 表，结合实际包数据库计算增删及安装原因。
3. 处理旧表移除的软件，安装当前表中缺失的软件，并校正显式/依赖安装原因。
4. 在软件集合调整完成后清理孤儿包。
5. 保存软件缓存和对应的实际包清单。

采用以下依赖规则：

- 当前 pkg 表和受保护基础包入口都必须保留为显式安装。
- 从旧表移除的包如果仍为保留目标的必要依赖，应改为依赖安装并保留。
- 真正不再需要的旧目标才删除，不使用忽略依赖的强制删除。
- 当前表所有缺失项都要补装，包括表中未变化、但实际根中缺失的软件。
- 软件更新产生的新显式包若不在当前/受保护目标中，会降为依赖并参与孤儿清理；
  更新前已存在的未管理显式包保留。这使已移除目标的替代包也能按实际依赖处理。
- 删除与安装的具体顺序允许插件为依赖关系和包冲突安排；孤儿清理始终在最后。

因此“删除多余软件”指删除不再被当前目标需要的软件，不能简单等同于对
`旧表 − 新表` 中的每个包直接执行删除命令。

Arch 的安装使用 `pacman -S --needed --asexplicit --noconfirm --ask=4`，由 libalpm
在同一事务中处理已声明的包冲突并检查依赖。位 4 只应答冲突包删除问题，
不允许忽略依赖。所有当前和受保护目标都在提交前再次检查，目标被移除则阶段失败。
孤儿查询使用 `pacman -Qdtq`；
空集合直接跳过删除操作。插件只在包查询成功且确实为空时按空集合处理。

## CoW 提交与失败处理

基础、软件和 script 缓存使用 `btrfs subvolume snapshot -r`，恢复为工作根使用可写快照；
inherit 先创建父根与显式子卷快照，再设置父根只读。所有缓存快照强制采用 CoW，
失败时直接报错。阶段内的每条命令不单独建立缓存。

仅在阶段全部成功后，才在临时位置创建快照并提交完成记录。中断产生的不完整
快照不能作为已完成缓存。基础成功而软件失败时保留基础缓存，供下次构建复用。
已发布版本根不直接修改；失效阶段缓存按构建前清理规则删除。

基础流程始终在独立可写根维护后提交缓存，并记录实际来源。
所需阶段及主系统启动发布全部成功后才将当前版本标记为构建完成；失败记录保留成功阶段的缓存。

当前实现包含版本系统根、持久挂载配置、主系统启动项发布及清理。
运行系统动态维护和子系统运行管理仍待实现；全局持久子卷与 Arch AUR 的构建前准备见后文。

## 验证范围

测试覆盖：CLI 默认/`--all`/单系统选择、默认工作目录与可选覆盖、各步骤的无缓存/有缓存动作、固定基础维护、
失败阶段复用、实际包缺失与依赖保留、插件 API 不兼容及任务失败处理、
归档根及单层包装目录识别、隐藏文件保留、非法归档拒绝、临时会话清理，以及
CoW 失败不退回普通复制。
另覆盖插件配置声明及严格校验、变量原样传递、独立且稳定的机器身份、
独立初始安装、每次基础维护、安装文件保留/验签及缓存清理后的失败重试。
模拟执行层可验证控制流；真实 Btrfs、bootstrap 和 nspawn 链路另做集成验证。

新增验证覆盖目录插件冻结、目录摘要变化及压缩包/目录重复匹配，script 混合输入、
相对资源读写、执行顺序、资源清理、脚本变化/删除的软件缓存复用以及失败重试。
容器参数测试检查内部 cwd、配置只读绑定及原生文件名；模拟后端执行真实 Bash。
继承测试覆盖当前系统来源、强制 CoW、UUID 与内部存储检查、独立子卷发布、
系统 persistent 首次复制与复用、挂载接口及 fstab 转义与重复调用；启动和清理测试
覆盖文件所有权、启动项参数、运行系统及挂载边界保护。真实 Btrfs/nspawn 完整链路仍未验证。

## 可选预下载与全局持久目录

发行版协议保持一个可选操作 `predownload.sh 包名...`。核心不区分官方仓库、AUR
或其他来源。插件以 `predownload = true` 声明能力；省略或设置为 false 时不进入预下载流程。
声明后必须同时提供独立的 `predownload/install.sh` 和 `predownload/update.sh`。
这两个生命周期入口也允许省略 `.sh`，此时要求可执行并直接调用。
根内 `predownload` 名称被资源目录占用，预下载主入口仍使用 `predownload.sh`。
这两个生命周期入口负责预下载环境的初始化和维护配置。
此能力需要 API 3 的系统初始化接口。API 2/3 插件均须提供配置声明；不声明预下载则跳过此能力。

一次 build 先锁定并冻结所有选中系统的配置，再按完整插件 SHA-256、架构、发行版配置及有效环境分组，
将每组配置的原始软件需求与将复用缓存的已安装包清单去重、排序，作为独立参数传入一次预下载操作。
后者覆盖现有根完整更新所需的软件，包括随后才会删除的旧目标。只构建一个系统时
仅包含该系统；全部构建时覆盖本批所有同组系统。插件自行补充基础目标、解析依赖及验证结果。
输入是配置需求，不是把所有目标同时装入一个系统的事务，也不包含用户脚本临时发起的任意下载。
所有组预下载结束后才进入目标系统构建；某组预下载失败阻止该组全部系统，独立组仍可继续。
不限制构建网络，也不把失败的预下载视为可以由安装阶段补齐的成功结果。

预下载环境保存在 `<管理根>/predownload/<完整插件SHA-256>/<架构>/<发行版配置SHA-256>/completed/`：
`root` 是只读系统根子卷，`environment.toml` 保存来源、架构和实际软件清单。
首次创建独立空子卷，执行 bootstrap、基础安装及系统身份初始化，维护基础根后运行 install.sh；
以后从环境缓存创建可写副本，维护基础根并运行 update.sh。环境脚本的 `RSTAR_CONFIG_DIR`
指向插件自己的 `predownload/` 文件夹，不生成或读取目标系统的构建 INI。
其发行版配置变量来自该组已校验的冻结配置，保留同一来源、镜像等参数。
脚本和清单检查成功后保存新的只读环境缓存，再分离一个工作根执行 predownload.sh。
工作根在成功或失败后均销毁；下载失败仍保留已成功维护的环境缓存。
插件任意资源变化产生新环境缓存；目录替换保留旧环境到新环境发布成功，启动时恢复中断状态。

插件可声明全局持久子卷映射：

```toml
predownload = true
persistent_dirs = [
    "packages",                  # 全局直属子卷 → 插件工作目录同名文件夹
    "sources:state/sources",     # 全局直属子卷 → 指定相对目标文件夹
    "repository:state/repo",
]

[tools]
host = []
system = []
predownload = []                 # 仅在环境初始化后检查的额外工具
```

来源只能是 `<管理根>/persistent/` 下的一个直属名称，不能选择孙目录或任意宿主路径。
目录池不按插件分区，可供其他插件复用。系统 Mount 使用各系统自己的 persistent，
与此全局池分离。第一次申请时创建真实子卷。
目标允许多级相对子文件夹，不接受绝对路径、父级跳转、冒号或反斜杠；不允许重复来源、
重叠目标、经过链接、遮盖文件或非空资源目录。可预留空目录作为挂载点。
临时资源副本提供挂载点，冻结插件资源始终不修改。容器操作在
`/run/rstarsleep/plugin/<目标>` 获得可写绑定；宿主 bootstrap 使用等价临时工作目录和指向
事务副本的链接。操作缺少控制器分配的映射时报错，不静默忽略声明。

每次插件调用（包括 bootstrap、初装/维护、预下载和查询）均取得全局持久锁，将所申请子卷
建立可写 CoW 快照后挂入工作环境。脚本非零则回滚全部副本；脚本成功后检查全部副本，再提交。
事务粒度为单次插件调用；环境根缓存提交和持久数据提交是独立事务，不承诺整次 build 回滚。
`persistent-state/transaction/journal.toml` 在所有子卷交换前写入，旧卷保留到整批提交决定持久化。
进程中断后，在下一次申请持久目录前按日志恢复：未提交则恢复原卷，已提交则完成旧卷清理。
锁持续覆盖恢复、快照、插件运行和提交；其他调用遇到占用时明确失败，避免互相覆盖。

快照前检查原卷、提交前检查工作卷的嵌套子卷；发现即报错，不发布任何本次持久修改。
原生检查用 Btrfs 枚举后代，诊断保留路径。失败清理不跟随链接，按最深层优先删除本次
所属工作子卷及插件创建的嵌套子卷。非子卷来源、损坏日志、未知事务条目均报错。
外部直接改动数据不受管理器锁保护；使用中的子卷不应被外部进程移动或挂载覆盖。

Arch 预设 0.5.0 默认启用这项能力，申请 arch-packages、arch-repository、arch-sources
及用于初始安装文件的 arch-bootstrap 四个全局子卷。独立环境安装 git、base-devel、Python 和 sudo，并建立非 root 的 rstar-build 用户。
官方软件用空 local 数据库解析各目标的完整依赖闭包，缓存本批解析结果；将包文件去重后，
以具体包名和版本批量下载，保留各目标的虚拟包提供者。下载阶段不重新合并依赖约束，
允许不同系统的冲突软件共存于缓存；同名不同版本另分批次，避免 pacman 忽略重复包名。
缓存中已有的文件仍执行校验，本轮已成功校验的文件不重复处理。
AUR 每个 pkgbase 的官方构建依赖及二进制运行依赖也按批下载，编译依赖仍按原顺序安装。
完成前逐包校验仓库 SHA-256 和包签名，保存本批仓库元数据与完成清单。
AUR 通过 RPC 获取 pkgbase，更新 Git 配方，解析拆分包及构建/运行依赖；源码下载和签名校验、
makepkg 编译由 rstar-build 执行。构建依赖安装在用后销毁的环境中；当前配方每批重新构建，
保留 Git 和源文件下载缓存，避免错误复用旧 ABI 或 VCS 版本的二进制。
无法解析的提供者、构建依赖循环、版本不满足、缺少产物、校验或构建失败均中断。

AUR 二进制写入共享包缓存，并由 repo-add 发布到标识为 `[rstarsleep]` 的本地仓库，
同组系统使用同一仓库。该本地仓库对程序生成的文件使用 `SigLevel = Optional TrustAll`；
官方仓库保留原有签名策略。当前没有管理本地仓库签名密钥。
所有 Arch 操作经同一个 pacman 包装函数使用共享缓存与本地仓库；目标系统消费准备好的
官方元数据，预下载环境维护仍正常刷新仓库。基础 rootfs 获取不属于软件预下载接口。
真实 Btrfs/nspawn、官方镜像与 AUR 构建链路需在具备相应运行条件的集成环境验证。


## 主系统启动发布

主系统由全局 `main_system` 指定。成功构建主系统时，从该版本 `/boot/vmlinuz-linux` 和
`/boot/initramfs-linux.img` 复制到 `/boot/RStarSleep/<版本>/vmlinuz` 和 `initramfs-linux.img`，
随后原子创建 `loader/entries/RStarSleep-<系统>-<版本>.conf`。
若版本的 `/boot` 中存在 `intel-ucode.img` 或 `amd-ucode.img`，一并复制到该版本的启动目录，
并在启动项中将其 `initrd` 行放在主 initramfs 之前；微码镜像缺失时跳过，存在时必须为普通文件。
通过 `findmnt` 自动读取该版本 Btrfs UUID 和顶层子卷路径，生成 `root=`、`rootflags=`、`ro`。
全局可设置 `boot_dir="/boot"` 和 `kernel_options=["quiet"]`；启动目录应指向 systemd-boot
实际读取的 ESP/XBOOTLDR。额外参数不能覆盖程序生成的根选项。
缺少启动文件、UUID 或启动目标冲突均报错，不标记构建完成。
各版本的 `boot.toml` 保存启动文件所有权，清理不依赖之后的主系统角色变更。

新初始化使用 `@RStarSleep`，已有 `@rstarsleep` 会继续复用。
每次构建将当前可执行程序同步到管理根的 `RStarSleep`，内容一致时不改写。

## 插件宿主子命令

插件可在 `plugin.toml` 顶层声明 `commands=["configure:tools/setup", "install-app"]`。
省略入口时使用同名入口，支持 `.sh` 或无后缀可执行文件；不能覆盖内置子命令，
多个插件也不能声明同一命令。宿主执行插件命令，stdin/stdout/stderr 直接连接终端，
所有参数原样透传（包括 `--root`）。管理根由 `RSTAR_MANAGER_ROOT` 环境变量选择，默认 `/rstarsleep`；
插件获得 `RSTAR_MANAGER_ROOT`、`RSTAR_PLUGIN_ROOT` 和 `RSTAR_PLUGIN_TMP`。
插件子命令必须自行申请 root（例如适当使用 sudo），管理器不自动提权。
这类入口可用于交互式安装软件并写入系统配置；应自行检查输入并传播失败。

## 版本清理

`RStarSleep clean <系统> <版本> [--root <管理根>]` 删除该系统截止版本及之前的版本记录、
系统根、对应阶段缓存、启动目录和启动项，保留当前配置、身份及 persistent。
清理与构建共用独占锁，拒绝删除运行中的系统根或跨越挂载边界，检查失败不会开始删除。
`version-counter` 保留已分配的最高编号，清空历史后也不会复用旧版本号。
删除过程遇到错误会停止，可重试；版本所有权记录随版本保留到对应外部启动文件删除完成。
