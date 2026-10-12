//! Simulate marked subvolumes and package-manager operations without host mutations.
//! 模拟有标记子卷和包管理操作，不修改宿主系统。

use rstarsleep::{
    config::read_build_config_at,
    controller::{BuildBackend, BuildContext, Error, Result, plugins::LoadedPlugin},
};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

pub const MARKER: &str = ".simulated-subvolume";
const DATABASE: &str = ".simulated-packages";

#[derive(Clone, Debug)]
pub struct Event {
    pub operation: String,
    pub arguments: Vec<String>,
    pub root: PathBuf,
    pub packages: Vec<String>,
    pub system_id: String,
    pub parameters: BTreeMap<String, String>,
    pub distribution_options: BTreeMap<String, String>,
    pub environment: rstarsleep::config::Environment,
    pub persistent_mounts: Vec<rstarsleep::controller::PersistentMount>,
}

/// One attempted CoW boundary between a completed root and a new root.
/// 完成根与新根之间一次已尝试的 CoW 边界。
#[derive(Clone, Debug)]
pub struct SnapshotEvent {
    /// Source root, which must remain unchanged.
    /// 必须保持不变的来源根。
    pub source: PathBuf,
    /// Newly reserved root path.
    /// 新预留根路径。
    pub destination: PathBuf,
    /// Expected read-only state after the snapshot.
    /// 快照后的预期只读状态。
    pub readonly: bool,
}

#[derive(Default)]
pub struct SimulatedBackend {
    pub events: RefCell<Vec<Event>>,
    pub running_root: RefCell<Option<PathBuf>>,
    pub fail_inheritance_copy: Cell<bool>,
    pub fail_cleanup: Cell<bool>,
    pub boot_publications: Cell<usize>,
    pub snapshots: RefCell<Vec<SnapshotEvent>>,
    pub fail_operation: RefCell<Option<&'static str>>,
    pub fail_restore: Cell<bool>,
    pub fail_restore_to: RefCell<Option<&'static str>>,
    pub protect_readonly_snapshots: Cell<bool>,
    pub fail_readonly_snapshot_after_create: Cell<bool>,
    pub remove_kept_on_update: Cell<bool>,
    pub mutate_editable: RefCell<Option<PathBuf>>,
    pub mutate_environment: RefCell<Option<(PathBuf, String)>>,
    generation: Cell<u32>,
}

impl SimulatedBackend {
    /// Count executions of one plugin operation since the latest reset.
    /// 统计最近重置之后某插件操作的执行次数。
    /// # Arguments
    ///
    /// - `self`: Recorded simulated operations.
    ///   已记录模拟操作。
    /// - `operation`: Operation to count.
    ///   待统计操作。
    pub fn count(&self, operation: &str) -> usize {
        self.events
            .borrow()
            .iter()
            .filter(|event| event.operation == operation)
            .count()
    }

    /// Read package identities and installation reasons from one simulated root.
    /// 从一个模拟根读取包标识和安装原因。
    /// # Arguments
    ///
    /// - `root`: Simulated system root.
    ///   模拟系统根。
    pub fn inventory(root: &Path) -> BTreeMap<String, bool> {
        fs::read_to_string(root.join(DATABASE))
            .unwrap_or_default()
            .lines()
            .map(|line| {
                let (name, reason) = line.split_once('\t').unwrap();
                (name.into(), reason == "explicit")
            })
            .collect()
    }

    /// Persist a simulated package database with deterministic ordering.
    /// 按确定顺序持久化模拟包数据库。
    /// # Arguments
    ///
    /// - `root`: Writable simulated root.
    ///   可写模拟根。
    /// - `packages`: Names and installation reasons.
    ///   包名和安装原因。
    fn save(root: &Path, packages: &BTreeMap<String, bool>) {
        let text: String = packages
            .iter()
            .map(|(name, explicit)| {
                format!(
                    "{name}\t{}\n",
                    if *explicit { "explicit" } else { "dependency" }
                )
            })
            .collect();
        fs::write(root.join(DATABASE), text).unwrap();
    }

    /// Return the small dependency graph used to verify maintenance behavior.
    /// 返回用于验证维护行为的小型依赖图。
    /// # Arguments
    ///
    /// - `name`: Concrete package name.
    ///   具体包名。
    fn dependencies(name: &str) -> &'static [&'static str] {
        match name {
            "base" => &["base-dependency"],
            "old" => &["old-dependency", "shared"],
            "new" | "editor" => &["shared"],
            _ => &[],
        }
    }

    /// Compute dependencies no longer reachable from an explicit package.
    /// 计算从显式包不可再到达的依赖。
    /// # Arguments
    ///
    /// - `packages`: Actual simulated inventory.
    ///   实际模拟清单。
    fn orphans(packages: &BTreeMap<String, bool>) -> Vec<String> {
        let mut needed: BTreeSet<String> = packages
            .iter()
            .filter(|(_, explicit)| **explicit)
            .map(|(name, _)| name.clone())
            .collect();
        let mut pending: Vec<String> = needed.iter().cloned().collect();
        while let Some(package) = pending.pop() {
            for dependency in Self::dependencies(&package) {
                if needed.insert((*dependency).into()) {
                    pending.push((*dependency).into());
                }
            }
        }
        packages
            .iter()
            .filter(|(name, explicit)| !**explicit && !needed.contains(*name))
            .map(|(name, _)| name.clone())
            .collect()
    }
}

/// Copy a simulated snapshot; production always uses the real Btrfs backend.
/// 复制模拟快照；生产始终使用真实 Btrfs 后端。
/// # Arguments
///
/// - `source`: Simulated source tree.
///   模拟来源树。
/// - `destination`: Absent snapshot tree.
///   不存在的快照树。
fn copy_simulated_snapshot(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_simulated_snapshot(&entry.path(), &target);
        } else if entry.file_type().unwrap().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path()).unwrap(), target).unwrap();
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn copy_inherited_stubs(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name() == MARKER {
            continue;
        }
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.path().join(MARKER).exists() {
                fs::create_dir_all(target).unwrap();
            } else {
                copy_inherited_stubs(&entry.path(), &target);
            }
        } else if entry.file_type().unwrap().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path()).unwrap(), target).unwrap();
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn copy_snapshot_stubs(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.path().join(MARKER).exists() {
                fs::create_dir(&target).unwrap();
            } else {
                copy_snapshot_stubs(&entry.path(), &target);
            }
        } else if entry.file_type().unwrap().is_symlink() {
            std::os::unix::fs::symlink(fs::read_link(entry.path()).unwrap(), target).unwrap();
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

impl BuildBackend for SimulatedBackend {
    fn publish_boot(
        &self,
        outcome: &rstarsleep::controller::BuildOutcome,
        _config: &rstarsleep::config::RuntimeConfig,
    ) -> Result<()> {
        self.check_subvolume(&outcome.root, true)?;
        self.boot_publications.set(self.boot_publications.get() + 1);
        Ok(())
    }
    fn set_readonly(&self, path: &Path, readonly: bool) -> Result<()> {
        fs::write(
            path.join(MARKER),
            if readonly { "readonly" } else { "writable" },
        )
        .unwrap();
        Ok(())
    }
    fn filesystem_location(&self, path: &Path) -> Result<(String, PathBuf)> {
        Ok(("simulated-uuid".into(), path.to_path_buf()))
    }
    fn copy_inherited(&self, source: &Path, target: &Path, _force_cow: bool) -> Result<()> {
        if self.fail_inheritance_copy.replace(false) {
            return Err(Error::Invalid("injected inheritance copy failure".into()));
        }
        if source.is_dir() {
            if _force_cow {
                copy_inherited_stubs(source, target);
            } else {
                copy_simulated_snapshot(source, target);
            }
        } else {
            fs::copy(source, target).unwrap();
        }
        Ok(())
    }
    fn check_cow_source(&self, _source: &Path, _parent: &Path) -> Result<()> {
        Ok(())
    }
    fn source_subvolume_id(&self, _path: &Path) -> Result<String> {
        Ok("simulated-id".into())
    }
    fn prepare_inheritance_initramfs(&self, _root: &Path) -> Result<()> {
        Ok(())
    }
    fn nested_subvolumes(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let mut pending = vec![path.to_path_buf()];
        let mut found = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    if entry.path().join(MARKER).exists() {
                        found.push(entry.path().strip_prefix(path).unwrap().to_path_buf());
                    }
                    pending.push(entry.path());
                }
            }
        }
        Ok(found)
    }

    fn is_running_root(&self, path: &Path) -> Result<bool> {
        Ok(self.running_root.borrow().as_deref() == Some(path))
    }
    fn check_no_nested_subvolumes(&self, path: &Path) -> Result<()> {
        let mut pending = vec![path.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    if entry.path().join(MARKER).exists() {
                        return Err(Error::Invalid("nested simulated subvolume".into()));
                    }
                    pending.push(entry.path());
                }
            }
        }
        Ok(())
    }

    /// Create a marked writable simulated subvolume.
    /// 创建有标记的可写模拟子卷。
    /// # Arguments
    ///
    /// - `self`: Simulation backend.
    ///   模拟后端。
    /// - `path`: New root path.
    ///   新根路径。
    fn create_subvolume(&self, path: &Path) -> Result<()> {
        assert!(!path.exists());
        fs::create_dir_all(path).unwrap();
        fs::write(path.join(MARKER), "writable").unwrap();
        Ok(())
    }

    /// Reject ordinary directories and unexpected simulated read-only properties.
    /// 拒绝普通目录和非预期的模拟只读属性。
    /// # Arguments
    ///
    /// - `self`: Simulation backend.
    ///   模拟后端。
    /// - `path`: Root to inspect.
    ///   待检查根。
    /// - `readonly`: Required read-only state.
    ///   必需只读状态。
    fn check_subvolume(&self, path: &Path, readonly: bool) -> Result<()> {
        let expected = if readonly { "readonly" } else { "writable" };
        if !fs::read_to_string(path.join(MARKER)).is_ok_and(|actual| actual == expected) {
            return Err(Error::Invalid(format!(
                "not a {expected} simulated subvolume: {}",
                path.display()
            )));
        }
        Ok(())
    }

    /// Snapshot marked roots while allowing an injected restore failure.
    /// 对有标记根建立快照，并允许注入恢复失败。
    /// # Arguments
    ///
    /// - `self`: Simulation backend.
    ///   模拟后端。
    /// - `source`: Existing simulated subvolume.
    ///   已有模拟子卷。
    /// - `destination`: Absent destination.
    ///   不存在的目标。
    /// - `readonly`: New snapshot property.
    ///   新快照属性。
    fn snapshot(&self, source: &Path, destination: &Path, readonly: bool) -> Result<()> {
        assert!(source.join(MARKER).is_file());
        assert!(!destination.exists());
        self.snapshots.borrow_mut().push(SnapshotEvent {
            source: source.into(),
            destination: destination.into(),
            readonly,
        });
        if !readonly && self.fail_restore.replace(false) {
            return Err(Error::Invalid("injected CoW restore failure".into()));
        }
        if !readonly
            && self
                .fail_restore_to
                .borrow()
                .as_ref()
                .is_some_and(|name| destination.ends_with(name))
        {
            self.fail_restore_to.borrow_mut().take();
            return Err(Error::Invalid("injected CoW restore failure".into()));
        }
        copy_snapshot_stubs(source, destination);
        fs::write(
            destination.join(MARKER),
            if readonly { "readonly" } else { "writable" },
        )
        .unwrap();
        if readonly && self.protect_readonly_snapshots.get() {
            // Directory moves across parents check write access on the moved root.
            // 跨父目录移动时，VFS 会检查被移动目录根的写权限。
            fs::set_permissions(destination, fs::Permissions::from_mode(0o555)).unwrap();
        }
        if readonly && self.fail_readonly_snapshot_after_create.replace(false) {
            return Err(Error::Invalid(
                "injected failure after snapshot creation".into(),
            ));
        }
        Ok(())
    }

    /// Delete only roots carrying simulated subvolume identity.
    /// 仅删除具有模拟子卷身份的根。
    /// # Arguments
    ///
    /// - `self`: Simulation backend.
    ///   模拟后端。
    /// - `path`: Owned scratch subvolume.
    ///   所属临时子卷。
    fn delete_subvolume_recursive(&self, path: &Path) -> Result<()> {
        if self.fail_cleanup.replace(false) {
            return Err(Error::Invalid("injected cleanup failure".into()));
        }
        self.delete_subvolume(path)
    }
    fn delete_subvolume(&self, path: &Path) -> Result<()> {
        assert!(path.join(MARKER).is_file());
        if self.protect_readonly_snapshots.get() {
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::remove_dir_all(path).unwrap();
        Ok(())
    }

    /// Simulate the fixed plugin protocol against one writable package database.
    /// 在一个可写包数据库上模拟固定插件协议。
    /// # Arguments
    ///
    /// - `self`: Simulation backend and injected failures.
    ///   模拟后端及注入失败。
    /// - `plugin`: Loaded preset metadata.
    ///   已加载预设元数据。
    /// - `operation`: Fixed plugin operation.
    ///   固定插件操作。
    /// - `args`: Individually supplied arguments.
    ///   独立提供的参数。
    /// - `root`: Writable simulated system root.
    ///   可写模拟系统根。
    /// - `context`: Actual frozen build inputs.
    ///   实际冻结构建输入。
    fn invoke(
        &self,
        plugin: &LoadedPlugin,
        operation: &str,
        args: &[String],
        root: &Path,
        context: &BuildContext,
    ) -> Result<String> {
        self.check_subvolume(root, false)?;
        assert_eq!(plugin.metadata.id, "arch");
        assert!(
            plugin
                .metadata
                .architectures
                .contains(&context.architecture)
        );
        let frozen_packages = if context.system_id == "predownload" {
            Vec::new()
        } else {
            read_build_config_at(&context.config)?.packages
        };
        self.events.borrow_mut().push(Event {
            operation: operation.into(),
            arguments: args.to_vec(),
            root: root.into(),
            packages: frozen_packages,
            system_id: context.system_id.clone(),
            distribution_options: context.distribution_options.clone(),
            environment: context.environment.clone(),
            parameters: context.parameters.clone(),
            persistent_mounts: context.persistent_mounts.clone(),
        });
        if operation == "predownload" {
            fs::write(root.join(".download-work-only"), "temporary").unwrap();
            for mount in &context.persistent_mounts {
                fs::write(mount.source.join(".prepared-batch"), args.join("\n")).unwrap();
            }
        }
        if self.fail_operation.borrow().as_ref() == Some(&operation) {
            self.fail_operation.borrow_mut().take();
            return Err(Error::Invalid(format!("injected {operation} failure")));
        }
        let mut packages = Self::inventory(root);
        let mut output = String::new();
        match operation {
            "bootstrap" => {
                if let Some((path, text)) = self.mutate_environment.borrow_mut().take() {
                    fs::write(path, text).unwrap();
                }
                let generation = self.generation.get() + 1;
                self.generation.set(generation);
                fs::write(root.join(".base-generation"), generation.to_string()).unwrap();
                if let Some(path) = self.mutate_editable.borrow_mut().take() {
                    fs::write(path, "[Config]\nSchemaVersion=1\n[Distribution]\nDistribution=arch\nArchitecture=x86_64\n[Content]\nPackages=mutated\n").unwrap();
                }
            }
            "base_install" => {
                packages.insert("base".into(), true);
                packages.insert("base-dependency".into(), false);
            }
            "system_init" => {
                assert_eq!(args.len(), 1);
                fs::create_dir_all(root.join("etc")).unwrap();
                fs::write(root.join("etc/machine-id"), format!("{}\n", args[0])).unwrap();
            }
            "predownload_install" | "predownload_update" | "predownload" => {}
            "mount_inheritance" => {
                fs::create_dir_all(root.join("etc")).unwrap();
                fs::write(
                    root.join("etc/fstab"),
                    format!("# BEGIN RStarSleep inherited mounts\n{}# END RStarSleep inherited mounts\n",args.chunks(4)
                        .map(|parts| format!("{}\n", parts.join(" ")))
                        .collect::<String>()),
                )
                .unwrap();
            }
            "update" => {
                if root.ends_with("root") && self.remove_kept_on_update.replace(false) {
                    packages.remove("kept");
                }
            }
            "resolve_packages" => {
                output = args.iter().map(|name| format!("{name}\n")).collect();
            }
            "query_packages" => {
                output = fs::read_to_string(root.join(DATABASE)).unwrap_or_default();
            }
            "query_package_files" => {
                output = packages
                    .keys()
                    .map(|name| format!("{name}\t/usr/bin/{name}\n"))
                    .collect();
            }
            "install" => {
                for name in args {
                    packages.insert(name.clone(), true);
                    for dependency in Self::dependencies(name) {
                        packages.entry((*dependency).into()).or_insert(false);
                    }
                }
            }
            "mark_reason" => {
                let explicit = match args[0].as_str() {
                    "explicit" => true,
                    "dependency" => false,
                    other => panic!("invalid reason {other}"),
                };
                for name in &args[1..] {
                    *packages.get_mut(name).expect("marking installed package") = explicit;
                }
            }
            "list_orphans" => {
                output = Self::orphans(&packages)
                    .iter()
                    .map(|name| format!("{name}\n"))
                    .collect();
            }
            "remove_orphans" => {
                let orphans = Self::orphans(&packages);
                for name in args {
                    assert!(orphans.contains(name));
                    packages.remove(name);
                }
            }
            other => panic!("unexpected plugin operation {other}"),
        }
        Self::save(root, &packages);
        Ok(output)
    }
    /// Execute test shell code in the copied payload with a simulated root environment.
    /// 在已复制资源中，使用模拟系统根环境执行测试 shell 代码。
    /// # Arguments
    /// - `self`: Simulated backend recording order.
    ///   记录顺序的模拟后端。
    /// - `root`: Writable isolated test root.
    ///   可写隔离测试根。
    /// - `directory`: Guest payload directory.
    ///   容器内资源目录。
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
        assert!(Self::inventory(root).contains_key("base"));
        self.events.borrow_mut().push(Event {
            operation: "script".into(),
            arguments: vec![entrypoint.to_string_lossy().into()],
            root: root.into(),
            packages: read_build_config_at(&context.config)?.packages,
            system_id: context.system_id.clone(),
            distribution_options: context.distribution_options.clone(),
            environment: context.environment.clone(),
            parameters: context.parameters.clone(),
            persistent_mounts: context.persistent_mounts.clone(),
        });
        let payload = root.join(directory.strip_prefix("/").unwrap());
        assert!(payload.starts_with(root));
        let output = std::process::Command::new("bash")
            .arg("--")
            .arg(entrypoint)
            .current_dir(&payload)
            .env("RSTAR_ROOT", root)
            .env("RSTAR_SYSTEM_ID", &context.system_id)
            .env("RSTAR_CONFIG_DIR", &context.config)
            .output()
            .unwrap();
        if !output.status.success() {
            return Err(Error::Command {
                command: "simulated script".into(),
                status: output.status.to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).into(),
            });
        }
        Ok(())
    }
}
