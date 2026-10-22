#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

# Remove independently supplied packages while protecting base.
# 删除独立传入的软件包，并保护 base。
validate_packages "$@"
if (( $# )); then
    for package in "$@"; do
        [[ $package != base ]] || { log '不能移除受保护的 base 软件包 / Cannot remove protected base package'; exit 64; }
    done
    pacman -R --noconfirm -- "$@"
fi
