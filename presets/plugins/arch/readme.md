# Arch Linux 预设插件

该目录在 Cargo 构建时打包成 `arch.tar.gz` 并嵌入程序，`init` 将压缩包安装到
工作目录的 `plugins/`。已有同名普通文件保持不变。

系统构建配置通过 `system_type = "arch"` 自动选择此插件，目标架构为 `x86_64`。
插件 API 为版本 3。每个操作是插件根内的同名 `.sh` 文件；例如安装调用为
`bash <插件根>/install.sh vim git`，查询调用为 `bash <插件根>/query_packages.sh`。
各文件只接收本操作参数，没有统一分派入口，元数据不再包含 `entry`。
`bootstrap` 在宿主运行，其他操作由控制器通过 nspawn
在系统根内运行；脚本不负责创建版本或保存缓存。

必需操作文件为 `bootstrap.sh`、`base_install.sh`、`update.sh`、`resolve_packages.sh`、
`query_packages.sh`、`install.sh`、`remove.sh`、`mark_reason.sh`、`list_orphans.sh` 和
`remove_orphans.sh`，以及 API 3 的 `template_clean.sh`、`system_init.sh`。
Arch 另外提供 `clean_orphans.sh`；公共函数位于 `lib/common.sh`。
加载时检查全部必需文件，调用前再检查当前文件；操作文件必须是普通文件。
API 1 插件需拆分文件并更新元数据后重新打包。已有 `plugins/arch.tar.gz`
会被 `init` 保留，升级时需主动替换旧包。
API 2 可继续维护已有缓存，无缓存创建需升级到 API 3。

bootstrap 需要宿主的 Bash、curl、tar、zstd、gpgv 和可信 Arch 签名密钥环。
默认使用官方最新 bootstrap 和 `/usr/share/pacman/keyrings/archlinux.gpg`。
`RSTAR_ARCH_BOOTSTRAP_URL`、`RSTAR_ARCH_BOOTSTRAP_SIGNATURE_URL`、
`RSTAR_ARCH_KEYRING`、`RSTAR_ARCH_MIRROR` 可覆盖来源和软件源。

`RSTAR_ROOT` 是已创建的空系统根子卷。`RSTAR_PLUGIN_ROOT` 为可见的插件资源根，
`RSTAR_PLUGIN_TMP` 为现有临时写入目录。系统内操作需要 Bash 和 pacman。

`plugin.toml` 的 `[tools]` 显式声明宿主及系统内工具。程序调用前检查对应环境中
所有声明的工具；缺失或工具文件不可执行时返回错误，操作脚本不会运行。
系统内检查使用 nspawn 和目标系统 `/bin/sh`，不以宿主安装了 pacman 代替检查。

查询输出：`query_packages` 每行 `包名<TAB>explicit|dependency`；
`resolve_packages` 和 `list_orphans` 每行一个包名。诊断写入 stderr。
包参数仅支持具体仓库包名；AUR、包组和版本表达式尚未实现。

基础目标为 `base`。`base_install` 完整更新并安装基础目标，整理 bootstrap
自带工具的安装原因并清理孤儿包。内核等用途相关软件由系统的 `packages` 指定。

基础系统在 `templates/<完整插件SHA-256>/<架构>/` 内构建，完成后保存只读
`completed/root` 和 `completed/template.toml`，所有系统可从中创建 CoW 副本。
模板构建的 `RSTAR_SYSTEM_ID` 固定为 `template`，`RSTAR_CONFIG_DIR` 指向固定
最小配置，`packages = []`；系统特定选项只交给副本上的 `system_init` 及后续操作。
完整归档的任何变化都产生新模板，不单独散列 `.sh` 文件。

`template_clean` 是模板发布前最后一次容器操作：清空 machine-id，移除 D-Bus
身份、SSH 主机密钥、随机种子、主机名、日志、临时文件和 pacman 本地密钥环。
发行版公开密钥仍保留。`system_init <machine-id>` 为副本写入独立身份，并重新
初始化及填充 pacman 密钥环；machine-id 由管理器保存于每系统 `identity.toml`，
同一系统重新从模板创建时仍保留该 ID。SSH 主机密钥由后续服务初始化生成。
复用旧模板创建系统时，在副本中执行完整更新；已有缓存的维护动作保持不变。
bootstrap 环境变量覆盖仅影响模板首次构建；需要改变已有模板的来源或镜像时，
应更新插件归档中的资源，或移除相应模板后重建。


元数据以 `protected_packages = ["base"]` 声明基础入口；内核和用途相关软件由系统
`packages` 指定。控制器传入留档目录 `RSTAR_CONFIG_DIR` 与系统标识
`RSTAR_SYSTEM_ID`，宿主与容器任务分别使用各自路径。

软件安装允许 libalpm 在依赖检查事务中替换冲突包（`--ask=4`），最终保留目标
由控制器再次验证。所有软件流程在提交前查询实际清单，完成缓存通过只读
Btrfs CoW 快照保存。真实下载与完整安装链路尚需有权限的集成环境验证。
