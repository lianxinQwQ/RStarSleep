#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

base_install() {
    local output package keyring_output target
    local -a dependencies=() protected=(base)
    local -A keyring_names=()
    pacman-key --init
    pacman-key --populate archlinux
    pacman -Syu --needed --asexplicit --noconfirm -- base
    keyring_output=$(configured_repository_keyring_targets)
    while IFS= read -r target; do
        [[ -n $target ]] || continue
        package=${target#*/}
        keyring_names[$package]=1
        protected+=("$package")
    done <<<"$keyring_output"
    output=$(pacman -Qq)
    while IFS= read -r package; do
        [[ $package == base || -n ${keyring_names[$package]:-} ]] || dependencies+=("$package")
    done <<<"$output"
    if (( ${#dependencies[@]} )); then
        pacman -D --asdeps -- "${dependencies[@]}"
    fi
    pacman -D --asexplicit -- "${protected[@]}"
    clean_orphan_dependencies
}

base_install "$@"
