#!/usr/bin/env bash
# Load shared helpers; this file accepts only this operation's arguments.
# 加载公共辅助函数；此文件只接收本操作的参数。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

# Create a fresh root from a verified installation archive retained in persistent storage.
# 从持久存储中保留的经验证安装归档创建全新根。
# Arguments: none; declared [Distribution] settings are supplied through RSTAR_ variables.
# 参数：无；已声明的 [Distribution] 设置通过 RSTAR_ 变量提供。
bootstrap() {
    local architecture url signature_url keyring keyring_header path cache_key cache
    architecture=${RSTAR_DISTRIBUTION_ARCHITECTURE:-x86_64}
    [[ $architecture == x86_64 ]] || { log "Unsupported architecture: $architecture"; return 64; }
    [[ ${RSTAR_ROOT:-} == /* && -d $RSTAR_ROOT && ! -L $RSTAR_ROOT ]] || {
        log 'RSTAR_ROOT must be an existing absolute root directory'; return 64;
    }
    local -a entries
    shopt -s nullglob dotglob
    entries=("$RSTAR_ROOT"/*)
    shopt -u nullglob dotglob
    (( ${#entries[@]} == 0 )) || { log 'Bootstrap destination must be empty'; return 64; }
    keyring=${RSTAR_DISTRIBUTION_KEYRING:-/usr/share/pacman/keyrings/archlinux.gpg}
    [[ -f $keyring ]] || { log "Missing trusted Arch signing keyring: $keyring"; return 1; }
    url=${RSTAR_DISTRIBUTION_BOOTSTRAPURL:-https://geo.mirror.pkgbuild.com/iso/latest/archlinux-bootstrap-x86_64.tar.zst}
    signature_url=${RSTAR_DISTRIBUTION_BOOTSTRAPSIGNATUREURL:-$url.sig}
    [[ -d $plugin_root/state/bootstrap ]] || { log 'Missing persistent bootstrap directory'; return 1; }
    cache_key=$(printf '%s\0%s' "$url" "$signature_url" | sha256sum)
    cache="$plugin_root/state/bootstrap/${cache_key%% *}"
    bootstrap_tmp=$(mktemp -d "${RSTAR_PLUGIN_TMP:-${TMPDIR:-/tmp}}/arch-bootstrap.XXXXXXXX")
    trap 'rm -rf -- "$bootstrap_tmp"' EXIT
    # gpgv requires a binary keyring; Arch may ship ASCII-armored public keys.
    # gpgv 需要二进制密钥环；Arch 提供的公钥文件可能采用 ASCII 装甲格式。
    IFS= read -r keyring_header <"$keyring" || :
    if [[ $keyring_header == '-----BEGIN PGP PUBLIC KEY BLOCK-----'* ]]; then
        gpg --batch --no-options --homedir "$bootstrap_tmp" --dearmor \
            --output "$bootstrap_tmp/keyring.gpg" "$keyring"
        keyring="$bootstrap_tmp/keyring.gpg"
    fi
    if [[ -f $cache/rootfs.tar.zst && -f $cache/rootfs.sig ]]; then
        log "Using retained Arch bootstrap: $url"
    else
        log "Downloading Arch bootstrap: $url"
        curl --fail --location --retry 3 --output "$bootstrap_tmp/rootfs.tar.zst" "$url"
        curl --fail --location --retry 3 --output "$bootstrap_tmp/rootfs.sig" "$signature_url"
        gpgv --keyring "$keyring" "$bootstrap_tmp/rootfs.sig" "$bootstrap_tmp/rootfs.tar.zst" >&2
        mkdir -p -- "$cache"
        cp -- "$bootstrap_tmp/rootfs.tar.zst" "$bootstrap_tmp/rootfs.sig" "$cache/"
    fi
    # Verify retained files on every use, including with a changed trusted keyring.
    # 每次使用均验证保留的文件，包括可信密钥环变化后的情况。
    gpgv --keyring "$keyring" "$cache/rootfs.sig" "$cache/rootfs.tar.zst" >&2
    tar -tf "$cache/rootfs.tar.zst" >"$bootstrap_tmp/entries"
    while IFS= read -r path; do
        path=${path#./}
        case "$path" in
            root.x86_64|root.x86_64/|root.x86_64/*) ;;
            *) log "Unexpected bootstrap archive path: $path"; return 1 ;;
        esac
        case "/$path/" in */../*) log "Invalid bootstrap archive path: $path"; return 1 ;; esac
    done <"$bootstrap_tmp/entries"
    tar -xpf "$cache/rootfs.tar.zst" --strip-components=1 -C "$RSTAR_ROOT"
    if [[ -n ${RSTAR_DISTRIBUTION_MIRROR:-} ]]; then
        printf 'Server = %s\n' "$RSTAR_DISTRIBUTION_MIRROR" >"$RSTAR_ROOT/etc/pacman.d/mirrorlist"
    else
        cp -- "$plugin_root/data/mirrorlist" "$RSTAR_ROOT/etc/pacman.d/mirrorlist"
    fi
}

bootstrap "$@"
