//! Keep each system's machine identity outside disposable independently installed roots.
//! 将每个系统的机器身份保存在独立安装的可清理根之外。
use super::{Error, Result, fs as disk};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

/// Durable identity independent of a build version and its plugin.
/// 独立于构建版本和插件的持久身份。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    /// Persisted identity schema.
    /// 持久身份格式。
    schema_version: u32,
    /// Stable nonzero 128-bit identity for this managed system.
    /// 此被管理系统稳定的非零 128 位身份。
    machine_id: String,
}

/// Validate a nonzero 128-bit machine ID in canonical lowercase hexadecimal form.
/// 校验以规范小写十六进制表示的非零 128 位机器 ID。
/// # Arguments
///
/// - `id`: Untrusted persisted or previously installed identity.
///   不可信的持久保存身份或之前安装的身份。
fn valid(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && id.bytes().any(|b| b != b'0')
}

/// Reuse this system's ID or generate one once when installing its independent base.
/// 独立安装系统基础根时复用此系统的 ID，或仅生成一次新 ID。
/// # Arguments
///
/// - `system`: Locked manager-owned directory for this system.
///   此系统已锁定的管理器所属目录。
/// - `previous_root`: This same system's previous root, used to adopt legacy identities.
///   此系统自己的之前根，用于接纳旧身份。
pub(super) fn machine_id(system: &Path, previous_root: Option<&Path>) -> Result<String> {
    let path = system.join("identity.toml");
    if disk::exists(&path)? {
        if !fs::symlink_metadata(&path)
            .map_err(|e| disk::io(&path, e))?
            .is_file()
        {
            return Err(Error::Invalid("系统身份记录必须是普通文件".into()));
        }
        let record: Identity =
            toml::from_str(&fs::read_to_string(&path).map_err(|e| disk::io(&path, e))?)
                .map_err(|e| Error::Invalid(format!("系统身份记录: {e}")))?;
        if record.schema_version != 1 || !valid(&record.machine_id) {
            return Err(Error::Invalid("系统身份记录包含无效 machine-id".into()));
        }
        return Ok(record.machine_id);
    }
    let mut existing = None;
    if let Some(root) = previous_root {
        let candidate = root.join("etc/machine-id");
        if disk::exists(&candidate)? {
            if !fs::symlink_metadata(&candidate)
                .map_err(|e| disk::io(&candidate, e))?
                .is_file()
            {
                return Err(Error::Invalid(
                    "之前系统的 machine-id 必须是普通文件".into(),
                ));
            }
            let content = fs::read_to_string(&candidate).map_err(|e| disk::io(&candidate, e))?;
            let id = content.trim();
            if valid(id) {
                existing = Some(id.to_owned());
            } else if !matches!(id, "" | "uninitialized") {
                return Err(Error::Invalid("之前系统含无效 machine-id".into()));
            }
        }
    }
    let id = if let Some(id) = existing {
        id
    } else {
        let random_path = Path::new("/dev/urandom");
        let mut file = File::open(random_path).map_err(|e| disk::io(random_path, e))?;
        loop {
            let mut bytes = [0u8; 16];
            file.read_exact(&mut bytes)
                .map_err(|e| disk::io(random_path, e))?;
            let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            if valid(&id) {
                break id;
            }
        }
    };
    disk::write(
        &path,
        &Identity {
            schema_version: 1,
            machine_id: id.clone(),
        },
    )?;
    Ok(id)
}
