#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

# Install independently supplied concrete package names explicitly.
# 将独立传入的具体包名作为显式目标安装。
validate_targets "$@"
targets=()
options=(--needed)
for target in "$@"; do
    targets+=("$(source_target "$target")")
    [[ $target != */* ]] || options=()
done
if (( $# )); then
    # Permit dependency-checked replacement of conflicting installed packages.
    # 允许在检查依赖的事务中替换冲突的已安装包。
    pacman -S "${options[@]}" --asexplicit --noconfirm --ask=4 -- "${targets[@]}"
fi
