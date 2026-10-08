//! Inspect Linux mounts without interpreting shell output or escaped whitespace.
//! 检查 Linux 挂载信息，不解析 shell 输出或被转义的空白。
use super::{Error, Result, fs};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// One mount covering a requested absolute path.
/// 覆盖所请求绝对路径的挂载记录。
#[derive(Debug, Clone, Deserialize)]
pub(super) struct Mount {
    pub target: PathBuf,
    pub source: String,
    pub fstype: String,
    pub options: String,
    pub uuid: Option<String>,
    pub fsroot: PathBuf,
}
#[derive(Deserialize)]
struct Mounts {
    filesystems: Vec<Mount>,
}

/// Query the covering mount with a stable structured findmnt response.
/// 通过稳定的 findmnt 结构化响应查询覆盖挂载。
/// # Arguments
/// - `path`: Existing host path. 已有宿主路径。
pub(super) fn covering(path: &Path) -> Result<Mount> {
    let output = Command::new("findmnt")
        .args([
            "--json",
            "--output",
            "TARGET,SOURCE,FSTYPE,OPTIONS,UUID,FSROOT",
            "--target",
        ])
        .arg(path)
        .output()
        .map_err(|e| fs::io(Path::new("findmnt"), e))?;
    if !output.status.success() {
        return Err(Error::Invalid(format!(
            "无法查询挂载 {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut mounts: Mounts = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Invalid(format!("findmnt 响应无效: {e}")))?;
    if mounts.filesystems.len() != 1 {
        return Err(Error::Invalid("findmnt 必须返回唯一覆盖挂载".into()));
    }
    Ok(mounts.filesystems.remove(0))
}

impl Mount {
    /// Resolve a host path to its Btrfs top-level path.
    /// 将宿主路径解析为 Btrfs 顶层路径。
    /// # Arguments
    /// - `self`: Covering Btrfs mount. 覆盖的 Btrfs 挂载。
    /// - `path`: Path below this mount. 此挂载下的路径。
    pub fn subvolume_path(&self, path: &Path) -> Result<PathBuf> {
        if self.fstype != "btrfs" {
            return Err(Error::Invalid("系统根必须位于 Btrfs".into()));
        }
        let relative = path
            .strip_prefix(&self.target)
            .map_err(|e| Error::Invalid(e.to_string()))?;
        Ok(self.fsroot.join(relative))
    }
}

/// Reject mounted descendants which ordinary snapshots cannot faithfully inherit.
/// 拒绝普通快照无法准确继承的后代挂载。
/// # Arguments
/// - `source`: File or directory in the current system. 当前系统中的文件或目录。
pub(super) fn check_descendant_mounts(source: &Path) -> Result<()> {
    let text = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|e| fs::io(Path::new("/proc/self/mountinfo"), e))?;
    for line in text.lines() {
        let target = line
            .split_whitespace()
            .nth(4)
            .ok_or_else(|| Error::Invalid("mountinfo 响应无效".into()))?;
        let target = target
            .replace("\\040", " ")
            .replace("\\011", "\t")
            .replace("\\012", "\n")
            .replace("\\134", "\\");
        let target = Path::new(&target);
        if target != source && target.starts_with(source) {
            return Err(Error::Invalid(format!(
                "继承来源包含挂载点: {}",
                target.display()
            )));
        }
    }
    Ok(())
}
