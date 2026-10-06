#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

query_packages() {
    local installed explicit package reason
    local -A explicit_packages=()
    installed=$(pacman -Qq)
    explicit=$(filtered_query -Qqe)
    if [[ -n $explicit ]]; then
        while IFS= read -r package; do explicit_packages["$package"]=1; done <<<"$explicit"
    fi
    while IFS= read -r package; do
        reason=dependency
        [[ -z ${explicit_packages[$package]:-} ]] || reason=explicit
        printf '%s\t%s\n' "$package" "$reason"
    done <<<"$installed"
}

query_packages "$@"
