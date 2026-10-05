//! Package preset plugin resources for embedding in the executable.
//! 将预设插件资源打包，以嵌入可执行程序。

use std::{env, fs, io, path::Path};

use flate2::{Compression, GzBuilder};
use tar::{Builder, Header};

/// Build a reproducible Arch plugin archive whenever its resources change.
/// 预设资源变化时，生成可复现的 Arch 插件压缩包。
///
/// # Arguments
///
/// No parameters; Cargo supplies the manifest and output directories.
/// 无参数；Cargo 提供项目和输出目录。
fn main() -> io::Result<()> {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory");
    let output = env::var_os("OUT_DIR").expect("Cargo output directory");
    let root = Path::new(&manifest).join("presets/plugins/arch");
    println!("cargo:rerun-if-changed={}", root.display());
    let destination = fs::File::create(Path::new(&output).join("arch.tar.gz"))?;
    let encoder = GzBuilder::new()
        .mtime(0)
        .write(destination, Compression::default());
    let mut archive = Builder::new(encoder);
    append_directory(&mut archive, &root, &root)?;
    archive.into_inner()?.finish()?;
    Ok(())
}

/// Append sorted resources with stable ownership, timestamps, and portable modes.
/// 按排序追加资源，统一所有者、时间戳和可移植权限。
///
/// # Arguments
///
/// - `archive`: Tar builder receiving the preset resources.
///   用于接收预设资源的 Tar 构建器。
/// - `root`: Source plugin root omitted from archive entry paths.
///   插件源码根；归档路径不包含此根目录。
/// - `directory`: Current directory whose children are packaged recursively.
///   当前目录；递归打包其中的子项。
fn append_directory<W: io::Write>(
    archive: &mut Builder<W>,
    root: &Path,
    directory: &Path,
) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let relative = path
            .strip_prefix(root)
            .expect("preset resource within root");
        let mut header = Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        if metadata.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(0o755);
            archive.append_data(&mut header, relative, io::empty())?;
            append_directory(archive, root, &path)?;
        } else if metadata.is_file() {
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(metadata.len());
            header.set_mode(if executable { 0o755 } else { 0o644 });
            archive.append_data(&mut header, relative, fs::File::open(&path)?)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "preset resource must be a regular file or directory: {}",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}
