#!/usr/bin/env bash
# Query owners inside the target; no arguments selects every installed package.
# 在目标内查询所属关系；无参数选择所有已安装包。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"
validate_packages "$@"
pacman -Ql -- "$@" | awk '
{
    split_at = index($0, " ")
    package = substr($0, 1, split_at - 1)
    path = substr($0, split_at + 1)
    if (split_at < 2 || substr(path, 1, 1) != "/" ||
        index(path, "\t") || index(path, "\r")) {
        print "invalid package file list" > "/dev/stderr"
        exit 1
    }
    if (path !~ /\/$/)
        printf "%s\t%s\n", package, path
}'
