//! Serialized preparation and registered machine lifecycle.
//! 串行准备与已注册机器生命周期。
use super::*;

impl Manager {
    /// Resolve one shared lock for each validated system ID.
    /// 为每个已校验系统标识解析唯一共享锁。
    /// # Arguments
    /// - `id`: Managed ID. 被管理标识。
    pub(super) fn slot(&self, id: &str) -> Result<Arc<Slot>> {
        protocol::validate_id(id)?;
        Ok(lock(&self.slots)?
            .entry(id.into())
            .or_insert_with(|| {
                Arc::new(Slot {
                    state: Mutex::new(State::default()),
                    monitoring: AtomicBool::new(false),
                })
            })
            .clone())
    }
    /// Reload trusted authorization for each request.
    /// 为每个请求重载可信授权。
    /// # Arguments
    /// - `id`: System ID. 系统标识。
    pub(super) fn config(&self, id: &str) -> Result<SubsystemConfig> {
        protocol::validate_id(id)?;
        trust::store(&self.store, Some(id))?;
        SubsystemConfig::read(&self.store, id)
    }
    /// Resolve durable lifecycle state.
    /// 解析持久生命周期状态。
    /// # Arguments
    /// - `id`: Validated system ID. 已校验系统标识。
    fn origin_path(&self, id: &str) -> PathBuf {
        self.directory.join(format!("{id}.origin.toml"))
    }
    /// Save readiness only after an actual in-guest execution succeeds.
    /// 只在容器实际执行成功后保存就绪状态。
    /// # Arguments
    /// - `id`, `state`: Machine identity/state. 机器身份及状态。
    fn save_origin(&self, id: &str, state: &State) -> Result<()> {
        let runtime = state
            .runtime
            .as_ref()
            .ok_or_else(|| Error::Invalid("缺少运行环境 / missing runtime".into()))?;
        write_record(
            &self.origin_path(id),
            &Origin {
                schema_version: 1,
                system_id: id.into(),
                machine: runtime.machine.clone(),
                version: runtime.version.clone(),
                invocation: state.invocation.clone(),
                origin: state
                    .origin
                    .clone()
                    .ok_or_else(|| Error::Invalid("缺少来源 / missing origin".into()))?,
            },
        )
    }
    /// Recover a supervised recorder before deciding whether a machine needs starting.
    /// 决定是否启动机器前，恢复受监督记录器。
    /// # Arguments
    /// - `id`, `state`: Locked subsystem and durable runtime. 已锁子系统及持久运行环境。
    pub(super) fn recover(&self, id: &str, state: &mut State) -> Result<()> {
        if state.runtime.is_none() {
            let service = self
                .supervisor
                .query(&console::unit_name(&self.store, id)?)?;
            match super::preparation::load_for_drain(&self.store, id) {
                Ok(runtime) => state.runtime = Some(runtime),
                Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                    if service.live() {
                        return Err(Error::Invalid("控制台服务仍运行但运行记录缺失 / console service is live but its runtime record is missing".into()));
                    }
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        let runtime = state.runtime.as_ref().unwrap();
        let recorder = console::Recorder::new(&self.store, id, runtime)?;
        let service = recorder.status(self.supervisor.as_ref())?;
        let alive = commands::alive(&runtime.machine)?;
        if alive {
            let _root = super::filesystem::open_runtime_root(runtime)?;
            if !service.live() {
                console::ensure_idle(&self.store, id)?;
                return Err(Error::Invalid(format!(
                    "机器仍运行但控制台服务已丢失 / machine is still running but console service was lost: {} ({}, {})",
                    recorder.unit, service.result, service.exit_status
                )));
            }
            let Some(run) = recorder.run(&self.store, id, &service)? else {
                return Err(Error::Invalid("运行中机器缺少匹配的控制台启动记录 / running machine has no matching console invocation record".into()));
            };
            if run.finished {
                return Err(Error::Invalid("机器仍运行但控制台任务已结束 / machine is still running but console task has finished".into()));
            }
            if state.invocation != run.invocation {
                state.origin = None;
                state.leases = 0;
                state.generation += 1;
                state.invocation = run.invocation.clone();
            }
            if state.origin.is_none()
                && run.shutdown_offset.is_none()
                && commands::guest(runtime, &["/bin/true".into()]).is_ok_and(|o| o.status.success())
            {
                let path = self.origin_path(id);
                let mut origin = run.origin.ok_or_else(|| {
                    Error::Invalid(
                        "恢复记录缺少启动来源 / recovery record missing start origin".into(),
                    )
                })?;
                if path.exists() {
                    let saved: Origin = read_record(&path)?;
                    if saved.schema_version != 1
                        || saved.system_id != id
                        || saved.machine != runtime.machine
                        || saved.version != runtime.version
                    {
                        return Err(Error::Invalid(
                            "恢复的机器身份不匹配 / recovered machine identity mismatch".into(),
                        ));
                    }
                    if saved.invocation == run.invocation {
                        origin = saved.origin;
                    }
                }
                state.origin = Some(origin);
                state.released = Instant::now();
                self.save_origin(id, state)?;
            }
        } else if !service.live() {
            let had_origin = state.origin.take().is_some();
            let had_invocation = state.invocation.take().is_some();
            if had_origin || had_invocation {
                state.leases = 0;
                state.generation += 1;
                session::cleanup(runtime)?;
            }
            let path = self.origin_path(id);
            if path.exists() {
                fs::remove_file(path)?;
            }
        } else if state.origin.take().is_some() {
            // A recorder may still be draining after the machine registration disappears.
            // 机器注册消失后，记录器可能仍在排空输出。
            state.leases = 0;
            state.generation += 1;
        }
        Ok(())
    }

    /// Wait for both machine shutdown and recorder completion without terminating either.
    /// 等待机器关机和记录器完成，不终止任意一方。
    fn wait_drained(&self, id: &str, runtime: &PreparedRuntime, timeout: Duration) -> Result<()> {
        let recorder = console::Recorder::new(&self.store, id, runtime)?;
        let deadline = Instant::now() + timeout;
        loop {
            let service = recorder.status(self.supervisor.as_ref())?;
            if recorder.drained(
                &self.store,
                id,
                &service,
                commands::alive(&runtime.machine)?,
            )? {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Invalid(format!(
                    "等待机器关机及控制台排空超时；实例保留 / timed out waiting for machine shutdown and console drain; instance retained: {}",
                    recorder.unit
                )));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    /// Start the current artifact or resume an already supervised startup.
    /// 启动当前产物，或恢复已有的受监督启动。
    /// # Arguments
    /// - `id`, `state`, `origin`: Locked target and requested start intent. 已锁目标及请求启动意图。
    pub(super) fn start(&self, id: &str, state: &mut State, origin: StartOrigin) -> Result<()> {
        self.config(id)?;
        let binary = console::preflight(&self.store, id)?;
        self.supervisor
            .query(&console::unit_name(&self.store, id)?)?;
        let _activity = super::filesystem::activity(&self.store, id, false)?;
        self.recover(id, state)?;
        let mut reuse = false;
        if let Some(runtime) = state.runtime.as_ref() {
            let recorder = console::Recorder::new(&self.store, id, runtime)?;
            let service = recorder.status(self.supervisor.as_ref())?;
            let draining = service.phase == Phase::Stopping
                || recorder.shutdown_requested(&self.store, id, &service)?
                || recorder
                    .run(&self.store, id, &service)?
                    .is_some_and(|run| run.finished || run.shutdown_offset.is_some());
            if service.live() && draining {
                self.stop(id, state)?;
                self.recover(id, state)?;
            } else {
                reuse = service.live();
            }
        }
        if state.origin.is_some() {
            if (origin == StartOrigin::Explicit && state.origin != Some(StartOrigin::Explicit))
                || (origin == StartOrigin::Autostart && state.origin == Some(StartOrigin::Demand))
            {
                state.origin = Some(origin);
                self.save_origin(id, state)?;
            }
            return Ok(());
        }
        let runtime = if reuse {
            state.runtime.as_ref().unwrap().clone()
        } else {
            console::ensure_idle(&self.store, id)?;
            let runtime = prepare(&self.store, id)?;
            // Retain the prepared identity even if service submission or readiness fails.
            // 即使服务提交或就绪失败，也保留已准备身份。
            state.runtime = Some(runtime.clone());
            let recorder = console::Recorder::new(&self.store, id, &runtime)?;
            recorder.launch(
                self.supervisor.as_ref(),
                &self.store,
                id,
                &runtime,
                origin.clone(),
                binary,
            )?;
            runtime
        };
        let recorder = console::Recorder::new(&self.store, id, &runtime)?;
        let deadline = Instant::now() + Duration::from_secs(45);
        let run = loop {
            let service = recorder.status(self.supervisor.as_ref())?;
            if !service.live() {
                let detail = console::metadata(&self.store, id)?
                    .and_then(|r| r.error)
                    .unwrap_or_default();
                return Err(Error::Invalid(format!(
                    "子系统启动服务已退出 / subsystem boot service exited: {} ({}, {}); {detail}",
                    recorder.unit, service.result, service.exit_status
                )));
            }
            if let Some(run) = recorder.run(&self.store, id, &service)? {
                if run.finished || run.shutdown_offset.is_some() || service.phase == Phase::Stopping
                {
                    return Err(Error::Invalid(
                        "启动期间控制台已进入关机 / console entered shutdown during startup".into(),
                    ));
                }
                if commands::alive(&runtime.machine)? {
                    let _root = super::filesystem::open_runtime_root(&runtime)?;
                    if commands::guest(&runtime, &["/bin/true".into()])
                        .is_ok_and(|o| o.status.success())
                    {
                        break run;
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::Invalid(format!(
                    "子系统启动超时；服务保留以供检查或正常关机 / subsystem boot timeout; service retained for inspection or graceful shutdown: {}",
                    recorder.unit
                )));
            }
            thread::sleep(Duration::from_millis(150));
        };
        state.runtime = Some(runtime);
        let recovered_origin = run.origin.ok_or_else(|| {
            Error::Invalid("启动记录缺少来源 / boot record missing start origin".into())
        })?;
        state.origin = Some(match (&origin, &recovered_origin) {
            (StartOrigin::Explicit, _) => StartOrigin::Explicit,
            (StartOrigin::Autostart, StartOrigin::Demand) => StartOrigin::Autostart,
            _ => recovered_origin,
        });
        state.invocation = run.invocation;
        state.leases = 0;
        state.generation += 1;
        state.released = Instant::now();
        self.save_origin(id, state)?;
        let version = state.runtime.as_ref().unwrap().version.clone();
        if let Err(e) = refresh_exports(state, &version, true, || {
            super::exports::sync_registered(&self.store, id)
        }) {
            eprintln!("刷新子系统导出 {id} / subsystem export refresh {id}: {e}");
        }
        if let Err(e) =
            super::prune_with_backend(&self.store, id, &crate::controller::SystemBackend)
        {
            eprintln!("清理子系统运行环境 {id} / subsystem runtime retirement {id}: {e}");
        }
        Ok(())
    }

    /// Power off gracefully, including recovered or still-starting services, then drain logs.
    /// 优雅关闭恢复或仍在启动的服务中的机器，然后排空日志。
    /// # Arguments
    /// - `id`, `state`: Locked target. 已锁目标。
    pub(super) fn stop(&self, id: &str, state: &mut State) -> Result<()> {
        let _activity = super::filesystem::activity(&self.store, id, false)?;
        self.recover(id, state)?;
        if let Some(runtime) = state.runtime.as_ref() {
            let recorder = console::Recorder::new(&self.store, id, runtime)?;
            let service = recorder.status(self.supervisor.as_ref())?;
            recorder.request_shutdown(&self.store, id, &service)?;
            let deadline = Instant::now() + Duration::from_secs(45);
            loop {
                let service = recorder.status(self.supervisor.as_ref())?;
                if commands::alive(&runtime.machine)? {
                    let _root = super::filesystem::open_runtime_root(runtime)?;
                    if recorder.run(&self.store, id, &service)?.is_none() {
                        return Err(Error::Invalid(
                            "关机缺少匹配的控制台记录 / shutdown missing matching console record"
                                .into(),
                        ));
                    }
                    let mut command = Command::new(commands::MACHINECTL);
                    command.args(["poweroff", &runtime.machine]);
                    console::mark_shutdown(&self.store, id)?;
                    let result = commands::output(command, Duration::from_secs(5))?;
                    if !result.status.success() {
                        return Err(Error::Invalid(format!(
                            "关机请求失败 / poweroff request failed: {}",
                            String::from_utf8_lossy(&result.stderr)
                        )));
                    }
                    break;
                }
                if !service.live() {
                    break;
                }
                if recorder
                    .run(&self.store, id, &service)?
                    .is_some_and(|run| run.finished || run.shutdown_offset.is_some())
                    || service.phase == Phase::Stopping
                {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(Error::Invalid(format!(
                        "服务尚未注册机器，无法完成正常关机 / service has not registered a machine; graceful shutdown could not complete: {}",
                        recorder.unit
                    )));
                }
                thread::sleep(Duration::from_millis(100));
            }
            self.wait_drained(id, runtime, Duration::from_secs(35))?;
            session::cleanup(runtime)?;
        }
        state.origin = None;
        state.invocation = None;
        state.leases = 0;
        state.generation += 1;
        let path = self.origin_path(id);
        if path.exists() {
            fs::remove_file(path)?;
        }
        Ok(())
    }
    /// Read status while serializing reconciliation.
    /// 串行协调期间读取状态。
    /// # Arguments
    /// - `id`: Target ID. 目标标识。
    pub(super) fn status(&self, id: &str) -> Result<Status> {
        let slot = self.slot(id)?;
        let mut state = lock(&slot.state)?;
        self.recover(id, &mut state)?;
        Ok(Status {
            id: id.into(),
            machine: state.runtime.as_ref().map(|r| r.machine.clone()),
            version: state.runtime.as_ref().map(|r| r.version.clone()),
            running: state.origin.is_some(),
            origin: state.origin.clone(),
            leases: state.leases,
        })
    }
    /// Authorize before preparation, then acquire a generation-scoped lease.
    /// 先于准备授权，再取得运行代所属租约。
    /// # Arguments
    /// - `id`, `peer`: Target and authenticated caller. 目标及认证调用者。
    pub(super) fn lease(&self, id: &str, peer: Peer) -> Result<(PreparedRuntime, Lease)> {
        let config = self.config(id)?;
        if !config.allows(peer.uid) {
            return Err(Error::Invalid(
                "子系统访问被拒绝 / subsystem access denied".into(),
            ));
        }
        let activity = super::filesystem::activity(&self.store, id, false)?;
        let slot = self.slot(id)?;
        let mut state = lock(&slot.state)?;
        self.start(id, &mut state, StartOrigin::Demand)?;
        let mut runtime = state.runtime.as_ref().unwrap().clone();
        runtime.config.allowed_uids = config.allowed_uids;
        state.leases += 1;
        let generation = state.generation;
        drop(state);
        Ok((
            runtime,
            Lease {
                slot,
                generation,
                _activity: Some(activity),
            },
        ))
    }
}
