#!/usr/bin/env bash
# Independent environment initialization; no system build INI is consumed.
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)}
source "$plugin_root/lib/common.sh"
pacman -Syu --needed --noconfirm -- git base-devel python sudo
if ! id -u rstar-build >/dev/null 2>&1; then
    useradd --system --user-group --create-home --home-dir /var/lib/rstarsleep-build --shell /bin/bash rstar-build
fi
[[ $(id -u rstar-build) != 0 ]] || { log 'AUR build user must not be root'; exit 1; }
install -d -m 0755 -o rstar-build -g "$(id -gn rstar-build)" /var/lib/rstarsleep-build
