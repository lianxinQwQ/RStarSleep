#!/usr/bin/env bash
# Query the target installation closure using the same resolver as shared preparation.
# 使用共享准备的同一解析器查询目标安装闭包。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"
validate_packages "$@"
exec python3 "$plugin_root/lib/predownload.py" --collect "$@"
