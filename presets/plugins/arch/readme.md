# Arch Linux 预设插件

该目录在 Cargo 构建时打包成 `arch.tar.gz` 并嵌入程序，`init` 将压缩包安装到
工作目录的 `plugins/`。已有同名普通文件保持不变。

系统构建配置通过 `system_type = "arch"` 自动选择此插件，目标架构为 `x86_64`。
插件入口是 `plugin.sh`。`bootstrap` 在宿主运行，其他操作由控制器通过 nspawn
在系统根内运行；脚本不负责创建版本或保存缓存。

bootstrap 需要宿主的 Bash、curl、tar、zstd、gpgv 和可信 Arch 签名密钥环。
默认使用官方最新 bootstrap 和 `/usr/share/pacman/keyrings/archlinux.gpg`。
`RSTAR_ARCH_BOOTSTRAP_URL`、`RSTAR_ARCH_BOOTSTRAP_SIGNATURE_URL`、
`RSTAR_ARCH_KEYRING`、`RSTAR_ARCH_MIRROR` 可覆盖来源和软件源。

`RSTAR_ROOT` 是已创建的空系统根子卷。`RSTAR_PLUGIN_ROOT` 为可见的插件资源根，
`RSTAR_PLUGIN_TMP` 为现有临时写入目录。系统内操作需要 Bash 和 pacman。

查询输出：`query_packages` 每行 `包名<TAB>explicit|dependency`；
`resolve_packages` 和 `list_orphans` 每行一个包名。诊断写入 stderr。
包参数仅支持具体仓库包名；AUR、包组和版本表达式尚未实现。

基础目标为 `base`。`base_install` 完整更新并安装基础目标，整理 bootstrap
自带工具的安装原因并清理孤儿包。内核等用途相关软件由系统的 `packages` 指定。
