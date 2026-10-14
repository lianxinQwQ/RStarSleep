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
        # Keep packages still required by installed old versions. A second pass
        # after upgrading the complete plan removes those obsolete dependencies.
        # 保留旧版本仍需的包；完整计划更新后再清理失去用途的旧依赖。
        pacman -R --unneeded --noconfirm -- "${obsolete[@]}" >&2 || return
    fi
}

maintain_packages() {
    local package output separator explicit requested_output
    local -a plan=() targets=() roots=() requested=() dependencies=() installed=() keyring_targets=()
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
    for package in "${plan[@]}"; do
        [[ -n ${root_names[$package]:-} ]] || dependencies+=("$package")
    done

    local -a install_plan=("${plan[@]}") install_options=(--needed)
    local target name index
    for target in "${targets[@]}" "${keyring_targets[@]}"; do
        [[ $target == */* ]] || continue
        install_options=()
        name=${target#*/}
        for index in "${!install_plan[@]}"; do
            [[ ${install_plan[$index]##*/} != "$name" ]] || install_plan[$index]=$(source_target "$target")
        done
    done
    maintenance_prune || return
    # Every retained package is an explicit transaction target, so old unrelated
    # packages are never upgraded merely because they remain installed.
    # 将所有保留包纳入更新事务，避免顺带升级尚未删除的无关旧软件。
    pacman -S "${install_options[@]}" --noconfirm --ask=4 -- "${install_plan[@]}" >&2 || return
    maintenance_prune || return
    pacman -D --asexplicit -- "${roots[@]}" >&2 || return
    if (( ${#dependencies[@]} )); then
        pacman -D --asdeps -- "${dependencies[@]}" >&2 || return
    fi

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
