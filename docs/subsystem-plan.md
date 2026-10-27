# 子系统运行与应用集成设计

本文保留参照 meowSub 的设计依据，并按当前实现更新实施状态。最初参考
RStarSleep `d3c747e` 和软链接 `meowSub -> ../meowSub/` 指向的 `32923ed`；
对 meowSub 的判断来自源码和测试阅读，未运行其真实容器。实际配置和命令见
[子系统使用说明](subsystem-usage.md)。

当前已实现构建发布、启动前继承收尾、常驻/按需运行、终端、图形会话接入、用户入口
导出和基于 SSH 的子系统管理（CLI、编辑器、TUI、SFTP 与 root shell）。自动化验证覆盖
主要逻辑及部分真实进程/Unix socket、SSH/SFTP 行为；
特权 Btrfs+nspawn 和真实桌面链路仍需在隔离环境验收。

版本切换不依赖运行内容的变更程度，不比较运行根与基线，也不要求确认丢弃修改。
一部分构建任务延后到版本启动时完成：主系统由 initramfs 触发，子系统由宿主 daemon
在 nspawn 引导前触发。已运行机器继续使用当前版本，新版本在下次启动或重启时生效。
子系统直接使用构建模块发布的当前产物，不提供版本选择、固定旧版本或历史回退入口。
版本标识仅用于内部来源记录、待办收尾、故障恢复与清理保护。

## 1. 实施概况

| 项目 | 当前实现 |
| --- | --- |
| 系统与角色 | `RuntimeConfig.subsystems`、重复/主子角色冲突校验；角色撤销后的启停及入口清理 |
| 构建与版本 | `build-current.toml` 原子发布；失败保持上一引用；异常引用拒绝历史兜底 |
| 配置 | 运行字段、默认值和边界校验；授权对新请求生效，启动配置下次启动生效 |
| nspawn | `--boot` 常驻机器、machined 注册、客体执行就绪探针、会话执行 |
| 存储 | 完整子卷树可写运行代、存储准备事务和恢复；独立机器就绪来源记录 |
| 继承 | 共用启动前收尾；子系统构建不安装主系统 initramfs 集成；持久数据运行期 bind |
| 清理 | 当前构建/运行/事务引用保护；旧代回收核对 pending 来源、运行根及挂载命名空间 |
| 发行版能力 | 可选 `query_package_files`；Arch 已实现；构建生成候选索引，运行时再验证文件 |
| 用户集成 | 系统前缀命令/desktop/图标；显式登记用户，降权写入并按内容所有权同步 |
| SSH 管理 | 公钥绑定子系统身份；build/script 共享视图、类 Vim 编辑、TUI、SFTP、持久任务和 root shell |

主要依据：[`runtime.rs`](../src/config/runtime.rs)、
[`layout.md`](../src/config/layout.md)、[`container.rs`](../src/controller/container.rs)、
[`stages.rs`](../src/controller/stages.rs)、[`backend.rs`](../src/controller/backend.rs)、
[`clean.rs`](../src/controller/clean.rs)、[`deferred.rs`](../src/controller/deferred.rs)、
[`首次启动继承说明`](initramfs-inheritance.md)。真实 Btrfs/nspawn 完整链路仍待验证，
因此运行模块的完成标准必须包含真实环境验证。

## 2. meowSub 可借鉴的点

| 能力 | 参考实现 | 本项目采用方式 |
| --- | --- | --- |
| 特权助手 | `internal/daemon/{proto,client,server,install}.go` | 同一 Rust 二进制提供 daemon 子命令；Unix socket 协议；内核对端身份认证 |
| 三种启动来源 | `proto.go`、`occupancy.go` | 区分自启、显式启动、按需启动；只有按需启动实例参与闲置回收 |
| 实例部署 | `internal/runner/{run,deploy_linux}.go` | 借鉴版本来源与独立运行快照的边界；使用本项目版本，不引入 meowSub 的基线及变更比较 |
| 启动切换 | `run.go` 的部署入口 | 改为启动/重启时完成待办构建、生成运行根并切换版本，无差异门禁 |
| 中断恢复 | `deploy_linux.go` 的 journal、`RecoverPending` | 借鉴持久日志；版本收尾复用已有提交恢复，运行根切换单独记录 |
| 终端与执行 | `cmd_shell.go`、`server.go` 的 `bridgeShell` | 普通用户通过 daemon 进入；终端流、退出码、信号和尺寸变化分别处理 |
| 图形直通 | `internal/graphical/graphical.go`、`bootbind.go` | 接入调用者 Wayland/X11、音频、用户总线；请求时检测会话资源更换 |
| 应用索引与入口 | `internal/builder/bake.go`、`daemon/{exports,desktop}.go` | 插件查询包文件，核心生成命令/desktop/图标；维护所有权和期望状态 |
| 子系统管理 | `internal/daemon/guest.go`、`internal/persistence/` | 以 SSH 公钥绑定系统身份，直接编辑声明式构建配置并提交任务；不采用 guest 附加接口 |
| 配置更新 | `server.go` 的 `reloadCfg` | 每请求读取并校验当前配置；区分立即生效项和下次启动项；无效配置报错 |

参考仓库的部署中断测试、会话重登录测试、占用矩阵测试和入口清扫测试
适合转化为本项目的行为验收用例。Go 代码作为协议与行为参考，Rust
实现仍按本项目的小模块、可注入后端和双语接口注释约定组织。

需要调整的具体实现：

- meowSub 全局 `bin/<命令名>` 会让不同组的同名命令争用路径。本项目默认导出
  `<系统>-<命令>`；当前没有短名别名配置，后续若增加需显式唯一。
- `gateRedeploy`、`diffTree` 和 `master` 基线不采用。是否更新运行根由当前构建产物、
  机器生命周期和任务完成状态决定；运行修改不会阻止更新或重新生成运行根。
- meowSub 非交互命令把退出码写进响应文本。本项目协议保留数值退出码，CLI 正确
  返回该值，并区分 stdout/stderr。
- meowSub `native` 模式缺少 `xdg-dbus-proxy` 时回退直连宿主总线。本项目校验依赖，
  缺失时明确失败；以命令行参数传值，不把应用参数拼成 shell 源码。
- meowSub 引导时扫描所有 `/run/user/<uid>`。本项目只接入已授权请求对应的用户会话，
  后续需要其他用户时再追加。
- meowSub 会最小化重建 desktop 条目。本项目按支持范围保留名称翻译、分类、
  `Terminal`、文件参数等信息，并针对 Desktop Entry 规范实现解析和测试。

## 3. 核心设计

### 3.1 构建准备与启动完成共用版本体系

数据流程为：`构建准备 → pending-inheritance → 启动前继承收尾 → complete/root
→ 可写运行根 → nspawn 机器`。没有延后任务的版本从 `complete/root` 开始。

构建成功可以交付完整版本，也可以交付独立准备根和待办清单。正在运行的子系统
继续使用现有运行根；下次冷启动或显式重启直接读取构建模块发布的当前产物引用。
引用在构建成功交付 `complete` 或有效 `pending-inheritance` 产物后原子更新；
构建失败不更新引用，沿用上次成功交付的产物。运行模块不提供版本列表选择、
指定版本启动或历史回退，也不维护独立的版本选择策略。

首次使用没有当前产物时提示先构建；引用不存在、损坏或指向已取消/失败任务时明确
报错，不临时扫描历史版本寻找替代项。阶段缓存不是启动来源。已有历史系统可在
下一次成功构建时建立引用。

`start` 遇到已运行机器只复用；`shell`/应用请求复用现有机器，未运行时走完整启动流程。
`restart` 先退出旧机器，再读取当前产物、完成待办和引导。停止本身不执行构建任务。
新版本切换直接生成对应运行根，不扫描旧根，不因修改多少而暂停或要求确认。

任务时机沿用已有继承行为，明确区分“每次启动执行的准备入口”和“每版本一次的收尾”：

| 任务 | 执行时机 | 与当前代码的关系 |
| --- | --- | --- |
| bootstrap、包安装/更新、现有构建 script | 常规构建阶段 | 保留现有流程，不把包管理任务搬到启动路径 |
| CopyOnWriteBuild | packages 缓存提交后、post_maintenance 及用户脚本前 | 在新建 script-root 中继承，结果随 script 缓存提交 |
| Mount 初次初始化及构建 fstab 生成 | 常规构建阶段 | 现有实现仍在构建时完成，不随本次计划迁移 |
| CoW 来源定位与身份固定、prepared-root/待办发布 | 常规构建阶段 | 复用 `deferred::prepare`；启动时才读取来源内容 |
| 延后 CoW 继承、完整子卷树封存、最终 root 发布 | 当前产物首次用于启动/重启时 | 复用 `finalize_inheritance` 和提交恢复逻辑 |
| 已完成继承的提交记录校验 | 后续启动/重启 | 不重新读取旧来源，不重复继承 |
| 运行根、持久 bind 与启动配置准备 | 每次冷启动/重启 | 产物和启动配置相同时可复用现有运行根 |
| 调用者账户与会话资源 | 每次应用请求 | 核对 UID/GID、固定资源 inode 并修复过期绑定 |

同产物和启动配置的普通重启保留运行根；换产物或挂载/tmpfs/GPU 配置时生成新根。
复用由产物及启动配置摘要决定，授权/导出配置变化不重建根，也不检查运行内容。运行配置需要重建副本时直接重建；必须跨重建保留的数据
通过声明的 CoW 继承或 Mount 管理，不自动保存全部运行修改。

以后若增加其他延后构建任务，应将其作为显式版本待办，定义顺序、结果和幂等提交。
第一版只接入已有 CoW 收尾；不自动把当前构建脚本改为每次开机运行的脚本。

### 3.2 在宿主引导子系统前复用继承收尾

主系统路径是 `initramfs → finalize_inheritance → 挂载正式根`；子系统路径是
`宿主 daemon → finalize_inheritance → 准备运行根 → systemd-nspawn --boot`。
子系统共享数据与提交语义，不需要模拟内核启动，也不经过自己的 initramfs。

`stages::inheritance_step` 已把通用的准备根/待办发布与主系统专属的
早期执行器/mkinitcpio 集成分开。子系统准备不要求 linux、mkinitcpio 或 initramfs，
仍需具备可引导的 systemd。`CopyOnWriteBuild` 在 `packages` 缓存提交后、插件后处理及用户脚本前
继承到 `script-root`，`CopyOnWriteBoot` 在子系统
启动准备阶段完成，两份列表可同时配置，分别处理。后续转为主系统时需重新构建并发布启动产物。

daemon 必须取得待办清单对应文件系统的 `subvolid=5` 顶层视图，验证 UUID 并以
正确路径调用收尾接口，不能把普通 `/rstarsleep` 管理子卷挂载当作顶层根。
复用来源路径/类型/子卷 ID 校验、提交记录、错误文件和状态修复；不重新创建一套
仅供子系统使用的继承状态。已完成版本即使旧来源已删除，也能按提交记录校验启动。

当前来源位置由构建阶段固定，内容在启动前读取。子系统不会因执行发生在宿主而
重新从宿主 `/` 猜测来源，也不自动把旧子系统运行根替换为来源。若希望继承来自
旧子系统，需显式登记旧运行根的实际 Btrfs 来源映射，并纳入待办来源保护。

新代码已提供 `snapshot_recursive`，会保留脚本或继承创建的内部子卷，并分别控制
父/子卷读写属性。直接复用它从完成版本生成可写运行根，避免重复实现子卷树复制。
`DirectorySubvolume=yes/no` 的行为也原样保留。

运行挂载结构从版本冻结配置、已有继承记录和当前运行配置读取；仅对缺少的持久
对象/目标映射增加版本级运行元数据，不用新的 `runtime-source.toml` 替代已有待办
及提交记录，也不依赖会被清理的 `cache/inherit.toml`。

运行数据目录：

```text
systems/<系统>/
├── config/
│   ├── build/                          # 可编辑构建配置 / Editable build configuration
│   ├── script/                         # 可编辑脚本资源 / Editable scripts and resources
│   └── subsystem/
│       ├── config.toml                 # 管理员运行策略 / Administrator runtime policy
│       └── authorized_keys             # 可选 SSH 公钥 / Optional SSH public keys
├── config.lock                         # 编辑提交与冻结互斥 / Edit and freeze exclusion
├── build-current.toml                  # 构建成功发布的当前产物引用
├── versions/<版本>/
│   ├── application-files.tsv           # 可选，包文件候选索引
│   ├── prepared-root/                  # 延后任务的独立构建产物
│   ├── inheritance-pending.toml         # 已有待办清单
│   ├── inheritance-commit.toml          # 已有最终根提交记录
│   └── root/                           # 收尾完成后发布的可写版本根
├── runtime/
│   ├── activity.lock                   # 应用和启停活动锁 / Application and lifecycle activity lock
│   ├── current.toml                    # 已同步存储代及启动配置摘要
│   ├── transaction.toml                # 未完成的存储准备
│   ├── fstab                           # 注入客体的独立只读配置
│   ├── exports-users.toml              # 主动集成入口的用户
│   ├── jobs/                           # 管理任务状态、锁和日志 / Management jobs, locks and logs
│   ├── console/                        # 原始 PTY 日志及启动记录 / Raw PTY log and boot records
│   └── generations/<代>/root/          # 可写运行根；不建立比较基线
└── persistent/
```

运行存储事务独立于版本继承提交记录。候选运行根完整同步后提交 `runtime/current.toml`，
这个记录只表示存储已准备，不声称机器已运行。machined 注册和实际客体执行探针通过后，
才在 `/run/rstarsleep/<管理根摘要>/<系统>.origin.toml` 提交机器就绪来源。
daemon 恢复核对 leader、实际根 dev/inode 与来源记录，并检查残留应用服务。
旧运行代保留到新机器就绪后再回收；仍被挂载或待办来源引用的根继续保留。

收尾失败时不发布半成品根、不引导该产物，保留待办及错误，修复后重试或重新构建。
不自动降级到历史产物；已准备的新根可在修复后重试引导。
版本失败与机器引导失败分别记录；机器引导失败不把已完成版本改回待继承。

### 3.3 持久挂载、锁与清理

运行期 bind 合并版本的 Mount 与当前运行配置，对重复/相互包含目标及控制 socket、
运行时目录等保留位置统一检查，文件 bind 也需覆盖。继承结果属于完成版本及运行
副本，Mount 数据继续归 `persistent/`。

以独立只读 fstab 覆盖移除主系统用的管理器挂载块，由 nspawn 注入 bind；
保留支持的 tmpfs/bind 条目，其他宿主依赖条目明确报错。共享 `/etc` 和持久 fstab 不改写。
容器不读取整个管理根，也不注入 guest 管理 socket 或管理器程序。SSH/SFTP 在宿主
daemon 内服务，只向已认证系统开放 build/script 配置视图。

不再使用每系统 `build.lock`。finalize/clean 保留管理器级继承事务锁及来源引用检查。
daemon 内存 map 只跟踪 SSH 提交的当前构建，供 SSH 状态显示及防止重复提交，
不用于主系统构建或提供跨进程互斥。SSH 断开不清除登记，任务结束或启动失败时清除；
daemon 重启后不从历史任务恢复此表。
Per-system build lock files are no longer used; finalize/clean retain inheritance transaction protection.
The daemon's in-memory map tracks SSH submissions for status and duplicate checks, without cross-process exclusion.
Registrations survive SSH disconnects, end with the task, and are not restored from history after daemon restart.

`clean` 沿用现有待办来源保护，再增加当前构建产物、当前运行版本、切换中的产物及
恢复所需旧代引用。当前产物指向的版本在被新构建替换前不能删除。
引用记录损坏时报错，实际机器发现补充检查 daemon 停止时的运行状态。删除运行代前
还需检查容器命名空间挂载和后代子卷；不能因不比较变更而跳过这些结构检查。

应用租约和启停保留 activity 锁。配置编辑提交、SFTP 上传与构建输入冻结通过独立的
`config.lock` 互斥；活动上传结束后才能冻结构建输入，后续编辑不改变本次冻结配置。
CoW 来源也要有稳定读取时机，停止旧机器不会自动停止其他写入者。
全局插件下载缓存继续使用现有持久事务，不作为应用卷。

退出子系统名单时阻止新会话，后台协调停止机器并撤销未修改的自有入口，
保留版本和持久数据。默认拒绝同一系统同时承担主/子系统角色，后续双角色需明确
身份与共享数据语义。

### 3.4 daemon、身份与生命周期

同一二进制提供 `daemon run` 和幂等 `daemon install`，正式服务使用稳定安装路径。
管理根选择在 daemon 启动时固定；客户端 `--root` 必须匹配该服务，不能让普通
请求指定任意宿主路径。服务和 socket 名以规范管理根摘要区分。

Unix socket 使用版本化结构协议；连接层用 `SO_PEERCRED` 读取 UID/GID/PID，
获取失败直接拒绝，客户端不能提交可信身份。管理动作仅管理员执行；普通用户
按每系统访问规则执行应用。配置、部署日志和特权程序要求宿主管理员控制。

SSH 是同一 daemon 的公钥管理入口，以登录用户名选择已登记子系统，并使用该系统
`config/subsystem/authorized_keys` 验证公钥。每次认证重载角色及公钥；撤销公钥不
主动关闭既有会话。SSH 管理权限包含该系统的构建、启停和 root shell，与本地
`allowed_uids` 的调用者应用授权分别配置。主机身份和监听配置位于管理根，不向 SFTP 开放。

第一版每个系统一个机器，名字从管理根身份和系统标识确定，长度与冲突规则固定。
保留现有每系统 machine-id。用户进入前准备匹配 UID/GID 的容器账户和明确 home；
不自动共享宿主 home，不开放普通用户切换为任意 UID/root。容器权限也依赖可写根
的信任边界，不能把运行用户检查当成对容器 root 的隔离保证。

CLI 状态显示 running/stopped、机器名、启动来源和租约数；准备/引导错误由响应和
journal 记录。启动成功需通过 machined 注册和容器内实际执行探针。失败回收未成功
启动的资源；停止先优雅关机，超时后 terminate；重启明确等待旧机器退出。
不同系统的后台自启/恢复并行协调，同一系统的生命周期由独立槽位串行处理。

按需启动采用会话/应用 lease；显式启动和自启不随 lease 归零停止。加入短暂闲置
宽限期，避免连续打开应用反复冷启动。GUI 应用可能派生或复用已有进程，不能只
跟踪启动器 PID；使用容器内应用 scope/cgroup 或用户会话存活来确认实际占用。

### 3.5 会话和应用入口

图形接入已支持 `off`、默认 `isolated` 和 `native`：isolated 使用容器私有 D-Bus，
native 通过降权代理只允许 IBus/Fcitx，不开放宿主 portal。需要输入法的系统显式配置
`native`。`rw` 整个 runtime 目录直挂留到兼容扩展阶段。

仅透传允许列表中的调用者环境，如 `XDG_RUNTIME_DIR`、`WAYLAND_DISPLAY`、
`DISPLAY`、`DBUS_SESSION_BUS_ADDRESS` 和按需的音频/认证变量。宿主路径必须
与已认证用户会话对应；X11 同时处理认证，GPU 按需配置 `/dev/dri`。
会话重登录时检查宿主 dev/inode 和容器可见性，修复被 logind 遮蔽或失效的 bind。
共享 socket 带来相应宿主服务访问权限，`ro` bind 不代表 IPC 只读。

导出支持显式可执行路径、desktop 路径及按包自动发现。插件顶层可选
`query_package_files=true` 要求同名脚本，默认未声明时禁用，API 2/3 均兼容此扩展。
Arch 用 `pacman -Ql` 返回包名和绝对文件路径的 TSV；构建核心校验并保存候选索引，
查询失败阻止当前产物发布。`query_packages` 继续只负责包名/安装原因。

核心负责文件分类、Desktop Entry 解析、图标解析和转接，不直接读 pacman 数据库。
从当前运行产物生成入口；未运行时使用当前构建产物的入口索引，点击后走
统一启动准备流程。待继承版本在收尾前不要求最终 root 已存在；准备索引只作提示，
运行时以最终版本中的实际入口为准。版本完成/切换及角色变化后做幂等同步。
desktop、图标和命令均采用系统命名空间；短名别名属于后续扩展。

入口采用用户主动执行 `sync-exports` 的方式，写入用户 `~/.local/bin` 和 XDG 数据
目录，不要求宿主为本项目主系统，也不默认写 `/usr/share`。文件操作在永久降权后执行，
按路径及内容摘要维护所有权，仅更新/撤销仍属于管理器的文件，保留外部、用户修改或
符号链接入口。产物/运行态切换刷新已登记用户，角色/UID 撤销时撤销自有未修改入口。

Desktop Entry 的 `Exec` 按规范展开为 argv，支持的字段明确测试；未支持项报诊断，
不把整行交给 shell。处理文件 URL 时检查容器内可达路径：第一版要求显式 home/
工作目录 bind，不自动挂载任意宿主文件。当前 file URL 仅支持绝对未编码路径；
portal 文件转交后续单独实现。

## 4. 配置与命令

延用全局 TOML 和每系统 `config/subsystem/config.toml`。运行配置以当前文件为准，
每次启动记录摘要；构建留档中的同名文件只作历史来源，不锁定之后的运行策略。
新增字段有默认值，现有仅含 `schema_version=1` 的模板继续有效。

```toml
# Global configuration / 全局配置
schema_version = 1
main_system = "desktop"
subsystems = ["apps", "dev"]
```

```toml
# Per-system runtime configuration / 每系统运行配置
schema_version = 1
autostart = false
idle_timeout_seconds = 30
session_mode = "isolated"
allowed_uids = [1000]
# Execution uses the authenticated caller; no implicit host home sharing.
# 以认证调用者身份执行，不隐式共享宿主 home。
session_user = "caller"

[[mounts]]
source = "/home/alice/Projects"
target = "/home/alice/Projects"
read_only = false

[exports]
packages = ["firefox"]
commands = ["/usr/bin/rg"]
desktop_files = []
```

默认仅管理员可用，显式登记允许 UID 后向普通用户开放。挂载、tmpfs 和 GPU 在下次
启动生效；授权立即应用到新请求，autostart 由后台协调应用；入口配置通过显式同步刷新。
初期网络沿用 nspawn 宿主网络模式，记录其共享边界；私有网络后续扩展。
孤立图形模式的 D-Bus、原生模式的代理及容器启动依赖要在操作前检查。

宿主本地操作使用 `subsystem` 命令组：

```text
RStarSleep daemon install|run
RStarSleep subsystem list|status
RStarSleep subsystem start|restart <系统>
RStarSleep subsystem stop <系统>
RStarSleep subsystem shell <系统>
RStarSleep subsystem exec <系统> -- <程序> [参数...]
RStarSleep subsystem open <系统> <应用或命令> [参数...]
RStarSleep subsystem run-desktop <系统> <desktop路径> [参数...]
RStarSleep subsystem sync-exports [<系统>]
```

相关命令接受 `--root`（应用 `--` 之后不再解析）；`subsystem`、`daemon`
加入内置命令及插件命令冲突校验。`guest` 子命令及专属接口已经移除。
启动统一读取当前构建产物并完成运行根
准备，不提供指定版本、历史回退、独立 deploy/diff 或丢弃修改确认参数；重启不删除 persistent。

管理根的可选 `config/ssh.toml` 启用 SSH，最小配置为 `schema_version = 1`；默认
`listen = "127.0.0.1:2222"`、`host_key = "config/ssh_host_ed25519_key"`。
相对密钥路径基于管理根，首次启动生成缺失的 Ed25519 私钥，之后复用。
远程监听可设置 `0.0.0.0:2222`，客户端连接宿主实际地址，例如：

```sh
ssh -p 2222 apps@host.example
ssh -p 2222 apps@host.example 'status'
ssh -t -p 2222 apps@host.example shell
sftp -P 2222 apps@host.example
```

交互 SSH 默认进入类 shell 管理 CLI，支持历史、补全、文件导航、内置类 Vim 编辑器、
配置检查、构建、任务、状态、启停和日志；输入 `tui` 打开全屏快捷键面板。
CLI 参数按字面解析，不执行宿主 shell，不支持管道、重定向和展开。
CLI／编辑器／SFTP 共用仅含 `/build`、`/script` 的文件视图，禁止访问 subsystem 配置、
其他系统及宿主路径。写入暂存到成功关闭时原子发布，保存冲突报错。

`shell` 进入已认证子系统 root 账户配置的登录 shell，关闭桌面会话继承并桥接终端流、
窗口变化及退出状态。宿主本地 `subsystem shell` 则按调用者 UID/GID 执行 `/bin/sh -l`。
两种入口分别服务远程管理和本地调用者会话。

保存配置后显式 `check`、`build`，成功后 `restart` 消费新产物；任务状态和日志持久保存于
`runtime/jobs`，原始机器控制台及启动／关机偏移保存在 `runtime/console`。
SSH 断连不取消已提交任务，重新登录可继续查询；取消日志订阅也不会取消任务。
完整配置、编辑器和命令说明见[子系统使用说明](subsystem-usage.md)。

## 5. 实施顺序与验收

P0–P6 的代码均已落地；下表保留功能验收目标。自动化已覆盖主要逻辑和部分真实
进程/Unix socket、SSH/SFTP 行为，真实特权和图形验收仍未完成。

| 阶段 | 交付内容 | 完成标准 |
| --- | --- | --- |
| P0：构建/启动契约 | 角色/运行配置；当前构建产物引用及原子发布；通用待办准备与 initramfs 集成解耦；复用递归快照与锁接口 | 成功构建更新引用、失败构建不更新；引用异常明确报错；子系统准备不依赖内核/initramfs；来源及清单固定正确 |
| P1：启动前构建收尾 | daemon 前置收尾接口；待办来源保护；提交恢复；可写运行根和切换事务；清理运行引用 | 启动读取最新来源内容；同版本重启不重复继承；修改不阻止切换；运行修改不回写版本；故障不启动半成品；引用不可 clean |
| P2：常驻运行与执行 | daemon 安装/协议/鉴权；启动停止重启；状态/日志；非交互 `exec` | 普通授权用户正确 UID/GID 执行；实际退出码和双流传递；超时/异常启动可恢复；重启 daemon 能重建机器状态 |
| P3：终端与按需生命周期 | PTY shell；窗口/信号处理；自启、显式、按需三来源；lease 与回收 | 两个并发会话复用一次启动；一个会话退出不误停；后台任务受跟踪；显式实例不闲置回收；断连能收尾 |
| P4：图形与用户会话 | Wayland/X11/音频；isolated/native；登录后接入与重新登录修复 | 真实 GUI 出窗、有声音、文件路径可达；native 输入法可用；重登录后恢复；不带入其他用户会话；缺依赖明确失败 |
| P5：应用集成 | 显式入口、插件包文件查询、desktop/图标、所有权同步 | 系统前缀避免同名冲突；只读宿主可用户级集成；重复同步不改写；移出角色只撤销自有未修改入口 |
| P6：SSH 子系统管理 | 公钥认证；CLI／编辑器／TUI；build/script 的 SFTP 视图；持久构建及启停任务；root shell 和控制台日志 | 身份绑定单个子系统；配置视图统一且不开放 subsystem；保存和冻结互斥；断连保留后台任务；构建失败不影响当前机器；成功后显式重启生效 |

P6 已改用 SSH 配置管理，移除原 guest install/rebuild/restart/add-copy/add-mount/migrate
及其专属 socket、受控包请求、捕获、持久迁移和热绑定机制。
软件包直接编辑 build 中的声明式 Packages，系统修改通过构建脚本及资源保存，
继承和持久化继续使用现有 `[Inheritance]` 配置及来源语义。
任务成功发布完整或待启动完成的当前产物，通过管理 CLI 的 `restart` 走同一启动完成
流程，不加入运行修改确认。

当前 P0–P6 覆盖本轮功能目标，真实环境尚需验证完整闭环。同一系统并行多个实例、
私有网络、整目录 `rw` 会话、portal、短名别名及持久数据快照/回滚留作后续扩展。

## 6. 代码组织与验证方式

实现位于 `src/subsystem/`：config/model、preparation、commands/identity、
protocol/client/terminal、execution、session、desktop、exports、admin、console、
management、ssh、daemon 和共享 filesystem/trust。复杂 daemon/session/management/ssh
模块按职责拆为子目录；CLI 位于
`src/cli/subsystem.rs`。复用 `controller::deferred` 收尾和递归快照接口；
没有为子系统另建一套版本继承状态。

验证分三层：配置/产物引用/协议/desktop 的纯逻辑测试；可注入后端的收尾与切换故障、
并发启停、身份拒绝和入口所有权测试；隔离 VM 中真实 Btrfs+nspawn/systemd 的
完整启动、终端、持久文件/目录 bind、重登录和清理测试。
至少演练“构建准备 v1 → 更新来源内容 → 启动完成 v1 → 修改运行根 → 构建准备 v2
→ 重启完成并切换 v2（不扫描或确认修改）→ persistent 保留 → 同版本重启不重复继承
→ 构建 v3 失败且当前产物仍为 v2 → 待办来源、当前产物及运行根清理保护”的完整路径。已有
[`deferred 测试`](../tests/build_workflow/deferred.rs) 可作为首次读取、幂等、失败重试、
提交状态修复与来源清理保护的基础，再覆盖子系统入口与并发启停。

自动化还覆盖真实双流、PTY、信号、数值退出状态、固定 socket inode 和所有权协调。
[`管理工作区及终端测试`](../src/subsystem/management/tests.rs) 覆盖视图边界、事务上传、
保存冲突、UTF-8 编辑与 TUI 返回；[`SSH/SFTP 测试`](../src/subsystem/ssh/tests.rs)
覆盖真实回环握手、公钥认证与撤销、管理命令和二进制上传。
测试结果以实际命令输出为准，不在此维护易过期的数量。

剩余环境验收包括真实 Btrfs/nspawn 启停、daemon 中断恢复、并发/长寿命图形应用的
闲置回收、音频、native 输入法、重登录、声明式持久挂载，以及 SSH root shell、
配置编辑、重建重启和断连后任务查询闭环。这些特权验收需要隔离环境完成。
无需安排基线、运行变更程度扫描、修改确认或版本选择/历史回退的实现工作。
