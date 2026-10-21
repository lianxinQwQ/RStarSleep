# 首次启动时继承文件

`CopyOnWriteBuild` 定义构建时继承的路径列表，`CopyOnWriteBoot` 定义目标版本首次启动时
继承的路径列表。两份列表可同时存在，各自处理，不使用全局时机开关。
`Mount` 的首次初始化及 fstab 生成始终在构建阶段完成。

```ini
[Inheritance]
DirectorySubvolume=yes
NocowHandling=error
CopyOnWriteBuild=/etc/hostname
CopyOnWriteBoot=/home/alice/
CopyOnWriteBoot=/var/lib/myapp/
```

两份列表均默认为空，支持重复赋值和多行值；空列表不触发对应阶段的继承。
`DirectorySubvolume` 同时作用于两份列表，只接受 `yes`、`no`，默认 `yes`，
为单次赋值，不支持通过重复赋值覆盖。两份列表与 `Mount` 统一拒绝路径重复、
父子路径重叠及编码名称冲突。

旧配置迁移：删除 `Timing`，将原 `CopyOnWrite` 按用途改为 `CopyOnWriteBuild`
（原 `Timing=build` 或未设置时机）或 `CopyOnWriteBoot`（原 `Timing=initramfs`）。
也可以拆分原列表，分别填入两项。旧的 `Timing` 和 `CopyOnWrite` 会报错并提示迁移。

继承路径必须以 `/` 开头；目录必须以 `/` 结尾，普通文件不能以 `/` 结尾。末尾 `/`
声明路径类型，构建准备清单和首次启动时均核验，内部挂载的目录映射也保留此标记。
`Mount` 首次初始化时核验来源，复用时核验持久对象，不要求当前来源仍存在。

继承不设置 `/boot`、`/usr`、内核模块或包数据库等路径黑名单。CLI 在构建开始前
展示继承路径及策略，给出 3 秒 Ctrl+C 取消时间，之后才同步管理器、分配版本或清理缓存。
继承内容和系统兼容性由用户负责。规范绝对路径、冲突检测、来源类型、同一 Btrfs UUID
以及防止来源包含当前目标存储等机械检查仍然适用；不支持把 `/` 作为继承项目。
来源符号链接解析到实际文件或目录；目标祖先链接按目标系统根解析，绝对链接不会指向宿主。

## 目录和子卷

`DirectorySubvolume=yes` 时，来源已经是 Btrfs 子卷便直接递归快照，不逐文件复制。
普通目录在目标对应位置创建子卷，再按 `NocowHandling` 填充；默认不会改变实际来源目录。
未来从生成的子卷继承，才能直接快照。`move-snapshot` 显式授权转换实际来源，见下文。

`DirectorySubvolume=no` 时，最外层目标目录保持普通目录。来源原有内部子卷仍逐个保留。
默认普通目录复制使用 GNU cp 的 `--archive --reflink=always --one-file-system`，Btrfs 内部子卷
的内容由显式递归快照补齐。策略之间不会自动降级。

基础、软件、script、inherit 的缓存保存、缓存恢复和最终版本发布统一处理完整子卷树，
包括构建脚本创建的子卷。缓存先从外到内建立可写快照，再从内到外封存只读属性。
父子卷只读不会自动影响子卷；只读缓存会逐个校验。工作根及最终版本根的全部子卷保持可写。
临时根和版本清理均递归删除子卷。

## NOCOW 处理策略

`NocowHandling` 是同时作用于两份 CoW 列表的单次赋值枚举，默认 `error`。
它随构建配置、缓存输入及启动待办固定；编辑当前配置不会改变已生成待办的策略。
旧待办没有该字段时按 `error` 处理。它不改变 `Mount` 的初始化语义。

| 值 | 行为 | `DirectorySubvolume=no` |
| --- | --- | --- |
| `error` | 保持原有强制 reflink；无法执行即失败 | 支持 |
| `reflink` | 在空目标 inode 上匹配来源的 CoW/NOCOW 属性，再强制 reflink；失败即退出 | 支持 |
| `copy` | NOCOW 普通文件明确完整复制并保留 NOCOW 属性，其他普通文件仍强制 reflink | 支持 |
| `move-snapshot` | 将含 NOCOW 文件的实际普通来源目录转换为子卷，再递归快照继承 | 配置错误 |

来源已经是子卷且开启目录子卷化时，四种策略均直接递归快照。NOCOW 文件本身不阻止子卷快照；
快照共享数据后，文件的后续写入仍可能发生 CoW。

例如，要自动处理离线旧版本中普通 `/var` 目录里的 journald NOCOW 文件：

```ini
[Inheritance]
DirectorySubvolume=yes
NocowHandling=move-snapshot
CopyOnWriteBoot=/var/
```

迁移处理的是清单记录的**实际来源**，不能用当前运行系统的 `/var` 推断旧版本的布局。
选择 `move-snapshot` 即授权改变来源子卷布局，并授权跨子卷迁移可能发生的完整复制及额外空间占用。
实现先复制到临时子卷并校验内容、权限、所有者、修改时间、xattr、硬链接、NOCOW 及内部子卷布局，
再递归快照、恢复内部子卷只读属性，将快照与原目录原子交换。原目录完整备份保留到提交同步后才删除；
这样达到创建子卷、迁入内容、以快照恢复原路径的结果，并保留中断前的完整来源。

来源须离线并停止写入；程序检查挂载命名空间、进程句柄和内存映射，无法读取这些状态时保守退出。
该检查不能阻止之后外部程序重新打开来源，用户须在迁移期间保持停写。
当前仍使用的来源子卷、有其他待办重叠引用、目录外硬链接、不可变/追加属性及不支持的特殊文件均拒绝迁移。
单独配置的 NOCOW 文件无法采用此策略，程序报错，不会自动扩大到其父目录。

迁移记录位于管理根 `inheritance-migrations/`，临时数据位于来源旁的 `.rstarsleep-migration-*`。
记录绑定文件系统 UUID、来源路径、旧目录及新子卷身份和启动清单摘要；重试不会改写原待办摘要。
未提交迁移会保护来源免于清理。中断后使用同一待办重试；来源或临时对象身份不匹配时停止并保留现场，
需在离线恢复环境核对记录、原路径及临时备份后手动恢复，不能直接删除未知临时数据。
若不希望自动修改来源，可选 `reflink` 或 `copy`；也可保持 `error`，离线手动将实际来源 `/var`
迁成独立子卷后重新构建待办，以固定新的子卷身份。

## 构建和启动流程

构建前先冻结并检查配置。主系统必须满足插件的通用启动声明；主系统的
`CopyOnWriteBoot` 非空时，另检查启动时 CoW 声明和集成动作入口。
声明的包与当前系统在需求收集时得到的安装计划比较，软件组、基础系统包和运行依赖均可满足要求。
计划缺项即退出，不自动补包；失败不分配目标版本、清理旧目标缓存或预下载目标软件。
独立解析环境初始化可能下载自己的基础包和工具，查询会刷新仓库元数据并获取 AUR 配方。
子系统跳过这两组启动要求及镜像集成动作。

构建先完成软件安装/维护并提交 `packages` 缓存，再从该缓存创建可写 `script-root`，
应用 `CopyOnWriteBuild`，然后执行 `post_maintenance` 和用户脚本，成功后提交 `script` 缓存。
随后完成 Mount 配置。`CopyOnWriteBoot` 非空时，
主系统在可写工作根中安装早期执行器，再由插件重新生成 initramfs；子系统跳过镜像集成。
随后独立保存包含构建继承结果的只读 `versions/<版本>/prepared-root`。
软件阶段清单记录的是继承之前的构建结果；启动时不会运行包管理器或插件重新查询继承后的包数据库。

版本目录保存 `inheritance-pending.toml`，只登记 `CopyOnWriteBoot` 来源，固定其实际 Btrfs 顶层路径、
包含它的子卷 ID、来源类型、逻辑目标路径和准备根子卷 ID。内部挂载作为单独来源映射记录，必须同属目标
Btrfs 文件系统；来源的挂载布局在构建时固定，数据内容在首次启动执行时读取。
不同文件系统的数据无法直接快照或 reflink，应使用其他数据迁移方式。

构建主系统时预先发布候选启动项，状态为 `pending-inheritance`。启动文件从准备根取出，
但 `rootflags` 指向尚待生成的最终 `root`，根挂载参数为 `rw`，启动项同时携带：

```text
rstarsleep.storage=UUID=<Btrfs 文件系统 UUID>
rstarsleep.pending=/<Btrfs 顶层内的版本目录>/inheritance-pending.toml
```

第一次选择该版本时，initramfs 在设备发现及必要解密之后、正式根挂载之前，以
`rw,subvolid=5` 挂载管理存储，校验来源身份，从准备根生成 `inheritance-staging`，完成启动列表
继承后发布可写 `root`，保留准备根中的构建继承结果，最后进入正常根挂载和 switch_root。
之后每次启动校验清单摘要及根子卷身份，
允许修改根内文件及子卷布局，不会再读取来源数据或重新继承。即使旧来源已被清理，完成版本仍可启动；待办清单需保留。

子系统由宿主在首次使用该产物的冷启动或 `restart` 准备阶段执行同一继承收尾，
完成后才创建运行根并启动 nspawn。运行中的子系统不因新构建立即切换产物。

内核和 initramfs 固定为构建时启动产物。继承 `/boot`、内核模块、`/usr` 或其他启动相关
内容后，用户须确保最终根仍与这些产物兼容；程序不会阻止继承，也不会在早期启动过程中
重新编译 initramfs 或替换已经加载的内核。

## Arch initramfs 集成

Arch 插件的 `prepare_inheritance_initramfs` 动作调用 mkinitcpio，并拥有其配置、hook
和 systemd 接线资源。核心只打包通用早期执行器及私有宿主动态依赖，不直接调用 mkinitcpio。
启动产物路径由插件 `[boot] artifacts` 声明，Arch 使用 `/boot/vmlinuz-linux` 和
`/boot/initramfs-linux.img`。`[boot] packages` 要求配置 `linux`、`mkinitcpio`；
`[initramfs_inheritance] packages` 额外要求 `btrfs-progs`、`coreutils`、`util-linux`、
`bash`、`findutils`。这些包已由软件组、`base` 或其他包的运行依赖带入时，无需重复写入配置。
`[package_plan] base_packages=["base"]` 声明初始系统目标；`collect_packages` 查询返回
展开后的具体包清单，只包含当前系统的目标及运行依赖，不包含临时 AUR 编译依赖。
宿主需要 `ldd` 和 systemd-nspawn。目标工具按动作元数据由统一 runner 检查，实际打包
或镜像生成失败会阻止候选发布；错误保留 stdout、stderr 和退出状态。

使用独立的 mkinitcpio 配置读取用户配置及 drop-in，在 `filesystems` 前插入 `rstarsleep`，
不改变用户原配置。用户的设备发现及解密 hook 应在 `filesystems` 前，配置缺少该 hook 时失败。
传统 shell initramfs 使用 `run_hook`；systemd initramfs 为 `sysroot.mount` 安装强制前置依赖。
继承失败时进入恢复环境，不继续启动半成品系统。

早期执行器及宿主动态依赖保存于私有目录，通过私有动态加载器运行；Btrfs、GNU cp 等工具
使用目标系统依赖。GNU cp 放在私有工具目录，避免与 BusyBox applet 冲突。

## 提交、恢复和清理

最终根发布前，版本目录写入 `inheritance-commit.toml`，包含待办清单摘要、暂存根子卷 ID
及全部内部子卷的路径和 ID。提交记录位于系统根外，不占用用户继承文件的路径。
保持根及内部子卷可写并同步文件系统后，将暂存根改名为最终根，同步目录及文件系统，再原子更新
`state.toml` 为 `complete`。这些步骤并非单个 Btrfs 原子事务，恢复依据提交记录判断进度。

- 最终根尚不存在：递归清理旧暂存根，从独立准备根重试启动列表；保留构建列表的既有结果，不重新读取其来源。
- 最终根已存在、状态更新中断：验证摘要、根及全部子卷身份和可写属性，补齐完成状态。
- 版本已完成：验证摘要及根子卷身份，保留运行时对文件和内部子卷的修改。
- 来源缺失、身份改变或提交记录不匹配：停止启动，保留待办；复制阶段的错误写入
  `inheritance-error.txt`。恢复 shell 可使用同一 `finalize-inheritance` 命令重试，或重启选择旧版本。

继承事务、待办来源登记及清理共用管理器级锁。保留的待办版本会保护其来源版本不被
`clean` 删除；清理包含待办自身时，先持久化取消状态，再撤销启动项及删除产物。
后续构建可以清理 script 缓存，独立准备根仍保留。关机或启动断电不会发布部分继承结果，
但异常断电后的来源内容只具有文件系统恢复后的状态，不能额外保证应用事务完整性。

## 验证范围

测试覆盖首次启动读取最新来源、完成后不重复继承、缓存清理后的准备根存续、失败重试、
提交后状态修复、身份不匹配拒绝、来源版本清理保护、目录选项、嵌套子卷、Ctrl+C 取消，
NOCOW 混合属性及硬链接、真实 Btrfs 目录转换和提交前后中断恢复，
以及 mkinitcpio hook 顺序、早期执行器失败传播、systemd 根挂载依赖和私有加载器。
真实 Btrfs/nspawn 完整构建与真实机器或 VM 首次启动仍需集成验证。
