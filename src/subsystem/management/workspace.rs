//! A single restricted file view for the CLI, editor and SFTP.
//! CLI、编辑器和 SFTP 共用的受限文件视图。
use super::{Error, Result};
use crate::config::ConfigStore;
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

#[derive(Clone)]
pub struct Workspace {
    pub store: ConfigStore,
    pub id: String,
    root: PathBuf,
}
pub struct Upload {
    temporary: tempfile::NamedTempFile,
    destination: PathBuf,
    original: Option<Vec<u8>>,
    _lock: File,
    failed: bool,
}

impl Workspace {
    /// Select one registered subsystem's build/script inputs.
    /// 选择一个已登记子系统的 build/script 输入。
    /// # Arguments
    /// - `store`, `id`: Fixed manager and registered identity. 固定管理器及登记身份。
    pub fn new(store: ConfigStore, id: &str) -> Result<Self> {
        super::super::SubsystemConfig::read(&store, id)?;
        let root = store.system_layout(id)?.config;
        Ok(Self {
            store,
            id: id.into(),
            root,
        })
    }
    /// Normalize virtual paths without allowing access above the virtual root.
    /// 规范虚拟路径，禁止越过虚拟根。
    /// # Arguments
    /// - `cwd`, `path`: Virtual current directory and requested path. 虚拟当前目录及请求路径。
    pub fn normalize(cwd: &str, path: &str) -> Result<String> {
        if path.contains(['\0', '\\']) || path.chars().any(char::is_control) {
            return Err(Error::Invalid("文件路径无效 / invalid file path".into()));
        }
        let mut parts = Vec::new();
        let joined = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("{cwd}/{path}")
        };
        for part in joined.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return Err(Error::Invalid(
                            "路径越过配置根 / path escapes configuration root".into(),
                        ));
                    }
                }
                part => parts.push(part),
            }
        }
        if parts
            .first()
            .is_some_and(|p| !matches!(*p, "build" | "script"))
        {
            return Err(Error::Invalid(
                "只允许访问 build/script / only build/script are accessible".into(),
            ));
        }
        Ok(format!("/{}", parts.join("/")))
    }
    /// Resolve a virtual path while rejecting symbolic links and special files.
    /// 解析虚拟路径，拒绝符号链接和特殊文件。
    /// # Arguments
    /// - `path`: Absolute or root-relative virtual path. 绝对或根相对虚拟路径。
    pub fn path(&self, path: &str) -> Result<PathBuf> {
        let normalized = Self::normalize("/", path)?;
        let mut result = self.root.clone();
        super::super::checked(&result)?;
        for part in Path::new(&normalized).components() {
            if let Component::Normal(part) = part {
                result.push(part);
                match fs::symlink_metadata(&result) {
                Ok(m) if !m.is_dir() && !m.is_file() => return Err(Error::Invalid("配置视图拒绝链接和特殊文件 / configuration view refuses links and special files".into())),
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()), _ => {}
            }
            }
        }
        Ok(result)
    }
    /// List only the public configuration trees at the root.
    /// 在根目录仅列出公开配置树。
    /// # Arguments
    /// - `path`: Virtual directory. 虚拟目录。
    pub fn list(&self, path: &str) -> Result<Vec<String>> {
        if Self::normalize("/", path)? == "/" {
            return Ok(vec!["build".into(), "script".into()]);
        }
        let mut names = fs::read_dir(self.path(path)?)?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }
    /// Read regular configuration bytes with a bounded allocation.
    /// 有界读取普通配置文件字节。
    /// # Arguments
    /// - `path`: Virtual file. 虚拟文件。
    pub fn read(&self, path: &str) -> Result<Vec<u8>> {
        let file = File::open(self.path(path)?)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > 256 * 1024 * 1024 {
            return Err(Error::Invalid(
                "文件类型或大小不受支持 / unsupported file type or size".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.take(256 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 256 * 1024 * 1024 {
            return Err(Error::Invalid("文件超出限制 / file exceeds limit".into()));
        }
        Ok(bytes)
    }
    /// Open an isolated upload and exclude configuration freezing until close.
    /// 打开独立上传，关闭前排除配置冻结。
    /// # Arguments
    /// - `path`, `truncate`, `exclusive`: Destination and open semantics. 目标及打开语义。
    pub fn upload(&self, path: &str, truncate: bool, exclusive: bool) -> Result<Upload> {
        let normalized = Self::normalize("/", path)?;
        if matches!(normalized.as_str(), "/" | "/build" | "/script") {
            return Err(Error::Invalid(
                "不能替换配置树 / cannot replace configuration tree".into(),
            ));
        }
        let layout = self.store.system_layout(&self.id)?;
        let lock = crate::config::lock_configuration(&layout.root)?;
        let destination = self.path(path)?;
        let original = match fs::read(&destination) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if exclusive && original.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "文件已存在 / file already exists",
            )
            .into());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&layout.root)?;
        if !truncate {
            if let Some(bytes) = &original {
                temporary.write_all(bytes)?;
            }
        }
        Ok(Upload {
            temporary,
            destination,
            original,
            _lock: lock,
            failed: false,
        })
    }
    /// Atomically save one editor buffer.
    /// 原子保存单个编辑缓冲区。
    /// # Arguments
    /// - `path`, `bytes`: Virtual destination and content. 虚拟目标及内容。
    pub fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let mut upload = self.upload(path, true, false)?;
        upload.write(0, bytes)?;
        upload.commit()
    }
    /// Compare the editor's original bytes under the publication lock before saving.
    /// 保存前在发布锁下比较编辑器原始字节。
    /// # Arguments
    /// - `path`, `original`, `bytes`: Target, initial content and replacement. 目标、初始内容及替换内容。
    pub fn write_expected(&self, path: &str, original: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
        let mut upload = self.upload(path, true, false)?;
        if upload.original.as_deref() != original {
            return Err(Error::Invalid(
                "文件已被其他会话修改，请重新打开 / file changed in another session; reopen it"
                    .into(),
            ));
        }
        upload.write(0, bytes)?;
        upload.commit()
    }
    /// Mutate a directory or file under the same configuration transaction lock.
    /// 在同一配置事务锁下修改目录或文件。
    /// # Arguments
    /// - `operation`, `path`, `destination`: mkdir/rm/rmdir/mv and virtual paths. 操作及虚拟路径。
    pub fn mutate(&self, operation: &str, path: &str, destination: Option<&str>) -> Result<()> {
        let _lock = crate::config::lock_configuration(&self.store.system_layout(&self.id)?.root)?;
        let normalized = Self::normalize("/", path)?;
        if matches!(normalized.as_str(), "/" | "/build" | "/script") {
            return Err(Error::Invalid(
                "不能修改配置树根 / cannot mutate configuration tree root".into(),
            ));
        }
        let source = self.path(path)?;
        match operation {
            "mkdir" => fs::create_dir(&source)?,
            "rm" => fs::remove_file(&source)?,
            "rmdir" => fs::remove_dir(&source)?,
            "mv" => {
                let target = destination
                    .ok_or_else(|| Error::Invalid("缺少目标 / missing destination".into()))?;
                if matches!(
                    Self::normalize("/", target)?.as_str(),
                    "/" | "/build" | "/script"
                ) {
                    return Err(Error::Invalid(
                        "不能覆盖配置树根 / cannot replace configuration tree root".into(),
                    ));
                }
                fs::rename(&source, self.path(target)?)?;
            }
            _ => {
                return Err(Error::Invalid(
                    "未知文件操作 / unknown file operation".into(),
                ));
            }
        }
        if let Some(parent) = source.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}
impl Upload {
    /// Write a packet at its requested offset without publishing partial data.
    /// 按请求偏移写入数据包，不发布部分数据。
    /// # Arguments
    /// - `offset`, `bytes`: Packet offset and content. 包偏移及内容。
    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<()> {
        let result = (|| -> Result<()> {
            if offset.saturating_add(bytes.len() as u64) > 256 * 1024 * 1024 {
                return Err(Error::Invalid("上传超出文件限制 / upload exceeds file limit".into()));
            }
            self.temporary.seek(SeekFrom::Start(offset))?;
            self.temporary.write_all(bytes)?;
            Ok(())
        })();
        if result.is_err() { self.failed = true; }
        result
    }

    /// Inspect the staged file without exposing its host path.
    /// 检查暂存文件，不暴露宿主路径。
    /// # Arguments
    /// - `self`: Active isolated upload. 活动独立上传。
    pub fn file(&self) -> &File {
        self.temporary.as_file()
    }
    /// Durably publish a complete upload, rejecting concurrent external edits.
    /// 持久发布完整上传，拒绝并发外部编辑。
    /// # Arguments
    /// - `self`: Completed isolated upload. 已完成独立上传。
    pub fn commit(self) -> Result<()> {
        if self.failed { return Err(Error::Invalid("上传曾经失败，拒绝发布部分内容 / upload failed; refusing to publish partial content".into())); }
        let current = match fs::read(&self.destination) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        if current != self.original {
            return Err(Error::Invalid("文件已被其他操作修改，请重新读取 / file changed concurrently; reload before saving".into()));
        }
        self.temporary.as_file().sync_all()?;
        let parent = self.destination.parent().unwrap().to_owned();
        self.temporary
            .persist(&self.destination)
            .map_err(|e| e.error)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    }
}
