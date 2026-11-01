#!/usr/bin/env bash
# Refresh metadata; dependency pruning and upgrades follow full dependency planning.
# 刷新元数据；依赖清理和升级在完整依赖规划后执行。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
source "$plugin_root/lib/common.sh"
pacman -Sy --noconfirm
id -u rstar-build >/dev/null
[[ $(id -u rstar-build) != 0 ]] || { log 'AUR 构建用户不能是 root / AUR build user must not be root'; exit 1; }
install -d -m 0755 -o rstar-build -g "$(id -gn rstar-build)" /var/lib/rstarsleep-build
