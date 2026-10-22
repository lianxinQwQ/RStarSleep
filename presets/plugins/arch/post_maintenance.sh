#!/usr/bin/env bash
# Apply optional system configuration after package maintenance, before user scripts.
# 在软件维护后、用户脚本前应用可选系统配置。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

(( $# == 0 )) || { log 'post_maintenance 不接受参数 / post_maintenance takes no arguments'; exit 64; }
password=${RSTAR_CONTENT_ROOTPASSWORD:-}
unset RSTAR_CONTENT_ROOTPASSWORD
[[ -n $password ]] || exit 0
[[ $password != *$'\n'* && $password != *$'\r'* ]] || {
    log 'RootPassword 不能包含换行符 / RootPassword must not contain line breaks'; exit 64;
}
[[ ${RSTAR_ROOT:-} == /* && -d $RSTAR_ROOT && ! -L $RSTAR_ROOT ]] || {
    log 'post_maintenance 要求 RSTAR_ROOT 为现有根目录的绝对路径 / post_maintenance requires an existing absolute RSTAR_ROOT'; exit 64;
}
case "$RSTAR_ROOT/" in
    *'/../'*|*'/./'*) log 'RSTAR_ROOT 不能包含路径遍历分量 / RSTAR_ROOT must not contain traversal components'; exit 64 ;;
esac

# Keep the password on stdin and suppress child output that might echo its input.
# 密码仅通过 stdin 传递，并抑制可能回显输入的子进程输出。
if printf 'root:%s\n' "$password" | chpasswd --root "$RSTAR_ROOT" >/dev/null 2>&1; then
    unset password
else
    status=$?
    unset password
    log '设置 root 密码失败 / Failed to set the root password'
    exit "$status"
fi
