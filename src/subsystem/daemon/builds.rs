//! Track builds submitted through this daemon's SSH service.
//! 记录通过此 daemon 的 SSH 服务提交的构建。
use super::{Error, Result};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Volatile SSH build state for one management root, shared by all connections.
/// 单个管理根的易失 SSH 构建状态，由全部连接共享。
#[derive(Default)]
pub struct BuildState {
    tasks: Mutex<BTreeMap<String, BuildTask>>,
}

/// Distinguish preflight from a task whose durable record has been published.
/// 区分预检阶段和已发布持久记录的任务。
#[derive(Clone)]
pub(crate) struct BuildTask {
    pub task: String,
    pub recorded: bool,
}

/// Retain a build registration independently of its SSH channel.
/// 独立于 SSH 通道保留构建登记。
pub(crate) struct BuildRegistration {
    state: Arc<BuildState>,
    id: String,
    task: String,
}

impl BuildState {
    /// Register a task before launching it; an existing SSH build stays observable.
    /// 启动前登记任务；已有 SSH 构建继续保持可观察状态。
    pub(crate) fn begin(self: &Arc<Self>, id: &str, task: &str) -> Result<BuildRegistration> {
        super::protocol::validate_id(id)?;
        let mut tasks = super::lock(&self.tasks)?;
        if tasks.contains_key(id) {
            return Err(Error::Invalid("当前系统正在构建；使用 build status 查看或 build stop 停止 / this system is already building; use build status to view or build stop to stop".into()));
        }
        tasks.insert(
            id.into(),
            BuildTask {
                task: task.into(),
                recorded: false,
            },
        );
        Ok(BuildRegistration {
            state: self.clone(),
            id: id.into(),
            task: task.into(),
        })
    }

    /// Read only this daemon's current task, without importing historical records.
    /// 仅读取此 daemon 的当前任务，不导入历史记录。
    pub(crate) fn active(&self, id: &str) -> Result<Option<BuildTask>> {
        Ok(super::lock(&self.tasks)?.get(id).cloned())
    }
}

impl BuildRegistration {
    /// Mark the durable task record as published before starting its worker.
    /// 在启动工作进程前标记持久任务记录已发布。
    pub(crate) fn recorded(&self) -> Result<()> {
        let mut tasks = super::lock(&self.state.tasks)?;
        let task = tasks
            .get_mut(&self.id)
            .filter(|task| task.task == self.task)
            .ok_or_else(|| Error::Invalid("构建登记丢失 / build registration was lost".into()))?;
        task.recorded = true;
        Ok(())
    }
}

impl Drop for BuildRegistration {
    fn drop(&mut self) {
        if let Ok(mut tasks) = self.state.tasks.lock()
            && tasks
                .get(&self.id)
                .is_some_and(|task| task.task == self.task)
        {
            tasks.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concurrent SSH submissions share one task per system and release it on exit.
    /// 并发 SSH 提交为每个系统共享一个任务，并在退出时清除。
    #[test]
    fn registration_is_atomic_and_scoped_to_the_daemon() {
        let state = Arc::new(BuildState::default());
        let barrier = Arc::new(std::sync::Barrier::new(9));
        let threads = (0..8)
            .map(|index| {
                let state = state.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    state.begin("apps", &format!("build-{index}"))
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let registrations = threads
            .into_iter()
            .filter_map(|thread| thread.join().unwrap().ok())
            .collect::<Vec<_>>();
        assert_eq!(registrations.len(), 1);
        assert!(state.active("apps").unwrap().is_some());
        let other = state.begin("other", "build-other").unwrap();
        assert!(BuildState::default().active("apps").unwrap().is_none());
        drop(registrations);
        assert!(state.active("apps").unwrap().is_none());
        drop(state.begin("apps", "build-next").unwrap());
        assert!(state.active("apps").unwrap().is_none());
        assert_eq!(
            state
                .active("other")
                .unwrap()
                .map(|task| task.task)
                .as_deref(),
            Some("build-other")
        );
        drop(other);
    }

    /// Failed submission and panic unwinding leave the system ready for another build.
    /// 提交失败及 panic 栈展开后，系统可再次提交构建。
    #[test]
    fn failed_submission_releases_the_registration() {
        let state = Arc::new(BuildState::default());
        let result = std::panic::catch_unwind({
            let state = state.clone();
            move || {
                let _registration = state.begin("apps", "build-failed").unwrap();
                panic!("模拟提交失败 / simulated submission failure");
            }
        });
        assert!(result.is_err());
        assert!(state.active("apps").unwrap().is_none());
        assert!(state.begin("../apps", "invalid").is_err());
        assert!(state.begin("apps", "build-retry").is_ok());
    }
}
