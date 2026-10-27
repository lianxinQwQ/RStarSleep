//! Root-owned Unix manager and serialized machine lifecycles.
//! root 所属 Unix 管理器及串行机器生命周期。
use super::{
    Error, PreparedRuntime, Result, SubsystemConfig, atomic_write, client, commands, console,
    desktop,
    execution::{self, Wire},
    exports, filesystem,
    identity::{self, Peer},
    preparation, prepare,
    protocol::{self, ClientMessage, Event, Operation, StartOrigin, Status},
    prune_with_backend, read_record, session,
    supervision::{Phase, Supervisor, systemd::Systemd},
    trust, write_record,
};
use crate::config::ConfigStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, BufReader},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct State {
    runtime: Option<PreparedRuntime>,
    invocation: Option<String>,
    origin: Option<StartOrigin>,
    leases: usize,
    generation: u64,
    released: Instant,
    indexed_version: Option<String>,
    reconcile_retry_at: Instant,
    reconcile_failures: u32,
    reconcile_error: Option<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            runtime: None,
            invocation: None,
            origin: None,
            leases: 0,
            generation: 0,
            released: Instant::now(),
            indexed_version: None,
            reconcile_retry_at: Instant::now(),
            reconcile_failures: 0,
            reconcile_error: None,
        }
    }
}
struct Slot {
    state: Mutex<State>,
    monitoring: AtomicBool,
}
struct Manager {
    store: ConfigStore,
    directory: PathBuf,
    slots: Mutex<BTreeMap<String, Arc<Slot>>>,
    connections: Mutex<BTreeMap<u32, usize>>,
    builds: Arc<BuildState>,
    supervisor: Arc<dyn Supervisor>,
}
struct Lease {
    slot: Arc<Slot>,
    generation: u64,
    _activity: Option<fs::File>,
}
struct MonitorTask(Arc<Slot>);
impl Drop for MonitorTask {
    fn drop(&mut self) {
        self.0.monitoring.store(false, Ordering::Release);
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.slot.state.lock()
            && state.generation == self.generation
        {
            state.leases = state.leases.saturating_sub(1);
            if state.leases == 0 {
                state.released = Instant::now();
            }
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Origin {
    schema_version: u32,
    system_id: String,
    machine: String,
    version: String,
    origin: StartOrigin,
    #[serde(default)]
    invocation: Option<String>,
}

/// Lock shared state without silently recovering poisoned state.
/// 获取共享锁，不静默恢复中毒状态。
/// # Arguments
/// - `mutex`: State mutex. 状态锁。
fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| Error::Invalid("运行环境锁损坏 / runtime lock poisoned".into()))
}
/// Determine idle eligibility independently of machine probes.
/// 独立于机器探测判断空闲资格。
/// # Arguments
/// - `state`: Lease/origin state. 租约及来源状态。
/// - `seconds`: Configured grace. 配置宽限时间。
fn idle(state: &State, seconds: u64) -> bool {
    seconds > 0
        && state.origin == Some(StartOrigin::Demand)
        && state.leases == 0
        && state.released.elapsed() >= Duration::from_secs(seconds)
}

/// Begin the idle grace only after recovered applications are confirmed gone.
/// 仅在确认恢复的应用已退出后开始空闲宽限时间。
/// # Arguments
/// - `state`, `seconds`: Demand lifecycle state and configured grace. 按需生命周期状态及配置宽限时间。
/// - `occupied`: Application probe; failures conservatively retain the machine. 应用探测；失败时保守保留机器。
fn idle_after_probe(state: &mut State, seconds: u64, occupied: Result<bool>) -> bool {
    if !matches!(occupied, Ok(false)) {
        state.released = Instant::now();
        return false;
    }
    idle(state, seconds)
}

/// Commit an export version only after synchronization succeeds, retrying failures.
/// 只在同步成功后提交导出版本，失败保留重试机会。
/// # Arguments
/// - `state`, `version`: Lifecycle state and desired export version. 生命周期状态及期望导出版本。
/// - `force`: Refresh even after restarting the same version. 同版本重启后也刷新。
/// - `sync`: Reconcile registered users' exports. 协调已登记用户的导出。
fn refresh_exports(
    state: &mut State,
    version: &str,
    force: bool,
    sync: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if !force && state.indexed_version.as_deref() == Some(version) {
        return Ok(());
    }
    state.indexed_version = None;
    sync()?;
    state.indexed_version = Some(version.to_owned());
    Ok(())
}

pub(crate) mod builds;
mod installation;
mod lifecycle;
mod monitor;
mod requests;
mod server;
#[cfg(test)]
mod tests;
pub use builds::BuildState;
pub use installation::install;
pub use server::run;
