# Explicit repository trust for unattended package operations. Never evaluate configuration.
# 无人值守包操作的显式仓库信任；不把配置作为 shell 代码执行。

load_repository_keyrings() {
    local configuration=$1 line section= field value fingerprint seen_key number=0
    local section_pattern='^\[([A-Za-z0-9@_+][A-Za-z0-9@._+-]*)\]$'
    local -a fingerprints=()
    keyring_repositories=()
    keyring_packages=()
    keyring_bootstrap_keys=()
    keyring_servers=()
    local -A fields=()
    [[ -e $configuration ]] || return 0
    [[ -f $configuration && -r $configuration ]] || { log "Unreadable repository keyring configuration: $configuration"; return 64; }
    while IFS= read -r line || [[ -n $line ]]; do
        (( number += 1 ))
        line=${line%%#*}
        line=${line#"${line%%[![:space:]]*}"}
        line=${line%"${line##*[![:space:]]}"}
        [[ -n $line ]] || continue
        if [[ $line =~ $section_pattern ]]; then
            section=${BASH_REMATCH[1]}
            [[ $section != options && $section != rstarsleep && -z ${fields[$section/section]:-} ]] || {
                log "Invalid or duplicate repository section at $configuration:$number"; return 64;
            }
            fields[$section/section]=1
            keyring_repositories+=("$section")
            continue
        fi
        [[ -n $section && $line == *=* ]] || { log "Invalid repository keyring setting at $configuration:$number"; return 64; }
        field=${line%%=*}
        field=${field%"${field##*[![:space:]]}"}
        value=${line#*=}
        value=${value#"${value%%[![:space:]]*}"}
        case $field in
            keyring_package|bootstrap_keys|keyserver) ;;
            *) log "Unknown repository keyring setting at $configuration:$number: $field"; return 64 ;;
        esac
        seen_key=$section/$field
        [[ -n $value && -z ${fields[$seen_key]:-} ]] || { log "Empty or duplicate $field at $configuration:$number"; return 64; }
        fields[$seen_key]=1
        case $field in
            keyring_package)
                validate_packages "$value" || return
                [[ $value != base ]] || { log 'The protected base package cannot be used as a repository keyring'; return 64; }
                keyring_packages[$section]=$value
                ;;
            bootstrap_keys)
                read -r -a fingerprints <<<"$value"
                value=
                for fingerprint in "${fingerprints[@]}"; do
                    [[ $fingerprint =~ ^([[:xdigit:]]{40}|[[:xdigit:]]{64})$ ]] || {
                        log "Repository $section requires full primary key fingerprints (40 or 64 hex digits): $fingerprint"; return 64;
                    }
                    value+="${fingerprint^^} "
                done
                keyring_bootstrap_keys[$section]=${value% }
                ;;
            keyserver)
                [[ $value =~ ^hkps://[^[:space:]]+$ ]] || { log "Invalid HKPS keyserver for repository $section"; return 64; }
                keyring_servers[$section]=$value
                ;;
        esac
    done <"$configuration"
    for section in "${keyring_repositories[@]}"; do
        [[ -n ${keyring_packages[$section]:-} && -n ${keyring_bootstrap_keys[$section]:-} ]] || {
            log "Repository $section requires keyring_package and bootstrap_keys in $configuration"; return 64;
        }
    done
}

# Also used by the resolver so keyring packages survive exact-plan maintenance.
repository_keyring_targets() {
    local -a keyring_repositories=()
    local -A keyring_packages=() keyring_bootstrap_keys=() keyring_servers=()
    local repository repositories
    local -A enabled=()
    load_repository_keyrings "$1" || return
    (( ${#keyring_repositories[@]} )) || return 0
    if [[ -n ${2:-} ]]; then
        repositories=$(pacman-conf --config "$2" --repo-list) || return
        while IFS= read -r repository; do [[ -z $repository ]] || enabled[$repository]=1; done <<<"$repositories"
    fi
    for repository in "${keyring_repositories[@]}"; do
        [[ -z ${2:-} || -n ${enabled[$repository]:-} ]] || continue
        printf '%s/%s\n' "$repository" "${keyring_packages[$repository]}"
    done
}

# Only a primary fingerprint establishes trust; subkey IDs must never match here.
# Return 2 for a revoked, expired or disabled primary key, and 1 if missing.
repository_key_status() {
    local gpgdir=$1 fingerprint=$2 details kind primary=0
    local -a columns=()
    details=$(gpg --no-options --homedir "$gpgdir" --batch --with-colons --fingerprint --list-keys "$fingerprint" 2>/dev/null) || return 1
    while IFS=: read -r -a columns; do
        kind=${columns[0]:-}
        if [[ $kind == pub ]]; then
            primary=1
            if [[ ${columns[1]:-} == r || ${columns[1]:-} == e || ${columns[1]:-} == d || ${columns[11]:-} == *D* ]]; then
                return 2
            fi
        elif [[ $kind == sub ]]; then
            primary=0
        elif [[ $kind == fpr && $primary == 1 && ${columns[9]:-} == "$fingerprint" ]]; then
            return 0
        fi
    done <<<"$details"
    return 1
}

prepare_repository_keyrings() {
    local configuration=$1 mode=${2:-reuse} resources=${RSTAR_PLUGIN_ROOT:-${plugin_root:-}}
    local -a keyring_repositories=() targets=() packages=() active_repositories=() fingerprints=() keyrings=() cache_options=()
    local -A keyring_packages=() keyring_bootstrap_keys=() keyring_servers=() enabled=() names=()
    local -a key_options=()
    local repository package repositories gpgdir fingerprint status key_cache temporary owned path name package_keyring_count signature_policy
    repository_keyrings_refreshed=0
    [[ $mode == refresh || $mode == reuse ]] || { log "Invalid keyring preparation mode: $mode"; return 64; }
    load_repository_keyrings "$resources/data/repository-keyrings.conf" || return
    (( ${#keyring_repositories[@]} )) || return 0
    repositories=$(pacman-conf --config "$configuration" --repo-list) || return
    while IFS= read -r repository; do
        [[ -z $repository ]] || enabled[$repository]=1
    done <<<"$repositories"
    for repository in "${keyring_repositories[@]}"; do
        [[ -n ${enabled[$repository]:-} ]] || continue
        signature_policy=$(pacman-conf --config "$configuration" --repo "$repository" SigLevel) || return
        if [[ -z $signature_policy ]]; then
            signature_policy=$(pacman-conf --config "$configuration" SigLevel) || return
        fi
        if [[ $'\n'$signature_policy$'\n' != *$'\nPackageRequired\n'* || $'\n'$signature_policy$'\n' != *$'\nPackageTrustedOnly\n'* ]]; then
            log "Repository $repository keyring bootstrap requires PackageRequired and PackageTrustedOnly signature policy"; return 64
        fi
        active_repositories+=("$repository")
        targets+=("$repository/${keyring_packages[$repository]}")
        packages+=("${keyring_packages[$repository]}")
    done
    (( ${#targets[@]} )) || return 0
    gpgdir=$(pacman-conf --config "$configuration" GPGDir) || return
    [[ $gpgdir == /* && $gpgdir != *$'\n'* ]] || { log 'Repository keyring preparation requires one absolute GPGDir'; return 64; }
    key_options=(--config "$configuration" --gpgdir "$gpgdir")
    log 'Preparing explicitly configured repository signing keyrings'
    pacman-key "${key_options[@]}" --init >&2 || return
    key_cache=$resources/state/repository/bootstrap-keys
    for repository in "${active_repositories[@]}"; do
        read -r -a fingerprints <<<"${keyring_bootstrap_keys[$repository]}"
        for fingerprint in "${fingerprints[@]}"; do
            status=0
            repository_key_status "$gpgdir" "$fingerprint" || status=$?
            if (( status == 2 )); then
                log "Repository $repository bootstrap key is revoked, expired or disabled: $fingerprint"; return 1
            fi
            if (( status )); then
                if [[ -s $key_cache/$fingerprint.gpg ]]; then
                    pacman-key "${key_options[@]}" --add "$key_cache/$fingerprint.gpg" >&2 || return
                else
                    log "Fetching repository $repository bootstrap key: $fingerprint"
                    timeout --kill-after=5s 60s pacman-key "${key_options[@]}" --keyserver "${keyring_servers[$repository]:-hkps://keyserver.ubuntu.com}" --recv-keys "$fingerprint" >&2 || {
                        log "Cannot fetch repository $repository bootstrap key: $fingerprint"; return 1;
                    }
                fi
                repository_key_status "$gpgdir" "$fingerprint" || {
                    log "Repository $repository bootstrap key fingerprint is missing, mismatched or unusable: $fingerprint"; return 1;
                }
            fi
            pacman-key "${key_options[@]}" --lsign-key "$fingerprint" >&2 || return
            # Retain public material for target builds, including offline builds.
            if [[ -d $resources/state/repository ]]; then
                mkdir -p -- "$key_cache" || return
                chmod 0755 -- "$key_cache" || return
                temporary=$(mktemp "$key_cache/.key.XXXXXXXX") || return
                if gpg --no-options --homedir "$gpgdir" --batch --export "$fingerprint" >"$temporary" && [[ -s $temporary ]]; then
                    chmod 0644 -- "$temporary" && mv -f -- "$temporary" "$key_cache/$fingerprint.gpg" || { rm -f -- "$temporary"; return 1; }
                else
                    rm -f -- "$temporary"; log "Cannot cache repository bootstrap public key: $fingerprint"; return 1
                fi
            fi
        done
    done
    [[ ! -d $resources/state/packages ]] || cache_options=(--cachedir "$resources/state/packages")
    local sync=-S
    [[ $mode != refresh ]] || sync=-Sy
    # Bypass the wrapper to avoid recursion. Keep all configured signature checks.
    command pacman --config "$configuration" "${cache_options[@]}" --logfile /dev/null "$sync" --needed --noconfirm -- "${targets[@]}" >&2 || return
    for package in "${packages[@]}"; do
        owned=$(command pacman --config "$configuration" -Qlq -- "$package") || return
        package_keyring_count=0
        while IFS= read -r path; do
            [[ $path == /usr/share/pacman/keyrings/*.gpg && ${path#/usr/share/pacman/keyrings/} != */* ]] || continue
            name=${path##*/}
            name=${name%.gpg}
            validate_packages "$name" || return
            (( package_keyring_count += 1 ))
            if [[ -z ${names[$name]:-} ]]; then names[$name]=1; keyrings+=("$name"); fi
        done <<<"$owned"
        (( package_keyring_count )) || { log "Keyring package $package owns no /usr/share/pacman/keyrings/*.gpg files"; return 1; }
    done
    (( ${#keyrings[@]} )) || { log 'Configured keyring packages own no /usr/share/pacman/keyrings/*.gpg files'; return 1; }
    pacman-key "${key_options[@]}" --populate "${keyrings[@]}" >&2 || return
    [[ $mode != refresh ]] || repository_keyrings_refreshed=1
    return 0
}
