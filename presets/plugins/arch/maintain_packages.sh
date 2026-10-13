#!/usr/bin/env bash
# Reconcile a prepared package plan in one plugin-owned operation.
# 在一个由插件编排的操作中维护已准备的软件包计划。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"
source "$plugin_root/lib/maintenance.sh"

maintain_packages "$@"
