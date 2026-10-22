#!/usr/bin/env bash
# Initialize the stable identity of one independently installed system.
# 初始化独立安装的单个系统的稳定身份。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"
(( $# == 1 )) && [[ $1 =~ ^[0-9a-f]{32}$ && $1 != 00000000000000000000000000000000 ]] || {
    log 'system_init 需要一个非零的 32 字符 machine-id / system_init requires one nonzero 32-character machine-id'; exit 64;
}
identity_root
root=${RSTAR_ROOT%/}
# Unlink first so writes cannot follow installation-provided identity symlinks or hardlinks.
# 先解除链接，避免写入跟随安装文件中身份文件的符号链接或硬链接。
rm -f -- "$root/etc/machine-id" "$root/var/lib/dbus/machine-id"
printf '%s\n' "$1" >"$root/etc/machine-id"
if [[ -d $root/var/lib/dbus ]]; then
    printf '%s\n' "$1" >"$root/var/lib/dbus/machine-id"
fi
pacman-key --gpgdir "$root/etc/pacman.d/gnupg" --init
pacman-key --gpgdir "$root/etc/pacman.d/gnupg" --populate archlinux
