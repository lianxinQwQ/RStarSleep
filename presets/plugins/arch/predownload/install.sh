#!/usr/bin/env bash
# Independent environment initialization; no system build INI is consumed.
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
source "$plugin_root/lib/common.sh"
# base_install already updated this newly created root and its sync databases.
# 新环境已由 base_install 更新，只补装准备工具。
pacman -S --needed --noconfirm -- git base-devel python sudo
if ! id -u rstar-build >/dev/null 2>&1; then
    useradd --system --user-group --create-home --home-dir /var/lib/rstarsleep-build --shell /bin/bash rstar-build
fi
[[ $(id -u rstar-build) != 0 ]] || { log 'AUR build user must not be root'; exit 1; }
install -d -m 0755 -o rstar-build -g "$(id -gn rstar-build)" /var/lib/rstarsleep-build
