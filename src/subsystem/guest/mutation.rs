//! Capture configuration and publish per-instance persistent data.
//! 保存配置并发布每实例持久数据。
use super::copy::{CopyBudget, capture_script, copy_object};
use super::migration::publish;
use super::*;

/// Save a root-owned archive and script without exposing editable host policy.
/// 保存 root 所有归档与脚本，不暴露可编辑宿主策略。
/// # Arguments
/// - `store`, `runtime`, `root`, `target`: Authenticated fixed source. 已认证固定来源。
pub(super) fn add_copy(
    store: &ConfigStore,
    runtime: &PreparedRuntime,
    root: &GuestRoot,
    target: &Path,
) -> Result<String> {
    target_path(target)?;
    let resource_key = key(target);
    let _activity = super::filesystem::activity(store, &runtime.system_id, true)?;
    if execution::occupied(runtime)? {
        return Err(Error::Invalid(
            "stop managed applications and other writers before capturing build inputs".into(),
        ));
    }
    let layout = store.system_layout(&runtime.system_id)?;
    let _system = controller::lock_system_directory(&layout.root)?;
    if !mounts_at(root, target)?.is_empty() {
        return Err(Error::Invalid(
            "capture target has mounted descendants; capture an unmounted path".into(),
        ));
    }
    let source = open_target(&root.root, target)?;
    if !source.metadata()?.is_file() && !source.metadata()?.is_dir() {
        return Err(Error::Invalid(
            "capture source must be a regular file or directory".into(),
        ));
    }
    let parent = layout.config.join("script");
    checked(&parent)?;
    fs::create_dir_all(&parent)?;
    let temporary = layout.root.join("runtime/guest");
    checked(&temporary)?;
    fs::create_dir_all(&temporary)?;
    let stage = tempfile::Builder::new()
        .prefix(".guest-copy-")
        .tempdir_in(&temporary)?;
    let resource = stage.path().join("resource");
    fs::create_dir(&resource)?;
    let data = stage.path().join("data");
    copy_object(&source, &data, &mut CopyBudget::new(1024 * 1024 * 1024), 0)?;
    let archive = resource.join("data.tar");
    let mut tar = Command::new("/usr/bin/tar");
    tar.args([
        "--create",
        "--format=pax",
        "--numeric-owner",
        "--xattrs",
        "--acls",
        "--file",
    ])
    .arg(&archive)
    .arg("--directory")
    .arg(stage.path())
    .args(["--", "data"]);
    let result = commands::output(tar, Duration::from_secs(120))?;
    if !result.status.success() {
        return Err(Error::Invalid(format!(
            "capture archive failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o600))?;
    File::open(&archive)?.sync_all()?;
    let script = capture_script(target, source.metadata()?.is_dir())?;
    atomic_write(&resource.join("main.sh"), script.as_bytes(), 0o700)?;
    File::open(&resource)?.sync_all()?;
    still_running(runtime, root)?;
    let destination = parent.join(format!("90-guest-copy-{resource_key}"));
    checked(&destination)?;
    // Directory exchange preserves the old complete resource until the new one is published.
    // 目录交换使旧完整资源保留到新资源发布。
    publish(&resource, &destination, destination.try_exists()?)?;
    Ok("已保存构建资源；下次成功构建和启动时应用".into())
}
