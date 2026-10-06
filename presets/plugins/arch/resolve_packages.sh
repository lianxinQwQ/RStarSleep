#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

resolve_packages() {
    local package information
    validate_packages "$@"
    for package in "$@"; do
        information=$(pacman -Si -- "$package")
        awk '$1 == "Name" && $2 == ":" { print $3 }' <<<"$information"
    done
}

resolve_packages "$@"
