//! Validated plugin fixtures and a dependency-aware simulated package manager.
//! 已校验插件测试包及具备依赖关系的模拟包管理器。

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use crate::controller::{
    BuildBackend, BuildContext, Error, Result,
    plugins::{self, LoadedPlugin},
};

use super::{InstalledPackage, PackageSession, SoftwareState};

/// Simulate package reasons, dependencies, and query corruption without executing commands.
/// 模拟包安装原因、依赖关系和查询损坏，不执行命令。
#[derive(Default)]
pub(super) struct PackageBackend {
    pub(super) packages: RefCell<BTreeMap<String, bool>>,
    dependencies: BTreeMap<String, Vec<String>>,
    pub(super) calls: RefCell<Vec<(String, Vec<String>)>>,
    pub(super) remove_on_update: Vec<String>,
    pub(super) replace_on_update: Vec<(String, String)>,
    pub(super) inventory_override: Option<String>,
    pub(super) orphan_override: Option<String>,
    pub(super) resolution_override: Option<String>,
}

impl PackageBackend {
    /// Create an installed database and repository dependency graph.
    /// 创建已安装数据库和仓库依赖图。
    ///
    /// # Arguments
    ///
    /// - `packages`: Installed names and explicit installation flags.
    ///   已安装包名和显式安装标记。
    /// - `dependencies`: Repository packages and their direct dependencies.
    ///   仓库包及其直接依赖。
    pub(super) fn new(packages: &[(&str, bool)], dependencies: &[(&str, &[&str])]) -> Self {
        Self {
            packages: RefCell::new(
                packages
                    .iter()
                    .map(|(name, explicit)| ((*name).into(), *explicit))
                    .collect(),
            ),
            dependencies: dependencies
                .iter()
                .map(|(name, required)| {
                    (
                        (*name).into(),
                        required.iter().map(|name| (*name).into()).collect(),
                    )
                })
                .collect(),
            ..Self::default()
        }
    }

    /// Install one explicit root and its transitive dependency closure.
    /// 安装一个显式根及其传递依赖闭包。
    ///
    /// # Arguments
    ///
    /// - `self`: Simulated repository and installed database.
    ///   模拟仓库和已安装数据库。
    /// - `name`: Requested explicit package root.
    ///   请求显式安装的包根。
    fn install(&self, name: &str) {
        let mut packages = self.packages.borrow_mut();
        packages.insert(name.into(), true);
        let mut pending = vec![name.to_owned()];
        let mut visited = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            for dependency in self.dependencies.get(&name).into_iter().flatten() {
                packages.entry(dependency.clone()).or_insert(false);
                pending.push(dependency.clone());
            }
        }
    }

    /// Identify dependency packages with no installed reverse dependencies.
    /// 找出没有已安装反向依赖的依赖包。
    ///
    /// # Arguments
    ///
    /// - `self`: Repository relationships used for reverse dependency checks.
    ///   用于反向依赖检查的仓库关系。
    /// - `packages`: Current installed database and installation reasons.
    ///   当前已安装数据库和安装原因。
    fn orphans(&self, packages: &BTreeMap<String, bool>) -> Vec<String> {
        packages
            .iter()
            .filter(|(name, explicit)| {
                !**explicit
                    && !packages.keys().any(|dependent| {
                        self.dependencies
                            .get(dependent)
                            .is_some_and(|dependencies| dependencies.contains(name))
                    })
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Remove orphan roots and newly unneeded dependency chains as pacman -Rns does.
    /// 类似 pacman -Rns，删除孤儿根及随后不再需要的依赖链。
    ///
    /// # Arguments
    ///
    /// - `self`: Simulated dependency graph and mutable installed database.
    ///   模拟依赖图和可变已安装数据库。
    /// - `roots`: Verified orphan roots passed by the software workflow.
    ///   软件流程传入的已校验孤儿根。
    fn remove_orphans(&self, roots: &[String]) {
        let mut packages = self.packages.borrow_mut();
        let mut candidates = BTreeSet::new();
        for root in roots {
            packages.remove(root);
            candidates.extend(self.dependencies.get(root).into_iter().flatten().cloned());
        }
        loop {
            let removable: Vec<_> = self
                .orphans(&packages)
                .into_iter()
                .filter(|name| candidates.contains(name))
                .collect();
            if removable.is_empty() {
                break;
            }
            for name in removable {
                packages.remove(&name);
                candidates.extend(self.dependencies.get(&name).into_iter().flatten().cloned());
            }
        }
    }

    /// Return the number of invocations of a fixed operation.
    /// 返回一个固定操作的调用次数。
    ///
    /// # Arguments
    ///
    /// - `self`: Recorded simulated operations.
    ///   已记录的模拟操作。
    /// - `operation`: Operation name whose occurrences are counted.
    ///   待计数的操作名称。
    pub(super) fn count(&self, operation: &str) -> usize {
        self.calls
            .borrow()
            .iter()
            .filter(|(name, _)| name == operation)
            .count()
    }
}

impl BuildBackend for PackageBackend {
    /// Reject unexpected storage creation in package-only tests.
    /// 在纯软件测试中拒绝意外存储创建。
    ///
    /// # Arguments
    ///
    /// - `self`: Unused package simulation backend.
    ///   未使用的软件模拟后端。
    /// - `path`: Unexpected subvolume destination, reported in the panic.
    ///   意外子卷目标，在 panic 中报告。
    fn create_subvolume(&self, path: &Path) -> Result<()> {
        panic!("unexpected subvolume creation: {path:?}")
    }

    /// Reject unexpected storage checks in package-only tests.
    /// 在纯软件测试中拒绝意外存储检查。
    ///
    /// # Arguments
    ///
    /// - `self`: Unused package simulation backend.
    ///   未使用的软件模拟后端。
    /// - `path`: Unexpected subvolume path.
    ///   意外子卷路径。
    /// - `readonly`: Unexpected required subvolume state.
    ///   意外要求的子卷状态。
    fn check_subvolume(&self, path: &Path, readonly: bool) -> Result<()> {
        panic!("unexpected subvolume check: {path:?}, readonly={readonly}")
    }

    /// Reject unexpected snapshots in package-only tests.
    /// 在纯软件测试中拒绝意外快照。
    ///
    /// # Arguments
    ///
    /// - `self`: Unused package simulation backend.
    ///   未使用的软件模拟后端。
    /// - `source`: Unexpected snapshot source.
    ///   意外快照来源。
    /// - `destination`: Unexpected snapshot destination.
    ///   意外快照目标。
    /// - `readonly`: Unexpected required snapshot state.
    ///   意外要求的快照状态。
    fn snapshot(&self, source: &Path, destination: &Path, readonly: bool) -> Result<()> {
        panic!("unexpected snapshot: {source:?} -> {destination:?}, readonly={readonly}")
    }

    /// Reject unexpected storage deletion in package-only tests.
    /// 在纯软件测试中拒绝意外存储删除。
    ///
    /// # Arguments
    ///
    /// - `self`: Unused package simulation backend.
    ///   未使用的软件模拟后端。
    /// - `path`: Unexpected subvolume deletion target.
    ///   意外子卷删除目标。
    fn delete_subvolume(&self, path: &Path) -> Result<()> {
        panic!("unexpected subvolume deletion: {path:?}")
    }

    /// Simulate fixed plugin operations with the same names and argument shapes as Arch.
    /// 使用与 Arch 相同的名称和参数形式模拟固定插件操作。
    ///
    /// # Arguments
    ///
    /// - `self`: Mutable simulated package database and recorded calls.
    ///   可变模拟包数据库和调用记录。
    /// - `_plugin`: Unused loaded fixture; no plugin script is executed.
    ///   未使用的加载测试包；不执行插件脚本。
    /// - `operation`: Fixed package API operation.
    ///   固定软件 API 操作。
    /// - `args`: Individually supplied package names or installation reason.
    ///   独立传入的包名或安装原因。
    /// - `_root`: Unused system root; no real package database is touched.
    ///   未使用的系统根；不访问真实包数据库。
    /// - `_context`: Unused frozen inputs; this simulation has no external effects.
    ///   未使用的冻结输入；本模拟没有外部影响。
    fn invoke(
        &self,
        _plugin: &LoadedPlugin,
        operation: &str,
        args: &[String],
        _root: &Path,
        _context: &BuildContext,
    ) -> Result<String> {
        self.calls
            .borrow_mut()
            .push((operation.into(), args.to_vec()));
        match operation {
            "update" => {
                let mut packages = self.packages.borrow_mut();
                for name in &self.remove_on_update {
                    packages.remove(name);
                }
                for (previous, replacement) in &self.replace_on_update {
                    if let Some(explicit) = packages.remove(previous) {
                        packages.insert(replacement.clone(), explicit);
                    }
                }
            }
            "resolve_packages" => {
                assert_eq!(args.len(), 1);
                return Ok(self
                    .resolution_override
                    .clone()
                    .unwrap_or_else(|| format!("{}\n", args[0])));
            }
            "query_packages" => {
                return Ok(self.inventory_override.clone().unwrap_or_else(|| {
                    self.packages
                        .borrow()
                        .iter()
                        .map(|(name, explicit)| {
                            format!(
                                "{name}\t{}\n",
                                if *explicit { "explicit" } else { "dependency" }
                            )
                        })
                        .collect()
                }));
            }
            "install" => {
                for name in args {
                    self.install(name);
                }
            }
            "mark_reason" => {
                let (reason, names) = args.split_first().expect("missing installation reason");
                assert!(reason == "explicit" || reason == "dependency");
                let mut packages = self.packages.borrow_mut();
                for name in names {
                    *packages.get_mut(name).expect("marking an absent package") =
                        reason == "explicit";
                }
            }
            "list_orphans" => {
                return Ok(self
                    .orphan_override
                    .clone()
                    .unwrap_or_else(|| self.orphans(&self.packages.borrow()).join("\n")));
            }
            "remove_orphans" => self.remove_orphans(args),
            _ => return Err(Error::Invalid(format!("unexpected operation: {operation}"))),
        }
        Ok(String::new())
    }
}

/// Own a real validated plugin archive while every operation uses the simulated backend.
/// 持有真实已校验插件归档，同时全部操作使用模拟后端。
pub(super) struct Fixture {
    _temporary: tempfile::TempDir,
    plugin: LoadedPlugin,
    pub(super) backend: PackageBackend,
    context: BuildContext,
    root: PathBuf,
}

impl Fixture {
    /// Load a minimal plugin whose base target is protected.
    /// 加载基础目标受保护的最小插件。
    ///
    /// # Arguments
    ///
    /// - `backend`: Simulated installed database and repository.
    ///   模拟已安装数据库和仓库。
    pub(super) fn new(backend: PackageBackend) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let archive_path = temporary.path().join("plugin.tar");
        let mut archive = tar::Builder::new(fs::File::create(&archive_path).unwrap());
        let metadata = b"schema_version=1\napi_version=2\nid='test'\nversion='1'\narchitectures=['x86_64']\ncache_format='test-v1'\nprotected_packages=['base']\n[tools]\nhost=[]\nsystem=[]\n";
        let mut files = vec![("plugin.toml".to_owned(), metadata.as_slice())];
        files.extend(
            plugins::REQUIRED_OPERATIONS
                .iter()
                .map(|name| (format!("{name}.sh"), b"#!/bin/bash\nexit 99\n".as_slice())),
        );
        for (name, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_size(contents.len() as u64);
            header.set_cksum();
            archive.append_data(&mut header, name, contents).unwrap();
        }
        archive.finish().unwrap();
        let plugin = plugins::load(&archive_path).unwrap();
        let root = temporary.path().join("root");
        let context = BuildContext {
            system_id: "desktop".into(),
            architecture: "x86_64".into(),
            config: temporary.path().join("config"),
        };
        Self {
            _temporary: temporary,
            plugin,
            backend,
            context,
            root,
        }
    }

    /// Borrow a software stage bound to this fixture's simulated execution environment.
    /// 借用绑定本测试模拟执行环境的软件阶段。
    ///
    /// # Arguments
    ///
    /// - `self`: Fixture owning the plugin, backend, and archived inputs.
    ///   持有插件、后端和留档输入的测试环境。
    pub(super) fn session(&self) -> PackageSession<'_, PackageBackend> {
        PackageSession {
            backend: &self.backend,
            plugin: &self.plugin,
            root: &self.root,
            context: &self.context,
        }
    }
}

/// Capture a prior successful software stage with explicit managed targets.
/// 捕获管理目标显式安装的过往成功软件阶段。
///
/// # Arguments
///
/// - `requested`: Previously managed concrete targets.
///   先前管理的具体目标。
/// - `packages`: Previous actual inventory and installation reasons.
///   先前实际清单和安装原因。
pub(super) fn previous(requested: &[&str], packages: &[(&str, bool)]) -> SoftwareState {
    SoftwareState {
        requested: requested.iter().map(|name| (*name).into()).collect(),
        installed: packages
            .iter()
            .map(|(name, explicit)| InstalledPackage {
                name: (*name).into(),
                explicit: *explicit,
            })
            .collect(),
    }
}
