#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

mark_reason() {
    local reason=${1:-} package
    [[ $reason == explicit || $reason == dependency ]] || { log '应为 explicit 或 dependency / Expected explicit or dependency'; return 64; }
    shift
    validate_packages "$@"
    (( $# )) || return 0
    if [[ $reason == dependency ]]; then
        for package in "$@"; do
            [[ $package != base ]] || { log '不能将受保护的 base 软件包降为依赖安装 / Cannot demote protected base package'; return 64; }
        done
        pacman -D --asdeps -- "$@"
    else
        pacman -D --asexplicit -- "$@"
    fi
}

mark_reason "$@"
