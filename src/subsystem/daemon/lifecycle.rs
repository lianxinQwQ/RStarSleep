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
            .ok_or_else(|| Error::Invalid("missing runtime".into()))?;
        write_record(
            &self.origin_path(id),
            &Origin {
                schema_version: 1,
                system_id: id.into(),
                machine: runtime.machine.clone(),
                version: runtime.version.clone(),
                origin: state
                    .origin
                    .clone()
                    .ok_or_else(|| Error::Invalid("missing origin".into()))?,
            },
        )
    }
    /// Reconstruct registrations while retaining demand occupancy through guest units.
    /// 重建注册，并通过容器服务保留按需占用。
    /// # Arguments
    /// - `id`, `state`: Locked machine state. 已锁定机器状态。
    pub(super) fn recover(&self, id: &str, state: &mut State) -> Result<()> {
        if state.runtime.is_none() {
            match super::preparation::load_for_drain(&self.store, id) {
                Ok(runtime) => state.runtime = Some(runtime),
                Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        let runtime = state.runtime.as_ref().unwrap();
        let alive = commands::alive(&runtime.machine)?;
        if alive {
            let leader = commands::leader(runtime)?;
            let actual = fs::metadata(format!("/proc/{leader}/root"))?;
            let expected = fs::metadata(&runtime.root)?;
            if actual.dev() != expected.dev() || actual.ino() != expected.ino() {
                return Err(Error::Invalid(
                    "registered machine root does not match the owned runtime".into(),
                ));
            }
        }
        if alive && state.origin.is_none() {
            let path = self.origin_path(id);
            let origin = if path.exists() {
                let saved: Origin = read_record(&path)?;
                if saved.schema_version != 1
                    || saved.system_id != id
                    || saved.machine != runtime.machine
                    || saved.version != runtime.version
                {
                    return Err(Error::Invalid("recovered machine identity mismatch".into()));
                }
                saved.origin
            } else {
                StartOrigin::Explicit
            };
            if self
                .store
                .runtime_config()?
                .subsystems
                .iter()
                .any(|configured| configured == id)
            {
                super::guest::reconnect(&self.store, runtime)?;
            }
            state.origin = Some(origin);
            state.generation += 1;
            state.released = Instant::now();
        }
        if !alive && state.origin.is_some() {
            if let Some(mut child) = state.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            state.origin = None;
            state.leases = 0;
            state.generation += 1;
            let _ = fs::remove_file(self.origin_path(id));
            session::cleanup(runtime)?;
        }
        Ok(())
    }
    /// Start exactly the current artifact; reuse any already-running machine.
    /// 只启动当前产物，复用已运行机器。
    /// # Arguments
    /// - `id`, `state`, `origin`: Locked target and start intent. 已锁定目标及启动意图。
    pub(super) fn start(&self, id: &str, state: &mut State, origin: StartOrigin) -> Result<()> {
        let _activity = super::filesystem::activity(&self.store, id, false)?;
        self.recover(id, state)?;
        if state.origin.is_some() {
            if (origin == StartOrigin::Explicit && state.origin != Some(StartOrigin::Explicit))
                || (origin == StartOrigin::Autostart && state.origin == Some(StartOrigin::Demand))
            {
                state.origin = Some(origin);
                self.save_origin(id, state)?;
            }
            return Ok(());
        }
        self.config(id)?;
        let runtime = prepare(&self.store, id)?;
        let mut command = Command::new("/usr/bin/systemd-nspawn");
        command
            .args(["--quiet", "--boot", "--register=yes"])
            .arg(format!("--machine={}", runtime.machine))
            .arg("--directory")
            .arg(&runtime.root)
            .args(&runtime.boot_args);
        // Dedicated per-machine endpoint exposes no management tree to the guest.
        // 每机器专用端点不会向容器暴露管理树。
        let endpoint = super::guest::listen(&self.store, &runtime)?;
        command.arg(format!(
            "--bind={}:{}",
            endpoint.display(),
            "/run/rstarsleep/guest.sock"
        ));
        let binary = fs::canonicalize(std::env::current_exe()?)?;
        trust::path(&binary)?;
        command.arg(format!(
            "--bind-ro={}:{}",
            binary.display(),
            "/run/rstarsleep/RStarSleep"
        ));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        let mut child = command.spawn()?;
        let ready = (|| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(45);
            loop {
                if let Some(status) = child.try_wait()? {
                    return Err(Error::Invalid(format!("subsystem boot exited: {status}")));
                }
                if commands::alive(&runtime.machine)?
                    && commands::guest(&runtime, &["/bin/true".into()]).is_ok()
                {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(Error::Invalid("subsystem boot timeout".into()));
                }
                thread::sleep(Duration::from_millis(150));
            }
        })();
        if let Err(e) = ready {
            let mut command = Command::new(commands::MACHINECTL);
            command.args(["terminate", &runtime.machine]);
            let _ = commands::output(command, Duration::from_secs(5));
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        state.runtime = Some(runtime);
        state.child = Some(child);
        state.origin = Some(origin);
        state.leases = 0;
        state.generation += 1;
        state.released = Instant::now();
        if let Err(e) = self.save_origin(id, state) {
            self.stop(id, state)?;
            return Err(e);
        }
        let version = state.runtime.as_ref().unwrap().version.clone();
        if let Err(e) = refresh_exports(state, &version, true, || {
            super::exports::sync_registered(&self.store, id)
        }) {
            eprintln!("subsystem export refresh {id}: {e}");
        }
        if let Err(e) =
            super::prune_with_backend(&self.store, id, &crate::controller::SystemBackend)
        {
            eprintln!("subsystem runtime retirement {id}: {e}");
        }
        Ok(())
    }
    /// Power off gracefully then terminate after a bounded grace period.
    /// 先优雅关机，再在有界宽限时间后终止。
    /// # Arguments
    /// - `id`, `state`: Locked target. 已锁定目标。
    pub(super) fn stop(&self, id: &str, state: &mut State) -> Result<()> {
        let _activity = super::filesystem::activity(&self.store, id, false)?;
        self.recover(id, state)?;
        if state.origin.is_some() {
            let runtime = state.runtime.as_ref().unwrap();
            let mut command = Command::new(commands::MACHINECTL);
            command.args(["poweroff", &runtime.machine]);
            let _ = commands::output(command, Duration::from_secs(5));
            let deadline = Instant::now() + Duration::from_secs(30);
            while commands::alive(&runtime.machine)? && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(100));
            }
            if commands::alive(&runtime.machine)? {
                let mut command = Command::new(commands::MACHINECTL);
                command.args(["terminate", &runtime.machine]);
                commands::output(command, Duration::from_secs(5))?;
                let deadline = Instant::now() + Duration::from_secs(10);
                while commands::alive(&runtime.machine)? && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(100));
                }
                if commands::alive(&runtime.machine)? {
                    return Err(Error::Invalid(
                        "machine still registered after terminate".into(),
                    ));
                }
            }
            session::cleanup(runtime)?;
        }
        if let Some(mut child) = state.child.take() {
            if child.try_wait()?.is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        state.origin = None;
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
            return Err(Error::Invalid("subsystem access denied".into()));
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
