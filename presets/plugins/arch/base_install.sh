#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

base_install() {
    local output package
    local -a dependencies=()
    pacman-key --init
    pacman-key --populate archlinux
    pacman -Syu --needed --asexplicit --noconfirm -- base
    output=$(pacman -Qq)
    while IFS= read -r package; do
        [[ $package == base ]] || dependencies+=("$package")
    done <<<"$output"
    if (( ${#dependencies[@]} )); then
        pacman -D --asdeps -- "${dependencies[@]}"
    fi
    pacman -D --asexplicit -- base
    clean_orphan_dependencies
}

base_install "$@"
