//! Pin and validate one caller's graphical and audio objects.
//! 固定并校验单个调用者的图形和音频对象。
use super::{Error, Resource, Result};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};
const MAX_AUTHORITY_BYTES: u64 = 1024 * 1024;

/// Open an object beneath a pinned directory without following any links.
/// 在固定目录下打开对象，不跟随任何链接。
/// # Arguments
/// - `directory`: Pinned host directory. 固定宿主目录。
/// - `relative`: Relative normalized object path. 规范相对对象路径。
pub(super) fn beneath(directory: &File, relative: &Path) -> Result<File> {
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(Error::Invalid(
            "session resource path must stay below its runtime directory".into(),
        ));
    }
    let name = std::ffi::CString::new(relative.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::Invalid("invalid session path".into()))?;
    #[repr(C)]
    struct How {
        flags: u64,
        mode: u64,
        resolve: u64,
    }
    let how = How {
        flags: (libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: 0x08 | 0x04 | 0x02,
    }; // BENEATH | NO_SYMLINKS | NO_MAGICLINKS
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            directory.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<How>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}

/// Pin one caller-owned private runtime directory.
/// 固定一个调用者所属的私有运行目录。
/// # Arguments
/// - `path`, `uid`: Exact runtime path and owner. 准确运行路径及属主。
pub(super) fn runtime_directory(path: &Path, uid: u32) -> Result<File> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = directory.metadata()?;
    if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(Error::Invalid(
            "host runtime directory must be private and owned by the caller".into(),
        ));
    }
    Ok(directory)
}

/// Validate a socket's descriptor rather than a replaceable path.
/// 校验 socket 描述符而非可替换路径。
/// # Arguments
/// - `file`, `uid`, `root_allowed`: Resource descriptor and accepted owners. 资源描述符及允许属主。
pub(super) fn socket(file: File, uid: u32, root_allowed: bool) -> Result<File> {
    let metadata = file.metadata()?;
    if !metadata.file_type().is_socket()
        || !(metadata.uid() == uid || (root_allowed && metadata.uid() == 0))
    {
        return Err(Error::Invalid(
            "session socket has an unexpected type or owner".into(),
        ));
    }
    Ok(file)
}

/// Admit only one socket component, never an arbitrary runtime tree.
/// 仅允许单个 socket 名称，不允许任意运行目录树。
/// # Arguments
/// - `value`: Requested socket basename. 请求的 socket 基名。
pub(super) fn component(value: &str) -> Result<&str> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains('/')
        || value.chars().any(char::is_control)
    {
        return Err(Error::Invalid("invalid session socket basename".into()));
    }
    Ok(value)
}

/// Require a local X server and derive its authenticated socket name.
/// 要求本地 X 服务器并推导其认证 socket 名称。
/// # Arguments
/// - `value`: Requested DISPLAY value. 请求的 DISPLAY 值。
pub(super) fn xdisplay(value: &str) -> Result<String> {
    let Some(number) = value.strip_prefix(':') else {
        return Err(Error::Invalid(
            "DISPLAY must name a local Unix X server".into(),
        ));
    };
    let mut pieces = number.split('.');
    let display = pieces.next().unwrap_or_default();
    let screen = pieces.next();
    if display.is_empty()
        || display.len() > 5
        || !display.bytes().all(|b| b.is_ascii_digit())
        || screen
            .is_some_and(|s| s.is_empty() || s.len() > 5 || !s.bytes().all(|b| b.is_ascii_digit()))
        || pieces.next().is_some()
    {
        return Err(Error::Invalid("invalid local DISPLAY".into()));
    }
    Ok(format!("X{display}"))
}

/// Pin a private caller-owned regular Xauthority file.
/// 固定调用者所属的私有普通 Xauthority 文件。
/// # Arguments
/// - `path`, `uid`: Explicit authority path and authenticated owner. 显式认证路径及认证属主。
fn authority(path: &Path, uid: u32) -> Result<File> {
    super::super::config::absolute(path, false)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_AUTHORITY_BYTES
    {
        return Err(Error::Invalid(
            "XAUTHORITY must be a private bounded file owned by the caller".into(),
        ));
    }
    Ok(file)
}

/// Build descriptor-pinned GUI/audio resources from an exact caller runtime.
/// 从准确调用者运行目录建立描述符固定的图形和音频资源。
/// # Arguments
/// - `uid`, `environment`: Authenticated UID and untrusted session hints. 认证 UID 和不可信会话提示。
pub(super) fn resources(uid: u32, environment: &BTreeMap<String, String>) -> Result<Vec<Resource>> {
    let host = PathBuf::from(format!("/run/user/{uid}"));
    if environment
        .get("XDG_RUNTIME_DIR")
        .is_some_and(|p| Path::new(p) != host)
    {
        return Err(Error::Invalid(
            "XDG_RUNTIME_DIR does not match the authenticated caller".into(),
        ));
    }
    let runtime = match runtime_directory(&host, uid) {
        Ok(runtime) => Some(runtime),
        Err(Error::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound
                && !environment.contains_key("WAYLAND_DISPLAY")
                && !environment.contains_key("PULSE_SERVER")
                && !environment.contains_key("PIPEWIRE_REMOTE") =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    let mut resources = Vec::new();
    if let Some(wayland) = environment.get("WAYLAND_DISPLAY").filter(|s| !s.is_empty()) {
        let name = if Path::new(wayland).is_absolute() {
            Path::new(wayland)
                .strip_prefix(&host)
                .ok()
                .and_then(|p| p.to_str())
                .ok_or_else(|| {
                    Error::Invalid("Wayland socket is outside the caller runtime".into())
                })?
        } else {
            wayland
        };
        let name = component(name)?;
        let source = socket(
            beneath(
                runtime
                    .as_ref()
                    .ok_or_else(|| Error::Invalid("Wayland runtime missing".into()))?,
                Path::new(name),
            )?,
            uid,
            false,
        )?;
        resources.push(Resource {
            source,
            target: host.join(name),
            read_only: false,
        });
    }
    if let Some(display) = environment.get("DISPLAY").filter(|s| !s.is_empty()) {
        let name = xdisplay(display)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open("/tmp/.X11-unix")?;
        if directory.metadata()?.uid() != 0 {
            return Err(Error::Invalid(
                "X11 socket directory must be root-owned".into(),
            ));
        }
        let source = socket(beneath(&directory, Path::new(&name))?, uid, true)?;
        resources.push(Resource {
            source,
            target: PathBuf::from("/tmp/.X11-unix").join(name),
            read_only: false,
        });
        let path = environment
            .get("XAUTHORITY")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                Error::Invalid("local X11 forwarding requires an explicit XAUTHORITY".into())
            })?;
        resources.push(Resource {
            source: authority(Path::new(path), uid)?,
            target: host.join("rstarsleep/Xauthority"),
            read_only: true,
        });
    }
    if let Some(runtime) = &runtime {
        let pipewire = component(
            environment
                .get("PIPEWIRE_REMOTE")
                .map(String::as_str)
                .unwrap_or("pipewire-0"),
        )?;
        match beneath(runtime, Path::new(pipewire)) {
            Ok(source) => resources.push(Resource {
                source: socket(source, uid, false)?,
                target: host.join(pipewire),
                read_only: false,
            }),
            Err(Error::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound
                    && !environment.contains_key("PIPEWIRE_REMOTE") => {}
            Err(error) => return Err(error),
        }
        if let Some(server) = environment.get("PULSE_SERVER")
            && server != &format!("unix:{}/pulse/native", host.display())
        {
            return Err(Error::Invalid(
                "PULSE_SERVER must name the caller's pulse/native socket".into(),
            ));
        }
        match beneath(runtime, Path::new("pulse/native")) {
            Ok(source) => resources.push(Resource {
                source: socket(source, uid, false)?,
                target: host.join("pulse/native"),
                read_only: false,
            }),
            Err(Error::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound
                    && !environment.contains_key("PULSE_SERVER") => {}
            Err(error) => return Err(error),
        }
    }
    Ok(resources)
}
