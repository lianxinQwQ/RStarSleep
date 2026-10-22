//! Copy script payloads into the system and coordinate their execution and cleanup.
//! 将脚本资源复制进系统，并协调执行及清理。
use super::inputs;
use crate::controller::{BuildBackend, BuildContext, Result, fs as disk, progress, resources};
use std::path::Path;

/// Copy each prepared payload into the system and run it with that directory as its cwd.
/// 将每项准备后的资源复制进系统，并以该目录为工作目录执行。
/// # Arguments
/// - `backend`: Target-system execution backend.
///   目标系统执行后端。
/// - `root`: Independent writable script-stage system root.
///   独立可写的脚本阶段系统根。
/// - `context`: Frozen configuration and system identity.
///   冻结配置及系统标识。
pub(in crate::controller) fn apply(
    backend: &impl BuildBackend,
    root: &Path,
    context: &BuildContext,
) -> Result<bool> {
    let scripts = inputs::load(&context.config.join("script"))?;
    let applied = !scripts.is_empty();
    let count = scripts.len();
    if !applied {
        progress::message(
            &context.system_id,
            "未配置用户脚本，跳过执行",
            "No user scripts configured; skip execution",
        );
    }
    for (index, script) in scripts.into_iter().enumerate() {
        progress::run(
            &context.system_id,
            format!(
                "执行用户脚本 {}/{count}：{}",
                index + 1,
                script.name.to_string_lossy()
            ),
            format!(
                "Run user script {}/{count}: {}",
                index + 1,
                script.name.to_string_lossy()
            ),
            || {
                // Place the payload outside /run and /tmp, which nspawn replaces with temporary mounts.
                // 避开会被 nspawn 临时挂载覆盖的 /run 和 /tmp。
                let payload = tempfile::Builder::new()
                    .prefix(".rstarsleep-script-")
                    .tempdir_in(root)
                    .map_err(|e| disk::io(root, e))?;
                resources::copy_tree(&script.root, payload.path())?;
                let directory = Path::new("/").join(
                    payload
                        .path()
                        .file_name()
                        .expect("临时目录名 / temporary directory name"),
                );
                backend.run_script(root, &directory, &script.entrypoint, context)?;
                payload.close().map_err(|e| disk::io(root, e))?;
                Ok(())
            },
        )?;
    }
    Ok(applied)
}
