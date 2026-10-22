#!/usr/bin/env bash
# Package the core's early payload with the target's Arch initramfs implementation.
# 使用目标 Arch 的 initramfs 实现打包核心提供的早期执行器。
set -euo pipefail
plugin_root=${RSTAR_PLUGIN_ROOT:-$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)}
source "$plugin_root/lib/common.sh"

(( $# == 0 )) || { log 'prepare_inheritance_initramfs 不接受参数 / prepare_inheritance_initramfs takes no arguments'; exit 64; }
[[ ${RSTAR_ROOT:-} == /* && -d $RSTAR_ROOT && ! -L $RSTAR_ROOT ]] || {
    log 'prepare_inheritance_initramfs 要求 RSTAR_ROOT 为现有根目录的绝对路径 / prepare_inheritance_initramfs requires an existing absolute RSTAR_ROOT'; exit 64;
}
case "$RSTAR_ROOT/" in
    *'/../'*|*'/./'*) log 'RSTAR_ROOT 不能包含路径遍历分量 / RSTAR_ROOT must not contain traversal components'; exit 64 ;;
esac
root=${RSTAR_ROOT%/}

# Reject links and special files before installing resources or writing the image.
# 安装资源或写入镜像前拒绝符号链接及特殊文件。
for relative in usr usr/lib usr/lib/initcpio usr/lib/initcpio/install usr/lib/initcpio/hooks \
    usr/lib/rstarsleep usr/lib/systemd usr/lib/systemd/system \
    usr/lib/systemd/system/sysroot.mount.d boot; do
    target=$root/$relative
    [[ ! -L $target && ( ! -e $target || -d $target ) ]] || {
        log "Initramfs 目标必须是真实目录：$relative / Initramfs destination must be a real directory: $relative"; exit 64;
    }
done
for relative in usr/lib/initcpio/install/rstarsleep usr/lib/initcpio/hooks/rstarsleep \
    usr/lib/rstarsleep/mkinitcpio.conf usr/lib/rstarsleep/mkinitcpio-hooks.conf \
    usr/lib/systemd/system/rstarsleep-inheritance.service \
    usr/lib/systemd/system/sysroot.mount.d/50-rstarsleep.conf boot/initramfs-linux.img; do
    target=$root/$relative
    [[ ! -L $target && ( ! -e $target || -f $target ) ]] || {
        log "Initramfs 目标必须是普通文件：$relative / Initramfs destination must be a regular file: $relative"; exit 64;
    }
done
[[ -f $root/boot/vmlinuz-linux && ! -L $root/boot/vmlinuz-linux ]] || {
    log 'Arch 启动内核必须是普通文件 /boot/vmlinuz-linux / Arch boot kernel must be a regular /boot/vmlinuz-linux'; exit 64;
}

install -Dm755 -- "$plugin_root/initramfs/install" "$root/usr/lib/initcpio/install/rstarsleep"
install -Dm755 -- "$plugin_root/initramfs/hook" "$root/usr/lib/initcpio/hooks/rstarsleep"
install -Dm644 -- "$plugin_root/initramfs/mkinitcpio.conf" "$root/usr/lib/rstarsleep/mkinitcpio.conf"
install -Dm644 -- "$plugin_root/initramfs/config" "$root/usr/lib/rstarsleep/mkinitcpio-hooks.conf"
install -Dm644 -- "$plugin_root/initramfs/rstarsleep-inheritance.service" \
    "$root/usr/lib/systemd/system/rstarsleep-inheritance.service"
install -Dm644 -- "$plugin_root/initramfs/sysroot.conf" \
    "$root/usr/lib/systemd/system/sysroot.mount.d/50-rstarsleep.conf"

# The runner already executes this action inside the target with both streams piped.
# runner 已在目标系统内执行此操作，并捕获两个输出流。
mkinitcpio -c "$root/usr/lib/rstarsleep/mkinitcpio.conf" \
    -k "$root/boot/vmlinuz-linux" -g "$root/boot/initramfs-linux.img"
[[ -s $root/boot/initramfs-linux.img && ! -L $root/boot/initramfs-linux.img \
    && -f $root/boot/initramfs-linux.img ]] || {
    log 'mkinitcpio 未生成非空的普通文件 /boot/initramfs-linux.img / mkinitcpio did not produce a regular, nonempty /boot/initramfs-linux.img'; exit 1;
}
