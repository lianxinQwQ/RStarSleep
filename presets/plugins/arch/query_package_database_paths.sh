#!/usr/bin/env bash
# Resolve the same effective database used by package queries, including Include/RootDir.
# 解析软件查询实际使用的数据库，包含 Include 和 RootDir 的效果。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"
(( $# == 0 )) || { log 'query_package_database_paths 不接受参数 / query_package_database_paths takes no arguments'; exit 64; }
rstar_local_query=1
directory=$(mktemp -d "${RSTAR_PLUGIN_TMP:-${TMPDIR:-/tmp}}/arch-database-path.XXXXXXXX")
trap 'rm -rf -- "$directory"' EXIT
write_pacman_configuration "$directory"
database=$(pacman-conf --config "$directory/pacman.conf" DBPath)
[[ $database == /* && $database != *$'\n'* && $database != *$'\r'* ]] || exit 1
database=${database%/}/local
printf '%s\n' "${database#/}"
