#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

resolve_packages() {
    local target information members package status
    local -a resolved=()
    local -A seen=()
    validate_packages "$@"
    (( $# )) || return 0
    resolve_tmp=$(mktemp -d "${RSTAR_PLUGIN_TMP:-${TMPDIR:-/tmp}}/arch-resolve.XXXXXXXX")
    trap 'rm -rf -- "$resolve_tmp"' EXIT
    for target in "$@"; do
        # Exact packages take precedence over groups, matching pacman's install targets.
        # 具体包优先于软件组，与 pacman 的安装目标语义一致。
        if information=$(pacman -Si -- "$target" 2>"$resolve_tmp/error"); then
            cat -- "$resolve_tmp/error" >&2
            members=$(awk '$1 == "Name" && $2 == ":" { print $3 }' <<<"$information")
        else
            status=$?
            # Only a missing package permits group lookup; preserve database/configuration errors.
            # 仅缺包时查询软件组；保留数据库和配置错误。
            if (( status != 1 )) || [[ $(<"$resolve_tmp/error") != "error: package '$target' was not found" ]]; then
                cat -- "$resolve_tmp/error" >&2
                return "$status"
            fi
            if members=$(pacman -Sgq -- "$target"); then
                :
            else
                status=$?
                log "Cannot resolve package or group: $target"
                return "$status"
            fi
        fi
        [[ -n $members ]] || { log "No packages resolved for target: $target"; return 1; }
        while IFS= read -r package; do
            validate_packages "$package" || return $?
            if [[ -z ${seen[$package]:-} ]]; then
                seen["$package"]=1
                resolved+=("$package")
            fi
        done <<<"$members"
    done
    printf '%s\n' "${resolved[@]}"
}

resolve_packages "$@"
