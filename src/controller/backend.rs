//! Native Btrfs CoW storage and checked plugin execution.
//! 原生 Btrfs CoW 存储和经检查的插件执行。
use super::{BuildBackend, BuildContext, Error, Result, fs, plugins::LoadedPlugin};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// Find nested roots using Btrfs's reserved subvolume-root inode, without parsing paths.
/// 使用 Btrfs 子卷根保留 inode 查找后代，不解析命令输出中的路径。
fn nested_subvolumes(root: &Path) -> Result<Vec<PathBuf>> {
    use std::os::unix::fs::MetadataExt;
    let mut pending = vec![root.to_path_buf()];
    let mut found = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(|e| fs::io(&directory, e))? {
            let entry = entry.map_err(|e| fs::io(&directory, e))?;
            let path = entry.path();
            let info = std::fs::symlink_metadata(&path).map_err(|e| fs::io(&path, e))?;
            if !info.is_dir() {
                continue;
            }
            if info.ino() == 256 {
                btrfs(&[
                    "subvolume".into(),
                    "show".into(),
                    "--".into(),
                    path.clone().into(),
                ])?;
                found.push(path.clone());
            }
            pending.push(path);
        }
    }
    found.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    Ok(found)
}

/// Production backend requiring real Btrfs snapshots and systemd-nspawn.
/// 要求真实 Btrfs 快照和 systemd-nspawn 的生产后端。
#[derive(Debug, Default)]
pub struct SystemBackend;

/// Execute Btrfs without shell interpolation, preserving native path bytes.
/// 不经 shell 插值执行 Btrfs，保留路径原生字节。
/// # Arguments
///
/// - `arguments`: Individually passed command arguments.
///   独立传递的命令参数。
fn btrfs(arguments: &[OsString]) -> Result<Output> {
    let output = Command::new("btrfs")
        .args(arguments)
        .output()
        .map_err(|e| fs::io(Path::new("btrfs"), e))?;
    if !output.status.success() {
        return Err(Error::Command {
            command: format!("btrfs {arguments:?}"),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    Ok(output)
}

/// Validate a destination and create its ordinary parent directories.
/// 校验目标并创建其普通父目录。
/// # Arguments
///
/// - `path`: New subvolume path.
///   新子卷路径。
fn destination(path: &Path) -> Result<()> {
    if fs::exists(path)? {
        return Err(Error::Invalid(format!("目标已存在: {}", path.display())));
    }
    fs::directory(
        path.parent()
            .ok_or_else(|| Error::Invalid("子卷缺少父目录".into()))?,
    )
}

impl BuildBackend for SystemBackend {
    fn sync_filesystem(&self, path: &Path) -> Result<()> {
        btrfs(&["filesystem".into(), "sync".into(), path.into()])?;
        Ok(())
    }
    fn nested_subvolumes(&self, path: &Path) -> Result<Vec<PathBuf>> {
        super::mounts::check_descendant_mounts(path)?;
        nested_subvolumes(path)?
            .into_iter()
            .map(|p| {
                p.strip_prefix(path)
                    .map(Path::to_path_buf)
                    .map_err(|e| Error::Invalid(e.to_string()))
            })
            .collect()
    }
    fn is_subvolume(&self, path: &Path) -> Result<bool> {
        use std::os::unix::fs::MetadataExt;
        fs::checked(path)?;
        let info = std::fs::symlink_metadata(path).map_err(|e| fs::io(path, e))?;
        if !info.is_dir() || info.ino() != 256 {
            return Ok(false);
        }
        btrfs(&["subvolume".into(), "show".into(), "--".into(), path.into()])?;
        Ok(true)
    }
    fn source_subvolume_id(&self, path: &Path) -> Result<String> {
        let out = btrfs(&["inspect-internal".into(), "rootid".into(), path.into()])?;
        let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Invalid("来源子卷 ID 无效".into()));
        }
        Ok(id)
    }
    fn prepare_inheritance_initramfs(&self, root: &Path) -> Result<()> {
        super::initramfs::prepare(root)
    }
    fn set_readonly(&self, path: &Path, readonly: bool) -> Result<()> {
        fs::checked(path)?;
        btrfs(&[
            "property".into(),
            "set".into(),
            "-ts".into(),
            "--".into(),
            path.into(),
            "ro".into(),
            readonly.to_string().into(),
        ])?;
        self.check_subvolume(path, readonly)
    }
    fn copy_inherited(&self, source: &Path, target: &Path, force_cow: bool) -> Result<()> {
        fs::checked(source)?;
        fs::checked(target)?;
        let directory = source.is_dir();
        let source = if directory {
            source.join(".")
        } else {
            source.to_path_buf()
        };
        let mut command = Command::new("cp");
        // Btrfs child subvolumes have separate device identities. Preserve them with
        // explicit recursive snapshots instead of descending into them during cp.
        if directory && force_cow {
            command.arg("--one-file-system");
        }
        let output = command
            .arg("--archive")
            .arg(if force_cow {
                "--reflink=always"
            } else {
                "--reflink=auto"
            })
            .arg("--")
            .arg(&source)
            .arg(target)
            .output()
            .map_err(|e| fs::io(target, e))?;
        if !output.status.success() {
            return Err(Error::Command {
                command: format!("cp inheritance {}", source.display()),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into(),
            });
        }
        Ok(())
    }
    fn is_running_root(&self, path: &Path) -> Result<bool> {
        let current = super::mounts::covering(Path::new("/"))?;
        let candidate = super::mounts::covering(path)?;
        if current.fstype != "btrfs" || candidate.fstype != "btrfs" {
            return Ok(false);
        }
        Ok(current.uuid == candidate.uuid && current.fsroot == candidate.subvolume_path(path)?)
    }
    fn publish_boot(
        &self,
        outcome: &super::BuildOutcome,
        config: &crate::config::RuntimeConfig,
    ) -> Result<()> {
        super::boot::publish(outcome, config)
    }
    fn check_no_nested_subvolumes(&self, path: &Path) -> Result<()> {
        fs::checked(path)?;
        super::mounts::check_descendant_mounts(path)?;
        use std::os::unix::fs::MetadataExt;
        if std::fs::metadata(path).map_err(|e| fs::io(path, e))?.ino() != 256 {
            let children = nested_subvolumes(path)?;
            if !children.is_empty() {
                return Err(Error::Invalid(format!("继承目录含嵌套子卷: {children:?}")));
            }
            return Ok(());
        }
        // Kernel enumeration also detects subvolumes hidden by a mounted directory.
        let output = btrfs(&[
            "subvolume".into(),
            "list".into(),
            "-o".into(),
            "--".into(),
            path.into(),
        ])?;
        if !output.stdout.is_empty() {
            return Err(Error::Invalid(format!(
                "持久目录含嵌套子卷，不能快照或提交 {}:\n{}",
                path.display(),
                String::from_utf8_lossy(&output.stdout)
            )));
        }
        Ok(())
    }

    fn delete_subvolume_recursive(&self, path: &Path) -> Result<()> {
        fs::checked(path)?;
        super::mounts::check_unmounted_tree(path)?;
        // Validate the owned root before deleting any descendants; readonly roots
        // are also valid cleanup targets after a failed plugin operation.
        btrfs(&["subvolume".into(), "show".into(), "--".into(), path.into()])?;
        let children = nested_subvolumes(path)?;
        if !children.is_empty() {
            self.set_readonly(path, false)?;
        }
        for child in children.iter().rev() {
            self.set_readonly(child, false)?;
        }
        for child in children {
            self.delete_subvolume(&child)?;
        }
        self.delete_subvolume(path)
    }
    /// Create and verify a writable subvolume.
    /// 创建并验证可写子卷。
    /// # Arguments
    ///
    /// - `self`: Native backend.
    ///   原生后端。
    /// - `path`: New subvolume.
    ///   新子卷。
    fn create_subvolume(&self, path: &Path) -> Result<()> {
        destination(path)?;
        btrfs(&[
            "subvolume".into(),
            "create".into(),
            "--".into(),
            path.into(),
        ])?;
        self.check_subvolume(path, false)
    }
    /// Verify subvolume identity and its read-only property.
    /// 验证子卷身份和只读属性。
    /// # Arguments
    ///
    /// - `self`: Native backend.
    ///   原生后端。
    /// - `path`: Existing subvolume.
    ///   已有子卷。
    /// - `readonly`: Expected property.
    ///   预期属性。
    fn check_subvolume(&self, path: &Path, readonly: bool) -> Result<()> {
        fs::checked(path)?;
        btrfs(&["subvolume".into(), "show".into(), "--".into(), path.into()])?;
        let output = btrfs(&[
            "property".into(),
            "get".into(),
            "-ts".into(),
            "--".into(),
            path.into(),
            "ro".into(),
        ])?;
        if String::from_utf8_lossy(&output.stdout).trim() != format!("ro={readonly}") {
            return Err(Error::Invalid(format!(
                "子卷只读属性不符: {}",
                path.display()
            )));
        }
        Ok(())
    }
    /// Create a CoW snapshot with no ordinary-copy fallback.
    /// 创建 CoW 快照，无普通复制回退。
    /// # Arguments
    ///
    /// - `self`: Native backend.
    ///   原生后端。
    /// - `source`: Existing subvolume.
    ///   已有子卷。
    /// - `destination_path`: New snapshot.
    ///   新快照。
    /// - `readonly`: Desired property.
    ///   所需属性。
    fn snapshot(&self, source: &Path, destination_path: &Path, readonly: bool) -> Result<()> {
        fs::checked(source)?;
        btrfs(&[
            "subvolume".into(),
            "show".into(),
            "--".into(),
            source.into(),
        ])?;
        destination(destination_path)?;
        let mut args = vec!["subvolume".into(), "snapshot".into()];
        if readonly {
            args.push("-r".into());
        }
        args.extend(["--".into(), source.into(), destination_path.into()]);
        btrfs(&args)?;
        self.check_subvolume(destination_path, readonly)
    }
    /// Delete a validated owned scratch subvolume.
    /// 删除经验证的所属临时子卷。
    /// # Arguments
    ///
    /// - `self`: Native backend.
    ///   原生后端。
    /// - `path`: Owned scratch root.
    ///   所属临时根。
    fn delete_subvolume(&self, path: &Path) -> Result<()> {
        fs::checked(path)?;
        btrfs(&["subvolume".into(), "show".into(), "--".into(), path.into()])?;
        btrfs(&[
            "subvolume".into(),
            "delete".into(),
            "--".into(),
            path.into(),
        ])?;
        Ok(())
    }
    /// Invoke checked plugin code and separate query output from diagnostics.
    /// 调用经检查的插件代码，分离查询输出和诊断。
    /// # Arguments
    ///
    /// - `self`: Native backend.
    ///   原生后端。
    /// - `plugin`: Loaded resources.
    ///   已加载资源。
    /// - `operation`: Task name.
    ///   任务名称。
    /// - `args`: Task arguments.
    ///   任务参数。
    /// - `root`: Writable root.
    ///   可写根。
    /// - `context`: Frozen inputs.
    ///   冻结输入。
    fn invoke(
        &self,
        plugin: &LoadedPlugin,
        operation: &str,
        args: &[String],
        root: &Path,
        context: &BuildContext,
    ) -> Result<String> {
        self.check_subvolume(root, false)?;
        let args: Vec<_> = args.iter().map(OsString::from).collect();
        let output = plugin.invoke_for_build(operation, &args, root, context)?;
        if super::plugins::is_query(operation)
            || (operation == "predownload" && plugin.metadata.predownload_plans)
        {
            String::from_utf8(output.stdout)
                .map_err(|_| Error::Invalid(format!("{operation} 查询不是 UTF-8")))
        } else {
            Ok(String::new())
        }
    }
    /// Execute a script in its copied directory inside a validated writable system.
    /// 在已校验的可写系统内，以已复制目录执行脚本。
    /// # Arguments
    /// - `self`: Native backend.
    ///   原生后端。
    /// - `root`: Writable system root.
    ///   可写系统根。
    /// - `directory`: Guest working directory.
    ///   容器内工作目录。
    /// - `entrypoint`: Relative shell filename.
    ///   相对 shell 文件名。
    /// - `context`: Frozen inputs.
    ///   冻结输入。
    fn run_script(
        &self,
        root: &Path,
        directory: &Path,
        entrypoint: &Path,
        context: &BuildContext,
    ) -> Result<()> {
        self.check_subvolume(root, false)?;
        super::scripts::run(root, directory, entrypoint, context)
    }
}
