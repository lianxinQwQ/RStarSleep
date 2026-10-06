#!/usr/bin/env bash
# Arch Linux operations invoked by the controller in the designated environment.
# 控制器在指定环境调用的 Arch Linux 操作。
set -euo pipefail
export LC_ALL=C

# Locate all resources relative to this entry script.
# 相对此入口脚本定位所有资源。
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}

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

# Create an empty Arch root using an authenticated official bootstrap archive.
# 使用经过认证的官方 bootstrap 压缩包创建空 Arch 系统根。
# Arguments: none; RSTAR_ROOT selects the existing empty destination subvolume.
# 参数：无参数；RSTAR_ROOT 指定已存在的空目标子卷。
bootstrap() {
    local architecture url signature_url keyring path
    architecture=${RSTAR_ARCH:-x86_64}
    [[ $architecture == x86_64 ]] || { log "Unsupported architecture: $architecture"; return 64; }
    [[ ${RSTAR_ROOT:-} == /* && -d $RSTAR_ROOT && ! -L $RSTAR_ROOT ]] || {
        log 'RSTAR_ROOT must be an existing absolute root directory'; return 64;
    }
    local -a entries
    shopt -s nullglob dotglob
    entries=("$RSTAR_ROOT"/*)
    shopt -u nullglob dotglob
    (( ${#entries[@]} == 0 )) || { log 'Bootstrap destination must be empty'; return 64; }
    keyring=${RSTAR_ARCH_KEYRING:-/usr/share/pacman/keyrings/archlinux.gpg}
    [[ -f $keyring ]] || { log "Missing trusted Arch signing keyring: $keyring"; return 1; }
    url=${RSTAR_ARCH_BOOTSTRAP_URL:-https://geo.mirror.pkgbuild.com/iso/latest/archlinux-bootstrap-x86_64.tar.zst}
    signature_url=${RSTAR_ARCH_BOOTSTRAP_SIGNATURE_URL:-$url.sig}
    bootstrap_tmp=$(mktemp -d "${RSTAR_PLUGIN_TMP:-${TMPDIR:-/tmp}}/arch-bootstrap.XXXXXXXX")
    trap 'rm -rf -- "$bootstrap_tmp"' EXIT
    log "Downloading Arch bootstrap: $url"
    curl --fail --location --retry 3 --output "$bootstrap_tmp/rootfs.tar.zst" "$url"
    curl --fail --location --retry 3 --output "$bootstrap_tmp/rootfs.sig" "$signature_url"
    gpgv --keyring "$keyring" "$bootstrap_tmp/rootfs.sig" "$bootstrap_tmp/rootfs.tar.zst" >&2
    tar -tf "$bootstrap_tmp/rootfs.tar.zst" >"$bootstrap_tmp/entries"
    while IFS= read -r path; do
        path=${path#./}
        case "$path" in
            root.x86_64|root.x86_64/|root.x86_64/*) ;;
            *) log "Unexpected bootstrap archive path: $path"; return 1 ;;
        esac
        case "/$path/" in */../*) log "Invalid bootstrap archive path: $path"; return 1 ;; esac
    done <"$bootstrap_tmp/entries"
    tar -xpf "$bootstrap_tmp/rootfs.tar.zst" --strip-components=1 -C "$RSTAR_ROOT"
    if [[ -n ${RSTAR_ARCH_MIRROR:-} ]]; then
        printf 'Server = %s\n' "$RSTAR_ARCH_MIRROR" >"$RSTAR_ROOT/etc/pacman.d/mirrorlist"
    else
        cp -- "$plugin_root/data/mirrorlist" "$RSTAR_ROOT/etc/pacman.d/mirrorlist"
    fi
}

# Remove orphan dependencies after the desired explicit targets have been established.
# 所需显式目标建立后，清理孤儿依赖。
# Arguments: none.
# 参数：无参数。
clean_orphans() {
    local output
    local -a packages=()
    output=$(filtered_query -Qdtq)
    if [[ -n $output ]]; then
        mapfile -t packages <<<"$output"
        pacman -Rns --noconfirm -- "${packages[@]}"
    fi
}

# Install the protected base target and discard unneeded bootstrap tools.
# 安装受保护的 base 目标，并删除不再需要的 bootstrap 工具。
# Arguments: none; runs within the target root through nspawn.
# 参数：无参数；由 nspawn 在目标系统根内运行。
base_install() {
    local output package
    local -a dependencies=()
    pacman-key --init
    pacman-key --populate archlinux
    pacman -Syu --needed --asexplicit --noconfirm -- base
    output=$(pacman -Qq)
    while IFS= read -r package; do
        [[ $package == base ]] || dependencies+=("$package")
    done <<<"$output"
    if (( ${#dependencies[@]} )); then
        pacman -D --asdeps -- "${dependencies[@]}"
    fi
    pacman -D --asexplicit -- base
    clean_orphans
}

# Report installed package names and normalized installation reasons.
# 输出已安装包名和规范化的安装原因。
# Arguments: none; output is package<TAB>explicit or package<TAB>dependency.
# 参数：无参数；输出为包名<TAB>explicit 或包名<TAB>dependency。
query_packages() {
    local installed explicit package reason
    local -A explicit_packages=()
    installed=$(pacman -Qq)
    explicit=$(filtered_query -Qqe)
    if [[ -n $explicit ]]; then
        while IFS= read -r package; do explicit_packages["$package"]=1; done <<<"$explicit"
    fi
    while IFS= read -r package; do
        reason=dependency
        [[ -z ${explicit_packages[$package]:-} ]] || reason=explicit
        printf '%s\t%s\n' "$package" "$reason"
    done <<<"$installed"
}

# Resolve concrete repository targets without including their transitive dependencies.
# 解析具体仓库目标，不将传递依赖混入结果。
# Arguments: each argument is a concrete package name, not a group or version expression.
# 参数：每个参数是具体软件包名，不是包组或版本表达式。
resolve_packages() {
    local package information
    validate_packages "$@"
    for package in "$@"; do
        information=$(pacman -Si -- "$package")
        awk '$1 == "Name" && $2 == ":" { print $3 }' <<<"$information"
    done
}

# Change installation reasons while protecting the base entry.
# 修改安装原因，同时保护 base 入口。
# Arguments: first explicit/dependency, followed by package names.
# 参数：首个参数为 explicit/dependency，后续为软件包名称。
mark_reason() {
    local reason=${1:-} package
    [[ $reason == explicit || $reason == dependency ]] || { log 'Expected explicit or dependency'; return 64; }
    shift
    validate_packages "$@"
    (( $# )) || return 0
    if [[ $reason == dependency ]]; then
        for package in "$@"; do
            [[ $package != base ]] || { log 'Cannot demote protected base package'; return 64; }
        done
        pacman -D --asdeps -- "$@"
    else
        pacman -D --asexplicit -- "$@"
    fi
}

# Dispatch one operation; bootstrap runs on the host, all others in nspawn.
# 分派一次操作；bootstrap 在宿主运行，其余操作在 nspawn 内运行。
# Arguments: operation name followed by its arguments.
# 参数：操作名称，后接该操作的参数。
main() {
    local operation=${1:-} package
    (( $# )) || { log 'Missing plugin operation'; return 64; }
    shift
    case "$operation" in
        bootstrap) bootstrap "$@" ;;
        base_install) base_install "$@" ;;
        update) pacman -Syu --noconfirm ;;
        query_packages) query_packages "$@" ;;
        resolve_packages) resolve_packages "$@" ;;
        install)
            validate_packages "$@"
            (( $# )) || return 0
            # Let libalpm replace conflicting installed packages in this dependency-checked transaction.
            # 允许 libalpm 在此次检查依赖的事务中替换冲突的已安装包。
            # Bit 4 answers only ALPM_QUESTION_CONFLICT_PKG; target validation precedes cache commit.
            # 位 4 仅应答 ALPM_QUESTION_CONFLICT_PKG；缓存提交前再次校验目标。
            pacman -S --needed --asexplicit --noconfirm --ask=4 -- "$@"
            ;;
        remove|remove_orphans)
            validate_packages "$@"
            (( $# )) || return 0
            for package in "$@"; do
                [[ $package != base ]] || { log 'Cannot remove protected base package'; return 64; }
            done
            if [[ $operation == remove ]]; then
                pacman -R --noconfirm -- "$@"
            else
                pacman -Rns --noconfirm -- "$@"
            fi
            ;;
        mark_reason) mark_reason "$@" ;;
        list_orphans) filtered_query -Qdtq ;;
        clean_orphans) clean_orphans "$@" ;;
        *) log "Unknown plugin operation: $operation"; return 64 ;;
    esac
}

main "$@"
