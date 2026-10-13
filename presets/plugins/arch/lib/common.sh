#!/usr/bin/env bash
# Arch Linux operations invoked by the controller in the designated environment.
# 控制器在指定环境调用的 Arch Linux 操作。
set -euo pipefail
export LC_ALL=C

# Report diagnostic text without contaminating query output.
# 输出诊断信息，不污染查询输出。
# Arguments: all arguments are diagnostic text fragments.
# 参数：所有参数均为诊断文本片段。
log() {
    printf '%s\n' "$*" >&2
}

# Validate concrete package names before passing them to pacman.
# 将具体包名传入 pacman 前进行校验。
# Arguments: each argument is one concrete package name.
# 参数：每个参数是一个具体软件包名称。
validate_packages() {
    local package
    for package in "$@"; do
        if [[ ! $package =~ ^[a-zA-Z0-9@_+][a-zA-Z0-9@_.+-]*$ ]]; then
            log "Invalid package name: $package"
            return 64
        fi
    done
}

# Normalize pacman's empty filtered query while preserving real query errors.
# 将 pacman 筛选结果为空的情形规范化，同时保留真正的查询错误。
# Arguments: pacman query options and optional package names.
# 参数：pacman 查询选项，以及可选的软件包名称。
filtered_query() {
    local query_tmp status
    query_tmp=$(mktemp -d "${RSTAR_PLUGIN_TMP:-${TMPDIR:-/tmp}}/arch-query.XXXXXXXX")
    if pacman "$@" >"$query_tmp/output" 2>"$query_tmp/error"; then
        status=0
    else
        status=$?
    fi
    cat -- "$query_tmp/error" >&2
    if (( status == 1 )) && [[ ! -s $query_tmp/output && ! -s $query_tmp/error ]]; then
        status=0
    fi
    cat -- "$query_tmp/output"
    rm -rf -- "$query_tmp"
    return "$status"
}

clean_orphan_dependencies() {
    local output
    local -a packages=()
    output=$(filtered_query -Qdtq)
    if [[ -n $output ]]; then
        mapfile -t packages <<<"$output"
        pacman -Rns --noconfirm -- "${packages[@]}"
    fi
}

# Require an explicit root and real parent directories before handling identity files.
# 处理身份文件前，要求显式根及真实父目录。
# Arguments: none; RSTAR_ROOT is supplied by the controller.
# 参数：无；RSTAR_ROOT 由控制器提供。
identity_root() {
    local relative
    [[ ${RSTAR_ROOT:-} == /* && -d $RSTAR_ROOT && ! -L $RSTAR_ROOT ]] || {
        log 'Identity operations require an existing absolute RSTAR_ROOT'; return 64;
    }
    for relative in etc etc/ssh etc/pacman.d var var/lib var/lib/dbus var/lib/systemd var/log tmp var/tmp; do
        [[ ! -L ${RSTAR_ROOT%/}/$relative ]] || {
            log "Identity directory must not be a link: $relative"; return 64;
        }
    done
}

# Repair public artifacts retained by older plugin versions without opening private state.
# 修复旧插件保留的公开文件权限，不修改私有状态。
prepare_shared_cache() {
    local directory artifact
    local -a artifacts=()
    for directory in "$@"; do
        [[ -d $directory ]] || continue
        chmod 0755 -- "$directory" || return
        for artifact in "$directory"/*.pkg.tar.* "$directory"/rstarsleep.db* "$directory"/rstarsleep.files*; do
            [[ -f $artifact && ! -L $artifact ]] || continue
            artifacts+=("$artifact")
        done
    done
    if (( ${#artifacts[@]} )); then
        chmod 0644 -- "${artifacts[@]}"
    fi
}

# One configuration path for all operations; only locally produced packages use
# the explicitly trusted rstarsleep repository. Official signature policy stays intact.
pacman() {
    local cache=${RSTAR_PLUGIN_ROOT:-}/state/packages
    local repository=${RSTAR_PLUGIN_ROOT:-}/state/repository
    local configuration status=0 cleanup_status=0
    local -a args=("$@")
    if [[ ! -d $cache ]]; then
        command pacman "${args[@]}"
        return
    fi
    prepare_shared_cache "$cache" "$repository" || return
    # Prepared official metadata is consumed by target builds. Preparation-environment
    # maintenance retains normal refresh semantics.
    if [[ ${RSTAR_SYSTEM_ID:-} != predownload && ( -f $repository/.prepared.json || -f $repository/.planned.json ) ]]; then
        mkdir -p /var/lib/pacman/sync || return
        local database database_count=0
        for database in "$repository"/official-sync/*.db "$repository"/official-sync/*.db.sig; do
            [[ -f $database ]] || continue
            cp --remove-destination -- "$database" "/var/lib/pacman/sync/${database##*/}" || return
            [[ $database != *.db ]] || (( database_count += 1 ))
        done
        (( database_count )) || { log 'Prepared official repository databases are missing'; return 1; }
        if [[ ${args[0]:-} == -Syu ]]; then args[0]=-Su; fi
    fi
    if [[ -f $repository/rstarsleep.db.tar.gz ]]; then
        configuration=$(mktemp "${RSTAR_PLUGIN_TMP:-/tmp}/pacman-conf.XXXXXXXX") || return
        # This wrapper is also called in if/|| contexts, where Bash disables
        # errexit. Explicitly stop when preparing repository metadata fails.
        # 此函数也用于 if/|| 条件，此时 Bash 禁用 errexit，须显式传播准备失败。
        if awk '/^[[:space:]]*\[/ { skip = ($0 ~ /^[[:space:]]*\[rstarsleep\]/) } !skip { print }' /etc/pacman.conf >"$configuration" &&
            printf '\n[rstarsleep]\nSigLevel = Optional TrustAll\nServer = file://%s\n' "$repository" >>"$configuration" &&
            mkdir -p /var/lib/pacman/sync &&
            cp --remove-destination -- "$repository/rstarsleep.db.tar.gz" /var/lib/pacman/sync/rstarsleep.db; then
            command pacman --config "$configuration" --cachedir "$cache" "${args[@]}" || status=$?
        else
            status=$?
        fi
        rm -f -- "$configuration" || cleanup_status=$?
        (( status )) || status=$cleanup_status
        return "$status"
    fi
    command pacman --cachedir "$cache" "${args[@]}"
}
