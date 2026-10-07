#!/usr/bin/env bash
# Maintain the cached preparation environment before every subsequent download.
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
source "$plugin_root/lib/common.sh"
pacman -Syu --needed --noconfirm -- git base-devel python sudo
id -u rstar-build >/dev/null
[[ $(id -u rstar-build) != 0 ]] || { log 'AUR build user must not be root'; exit 1; }
install -d -m 0755 -o rstar-build -g "$(id -gn rstar-build)" /var/lib/rstarsleep-build
