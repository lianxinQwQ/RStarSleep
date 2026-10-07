# Arch Linux 预设插件

此目录在 Cargo 构建时打包并嵌入程序，`init` 将其写入管理目录的
`plugins/arch.tar.gz`。已有同名普通文件保持不变，升级到 0.5.0 需主动替换旧插件包。
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

元数据必须显式声明接受的原始配置键：

```toml
[config]
required = []
optional = ["Architecture", "BootstrapUrl", "BootstrapSignatureUrl", "Keyring", "Mirror", "AurKeyserver"]
```

匹配成功后检查全部 `[Distribution]` 设置（不包括 `Distribution`）。必选设置缺失或为空、
出现未声明设置时均报错，不执行任何插件操作。键名区分大小写；同名重复赋值取最后一个值。
必选和可选列表均必须提供，适用时显式使用空列表；旧 API 2 插件也必须补充声明。

| 配置键 | Bash 环境变量 | 缺省行为 |
| --- | --- | --- |
| Architecture | RSTAR_ARCHITECTURE | 使用程序运行架构匹配插件并传入脚本 |
| BootstrapUrl | RSTAR_BOOTSTRAPURL | 官方 latest bootstrap |
| BootstrapSignatureUrl | RSTAR_BOOTSTRAPSIGNATUREURL | BootstrapUrl 加 `.sig` |
| Keyring | RSTAR_KEYRING | `/usr/share/pacman/keyrings/archlinux.gpg` |
| Mirror | RSTAR_MIRROR | 使用包内 `data/mirrorlist` |
| AurKeyserver | RSTAR_AURKEYSERVER | `hkps://keyserver.ubuntu.com` |

控制器把配置值直接放入进程环境，不作为位置参数或 shell 源码。
`RSTAR_ARCH` 及旧的 `RSTAR_ARCH_*` 配置覆盖名已替换为上述名称。
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
