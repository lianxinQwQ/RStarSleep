#!/usr/bin/env bash
# The controller supplies an exact closure and the original package/group targets.
# 控制器提供完整安装闭包以及原始软件包/软件组目标。

# Query and validate the current inventory. Bash's dynamic scope shares the
# operation's installed array and plan_names map with these private helpers.
# 查询并校验当前清单；这些内部函数共享维护操作的数组和计划映射。
maintenance_inventory() {
    local output package
    local -A seen=()
    output=$(filtered_query -Qq) || return
    installed=()
    [[ -n $output ]] || return 0
    while IFS= read -r package; do
        validate_packages "$package" || return
        [[ -z ${seen[$package]:-} ]] || { log "Duplicate installed package: $package"; return 1; }
        seen["$package"]=1
        installed+=("$package")
    done <<<"$output"
}

maintenance_prune() {
    local package
    local -a obsolete=()
    maintenance_inventory || return
    for package in "${installed[@]}"; do
        [[ -n ${plan_names[$package]:-} ]] || obsolete+=("$package")
    done
    if (( ${#obsolete[@]} )); then
        # Remove optional dependencies and cycles missed by orphan cleanup, but
        # keep packages still required by retained installed versions.
        # 删除孤儿清理遗漏的可选依赖和依赖环，保留当前版本仍需要的包。
        pacman -R --unneeded --noconfirm -- "${obsolete[@]}" >&2 || return
    fi
}

# Normalize only changed reasons before cleanup. Newly installed dependencies
# and system upgrades retain pacman's natural installation reasons.
# 清理前仅修改发生变化的安装原因；新依赖及系统升级沿用 pacman 的自然标记。
maintenance_reasons() {
    local package output
    local -a promote=() demote=()
    local -A current_names=() current_explicit=()
    maintenance_inventory || return
    for package in "${installed[@]}"; do current_names[$package]=1; done
    output=$(filtered_query -Qqe) || return
    if [[ -n $output ]]; then
        while IFS= read -r package; do
            validate_packages "$package" || return
            [[ -n ${current_names[$package]:-} && -z ${current_explicit[$package]:-} ]] || {
                log "Invalid explicit package query result: $package"; return 1;
            }
            current_explicit[$package]=1
        done <<<"$output"
    fi
    for package in "${installed[@]}"; do
        if [[ -n ${root_names[$package]:-} ]]; then
            [[ -n ${current_explicit[$package]:-} ]] || promote+=("$package")
        elif [[ -n ${current_explicit[$package]:-} ]]; then
            demote+=("$package")
        fi
    done
    if (( ${#promote[@]} )); then
        pacman -D --asexplicit -- "${promote[@]}" >&2 || return
    fi
    if (( ${#demote[@]} )); then
        pacman -D --asdeps -- "${demote[@]}" >&2 || return
    fi
}

# Remember an explicitly selected source after it has been applied once. Local
# descriptor timestamps detect subsequent reinstalls, including same-version ones.
source_target_identity() (
    local package=$1 source=$2 root=${RSTAR_ROOT:-} installed version selected selected_version descriptor
    [[ $root == /* && -d $root ]] || exit 1
    installed=$(pacman -Q -- "$package") || exit
    [[ $installed == "$package "* && $installed != *$'\n'* ]] || exit 1
    version=${installed#"$package "}
    [[ $version != */* && -n $version ]] || exit 1
    selected=$(pacman -Si -- "$source") || exit
    selected_version=$(awk '$1 == "Version" && $2 == ":" {print $3}' <<<"$selected")
    [[ $version == "$selected_version" ]] || exit 1
    descriptor=${root%/}/var/lib/pacman/local/$package-$version/desc
    [[ -f $descriptor && ! -L $descriptor ]] || exit 1
    {
        printf '%s\n%s\n' "$source" "$selected"
        stat -c '%s:%y:%z' -- "$descriptor" || exit
        sha256sum -- "$descriptor" || exit
    } | sha256sum
)

source_target_matches() {
    local package=$1 source=$2 root=${RSTAR_ROOT:-} identity receipt
    receipt=${root%/}/var/lib/pacman/.rstarsleep-sources/$package
    [[ -n $root && -f $receipt && ! -L $receipt ]] || return 1
    identity=$(source_target_identity "$package" "$source") || return 1
    [[ $(<"$receipt") == "$identity" ]]
}

save_source_target() {
    local package=$1 source=$2 root=${RSTAR_ROOT:-} identity directory temporary
    # Unknown/custom DB paths retain conservative reinstallation semantics.
    identity=$(source_target_identity "$package" "$source") || return 0
    directory=${root%/}/var/lib/pacman/.rstarsleep-sources
    mkdir -p -- "$directory" || return
    temporary=$(mktemp "$directory/.source.XXXXXXXX") || return
    printf '%s\n' "$identity" >"$temporary" && mv -f -- "$temporary" "$directory/$package" || {
        rm -f -- "$temporary"; return 1;
    }
}

maintain_packages() {
    local package output separator explicit requested_output
    local -a plan=() targets=() roots=() requested=() installed=() keyring_targets=()
    local -A plan_names=() root_names=() actual_names=() explicit_names=()
    local -A requested_names=()
    local keyring_output keyring_target

    keyring_output=$(configured_repository_keyring_targets) || return
    while IFS= read -r keyring_target; do
        [[ -n $keyring_target ]] || continue
        keyring_targets+=("$keyring_target")
    done <<<"$keyring_output"

    [[ ${1:-} == --plan ]] || { log 'Expected --plan PACKAGES --targets TARGETS'; return 64; }
    shift
    while (( $# )) && [[ $1 != --targets ]]; do plan+=("$1"); shift; done
    [[ ${1:-} == --targets ]] || { log 'Missing --targets separator'; return 64; }
    shift
    targets=("$@")
    validate_packages "${plan[@]}" || return
    validate_targets "${targets[@]}" || return
    for package in "${plan[@]}"; do
        [[ -z ${plan_names[$package]:-} ]] || { log "Duplicate planned package: $package"; return 64; }
        plan_names["$package"]=1
    done
    [[ -n ${plan_names[base]:-} ]] || { log 'Package plan must contain protected base'; return 64; }

    # The preparation stage supplies matching repository metadata even when
    # downloads are disabled. Do not refresh it and invalidate the frozen plan.
    # 准备阶段即使关闭预下载也提供对应仓库元数据，不在此刷新已冻结计划。
    # Repository keyrings are retained by this operation without changing what
    # the controller's base target means when inspecting an older stage cache.
    requested_output=$(bash "$plugin_root/resolve_packages.sh" base "${targets[@]}") || return
    output=$requested_output
    while IFS= read -r package; do
        validate_packages "$package" || return
        [[ $package == base ]] || requested_names[$package]=1
    done <<<"$requested_output"
    if (( ${#keyring_targets[@]} )); then
        keyring_output=$(bash "$plugin_root/resolve_packages.sh" "${keyring_targets[@]}") || return
        output+=$'\n'$keyring_output
    fi
    [[ -n $output ]] || { log 'No protected base target resolved'; return 1; }
    while IFS= read -r package; do
        validate_packages "$package" || return
        [[ -n ${plan_names[$package]:-} ]] || { log "Resolved target is absent from package plan: $package"; return 1; }
        [[ -z ${root_names[$package]:-} ]] || continue
        root_names["$package"]=1
        roots+=("$package")
        [[ -z ${requested_names[$package]:-} ]] || requested+=("$package")
    done <<<"$output"
    [[ -n ${root_names[base]:-} ]] || { log 'Protected base target did not resolve to base'; return 1; }

    local -a install_targets=() install_options=(--needed)
    local -A source_targets=() installed_names=()
    local target name
    for target in "${targets[@]}" "${keyring_targets[@]}"; do
        [[ $target == */* ]] || continue
        install_options=()
        name=${target#*/}
        source_targets[$name]=$(source_target "$target") || return
    done
    maintenance_reasons || return
    clean_orphan_dependencies >&2 || return
    maintenance_prune || return
    # Compare after cleanup, since an orphan needed by a new root may have been
    # removed. Apply changed sources at the same version; reuse unchanged receipts.
    # 清理后再比较；新目标所需的孤儿允许重装，限定来源变化仍支持同版本切换。
    for package in "${installed[@]}"; do installed_names[$package]=1; done
    for package in "${roots[@]}"; do
        if [[ -n ${source_targets[$package]:-} ]]; then
            if ! source_target_matches "$package" "${source_targets[$package]}"; then
                install_targets+=("${source_targets[$package]}")
            fi
        elif [[ -z ${installed_names[$package]:-} ]]; then
            install_targets+=("$package")
        fi
    done
    if (( ${#install_targets[@]} )); then
        pacman -S "${install_options[@]}" --noconfirm --ask=4 -- "${install_targets[@]}" >&2 || return
    fi
    # The wrapper reuses prepared repository databases without refreshing them.
    # Sysupgrade preserves dependency reasons; do not pass --asexplicit here.
    # 包装函数复用已准备的仓库数据库；集体升级保留依赖原因，不传 --asexplicit。
    pacman -Syu --noconfirm --ask=4 >&2 || return
    clean_orphan_dependencies >&2 || return
    maintenance_prune || return

    maintenance_inventory || return
    for package in "${installed[@]}"; do
        [[ -n ${plan_names[$package]:-} ]] || { log "Unexpected installed package after maintenance: $package"; return 1; }
        actual_names["$package"]=1
    done
    for package in "${plan[@]}"; do
        [[ -n ${actual_names[$package]:-} ]] || { log "Planned package missing after maintenance: $package"; return 1; }
    done
    output=$(filtered_query -Qqe) || return
    if [[ -n $output ]]; then
        while IFS= read -r package; do
            validate_packages "$package" || return
            [[ -n ${actual_names[$package]:-} && -z ${explicit_names[$package]:-} ]] || {
                log "Invalid explicit package query result: $package"; return 1;
            }
            explicit_names["$package"]=1
        done <<<"$output"
    fi
    for package in "${installed[@]}"; do
        [[ ${root_names[$package]:-} == "${explicit_names[$package]:-}" ]] || {
            log "Incorrect installation reason after maintenance: $package"; return 1;
        }
    done

    for package in "${!source_targets[@]}"; do
        save_source_target "$package" "${source_targets[$package]}" || return
    done

    # Validated package names need no JSON escaping. Only this final state goes
    # to stdout; package-manager progress and diagnostics remain on stderr.
    # 已校验包名无需 JSON 转义；仅最终状态写 stdout，包管理日志写 stderr。
    printf '{"requested":['
    separator=
    for package in "${requested[@]}"; do
        printf '%s"%s"' "$separator" "$package"
        separator=,
    done
    printf '],"installed":['
    separator=
    for package in "${installed[@]}"; do
        explicit=false
        [[ -z ${explicit_names[$package]:-} ]] || explicit=true
        printf '%s{"name":"%s","explicit":%s}' "$separator" "$package" "$explicit"
        separator=,
    done
    printf ']}\n'
}
