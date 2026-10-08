#!/usr/bin/env bash
# Replace the owned inherited-mount block after the build's scripts finish.
# 在构建脚本结束后替换所属的继承挂载配置块。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

(( $# % 4 == 0 )) || {
    log 'mount_inheritance requires source, absolute target, filesystem type and options groups'; exit 64;
}
[[ ${RSTAR_ROOT:-} == /* && -d $RSTAR_ROOT && ! -L $RSTAR_ROOT ]] || {
    log 'mount_inheritance requires an existing absolute RSTAR_ROOT'; exit 64;
}
root=${RSTAR_ROOT%/}
[[ -d $root/etc && ! -L $root/etc ]] || {
    log 'mount_inheritance requires a real etc directory'; exit 64;
}
fstab=$root/etc/fstab
[[ ! -L $fstab && ( ! -e $fstab || -f $fstab ) ]] || {
    log 'mount_inheritance requires a regular fstab without symbolic links'; exit 64;
}

# Encode every field before joining it into an fstab row; do not interpret shell text.
# 拼接 fstab 行之前对每个字段编码，不解释 shell 文本。
# Arguments: one unescaped fstab field.
# 参数：一个未经转义的 fstab 字段。
fstab_field() {
    local value=$1
    value=${value//\\/\\134}
    value=${value// /\\040}
    value=${value//$'\t'/\\011}
    value=${value//$'\n'/\\012}
    value=${value//#/\\043}
    printf '%s' "$value"
}

rows=()
while (( $# > 0 )); do
    [[ -n $1 && $2 == /* && $3 =~ ^[a-zA-Z0-9_.+-]+$ && -n $4 ]] || {
        log 'Invalid inherited mount source, target, filesystem type or options'; exit 64;
    }
    rows+=("$(fstab_field "$1") $(fstab_field "$2") $(fstab_field "$3") $(fstab_field "$4") 0 0")
    shift 4
done
extra=${RSTAR_CONTENT_FSTABLINES:-}
begin='# BEGIN RStarSleep inherited mounts'
end='# END RStarSleep inherited mounts'
while IFS= read -r line; do
    [[ $line != "$begin" && $line != "$end" ]] || {
        log 'FstabLines must not contain inherited-mount ownership markers'; exit 64;
    }
done <<<"$extra"
(( ${#rows[@]} > 0 )) || [[ -n $extra || -f $fstab ]] || exit 0

# Replace the destination so an installation-provided hard link cannot redirect writes.
# 替换目标文件，避免安装内容中的硬链接重定向写入。
umask 022
temporary=$(mktemp -d "$root/etc/.rstarsleep-fstab.XXXXXXXX")
trap 'rm -rf -- "$temporary"' EXIT
output=$temporary/fstab
if [[ -f $fstab ]]; then
    cp --preserve=mode -- "$fstab" "$output"
    # Persistent /etc may contain a prior build's generated rows; retain all user rows.
    # 持久 /etc 可能保留上次构建的生成行；保留所有用户行。
    if ! awk -v begin="$begin" -v end="$end" '
        $0 == begin { if (inside) { invalid = 1; exit 64 }; inside = 1; next }
        $0 == end { if (!inside) { invalid = 1; exit 64 }; inside = 0; next }
        !inside { print }
        END { if (inside || invalid) exit 64 }
    ' "$fstab" >"$output"; then
        log 'Malformed inherited-mount ownership block in fstab'; exit 64;
    fi
else
    : >"$output"
fi
if (( ${#rows[@]} > 0 )) || [[ -n $extra ]]; then
    printf '%s\n' "$begin" >>"$output"
    if (( ${#rows[@]} > 0 )); then
        printf '%s\n' "${rows[@]}" >>"$output"
    fi
    if [[ -n $extra ]]; then
        printf '%s\n' "$extra" >>"$output"
    fi
    printf '%s\n' "$end" >>"$output"
fi
mv -f -- "$output" "$fstab"
