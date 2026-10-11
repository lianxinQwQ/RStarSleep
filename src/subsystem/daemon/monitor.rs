//! Concurrent reconciliation without cross-machine boot delays.
//! 并发协调，避免机器间启动延迟传播。
use super::*;

impl Manager {
    /// Reconcile one slot without blocking unrelated machine maintenance.
    /// 协调单个槽位，不阻塞其他机器维护。
    /// # Arguments
    /// - `id`, `state`: Validated system and locked state. 已校验系统及已锁状态。
    fn reconcile(&self, id: &str, state: &mut State) -> Result<()> {
        if !self
            .store
            .runtime_config()?
            .subsystems
            .iter()
            .any(|configured| configured == id)
        {
            self.stop(id, state)?;
            super::exports::withdraw_all(&self.store, id)?;
            return Ok(());
        }
        let config = self.config(id)?;
        if let Err(error) = super::exports::withdraw_disallowed(&self.store, id) {
            eprintln!("subsystem export authorization {id}: {error}");
        }
        if config.autostart {
            self.start(id, state, StartOrigin::Autostart)?;
        } else {
            self.recover(id, state)?;
        }
        let export_version = if state.origin.is_some() {
            state.runtime.as_ref().map(|r| r.version.clone())
        } else {
            crate::controller::read_current_build(&self.store, id)
                .ok()
                .map(|c| c.version)
        };
        if let Some(version) = export_version
            && let Err(error) = refresh_exports(state, &version, false, || {
                super::exports::sync_registered(&self.store, id)
            })
        {
            eprintln!("subsystem export refresh {id}: {error}");
        }
        if state.origin == Some(StartOrigin::Demand)
            && state.leases == 0
            && config.idle_timeout_seconds > 0
            && let Some(runtime) = state.runtime.as_ref()
        {
            let occupied = execution::occupied(runtime);
            if idle_after_probe(state, config.idle_timeout_seconds, occupied) {
                self.stop(id, state)?;
            }
        }
        Ok(())
    }
    /// Dispatch at most one background reconciliation worker per system.
    /// 每系统最多分派一个后台协调工作线程。
    /// # Arguments
    /// - `self`: Shared root manager. 共享 root 管理器。
    pub(super) fn monitor(self: Arc<Self>) {
        loop {
            let ids = match trust::store(&self.store, None).and_then(|()| {
                self.store
                    .runtime_config()
                    .map(|c| c.subsystems)
                    .map_err(Error::from)
            }) {
                Ok(ids) => ids,
                Err(e) => {
                    eprintln!("subsystem policy: {e}");
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
            };
            for id in ids {
                let _ = self.slot(&id);
            }
            if let Ok(systems) = self.store.list_systems() {
                for id in systems {
                    if self
                        .store
                        .system_layout(&id)
                        .is_ok_and(|l| l.root.join("runtime/current.toml").exists())
                    {
                        let _ = self.slot(&id);
                    }
                }
            }
            let slots = match lock(&self.slots) {
                Ok(slots) => slots
                    .iter()
                    .map(|(id, slot)| (id.clone(), slot.clone()))
                    .collect::<Vec<_>>(),
                Err(_) => return,
            };
            for (id, slot) in slots {
                if slot
                    .monitoring
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
                {
                    continue;
                }
                let manager = self.clone();
                let task = MonitorTask(slot.clone());
                if let Err(error) = thread::Builder::new()
                    .name(format!("rstarsleep-monitor-{id}"))
                    .spawn(move || {
                        let _task = task;
                        let Ok(mut state) = slot.state.try_lock() else {
                            return;
                        };
                        if let Err(e) = manager.reconcile(&id, &mut state) {
                            eprintln!("subsystem reconciliation {id}: {e}");
                        }
                    })
                {
                    eprintln!("subsystem monitor worker: {error}");
                }
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
}
