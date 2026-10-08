# Arch Linux 预设插件

此目录在 Cargo 构建时打包并嵌入程序，`init` 将其写入管理目录的
`plugins/arch.tar.gz`。已有同名普通文件保持不变，升级到 0.7.0 需主动替换旧插件包。
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
]

# Base changes invalidate downstream caches; package targets are reconciled during maintenance.
# 基础变化使后续缓存失效；软件目标在维护中协调。
[cache]
base_system = ["RSTAR_CONFIG", "RSTAR_DISTRIBUTION"]
software = []
```

匹配成功后检查全部节及裸参数，包括 SchemaVersion、Distribution 和 Packages。
必选参数缺失或为空、出现未声明参数时均报错，不执行插件操作。
控制器独立读取的 `[Inheritance]` 不要求在插件配置中声明。
声明使用规范大写名称，重复赋值按原始列表保留；四个列表都必须提供，适用时显式使用空列表。
基础缓存检测 Config、Distribution 整节；Packages 变化由软件维护处理，不使缓存失效。
新增自定义参数时须登记到 required 或 optional，并按需加入缓存列表。
缓存可选择裸参数、整节或 `RSTAR_*节名_参数名`，规则见 [构建设计](../../../docs/build-design.md)。

| 配置键 | Bash 环境变量 | 缺省行为 |
| --- | --- | --- |
| Architecture | RSTAR_DISTRIBUTION_ARCHITECTURE | 使用程序运行架构匹配插件并传入脚本 |
| BootstrapUrl | RSTAR_DISTRIBUTION_BOOTSTRAPURL | 官方 latest bootstrap |
| BootstrapSignatureUrl | RSTAR_DISTRIBUTION_BOOTSTRAPSIGNATUREURL | BootstrapUrl 加 `.sig` |
| Keyring | RSTAR_DISTRIBUTION_KEYRING | `/usr/share/pacman/keyrings/archlinux.gpg` |
| Mirror | RSTAR_DISTRIBUTION_MIRROR | 使用包内 `data/mirrorlist` |
| AurKeyserver | RSTAR_DISTRIBUTION_AURKEYSERVER | `hkps://keyserver.ubuntu.com` |

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
API 2 可维护已有兼容缓存，新安装需要 API 3。

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

安装文件可能来自较早时期，即使版本名称不变也需要更新。新建或恢复基础根后，
控制器始终调用一次基础维护，成功后才保存基础缓存。软件有缓存时仍独立维护软件根。
不再通过共享已安装系统根生成新系统；每系统缓存与 CoW 阶段快照继续保留。
构建开始固定清理历史 script 及后续缓存；基础配置变化清理基础及后续缓存，
包列表和脚本变化保留基础缓存。已发布版本根不受缓存清理影响。

基础目标为 `base`；内核及用途相关软件由 `[Content] Packages` 指定。
`base_install` 安装基础入口，整理 bootstrap 自带工具的安装原因并清理孤儿包。
软件安装允许 libalpm 在依赖事务中替换冲突包（`--ask=4`），控制器提交前验证最终目标。
查询输出：`query_packages` 每行 `包名<TAB>explicit|dependency`，
`resolve_packages` 和 `list_orphans` 每行一个包名，诊断写入 stderr。

## 预下载及 AUR

0.5.0 默认 `predownload = true`。同一次 build 中同插件、同架构、相同发行版配置的
软件需求合并后调用一次 `predownload.sh`。下载环境独立 bootstrap、基础安装、身份初始化
并维护，首次运行 `predownload/install.sh`，后续运行 `predownload/update.sh`。
`RSTAR_CONFIG_DIR` 指向插件的 `predownload/`，发行版配置变量仍取自本批已校验冻结输入。
环境缓存按完整插件哈希、架构及发行版配置哈希保存，不依赖目标系统根。

AUR 编译以非 root 的 rstar-build 用户执行；仓库包优先从官方源解析，其他包使用 AUR RPC，
拆分包按 pkgbase 构建。提供者不明确、循环依赖或不满足版本约束均报错。
持久子卷 arch-packages、arch-repository、arch-sources 映射为
state/packages、state/repository、state/sources，保留包、仓库和源码缓存。
每次插件调用使用事务 CoW 副本，失败回滚，成功发布。本地仓库使用 Optional TrustAll，
官方包另按仓库摘要及签名校验。PGP 来源密钥缺失时从 AurKeyserver 获取。
禁用能力可设置 `predownload = false`，持久映射仍独立生效。

流程已通过模拟存储、模拟工具和真实 Bash 测试；真实 Btrfs/nspawn 完整安装链路待集成验证。
完整协议见 [构建设计](../../../docs/build-design.md)。


插件可在 `plugin.toml` 顶层声明 `commands=["configure:tools/setup", "install-app"]`。
省略入口时使用同名入口，支持 `.sh` 或无后缀可执行文件；不能覆盖内置子命令，
多个插件也不能声明同一命令。宿主执行插件命令，stdin/stdout/stderr 直接连接终端，
所有参数原样透传（包括 `--root`）。管理根由 `RSTAR_MANAGER_ROOT` 环境变量选择，默认 `/rstarsleep`；
插件获得 `RSTAR_MANAGER_ROOT`、`RSTAR_PLUGIN_ROOT` 和 `RSTAR_PLUGIN_TMP`。
插件子命令必须自行申请 root（例如适当使用 sudo），管理器不自动提权。
这类入口可用于交互式安装软件并写入系统配置；应自行检查输入并传播失败。
