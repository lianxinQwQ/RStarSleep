#!/usr/bin/env bash
# Initialize one system snapshot without modifying the shared template.
# 初始化单个系统快照，不修改共享模板。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"
(( $# == 1 )) && [[ $1 =~ ^[0-9a-f]{32}$ && $1 != 00000000000000000000000000000000 ]] || {
    log 'system_init requires one nonzero 32-character machine-id'; exit 64;
}
identity_root
root=${RSTAR_ROOT%/}
# Unlink first so writes cannot follow template-provided identity symlinks or hardlinks.
# 先解除链接，避免写入跟随模板身份文件的符号链接或硬链接。
rm -f -- "$root/etc/machine-id" "$root/var/lib/dbus/machine-id"
printf '%s\n' "$1" >"$root/etc/machine-id"
if [[ -d $root/var/lib/dbus ]]; then
    printf '%s\n' "$1" >"$root/var/lib/dbus/machine-id"
fi
pacman-key --gpgdir "$root/etc/pacman.d/gnupg" --init
pacman-key --gpgdir "$root/etc/pacman.d/gnupg" --populate archlinux
