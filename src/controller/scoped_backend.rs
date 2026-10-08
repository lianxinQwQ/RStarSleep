//! Apply global persistent CoW transactions to every plugin operation.
use super::{
    BuildBackend, BuildContext, Error, Result, fs, persistent::PersistentTransaction,
    plugins::LoadedPlugin,
};
use std::path::{Path, PathBuf};

pub(super) struct ScopedBackend<'a, B> {
    pub backend: &'a B,
    pub pool: PathBuf,
    pub state: PathBuf,
}

impl<B: BuildBackend> BuildBackend for ScopedBackend<'_, B> {
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
        let mappings = plugin.persistent_directories()?;
        if mappings.is_empty() {
            return self.backend.invoke(plugin, operation, args, root, context);
        }
        let mut transaction =
            PersistentTransaction::begin(self.backend, &self.pool, &self.state, &mappings)?;
        let mut context = context.clone();
        context.persistent_mounts = transaction.mounts();
        let result = self.backend.invoke(plugin, operation, args, root, &context);
        match result {
            Ok(output) => {
                transaction.commit(self.backend)?;
                Ok(output)
            }
            Err(error) => match transaction.rollback(self.backend) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(Error::Invalid(format!(
                    "{error}; 持久副本回滚失败: {cleanup}"
                ))),
            },
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
