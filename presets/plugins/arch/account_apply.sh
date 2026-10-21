#!/usr/bin/env bash
set -euo pipefail
exec python3 "$RSTAR_PLUGIN_ROOT/lib/accounts.py" "apply" "$@"
