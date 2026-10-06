#!/usr/bin/env bash
# Remove per-machine state as the last operation before publishing a shared template.
# 发布共享模板前，作为最后一个操作移除每台机器专属状态。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"
(( $# == 0 )) || { log 'template_clean does not accept arguments'; exit 64; }
identity_root
root=${RSTAR_ROOT%/}
rm -f -- "$root/etc/machine-id" "$root/var/lib/dbus/machine-id" \
    "$root/var/lib/systemd/random-seed" "$root/etc/hostname" "$root/etc/machine-info"
# Empty machine-id preserves systemd's first-boot semantics for an uninitialized template.
# 空 machine-id 保留未初始化模板的 systemd 首次启动语义。
: >"$root/etc/machine-id"
rm -f -- "$root/etc/ssh/"ssh_host_*
# Public distribution signing keys remain in /usr/share/pacman/keyrings; local keys are regenerated.
# 发行版公开签名密钥保留在 /usr/share/pacman/keyrings；本地密钥重新生成。
rm -rf -- "$root/etc/pacman.d/gnupg"
shopt -s nullglob dotglob
for directory in "$root/var/log" "$root/tmp" "$root/var/tmp"; do
    entries=("$directory"/*)
    if (( ${#entries[@]} )); then
        rm -rf -- "${entries[@]}"
    fi
done
