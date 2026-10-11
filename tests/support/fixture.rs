//! Create ordinary manager fixtures, preset archives, and editable build inputs.
//! 创建普通管理器夹具、预设压缩包和可编辑构建输入。

use rstarsleep::config::ConfigStore;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct Fixture {
    _temporary: tempfile::TempDir,
    pub store: ConfigStore,
}

impl Fixture {
    /// Create ordinary manager directories and a genuine preset plugin archive.
    /// 创建普通管理目录和真实预设插件压缩包。
    /// # Arguments
    ///
    /// No parameters.
    /// 无参数。
    pub fn new() -> Self {
        Self::with_predownload(false)
    }

    pub fn with_predownload(enabled: bool) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("manager");
        fs::create_dir_all(root.join("config")).unwrap();
        fs::create_dir_all(root.join("plugins")).unwrap();
        fs::write(root.join("config/rstarsleep.toml"), "schema_version = 1\n").unwrap();
        let source = temporary.path().join("plugin-source");
        copy_plugin(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("presets/plugins/arch"),
            &source,
        );
        let metadata = source.join("plugin.toml");
        let text = fs::read_to_string(&metadata).unwrap();
        fs::write(
            &metadata,
            text.replace(
                "predownload = true",
                if enabled {
                    "predownload = true"
                } else {
                    "predownload = false"
                },
            ),
        )
        .unwrap();
        let file = fs::File::create(root.join("plugins/arch.tar")).unwrap();
        let mut archive = tar::Builder::new(file);
        archive.append_dir_all(".", &source).unwrap();
        archive.finish().unwrap();
        Self {
            _temporary: temporary,
            store: ConfigStore::new(root).unwrap(),
        }
    }

    /// Initialize one system and write its editable package table.
    /// 初始化一个系统并写入可编辑包表。
    /// # Arguments
    ///
    /// - `self`: Isolated manager.
    ///   隔离管理器。
    /// - `id`: System identifier.
    ///   系统标识。
    /// - `packages`: Requested package names.
    ///   所需包名。
    pub fn system(&self, id: &str, packages: &[&str]) {
        self.store.initialize_system(id).unwrap();
        self.packages(id, packages);
    }

    /// Replace editable build inputs without touching archived versions.
    /// 替换可编辑构建输入，不修改留档版本。
    /// # Arguments
    ///
    /// - `self`: Isolated manager.
    ///   隔离管理器。
    /// - `id`: System identifier.
    ///   系统标识。
    /// - `packages`: New package names.
    ///   新包名。
    pub fn packages(&self, id: &str, packages: &[&str]) {
        let path = self
            .store
            .system_layout(id)
            .unwrap()
            .config
            .join("build/config.conf");
        fs::write(path, format!("[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\nArchitecture=x86_64\n[Content]\nPackages=\n    {}\n", packages.join("\n    "))).unwrap();
    }

    /// Locate one completed stage's root or metadata.
    /// 定位一个完成阶段的根或元数据。
    /// # Arguments
    ///
    /// - `self`: Isolated manager.
    ///   隔离管理器。
    /// - `version`: Recorded version.
    ///   留档版本。
    /// - `entry`: Stage name or sidecar filename.
    ///   阶段名或旁记录文件名。
    pub fn cache(&self, version: &str, entry: &str) -> PathBuf {
        self.store
            .system_layout("desktop")
            .unwrap()
            .cache
            .join(version)
            .join(entry)
    }

    /// Explicitly retire the test artifact before deleting all its build history.
    /// 删除全部测试构建历史前显式撤销当前产物。
    /// # Arguments
    /// - `self`: Isolated manager. 隔离管理器。
    /// - `id`: System whose current artifact is retired. 撤销当前产物的系统。
    pub fn retire_current(&self, id: &str) {
        fs::remove_file(
            self.store
                .system_layout(id)
                .unwrap()
                .root
                .join("build-current.toml"),
        )
        .unwrap();
    }

    /// Assert all scratch subvolumes were deleted after the attempted build.
    /// 验证构建尝试之后所有临时子卷均已删除。
    /// # Arguments
    ///
    /// - `self`: Manager owning default scratch space.
    ///   拥有默认临时空间的管理器。
    /// - `version`: Attempt to inspect.
    ///   待检查尝试。
    pub fn cleaned(&self, version: &str) {
        let scratch = self.store.root().join("work/desktop").join(version);
        assert!(!scratch.join("root").exists());
        assert!(!scratch.join("base-root").exists());
        assert!(!scratch.join("script-root").exists());
    }
}

fn copy_plugin(source: &Path, dest: &Path) {
    fs::create_dir(dest).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination = dest.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_plugin(&entry.path(), &destination);
        } else {
            fs::copy(entry.path(), destination).unwrap();
        }
    }
}
