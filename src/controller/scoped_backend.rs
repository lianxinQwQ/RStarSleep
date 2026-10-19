//! Apply global persistent CoW transactions to every plugin operation.
use super::{
    BuildBackend, BuildContext, Error, Result, fs,
    persistent::PersistentTransaction,
    plugins::{self, LoadedPlugin},
    progress,
};
use std::path::{Path, PathBuf};

pub(super) struct ScopedBackend<'a, B> {
    pub backend: &'a B,
    pub pool: PathBuf,
    pub state: PathBuf,
}

impl<B: BuildBackend> BuildBackend for ScopedBackend<'_, B> {
    fn deduplicate_file(
        &self,
        source: &std::fs::File,
        target: &std::fs::File,
    ) -> Result<super::DedupOutcome> {
        self.backend.deduplicate_file(source, target)
    }

    fn sync_filesystem(&self, path: &Path) -> Result<()> {
        self.backend.sync_filesystem(path)
    }
    fn nested_subvolumes(&self, path: &Path) -> Result<Vec<PathBuf>> {
        self.backend.nested_subvolumes(path)
    }
    fn is_subvolume(&self, path: &Path) -> Result<bool> {
        self.backend.is_subvolume(path)
    }
    fn source_subvolume_id(&self, path: &Path) -> Result<String> {
        self.backend.source_subvolume_id(path)
    }
    fn prepare_inheritance_initramfs(&self, root: &Path) -> Result<()> {
        self.backend.prepare_inheritance_initramfs(root)
    }
    fn filesystem_location(&self, path: &Path) -> Result<(String, PathBuf)> {
        self.backend.filesystem_location(path)
    }
    fn set_readonly(&self, path: &Path, readonly: bool) -> Result<()> {
        self.backend.set_readonly(path, readonly)
    }
    fn copy_inherited(&self, source: &Path, target: &Path, force_cow: bool) -> Result<()> {
        self.backend.copy_inherited(source, target, force_cow)
    }
    fn check_cow_source(&self, source: &Path, parent: &Path) -> Result<()> {
        self.backend.check_cow_source(source, parent)
    }
    fn publish_boot(
        &self,
        outcome: &super::BuildOutcome,
        config: &crate::config::RuntimeConfig,
    ) -> Result<()> {
        self.backend.publish_boot(outcome, config)
    }
    fn create_subvolume(&self, path: &Path) -> Result<()> {
        self.backend.create_subvolume(path)
    }
    fn check_subvolume(&self, path: &Path, readonly: bool) -> Result<()> {
        self.backend.check_subvolume(path, readonly)
    }
    fn snapshot(&self, source: &Path, destination: &Path, readonly: bool) -> Result<()> {
        self.backend.snapshot(source, destination, readonly)
    }
    fn delete_subvolume(&self, path: &Path) -> Result<()> {
        self.backend.delete_subvolume_recursive(path)
    }
    fn delete_subvolume_recursive(&self, path: &Path) -> Result<()> {
        self.backend.delete_subvolume_recursive(path)
    }
    fn check_no_nested_subvolumes(&self, path: &Path) -> Result<()> {
        self.backend.check_no_nested_subvolumes(path)
    }
    fn invoke(
        &self,
        plugin: &LoadedPlugin,
        operation: &str,
        args: &[String],
        root: &Path,
        context: &BuildContext,
    ) -> Result<String> {
        let query = plugins::is_query(operation) && operation != "maintain_packages";
        let execute = || self.invoke_transaction(plugin, operation, args, root, context, !query);
        if query {
            execute()
        } else {
            let (chinese, english) = progress::operation(operation);
            progress::run(
                &context.system_id,
                format!("{chinese}（{}:{operation}）", plugin.metadata.id),
                format!("{english} ({}:{operation})", plugin.metadata.id),
                execute,
            )
        }
    }
    fn run_script(
        &self,
        root: &Path,
        directory: &Path,
        entrypoint: &Path,
        context: &BuildContext,
    ) -> Result<()> {
        // User build scripts do not acquire plugin-owned mount declarations.
        fs::checked(root)?;
        self.backend
            .run_script(root, directory, entrypoint, context)
    }
}

impl<B: BuildBackend> ScopedBackend<'_, B> {
    /// Preserve plugin query data while reporting shared-cache preparation and publication.
    /// 保留插件查询数据，同时报告共享缓存的准备及发布。
    fn invoke_transaction(
        &self,
        plugin: &LoadedPlugin,
        operation: &str,
        args: &[String],
        root: &Path,
        context: &BuildContext,
        report: bool,
    ) -> Result<String> {
        let mappings = plugin.persistent_directories()?;
        if mappings.is_empty() {
            return self.backend.invoke(plugin, operation, args, root, context);
        }
        if report {
            progress::message(
                &context.system_id,
                "准备共享缓存的可写工作副本",
                "Prepare writable working copies of shared caches",
            );
        }
        let mut transaction =
            PersistentTransaction::begin(self.backend, &self.pool, &self.state, &mappings)?;
        let mut context = context.clone();
        context.persistent_mounts = transaction.mounts();
        let mut save_failure = None;
        let mut save = || {
            if let Some(error) = &save_failure {
                return Err(Error::Invalid(format!("此前保存失败: {error}")));
            }
            let result = progress::run(
                &context.system_id,
                "保存插件持久卷进度",
                "Save plugin persistent volume progress",
                || transaction.save(self.backend),
            );
            if let Err(error) = &result {
                save_failure = Some(error.to_string());
            }
            result
        };
        let result = self
            .backend
            .invoke_with_save(plugin, operation, args, root, &context, &mut save);
        let result = match save_failure {
            Some(error) => Err(Error::Invalid(format!("插件持久卷保存失败: {error}"))),
            None => result,
        };
        match result {
            Ok(output) => {
                if report {
                    progress::message(
                        &context.system_id,
                        "提交共享缓存事务",
                        "Commit shared cache transaction",
                    );
                }
                transaction.commit(self.backend)?;
                Ok(output)
            }
            Err(error) => match progress::run(
                &context.system_id,
                "回滚失败任务的共享缓存修改",
                "Roll back shared cache changes from the failed operation",
                || transaction.rollback(self.backend),
            ) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(Error::Invalid(format!(
                    "{error}; 持久副本回滚失败: {cleanup}"
                ))),
            },
        }
    }
}
