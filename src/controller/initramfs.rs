//! Install a self-contained early-boot executor and regenerate Arch initramfs.
use super::{Error, Result, fs as disk};
use std::os::unix::fs::PermissionsExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn install(root: &Path, relative: &str, text: &str, executable: bool) -> Result<()> {
    let target = root.join(relative);
    disk::directory(target.parent().unwrap())?;
    disk::write_text(&target, text)?;
    fs::set_permissions(
        &target,
        fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
    )
    .map_err(|e| disk::io(&target, e))
}

pub(super) fn prepare(root: &Path) -> Result<()> {
    disk::checked(root)?;
    let compiler = root.join("usr/bin/mkinitcpio");
    if !compiler.is_file() {
        return Err(Error::Invalid(
            "Timing=initramfs 当前需要目标系统安装 mkinitcpio、btrfs-progs 和 GNU coreutils".into(),
        ));
    }
    let runtime = root.join("usr/lib/rstarsleep/early-runtime");
    disk::directory(&runtime.join("bin"))?;
    disk::directory(&runtime.join("lib"))?;
    let executable = std::env::current_exe().map_err(|e| disk::io(root, e))?;
    let engine = runtime.join("bin/rstarsleep");
    disk::checked(&engine)?;
    fs::copy(&executable, &engine).map_err(|e| disk::io(&engine, e))?;
    let loader = bundle_dependencies(&executable, &runtime.join("lib"))?;
    if let Some(loader) = loader {
        disk::write_text(&runtime.join("loader-name"), &loader)?;
    } else if runtime.join("loader-name").exists() {
        fs::remove_file(runtime.join("loader-name")).map_err(|e| disk::io(&runtime, e))?;
    }
    install(
        root,
        "usr/lib/rstarsleep/early-runtime/run-rstarsleep",
        include_str!("../../presets/initramfs/run-rstarsleep"),
        true,
    )?;
    install(
        root,
        "usr/lib/rstarsleep/initramfs-finalize",
        include_str!("../../presets/initramfs/finalize"),
        true,
    )?;
    install(
        root,
        "usr/lib/initcpio/install/rstarsleep",
        include_str!("../../presets/initramfs/install"),
        true,
    )?;
    install(
        root,
        "usr/lib/initcpio/hooks/rstarsleep",
        include_str!("../../presets/initramfs/hook"),
        true,
    )?;
    install(
        root,
        "usr/lib/systemd/system/rstarsleep-inheritance.service",
        include_str!("../../presets/initramfs/rstarsleep-inheritance.service"),
        false,
    )?;
    install(
        root,
        "usr/lib/systemd/system/sysroot.mount.d/50-rstarsleep.conf",
        include_str!("../../presets/initramfs/sysroot.conf"),
        false,
    )?;
    // Use a dedicated compiler config, preserving /etc/mkinitcpio.conf and its drop-ins.
    install(
        root,
        "usr/lib/rstarsleep/mkinitcpio.conf",
        "source /etc/mkinitcpio.conf\nfor rstarsleep_config in /etc/mkinitcpio.conf.d/*.conf; do\n    [[ -f \"$rstarsleep_config\" ]] && source \"$rstarsleep_config\"\ndone\nsource /usr/lib/rstarsleep/mkinitcpio-hooks.conf\n",
        false,
    )?;
    install(
        root,
        "usr/lib/rstarsleep/mkinitcpio-hooks.conf",
        include_str!("../../presets/initramfs/config"),
        false,
    )?;
    // Explicit paths match boot publication and avoid presets overriding the dedicated config.
    let output = Command::new("systemd-nspawn")
        .args(["--quiet", "--register=no", "--directory"])
        .arg(root)
        .args([
            "--",
            "/usr/bin/mkinitcpio",
            "-c",
            "/usr/lib/rstarsleep/mkinitcpio.conf",
            "-k",
            "/boot/vmlinuz-linux",
            "-g",
            "/boot/initramfs-linux.img",
        ])
        .output()
        .map_err(|e| disk::io(Path::new("systemd-nspawn"), e))?;
    if !output.status.success() {
        return Err(Error::Command {
            command: "mkinitcpio RStarSleep inheritance".into(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    Ok(())
}

fn bundle_dependencies(executable: &Path, destination: &Path) -> Result<Option<String>> {
    let output = Command::new("ldd")
        .arg(executable)
        .output()
        .map_err(|e| disk::io(Path::new("ldd"), e))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if text.contains("not a dynamic executable") || text.contains("statically linked") {
        return Ok(None);
    }
    if !output.status.success() || text.contains("not found") {
        return Err(Error::Invalid(format!(
            "无法打包 initramfs 执行器依赖: {text}"
        )));
    }
    let mut loader = None;
    let mut bundled = std::collections::BTreeMap::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let (name, path, interpreter) = match fields.as_slice() {
            [name, "=>", path, ..] if path.starts_with('/') => {
                let interpreter = name.starts_with('/');
                let name = if interpreter {
                    Path::new(name)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    name.to_string()
                };
                (name, PathBuf::from(path), interpreter)
            }
            [path, ..] if path.starts_with('/') => {
                let source = PathBuf::from(path);
                let name = source
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| Error::Invalid("动态加载器名称无效".into()))?
                    .to_owned();
                (name, source, true)
            }
            _ => continue,
        };
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(Error::Invalid("动态依赖名称必须是单个文件名".into()));
        }
        if interpreter {
            loader = Some(name.clone());
        }
        let target = destination.join(&name);
        disk::checked(&target)?;
        let bytes = fs::read(&path).map_err(|e| disk::io(&path, e))?;
        if let Some(previous) = bundled.insert(name, bytes.clone())
            && previous != bytes
        {
            return Err(Error::Invalid("动态依赖文件名冲突".into()));
        }
        fs::write(&target, bytes).map_err(|e| disk::io(&target, e))?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
            .map_err(|e| disk::io(&target, e))?;
    }
    if loader.is_none() {
        return Err(Error::Invalid(
            "无法识别 initramfs 执行器的动态加载器".into(),
        ));
    }
    Ok(loader)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bundled_host_dependencies_can_execute_the_engine_with_a_private_loader() {
        let directory = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let loader = bundle_dependencies(&executable, directory.path()).unwrap();
        let output = if let Some(loader) = loader {
            Command::new(directory.path().join(loader))
                .arg("--library-path")
                .arg(directory.path())
                .arg(&executable)
                .arg("--list")
                .output()
                .unwrap()
        } else {
            Command::new(executable).arg("--list").output().unwrap()
        };
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("bundled_host_dependencies"));
    }
}
