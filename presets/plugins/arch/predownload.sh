#!/usr/bin/env bash
# Single batch preparation entrypoint; inputs are raw configured package names.
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"
if [[ ${1:-} != --systems-json ]]; then validate_packages "$@"; fi
exec python3 "$plugin_root/lib/predownload.py" "$@"
