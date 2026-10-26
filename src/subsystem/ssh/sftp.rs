//! SFTP v3 over the same bounded workspace used by management commands.
//! 在管理命令共用的有界工作区上提供 SFTP v3。
use super::super::management::{Upload, Workspace};
use russh_sftp::{
    protocol::{self, Attrs, Data, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode},
    server::StatusReply,
};
use std::{
    collections::HashMap,
    fs::{self, File},
    os::unix::fs::{FileExt, PermissionsExt},
    time::{Duration, UNIX_EPOCH},
};

enum Open {
    Read(File),
    Write(Upload, bool),
    Directory(String, bool),
}
pub(super) struct Files {
    workspace: Workspace,
    handles: HashMap<String, Open>,
    sequence: u64,
}
/// Preserve useful bilingual filesystem failures in SFTP status messages.
/// 在 SFTP 状态消息中保留有用的双语文件系统失败。
/// # Arguments
/// - `error`: Workspace failure. 工作区失败。
fn failure(error: super::Error) -> StatusReply {
    let code = match &error {
        super::Error::Io { source, .. } => match source.kind() {
            std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
            std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
            _ => StatusCode::Failure,
        },
        _ => StatusCode::Failure,
    };
    code.with_message(error.to_string())
}
/// Create a successful SFTP status.
/// 创建成功的 SFTP 状态。
/// # Arguments
/// - `id`: Request identifier. 请求标识。
fn success(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "成功 / success".into(),
        language_tag: String::new(),
    }
}
impl Files {
    /// Start one independent SFTP session.
    /// 启动独立 SFTP 会话。
    /// # Arguments
    /// - `workspace`: Shared authenticated file view. 共享已认证文件视图。
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            handles: HashMap::new(),
            sequence: 0,
        }
    }
    /// Allocate a bounded opaque handle.
    /// 分配有界不透明句柄。
    /// # Arguments
    /// - `self`, `id`, `entry`: Session, request and owned resource. 会话、请求及所属资源。
    fn handle(&mut self, id: u32, entry: Open) -> Result<Handle, StatusReply> {
        if self.handles.len() >= 128 {
            return Err(StatusCode::Failure.with_message("打开文件过多 / too many open files"));
        }
        self.sequence += 1;
        let handle = self.sequence.to_string();
        self.handles.insert(handle.clone(), entry);
        Ok(Handle { id, handle })
    }
    /// Resolve regular file attributes without leaking host paths.
    /// 解析普通文件属性，不泄露宿主路径。
    /// # Arguments
    /// - `self`, `path`: Session and virtual path. 会话及虚拟路径。
    fn attributes(&self, path: &str) -> Result<FileAttributes, StatusReply> {
        let path = self.workspace.path(path).map_err(failure)?;
        Ok(FileAttributes::from(
            &fs::metadata(path).map_err(|e| failure(e.into()))?,
        ))
    }
}
impl russh_sftp::server::Handler for Files {
    type Error = StatusReply;
    /// Refuse unsupported protocol operations explicitly.
    /// 显式拒绝不支持的协议操作。
    /// # Arguments
    /// - `self`: SFTP session. SFTP 会话。
    fn unimplemented(&self) -> StatusReply {
        StatusCode::OpUnsupported.with_message("不支持的 SFTP 操作 / unsupported SFTP operation")
    }
    /// Open reads directly and stage writes until a successful close.
    /// 直接打开读取，写入暂存至成功关闭。
    /// # Arguments
    /// - `self`, `id`, `filename`, `flags`, `attrs`: SFTP open request. SFTP 打开请求。
    async fn open(
        &mut self,
        id: u32,
        filename: String,
        flags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, StatusReply> {
        let path = self.workspace.path(&filename).map_err(failure)?;
        if flags.contains(OpenFlags::WRITE) {
            if !flags.contains(OpenFlags::CREATE)
                && !path.try_exists().map_err(|e| failure(e.into()))?
            {
                return Err(StatusCode::NoSuchFile.with_message("文件不存在 / file does not exist"));
            }
            let upload = self
                .workspace
                .upload(
                    &filename,
                    flags.contains(OpenFlags::TRUNCATE),
                    flags.contains(OpenFlags::EXCLUDE),
                )
                .map_err(failure)?;
            apply_attrs(upload.file(), &attrs)?;
            self.handle(id, Open::Write(upload, flags.contains(OpenFlags::APPEND)))
        } else if flags.contains(OpenFlags::READ)
            && !flags.intersects(
                OpenFlags::TRUNCATE | OpenFlags::APPEND | OpenFlags::CREATE | OpenFlags::EXCLUDE,
            )
        {
            let file = File::open(path).map_err(|e| failure(e.into()))?;
            if !file.metadata().map_err(|e| failure(e.into()))?.is_file() {
                return Err(
                    StatusCode::Failure.with_message("需要普通文件 / regular file required")
                );
            }
            self.handle(id, Open::Read(file))
        } else {
            Err(StatusCode::OpUnsupported.with_message("不支持的打开标志 / unsupported open flags"))
        }
    }
    /// Commit complete uploads; dropping an unclosed handle discards staged data.
    /// 提交完整上传；丢弃未关闭句柄会放弃暂存数据。
    /// # Arguments
    /// - `self`, `id`, `handle`: Session, request and opaque handle. 会话、请求及不透明句柄。
    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusReply> {
        let entry = self
            .handles
            .remove(&handle)
            .ok_or_else(|| StatusCode::Failure.with_message("句柄无效 / invalid handle"))?;
        if let Open::Write(upload, _) = entry {
            upload.commit().map_err(failure)?;
        }
        Ok(success(id))
    }
    /// Read a bounded packet at an independent offset.
    /// 在独立偏移读取有界数据包。
    /// # Arguments
    /// - `self`, `id`, `handle`, `offset`, `len`: Read request. 读取请求。
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusReply> {
        let file = match self.handles.get(&handle) {
            Some(Open::Read(file)) => file,
            Some(Open::Write(upload, _)) => upload.file(),
            _ => return Err(StatusCode::Failure.with_message("读取句柄无效 / invalid read handle")),
        };
        let mut data = vec![0; (len as usize).min(32768)];
        let n = file
            .read_at(&mut data, offset)
            .map_err(|e| failure(e.into()))?;
        if n == 0 {
            return Err(StatusCode::Eof.with_message("文件末尾 / end of file"));
        }
        data.truncate(n);
        Ok(Data { id, data })
    }
    /// Keep every write packet private until close.
    /// 关闭前保持每个写入包私有。
    /// # Arguments
    /// - `self`, `id`, `handle`, `offset`, `data`: Write request. 写入请求。
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusReply> {
        match self.handles.get_mut(&handle) {
            Some(Open::Write(upload, append)) => {
                let offset = if *append {
                    upload
                        .file()
                        .metadata()
                        .map_err(|e| failure(e.into()))?
                        .len()
                } else {
                    offset
                };
                upload.write(offset, &data).map_err(failure)?;
                Ok(success(id))
            }
            _ => {
                Err(StatusCode::PermissionDenied
                    .with_message("句柄不可写 / handle is not writable"))
            }
        }
    }
    /// Inspect a virtual path without following links.
    /// 检查虚拟路径，不跟随链接。
    /// # Arguments
    /// - `self`, `id`, `path`: Attribute request. 属性请求。
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        Ok(Attrs {
            id,
            attrs: self.attributes(&path)?,
        })
    }
    /// Inspect a regular virtual entry.
    /// 检查普通虚拟条目。
    /// # Arguments
    /// - `self`, `id`, `path`: Attribute request. 属性请求。
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusReply> {
        self.lstat(id, path).await
    }
    /// Inspect staged or readable file metadata.
    /// 检查暂存或可读文件元数据。
    /// # Arguments
    /// - `self`, `id`, `handle`: Handle attribute request. 句柄属性请求。
    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusReply> {
        let attrs = match self.handles.get(&handle) {
            Some(Open::Read(file)) => {
                FileAttributes::from(&file.metadata().map_err(|e| failure(e.into()))?)
            }
            Some(Open::Write(upload, _)) => {
                FileAttributes::from(&upload.file().metadata().map_err(|e| failure(e.into()))?)
            }
            Some(Open::Directory(path, _)) => self.attributes(path)?,
            None => return Err(StatusCode::Failure.with_message("句柄无效 / invalid handle")),
        };
        Ok(Attrs { id, attrs })
    }
    /// Apply safe attributes under the configuration transaction lock.
    /// 在配置事务锁下应用安全属性。
    /// # Arguments
    /// - `self`, `id`, `path`, `attrs`: Attribute mutation request. 属性修改请求。
    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        let _lock = crate::config::lock_configuration(
            &self
                .workspace
                .store
                .system_layout(&self.workspace.id)
                .map_err(|e| failure(e.into()))?
                .root,
        )
        .map_err(|e| failure(e.into()))?;
        let path = self.workspace.path(&path).map_err(failure)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(attrs.size.is_some())
            .open(path)
            .map_err(|e| failure(e.into()))?;
        apply_attrs(&file, &attrs)?;
        Ok(success(id))
    }
    /// Apply attributes to a staged write handle only.
    /// 仅对暂存写入句柄应用属性。
    /// # Arguments
    /// - `self`, `id`, `handle`, `attrs`: Handle mutation request. 句柄修改请求。
    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        match self.handles.get(&handle) {
            Some(Open::Write(upload, _)) => {
                apply_attrs(upload.file(), &attrs)?;
                Ok(success(id))
            }
            _ => {
                Err(StatusCode::PermissionDenied
                    .with_message("句柄不可写 / handle is not writable"))
            }
        }
    }
    /// Enumerate the same virtual directory names as CLI ls.
    /// 枚举与 CLI ls 相同的虚拟目录名。
    /// # Arguments
    /// - `self`, `id`, `path`: Directory-open request. 目录打开请求。
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusReply> {
        let path = Workspace::normalize("/", &path).map_err(failure)?;
        self.workspace.list(&path).map_err(failure)?;
        self.handle(id, Open::Directory(path, false))
    }
    /// Return one bounded directory listing followed by EOF.
    /// 返回一次有界目录列表，然后返回 EOF。
    /// # Arguments
    /// - `self`, `id`, `handle`: Directory-read request. 目录读取请求。
    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusReply> {
        let path = match self.handles.get_mut(&handle) {
            Some(Open::Directory(path, done)) if !*done => {
                *done = true;
                path.clone()
            }
            _ => return Err(StatusCode::Eof.with_message("目录末尾 / end of directory")),
        };
        let names = self.workspace.list(&path).map_err(failure)?;
        if names.len() > 8192 {
            return Err(StatusCode::Failure
                .with_message("目录条目超出限制 / directory entry limit exceeded"));
        }
        let mut files = Vec::new();
        for name in names {
            let attrs = self.attributes(&format!("{path}/{name}"))?;
            files.push(protocol::File::new(name, attrs));
        }
        Ok(Name { id, files })
    }
    /// Return virtual canonical paths, never host paths.
    /// 返回虚拟规范路径，绝不返回宿主路径。
    /// # Arguments
    /// - `self`, `id`, `path`: Canonicalization request. 规范化请求。
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusReply> {
        let path = Workspace::normalize("/", &path).map_err(failure)?;
        let attrs = self.attributes(&path)?;
        Ok(Name {
            id,
            files: vec![protocol::File::new(path, attrs)],
        })
    }
    /// Remove a regular virtual file.
    /// 删除普通虚拟文件。
    /// # Arguments
    /// - `self`, `id`, `filename`: Removal request. 删除请求。
    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, StatusReply> {
        self.workspace
            .mutate("rm", &filename, None)
            .map_err(failure)?;
        Ok(success(id))
    }
    /// Create a virtual directory without changing ownership.
    /// 创建虚拟目录，不改变所有权。
    /// # Arguments
    /// - `self`, `id`, `path`, `attrs`: Directory creation request. 目录创建请求。
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusReply> {
        validate_attrs(&attrs)?;
        self.workspace
            .mutate("mkdir", &path, None)
            .map_err(failure)?;
        Ok(success(id))
    }
    /// Remove an empty virtual directory.
    /// 删除空虚拟目录。
    /// # Arguments
    /// - `self`, `id`, `path`: Directory removal request. 目录删除请求。
    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, StatusReply> {
        self.workspace
            .mutate("rmdir", &path, None)
            .map_err(failure)?;
        Ok(success(id))
    }
    /// Publish client temporary filenames using an atomic rename.
    /// 通过原子重命名发布客户端临时文件名。
    /// # Arguments
    /// - `self`, `id`, `oldpath`, `newpath`: Rename request. 重命名请求。
    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, StatusReply> {
        self.workspace
            .mutate("mv", &oldpath, Some(&newpath))
            .map_err(failure)?;
        Ok(success(id))
    }
}
/// Reject attributes that would invalidate administrator-owned control inputs.
/// 拒绝会使管理员所属控制输入无效的属性。
/// # Arguments
/// - `attrs`: Requested metadata. 请求元数据。
fn validate_attrs(attrs: &FileAttributes) -> Result<(), StatusReply> {
    if attrs.uid.is_some_and(|uid| uid != 0)
        || attrs.gid.is_some_and(|gid| gid != 0)
        || attrs.permissions.is_some_and(|mode| mode & 0o7022 != 0)
        || attrs.size.is_some_and(|size| size > 256 * 1024 * 1024)
    {
        return Err(StatusCode::PermissionDenied.with_message(
            "配置所有权、权限或大小无效 / invalid configuration ownership, permissions or size",
        ));
    }
    Ok(())
}
/// Preserve supported modes, sizes and timestamps on an already-owned descriptor.
/// 在已固定描述符上保存支持的权限、大小和时间戳。
/// # Arguments
/// - `file`, `attrs`: Staged or locked file and metadata. 暂存或已锁定文件及元数据。
fn apply_attrs(file: &File, attrs: &FileAttributes) -> Result<(), StatusReply> {
    validate_attrs(attrs)?;
    if let Some(size) = attrs.size {
        file.set_len(size).map_err(|e| failure(e.into()))?;
    }
    if let Some(mode) = attrs.permissions {
        file.set_permissions(fs::Permissions::from_mode(mode & 0o777))
            .map_err(|e| failure(e.into()))?;
    }
    let mut times = fs::FileTimes::new();
    if let Some(time) = attrs.atime {
        times = times.set_accessed(UNIX_EPOCH + Duration::from_secs(time.into()));
    }
    if let Some(time) = attrs.mtime {
        times = times.set_modified(UNIX_EPOCH + Duration::from_secs(time.into()));
    }
    if attrs.atime.is_some() || attrs.mtime.is_some() {
        file.set_times(times).map_err(|e| failure(e.into()))?;
    }
    Ok(())
}
