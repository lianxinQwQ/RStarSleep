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

source "${BASH_SOURCE[0]%/*}/keyrings.sh"

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

# Configured targets may select an explicit repository or AUR.
validate_targets() {
    local target
    for target in "$@"; do
        if [[ $target == */* ]]; then
            validate_packages "${target%%/*}" "${target#*/}" || return
        else
            validate_packages "$target" || return
        fi
    done
}

# AUR binaries are published in the managed local repository.
source_target() {
    if [[ $1 == aur/* ]]; then
        printf 'rstarsleep/%s\n' "${1#*/}"
    else
        printf '%s\n' "$1"
    fi
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
    output=$(filtered_query -Qdtq) || return
    if [[ -n $output ]]; then
        mapfile -t packages <<<"$output"
        pacman -Rns --noconfirm -- "${packages[@]}" || return
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

# Fill the mounted data directory without replacing existing configuration.
# 将默认文件补入挂载的 data 目录，保留已有配置。
prepare_plugin_data() {
    local resources=${1:-${RSTAR_PLUGIN_ROOT:-${plugin_root:-}}}
    [[ -d $resources/data ]] || { log 'Missing persistent data mount'; return 1; }
    if [[ -d $resources/default-data ]]; then
        cp -r --preserve=mode --update=none -- "$resources/default-data/." "$resources/data/" || return
        chmod 0755 -- "$resources/data" || return
    fi
}

# Generate readable runtime configuration without writing to the system /etc.
# 生成可读的运行时配置，不写入系统 /etc。
# Arguments: writable output directory; paths in Include must be absolute.
# 参数：可写输出目录；Include 中的路径必须为绝对路径。
write_pacman_configuration() {
    local directory=$1 resources=${RSTAR_PLUGIN_ROOT:-${plugin_root:-}} mirror line
    [[ -n $resources ]] || { log 'Missing plugin resource directory'; return 64; }
    resources=$(cd -- "$resources" && pwd -P) || return
    if [[ ${rstar_local_query:-0} != 1 ]]; then
        prepare_plugin_data "$resources" || return
    fi
    [[ -f $resources/data/pacman.conf.in && -f $resources/data/mirrorlist ]] || {
        log 'Missing plugin pacman template or mirrorlist'; return 1;
    }
    mirror=${RSTAR_DISTRIBUTION_MIRROR:-${RSTAR_MIRROR:-}}
    if [[ $mirror == *$'\n'* || $mirror == *$'\r'* ]]; then
        log 'Mirror must not contain line breaks'; return 64
    fi
    mkdir -p -- "$directory" || return
    directory=$(cd -- "$directory" && pwd -P) || return
    if [[ $resources == *$'\n'* || $resources == *$'\r'* || $directory == *$'\n'* || $directory == *$'\r'* ]]; then
        log 'Pacman configuration paths must not contain line breaks'; return 64
    fi
    if [[ -n $mirror ]]; then
        printf 'Server = %s\n' "$mirror" >"$directory/mirrorlist" || return
    else
        cp -- "$resources/data/mirrorlist" "$directory/mirrorlist" || return
    fi
    # Quoted literal replacement keeps ampersands, backslashes and pacman's
    # $repo/$arch variables intact; no configuration is evaluated as shell code.
    # 按字面替换，保留路径及 pacman 的变量，不把配置作为 shell 代码执行。
    while IFS= read -r line || [[ -n $line ]]; do
        line=${line//@MIRRORLIST@/"$directory/mirrorlist"}
        line=${line//@PLUGIN_ROOT@/"$resources"}
        printf '%s\n' "$line" || return
    done <"$resources/data/pacman.conf.in" >"$directory/pacman.conf" || return
    chmod 0755 -- "$directory" || return
    chmod 0644 -- "$directory/pacman.conf" "$directory/mirrorlist"
}

# Resolve only configured, enabled repository keyring roots; no trust changes.
configured_repository_keyring_targets() {
    local resources=${RSTAR_PLUGIN_ROOT:-${plugin_root:-}} directory status=0
    [[ -s $resources/data/repository-keyrings.conf ]] || return 0
    directory=$(mktemp -d "${RSTAR_PLUGIN_TMP:-${TMPDIR:-/tmp}}/arch-keyring-config.XXXXXXXX") || return
    if write_pacman_configuration "$directory" && repository_keyring_targets "$resources/data/repository-keyrings.conf" "$directory/pacman.conf"; then
        :
    else
        status=$?
    fi
    rm -rf -- "$directory" || return
    return "$status"
}

# Execute a transaction with an already generated configuration. All failures
# must propagate even when the caller uses an if/|| conditional.
# 使用生成的配置执行事务，即使调用者处于 if/|| 条件中也传播全部失败。
run_configured_pacman() {
    local configuration=$1
    shift
    local resources=${RSTAR_PLUGIN_ROOT:-${plugin_root:-}}
    local cache=$resources/state/packages
    local repository=$resources/state/repository
    local -a args=("$@")
    local -a cache_options=()
    local repository_keyrings_refreshed=0 keyring_mode=reuse
    # Local queries need neither writable shared caches nor synchronized repositories.
    if [[ ${args[0]:-} == -Q* ]]; then
        command pacman --config "$configuration" "${args[@]}"
        return
    fi
    if [[ -d $cache ]]; then
        prepare_shared_cache "$cache" "$repository" || return
        cache_options=(--cachedir "$cache")
    fi
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
        printf '\n[rstarsleep]\nSigLevel = Optional TrustAll\nServer = file://%s\n' "$repository" >>"$configuration" || return
        mkdir -p /var/lib/pacman/sync || return
        cp --remove-destination -- "$repository/rstarsleep.db.tar.gz" /var/lib/pacman/sync/rstarsleep.db || return
    fi
    case ${args[0]:-} in
        -S|-Su|-Syu|-Sy|-Syyu|-Syy|-Sw)
            [[ ${args[0]} != *y* ]] || keyring_mode=refresh
            prepare_repository_keyrings "$configuration" "$keyring_mode" || return
            if (( repository_keyrings_refreshed )); then args[0]=${args[0]//y/}; fi
            ;;
    esac
    command pacman --config "$configuration" "${cache_options[@]}" "${args[@]}"
}

# One configuration source for every operation, including calls without caches.
# 所有操作使用同一配置来源，包括未挂载共享缓存时的调用。
pacman() {
    local directory status=0 cleanup_status=0 rstar_local_query=0
    [[ ${1:-} != -Q* ]] || rstar_local_query=1
    directory=$(mktemp -d "${RSTAR_PLUGIN_TMP:-${TMPDIR:-/tmp}}/arch-pacman.XXXXXXXX") || return
    if write_pacman_configuration "$directory" && run_configured_pacman "$directory/pacman.conf" "$@"; then
        :
    else
        status=$?
    fi
    rm -rf -- "$directory" || cleanup_status=$?
    (( status )) || status=$cleanup_status
    return "$status"
}
