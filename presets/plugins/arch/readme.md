# Arch Linux 预设插件

此目录在 Cargo 构建时打包并嵌入程序，`init` 将其写入管理目录的
`plugins/arch.tar.gz`。已有同名普通文件保持不变，升级到 0.13.0 需主动替换旧插件包。
插件 API 为 3，元数据 schema 为 1，当前支持官方 Arch 的 `x86_64`。

系统通过 `[Distribution] Distribution=arch` 自动选择插件，例如：

```ini
[Distribution]
Distribution=arch
Architecture=x86_64
# Optional overrides; omit these to use the defaults.
# 可选覆盖；省略时使用默认值。
# BootstrapUrl=https://geo.mirror.pkgbuild.com/iso/latest/archlinux-bootstrap-x86_64.tar.zst
# BootstrapSignatureUrl=https://geo.mirror.pkgbuild.com/iso/latest/archlinux-bootstrap-x86_64.tar.zst.sig
# Keyring=/usr/share/pacman/keyrings/archlinux.gpg
# Mirror=https://geo.mirror.pkgbuild.com/$repo/os/$arch
# AurKeyserver=hkps://keyserver.ubuntu.com
# AurDevel=true
# AurRebuild=none
```

元数据必须显式声明全部统一参数，以及基础和软件缓存检测字段：

```toml
[config]
required = []
optional = [
    "RSTAR_CONFIG_SCHEMAVERSION", "RSTAR_DISTRIBUTION_DISTRIBUTION", "RSTAR_CONTENT_PACKAGES",
    "RSTAR_DISTRIBUTION_ARCHITECTURE", "RSTAR_DISTRIBUTION_BOOTSTRAPURL",
    "RSTAR_DISTRIBUTION_BOOTSTRAPSIGNATUREURL", "RSTAR_DISTRIBUTION_KEYRING",
    "RSTAR_DISTRIBUTION_MIRROR", "RSTAR_DISTRIBUTION_AURKEYSERVER", "RSTAR_CONTENT_FSTABLINES",
    "RSTAR_CONTENT_ROOTPASSWORD",
    "RSTAR_DISTRIBUTION_AURDEVEL", "RSTAR_DISTRIBUTION_AURREBUILD",
]

# Base changes invalidate downstream caches; package targets are reconciled during maintenance.
# 基础变化使后续缓存失效；软件目标在维护中协调。
[cache]
base_system = ["RSTAR_CONFIG", "RSTAR_DISTRIBUTION_DISTRIBUTION", "RSTAR_DISTRIBUTION_ARCHITECTURE", "RSTAR_DISTRIBUTION_BOOTSTRAPURL", "RSTAR_DISTRIBUTION_BOOTSTRAPSIGNATUREURL", "RSTAR_DISTRIBUTION_KEYRING", "RSTAR_DISTRIBUTION_MIRROR", "RSTAR_DISTRIBUTION_AURKEYSERVER"]
software = []
```

匹配成功后检查全部节及裸参数，包括 SchemaVersion、Distribution 和 Packages。
必选参数缺失或为空、出现未声明参数时均报错，不执行插件操作。
控制器独立读取的 `[Inheritance]` 不要求在插件配置中声明。
声明使用规范大写名称，重复赋值按原始列表保留；四个列表都必须提供，适用时显式使用空列表。
基础缓存检测 Config 节及上述 Distribution 字段；AurDevel/AurRebuild 只影响软件准备，
不使基础或软件阶段缓存失效。Packages 变化由软件维护处理，不使缓存失效。
新增自定义参数时须登记到 required 或 optional，并按需加入缓存列表。
缓存可选择裸参数、整节或 `RSTAR_*节名_参数名`，规则见 [构建设计](../../../docs/build-design.md)。

| 配置键 | Bash 环境变量 | 缺省行为 |
| --- | --- | --- |
| Architecture | RSTAR_DISTRIBUTION_ARCHITECTURE | 使用程序运行架构匹配插件并传入脚本 |
| BootstrapUrl | RSTAR_DISTRIBUTION_BOOTSTRAPURL | 官方 latest bootstrap |
| BootstrapSignatureUrl | RSTAR_DISTRIBUTION_BOOTSTRAPSIGNATUREURL | BootstrapUrl 加 `.sig` |
| Keyring | RSTAR_DISTRIBUTION_KEYRING | `/usr/share/pacman/keyrings/archlinux.gpg` |
| Mirror | RSTAR_DISTRIBUTION_MIRROR | 使用全局 `persistent/arch-data/mirrorlist` 的工作副本 |
| AurKeyserver | RSTAR_DISTRIBUTION_AURKEYSERVER | `hkps://keyserver.ubuntu.com` |
| AurDevel | RSTAR_DISTRIBUTION_AURDEVEL | `true`，检查 Git 源码更新 |
| AurRebuild | RSTAR_DISTRIBUTION_AURREBUILD | `none`，按记录增量复用；可设 `all` 或 pkgbase 列表 |
| [Content] RootPassword | RSTAR_CONTENT_ROOTPASSWORD | 未配置或为空时不修改软件阶段输出中的密码 |

### 软件源配置模板

Arch 0.13.0 使用既有 `persistent_dirs` 中的 `"arch-data:data"`，将
`<管理根>/persistent/arch-data/` 的事务副本挂载到插件工作目录的 `data/`。
包内默认配置保存在 `default-data/`，`data/` 仅用作挂载点，可以为空或由控制器创建。
bootstrap 和软件包配置生成前，插件递归复制 `default-data/` 中缺失的文件到 `data/`，
保留已有模板、镜像清单及额外文件；不会覆盖用户内容。首次使用自动创建持久子卷并补齐默认配置，
无需手动初始化。调用成功后沿用原有事务提交，失败则回滚，包括本次补入的默认文件。

要自定义配置，修改全局 `persistent/arch-data/pacman.conf.in`、`mirrorlist` 或添加仓库文件。
子目录应可遍历（0755），配置文件应可读（0644），供 pacman 下载用户及非 root 构建用户访问。
所有插件包操作使用挂载的 `data/pacman.conf.in`，不读取环境自身的 `/etc/pacman.conf` 作为仓库定义。
默认启用 core、extra；取消持久模板中 multilib 的注释即可启用该仓库。仓库顺序也由模板决定。
模板在每次操作时通过公共函数 `write_pacman_configuration` 渲染到 `RSTAR_PLUGIN_TMP` 下的
独立目录，Bash 安装、更新、查询、维护及 Python 依赖解析、下载、AUR 构建依赖安装均使用
生成配置调用 `pacman --config`。操作完成后删除生成目录，不覆盖目标 `/etc/pacman.conf`。
bootstrap 为普通系统 pacman 设置 `/etc/pacman.d/mirrorlist` 的原有行为保留。

模板中的 `@MIRRORLIST@` 替换为本次生成镜像清单的绝对路径，`@PLUGIN_ROOT@` 替换为
当前容器内插件资源的绝对路径。`$repo`、`$arch` 保留给 pacman 展开；其他 shell 环境变量
不会自动展开。镜像清单优先取 `[Distribution] Mirror`，未设置时复制插件 `data/mirrorlist`；
统一预下载接口的旧变量 `RSTAR_MIRROR` 也使用相同逻辑。Mirror 不接受换行。
生成目录使用 0755、配置和镜像清单使用 0644，兼容下载用户及非 root 构建用户。

要让整个插件使用额外仓库，可在模板内添加仓库段，例如：

```ini
[archlinuxcn]
Server = https://repo.archlinuxcn.org/$arch
```

也可在模板中写 `Include = @PLUGIN_ROOT@/data/repositories.conf`，把仓库段保存为
`persistent/arch-data/repositories.conf`，通过同一挂载访问。
路径占位符仅在 `pacman.conf.in` 中替换；引用文件如需继续 Include，应使用容器内可访问的
绝对路径。不要引用宿主路径、假定相对路径相对于模板，或手工声明由插件管理的 `[rstarsleep]`。
修改全局持久目录中的模板会在后续调用统一生效，无须重打包插件或新增系统 INI 字段；
新增 INI 字段仍须登记到插件参数声明。

额外仓库默认不启用，也不默认信任其密钥。无人值守初始化通过
`persistent/arch-data/repository-keyrings.conf` 显式配置，例如：

```ini
[archlinuxcn]
keyring_package = archlinuxcn-keyring
bootstrap_keys = 完整的钥环包签名主密钥指纹
# 可选，默认使用以下 HKPS 服务器
keyserver = hkps://keyserver.ubuntu.com
```

这是独立的 INI 文件，不要把这些字段写入 `pacman.conf.in`。值不加引号；
`bootstrap_keys` 接受经可信渠道确认的 40 或 64 位十六进制主密钥指纹，多个指纹用空格分隔。
示例中的中文占位必须替换，报错中的 16 位短 key ID 不能使用。仓库仍须在
`pacman.conf.in` 或其 Include 中启用；未启用的仓库不会初始化或进入包计划。
所有仓库都使用同一机制，没有 archlinuxcn 专用默认信任。
archlinuxcn 公布的完整主密钥指纹可见其官方
[master-keyids](https://github.com/archlinuxcn/archlinuxcn-keyring/blob/master/master-keyids)。
应选当前钥环包的签名者，不能仅按其他软件包报错中的签名者选择。

首次包操作先按完整指纹取得公钥，核对主密钥指纹并拒绝已撤销、过期或禁用的密钥，再作本地签署；
每次联网取钥限制为 60 秒。随后保持原有签名策略，从指定仓库安装 `keyring_package`，
按该包实际拥有的 `/usr/share/pacman/keyrings/*.gpg` 调用 `pacman-key --populate`。
安装失败、签名失败或钥环初始化失败均终止操作，不等待人工输入，也不降级签名检查。
钥环包所在仓库必须启用 `PackageRequired` 和 `PackageTrustedOnly`（默认的 `Required` 已满足）；
若配置了 `Never`、`PackageOptional` 或 `TrustAll`，初始信任流程会明确拒绝执行。
初始指纹应能验证钥环包（及需要验证的数据库），不应随意填写普通软件包的签名者。

预下载环境及目标系统在包操作前执行这一流程，复用旧缓存也补齐初始化。
钥环包和依赖进入完整下载计划，钥环包保持显式安装，不作为临时 AUR 依赖或孤儿删除；
检查旧基础缓存时，基础目标仍为 `base`，缺少的钥环包在软件维护阶段补装。
以后随包版本更新加载新的仓库钥环及撤销信息。
公钥缓存保存在 `persistent/arch-repository/bootstrap-keys/`，供后续目标构建复用，
其中只有公钥，没有预下载环境的私钥。目标构建使用已准备的仓库数据库及包缓存，不另行刷新仓库。
新增初始密钥首次仍需联网，失败可重试；手工删去配置不会撤销环境中已经建立的信任。
如需撤销初始信任，应撤销对应钥匙或重建环境。

信任操作只在独立解析环境和目标系统的有效 `GPGDir` 中进行，不修改宿主钥环；
未配置 `GPGDir` 时仍使用系统默认的 `/etc/pacman.d/gnupg`。
预下载要求仓库元数据提供包摘要，并验证数据库内嵌的包签名或 pacman 下载到缓存的独立 `.sig`；
缺少签名或验证失败均不生成准备完成记录。`Keyring` 参数仅用于验证 bootstrap 归档。
系统启动后直接执行 pacman 仍读取系统的 `/etc/pacman.conf`，插件临时配置不作为运行系统的持久配置。

控制器把配置值直接放入进程环境，不作为位置参数或 shell 源码。
单系统接口和预下载环境的初始化、维护使用上述名称。
统一 `predownload` 下载接口仍接收原来的 `RSTAR_ARCHITECTURE`、`RSTAR_AURKEYSERVER` 等
Distribution 环境变量和独立包需求参数。旧插件需改名并补充 `[cache]` 声明。
`RSTAR_ROOT`、`RSTAR_PLUGIN_ROOT`、`RSTAR_PLUGIN_TMP`、`RSTAR_CONFIG_DIR`、
`RSTAR_SYSTEM_ID` 是控制器提供的上下文，不能由插件配置覆盖。

每个操作以同名 `.sh` 文件提供，只接收自己的操作参数。
必需文件为 `bootstrap.sh`、`base_install.sh`、`update.sh`、`resolve_packages.sh`、
`query_packages.sh`、`install.sh`、`remove.sh`、`mark_reason.sh`、`list_orphans.sh`、
`remove_orphans.sh`，以及 API 3 的 `system_init.sh`。
bootstrap 在宿主运行，其他操作通过 nspawn 在目标系统中运行。
`[tools]` 声明所需工具，运行前检查对应环境，缺失时不运行操作脚本。
bootstrap 需要可信 Arch 签名密钥环和元数据中声明的下载、验签、解包及摘要工具。
`Keyring` 支持二进制 OpenPGP 密钥环、GnuPG keybox 和 ASCII 装甲公钥；装甲公钥通过
宿主 `gpg` 转换到临时二进制密钥环后交给 `gpgv` 验签，不修改原密钥文件。
API 2 可维护已有兼容缓存，新安装需要 API 3。

## 软件维护后行为

插件声明顶层 `post_maintenance = true` 并提供无参数 `post_maintenance.sh`。
控制器每次在软件阶段完成后、单系统 `script/` 之前，在全新的可写脚本阶段根内执行；
复用软件缓存或没有用户脚本时仍执行。通用接口只表示“软件维护后行为”，
设置 root 密码是本 Arch 预设的具体实现：

```ini
[Content]
RootPassword=replace-with-your-password
```

`RootPassword` 为可选的单项明文值，未配置或为空时跳过；重复赋值和换行会报错。
沿用 INI 规则：保留内部空格，去除首尾空白，`#` 开始注释，不能用它表示密码中的字面量 `#`。
配置值会随构建配置以明文留档。密码只通过 stdin 交给目标系统的 `chpasswd`，
配置值及参数 JSON 不进入 nspawn 命令行，脚本不输出密码；设置失败阻止后续脚本及版本发布。
后处理结果不写回基础或软件缓存，修改或移除配置会从未做后处理的软件输出重新构建。
后处理与用户脚本共用 script 阶段快照，该阶段及后续快照仅用于调试和备份，不作下次构建来源。
后续用户脚本及继承仍按既有顺序执行，继承 `/etc` 或 `/etc/shadow` 时可能覆盖这里设置的密码。

## 主系统启动与延后继承

插件通过 `[boot]` 声明主系统必需的软件包，以及目标根内的启动文件。预设要求目标安装计划包含
`linux`、`mkinitcpio`，发布 `/boot/vmlinuz-linux`、`/boot/initramfs-linux.img`，并将存在的
Intel/AMD 微码镜像放在主 initramfs 前。例如：

```toml
[boot]
packages = ["linux", "mkinitcpio"]
artifacts = [
    { source = "boot/intel-ucode.img", destination = "intel-ucode.img", kind = "initrd", optional = true },
    { source = "boot/amd-ucode.img", destination = "amd-ucode.img", kind = "initrd", optional = true },
    { source = "boot/initramfs-linux.img", destination = "initramfs-linux.img", kind = "initrd" },
    { source = "boot/vmlinuz-linux", destination = "vmlinuz", kind = "kernel" },
]

[initramfs_inheritance]
packages = ["mkinitcpio", "btrfs-progs", "coreutils", "util-linux", "bash", "findutils"]
tools = ["mkinitcpio", "btrfs", "cp", "mount", "findmnt", "mkdir", "sleep", "bash", "find", "install"]
```

`source` 是目标根内的规范相对路径；`destination` 是核心发布目录中的单个文件名。
`kind` 只接受 `kernel`、`initrd`，产物声明中须有且只有一个必选内核；initrd 按声明顺序发布。
核心负责验证文件、复制到启动分区、记录所有权和发布启动项，插件不直接写宿主启动分区。

仅主系统启用 `Timing=initramfs` 且配置非空 `CopyOnWrite` 时检查
`[initramfs_inheritance]` 并调用 `prepare_inheritance_initramfs.sh`。控制器在分配目标版本及修改目标缓存前，
检查对应能力的 `packages` 是否出现在当前系统的具体目标计划中。软件组成员、基础包及
运行依赖均可满足要求；缺项才要求调整配置，程序不会自动把能力要求的软件包补入目标。
插件还声明 `[package_plan] base_packages=["base"]` 和仅收集时需要的 `tools`，提供
`collect_packages.sh`。它复用共享准备的解析器，在独立环境中接收当前系统的原始目标及
声明基础目标，输出 JSON `{"schema_version":1,"packages":[...]}`。将选中 AUR 拆分包的运行
依赖及提供者元数据与官方包、软件组及基础目标联合求解，避免逐个解析后合并产生错误提供者。
单独查询不下载目标二进制或编译包，也不把临时编译依赖、其他系统目标或旧缓存软件计入计划。
默认下载开启时，计划由同一次软件下载调用直接返回；关闭下载时才单独调用查询。
解析环境先独立初始化或更新，查询复用其仓库元数据并获取 AUR 配方。
所有系统配置先冻结；计划及启动要求检查通过后才分配目标版本、清理目标缓存。
新版统一下载会在返回计划前准备目标软件，启动要求缺项时这些下载可保留在共享缓存中。
版本目录的 `package-plan.toml` 保存本次校验的具体包清单。

实际调用前还会在目标内检查 `tools`，缺失时不会执行操作脚本。

此操作在软件及用户脚本完成后执行。核心先提供通用的早期 Rust 执行器、私有动态依赖及
收尾入口，插件再安装 `initramfs/` 中的 mkinitcpio 配置、hook 和 systemd 挂载依赖，调用
目标系统的 mkinitcpio。独立配置保留 `/etc/mkinitcpio.conf` 及其 drop-in，在 `filesystems`
前插入 `rstarsleep`；失败时保留编译器输出并阻止候选版本发布。

纯子系统不要求 `[boot]` 或 `[initramfs_inheritance]` 的包；宿主 daemon 在容器启动前完成
同一继承事务，子系统使用宿主内核。若同一系统同时是主系统，则仍按主系统检查。
用户为子系统安装内核或 mkinitcpio 时，pacman 自身的 hooks 仍可能生成启动镜像。

## 包文件候选索引

顶层可选能力 `query_package_files = true` 声明 `query_package_files.sh`。
这个能力默认关闭，未声明时控制器不调用；声明后必须提供有效操作入口。
API 2/3 均可使用这个扩展，不改变已有 `query_packages` 的包名/安装原因协议。

该操作在目标系统内执行，位置参数为包名；无参数查询全部已安装包。Arch 实现调用
`pacman -Ql -- "$@"`，每行输出 `包名<TAB>绝对文件路径`，跳过目录条目，保留路径
中的空格。查询失败、非规范输出或超出清单上限时构建失败，不替换当前成功产物引用。

构建在 script 完成后、继承前保存 `versions/<版本>/application-files.tsv`，核心负责
格式校验、去重和后续文件分类。清单只是构建时的包所属关系候选：继承或运行挂载可能
改变文件，入口导出及运行仍校验固定客体根中的实际对象。已运行子系统使用运行版本
的清单，未运行时使用当前成功构建产物；显式命令/desktop 导出不依赖此可选能力。

## 挂载继承

插件通过顶层 `mount_inheritance = true` 声明 `mount_inheritance.sh`。
控制器在用户脚本完成并创建目标路径后调用此接口，每组四个参数依次为挂载源、
系统内绝对挂载点、文件系统类型和挂载选项。插件转义字段并在 `/etc/fstab` 的
受管理块内写入 `0 0` 结尾的挂载行。目录挂载使用 Btrfs 的 UUID 与 persistent 子卷路径；文件挂载
使用 `/rstarsleep` 下的持久文件及 `bind,x-systemd.requires-mounts-for=/rstarsleep`。
挂载点在新根中的底层内容保留，插件调用与包查询期间绑定持久对象；继承 `/etc` 时
更新其持久 fstab 并同步到新根。重复调用替换 `# BEGIN RStarSleep inherited mounts`
和 `# END RStarSleep inherited mounts` 间的块，保留所有块外配置。

`[Content] FstabLines` 是自由列表，可重复赋值或使用多行值，为同一版本写入完整配置行。
这些行保持原文，需自行使用合法 fstab 转义；此字段变化不影响基础或软件缓存。
额外行不能包含上述所有权标记；既有标记不成对或相互嵌套时报错，不改写 fstab。
即使没有继承挂载，声明此能力的插件也会处理额外行。例如：

```ini
[Content]
# Append complete fstab rows after automatically generated inherited mounts.
# 在自动生成的继承挂载之后追加完整 fstab 行。
FstabLines=UUID=01234567-89ab-cdef-0123-456789abcdef /data ext4 defaults 0 2
FstabLines=tmpfs /tmp tmpfs defaults,nosuid,nodev 0 0
```

每个系统独立执行 bootstrap 和基础安装，随后 `system_init <machine-id>` 写入稳定身份、
初始化 pacman 密钥。machine-id 保存在此系统的 `identity.toml`，重建仍复用。
下载的 bootstrap 归档和签名必须保留于持久目录备用，本插件将全局子卷
`arch-bootstrap` 映射为工作目录 `state/bootstrap`，以下载 URL 和签名 URL 的 SHA-256
区分来源。相同来源复用已下载文件，每次使用都重新验签；更换 URL 使用独立文件。
默认 latest URL 的已保存文件也会复用，需删除对应持久文件才重新下载该 URL。
归档布局与官方 `mkarchiso` 一致：`root.x86_64/` 系统树、顶层 `pkglist.x86_64.txt`
包清单和 `version` 版本文件。解包去掉顶层路径，仅系统树写入系统根，两个元数据文件
不会写入系统根；其他顶层条目及父级跳转路径仍会被拒绝。

安装文件可能来自较早时期，即使版本名称不变也需要更新。无基础缓存时由基础安装整理并更新基础根。
有 Packages 时在软件层统一维护；Packages 为空时在恢复的基础根上维护并更新基础缓存，
软件层直接使用该基础输出，不再恢复含旧应用的软件缓存，也不重复更新。
不再通过共享已安装系统根生成新系统；每系统缓存与 CoW 阶段快照继续保留。
当前系统的配置及能力依赖预检成功后，构建才清理历史 script 及后续缓存；基础配置变化清理基础及后续缓存，
包列表和脚本变化保留基础缓存。已发布版本根不受缓存清理影响。

基础目标为 `base`；内核及用途相关软件由 `[Content] Packages` 指定。
`Packages` 支持 `仓库名/包名`（例如 `archlinuxcn/paru`）和 `aur/包名`。
限定仓库必须已在插件 pacman 配置中启用，缺包时直接失败，不回退到 AUR。
`aur/包名` 强制从 AUR 构建，即使已启用仓库提供同名包；最终从本地
`rstarsleep` 仓库安装。来源限定只用于具体包，安装清单仍保存实际包名。
未限定来源时保持仓库优先、找不到再使用 AUR 的行为。
切换来源时会重新安装，即使版本号相同。
`Packages` 可混合具体包名和软件组，例如 `linux gnome gnome-shell`。
`resolve_packages` 优先通过 `pacman -Si` 查询具体包，仅在包不存在时使用 `pacman -Sgq`
展开软件组；包与组同名时优先选择包。组成员去重后都作为显式目标安装，
不同组及单包之间的重叠成员只保留一次；未知目标、空结果或查询错误均使构建失败。
每次维护重新解析组成员，先将已安装的具体目标与基础包设为显式，原显式包中不再属于主动目标的改为依赖，
只修改发生变化的安装原因。随后清理孤儿包及计划外包，仅安装清理后尚未安装的主动目标，
依赖由 pacman 自动安装；已有普通目标不重复传给安装命令，限定来源的目标仍重装以支持同版本切换。
然后执行 `pacman -Syu` 集体升级；已有准备元数据时包装函数改用 `-Su`，不刷新冻结的仓库数据库。
预清理移除的本轮新依赖可从预下载缓存重新安装，新依赖由 pacman 自然标记，无需安装后批量改标。
升级后再次清理孤儿及计划外包，包括孤儿查询遗漏的可选依赖和依赖环；仍被保留包需要的包不会强制删除。
阶段记录保存展开后的具体目标及实际安装清单。
`base_install` 安装基础入口，整理 bootstrap 自带工具的安装原因并清理孤儿包。
软件安装允许 libalpm 在依赖事务中替换冲突包（`--ask=4`），控制器提交前验证最终目标。
0.10.0 默认启用 `package_maintenance = true`，核心调用
`maintain_packages.sh --plan <完整包计划...> --targets <原始配置目标...>`。
入口调用 `lib/maintenance.sh`，插件内部负责清理、安装、更新和安装原因整理，
集体升级包含预清理后仍安装的软件，最终校验实际包集合和安装原因与当前计划一致。
stdout 返回 `{"requested":[...],"installed":[{"name":"base","explicit":true},...]}`，
核心检查清单与计划一致及基础包的保护状态；失败不发布阶段缓存。
查询输出：`query_packages` 每行 `包名<TAB>explicit|dependency`，
`resolve_packages` 和 `list_orphans` 每行一个包名，诊断写入 stderr。

## 预下载及 AUR

构建阶段和预下载关键步骤通过 stderr 输出中英双语进度，前缀标识当前系统或
`arch:predownload`。控制器报告阶段开始、完成、失败及耗时；缓存复用、只读快照保存、
临时工作根清理和共享缓存提交也会明确提示。预下载脚本另报告依赖解析、下载批次、
AUR 元数据、配方、构建依赖、源码验证、编译、运行依赖及最终摘要核对，长清单每 25 项报告进度。

默认 `predownload = true`、`predownload_plans = true`。同一次 build 中同插件、同架构、
相同发行版配置和有效环境的系统调用一次 `predownload.sh --systems-json <JSON>`，
JSON 为 `{"schema_version":1,"systems":{"desktop":["linux","gdm"],"minimal":[]}}`。
仅传当前配置，不并入旧缓存的已安装清单；插件为每个系统添加 base，分别联合求解完整运行依赖，
再合并包文件下载，避免不同系统的冲突包混入一个安装事务。stdout 返回
`{"schema_version":1,"systems":{"desktop":{"schema_version":1,"packages":[...]},...}}`，
供启动要求检查、版本留档和软件维护直接复用，不再单独收集并重新解析同一计划。
下载环境独立 bootstrap、基础安装、身份初始化，首次运行 `predownload/install.sh` 仅补装工具，
后续仅运行 `predownload/update.sh`；核心不再额外调用通用 update。
`RSTAR_CONFIG_DIR` 指向插件的 `predownload/`，发行版配置变量仍取自本批已校验冻结输入。
环境缓存按完整插件哈希、架构及发行版配置哈希保存，不依赖目标系统根。

下载脚本复用独立环境已更新的同步数据库，不再执行额外的 -Sy/-Su。
官方需求使用空 local 数据库解析完整依赖，保留各系统选择的版本和虚拟包提供者。
同批结果按包文件去重，以具体包名和版本合并为一次下载及校验；同名不同版本分批处理。
下载阶段不再重复解析已经确定的依赖，本轮已校验文件不重复处理。
0.15.0 起，外部仓库必须实际启用 PackageRequired、PackageTrustedOnly。新包及没有可信记录的
旧缓存由 pacman 批量下载并验签；不再额外逐包调用 pacman-key。`.official-verifications.json`
在 repository 持久卷保存包来源、版本、SHA-256 和验证上下文。复用时核对实际文件摘要，
密钥、信任库、仓库签名策略或 pacman/GPG 版本变化，以及签名或密钥有效期到期时重新验证。
未知签名格式保守交给 pacman，不持久复用验签结果；提交前仍核对所有包摘要及上下文。
信任记录随持久卷事务一起提交，失败不发布；目标系统安装仍由 pacman 按常规策略验签。
最终摘要校验的进度在终端中原位刷新；出现其他输出或校验失败时先结束进度行。
重定向到文件或管道时继续输出逐行进度。安装归档仍保留原始输出字节，包括终端刷新序列。
AUR 每个 pkgbase 的官方构建依赖、二进制运行依赖也分别按批准备；编译依赖仍按原顺序安装。
AUR 构建完成后使用实际二进制运行依赖复核该系统计划，编译依赖不进入目标计划。

官方包名、软件组和提供者从已加载的仓库数据库建立索引，来源限定和版本约束同样在索引中检查；
每个系统的完整依赖事务仍交给 pacman 联合求解，避免改变提供者和冲突处理规则。
AUR 按需一次下载 `packages-meta-ext-v1.json.gz`，在 sources 持久卷缓存六小时，
建立包名、pkgbase 和 Provides 索引；不可用时回退 RPC，不用过期转储判定包不存在。
索引只用于发现配方，实际版本、依赖和提供者仍以 Git 配方及构建后的二进制为准。
AUR 编译以非 root 的 rstar-build 用户执行；仓库包优先从官方源解析，
拆分包按 pkgbase 构建。提供者不明确、循环依赖或不满足版本约束均报错。

### AUR 增量构建

0.12.0 起，配方 commit、架构、构建配置与源码状态一致时复用已校验二进制，
不重复验证源码、编译或安装临时构建依赖。拆分包整组校验；运行依赖仍使用实际
二进制元数据处理。`.aur-builds.json` 在 repository 持久卷按架构/pkgbase 保留记录，
与本次 `.prepared.json` 清单独立，移出目标后再次加入也能复用。
升级前的缓存缺少完整记录，首次按需重建一次，随后增量复用。

普通归档有固定校验和时按配方身份复用，配方改动即使未涨版本也重建。
默认检查 source 中的 Git HEAD、branch、tag 或固定完整 commit；不靠包名后缀识别。
动态 pkgver 不单独触发重建。先下载/验证并记录本地源码快照，再用 `--holdver` 编译，
避免 prepare() 新增提交被误当作上游更新。查询失败中止并回滚，不宣称已是最新。
其他 VCS、无法识别的 Git 引用及无固定校验和的远程来源保守重建。
固定校验和的归档按配方复用，无法发现维护者未修改配方的远端文件替换。

在 `[Distribution]` 中可配置：

```ini
AurDevel=true
AurRebuild=none
```

`AurDevel=false` 关闭 Git 上游检查，仍检查配方和产物。
`AurRebuild=all` 强制构建本批全部 AUR；也可用多行/重复赋值列出 pkgbase：

```ini
AurRebuild=sample
AurRebuild=other
```

`all`、`none` 必须单独使用。配置会持续生效，完成强制重建后应恢复 `none`。
统一下载入口兼容旧变量 `RSTAR_AURDEVEL`、`RSTAR_AURREBUILD`。
日志报告复用/重建数量及每个重建原因。

记录的 AUR 构建依赖产物变化会传递失效；已声明的版本化共享库依赖无法再从当前源
满足时也会重建。没有声明 SONAME 的普通依赖不能完整反映 ABI 变化，必要时使用
AurRebuild；不会因为所有官方依赖的小版本更新就重编全部 AUR。
自定义构建配置引入外部动态输入、配方函数额外下载源码时也需手动重建。

持久子卷 arch-packages、arch-repository、arch-sources 映射为
state/packages、state/repository、state/sources，保留包、仓库和源码缓存。
每次插件调用使用事务 CoW 副本，失败回滚，成功发布。本地仓库使用 Optional TrustAll，
官方包另按仓库摘要及签名校验。PGP 来源密钥缺失时从 AurKeyserver 获取。
公开包和仓库数据库以 0644 发布，缓存及仓库根目录使用 0755，兼容 pacman 的
`DownloadUser = alpm`。软件包操作也会修复旧公开文件的权限；私有状态和密钥不在此范围。
容器资源根及挂载路径的父目录可供下载和构建用户进入，宿主临时父目录仍保持私有。
禁用下载可设置 `predownload = false`，仍收集软件维护所需计划并保存同步元数据，持久映射仍独立生效。
未声明两个新能力的旧插件仍使用原有预下载参数和核心软件协调流程。

流程已通过模拟存储、模拟工具和真实 Bash 测试；增量构建另有真实本地 Git/makepkg
离线验证。真实 Btrfs/nspawn 完整安装链路待集成验证。
完整协议见 [构建设计](../../../docs/build-design.md)。


插件可在 `plugin.toml` 顶层声明 `commands=["configure:tools/setup", "install-app"]`。
省略入口时使用同名入口，支持 `.sh` 或无后缀可执行文件；不能覆盖内置子命令，
多个插件也不能声明同一命令。宿主执行插件命令，stdin/stdout/stderr 直接连接终端，
所有参数原样透传（包括 `--root`）。管理根由 `RSTAR_MANAGER_ROOT` 环境变量选择，默认 `/rstarsleep`；
插件获得 `RSTAR_MANAGER_ROOT`、`RSTAR_PLUGIN_ROOT` 和 `RSTAR_PLUGIN_TMP`。
插件子命令必须自行申请 root（例如适当使用 sudo），管理器不自动提权。
这类入口可用于交互式安装软件并写入系统配置；应自行检查输入并传播失败。
