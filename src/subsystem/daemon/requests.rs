//! Authorization and operation dispatch before privileged work.
//! 特权操作前的授权与分派。
use super::*;

impl Manager {
    /// Handle a bounded, authenticated request.
    /// 处理有界已认证请求。
    /// # Arguments
    /// - `reader`, `wire`, `peer`: Transport and kernel identity. 传输及内核身份。
    pub(super) fn serve_request(
        &self,
        reader: BufReader<UnixStream>,
        wire: Arc<Wire>,
        peer: Peer,
        first: ClientMessage,
    ) -> Result<()> {
        let request = match first {
            ClientMessage::Request { request } => request,
            _ => {
                return Err(Error::Protocol(
                    "需要初始请求 / initial request required".into(),
                ));
            }
        };
        protocol::validate_request(&request)?;
        reader.get_ref().set_read_timeout(None)?;
        match request.operation {
            Operation::List => {
                trust::store(&self.store, None)?;
                let mut statuses = Vec::new();
                for id in self.store.runtime_config()?.subsystems {
                    let config = self.config(&id)?;
                    if config.allows(peer.uid) {
                        statuses.push(self.status(&id)?);
                    }
                }
                wire.send(&Event::Result { statuses })?;
            }
            Operation::Status { id } => {
                if !self.config(&id)?.allows(peer.uid) {
                    return Err(Error::Invalid(
                        "子系统访问被拒绝 / subsystem access denied".into(),
                    ));
                }
                wire.send(&Event::Result {
                    statuses: vec![self.status(&id)?],
                })?;
            }
            op @ (Operation::Start { .. } | Operation::Stop { .. } | Operation::Restart { .. }) => {
                identity::admin(peer)?;
                let (id, restart, stop) = match op {
                    Operation::Start { id } => (id, false, false),
                    Operation::Stop { id } => (id, false, true),
                    Operation::Restart { id } => (id, true, false),
                    _ => unreachable!(),
                };
                self.config(&id)?;
                let slot = self.slot(&id)?;
                {
                    let mut state = lock(&slot.state)?;
                    if restart || stop {
                        self.stop(&id, &mut state)?;
                    }
                    if !stop {
                        self.start(&id, &mut state, StartOrigin::Explicit)?;
                    }
                }
                wire.send(&Event::Result {
                    statuses: vec![self.status(&id)?],
                })?;
            }
            Operation::Exec {
                id,
                argv,
                environment,
                pty,
                window,
            } => {
                let (runtime, _lease) = self.lease(&id, peer)?;
                execution::execute(
                    reader,
                    wire,
                    runtime,
                    peer,
                    execution::ExecutionRequest {
                        argv,
                        environment,
                        terminal: pty,
                        window,
                        detached: false,
                    },
                )?;
            }
            Operation::Open {
                id,
                target,
                args,
                environment,
                desktop,
                detach,
            } => {
                let (runtime, _lease) = self.lease(&id, peer)?;
                let target = if desktop && !target.ends_with(".desktop") {
                    return Err(Error::Invalid(
                        "run-desktop 需要 desktop 路径 / run-desktop requires a desktop path"
                            .into(),
                    ));
                } else {
                    target
                };
                let files = super::filesystem::open_runtime_root(&runtime)?;
                let desktop_target = if target.ends_with(".desktop") {
                    Some(if target.starts_with('/') {
                        target.clone()
                    } else {
                        format!("/usr/share/applications/{target}")
                    })
                } else if !target.contains('/')
                    && super::desktop::executable_at(&files, &target).is_err()
                {
                    Some(format!("/usr/share/applications/{target}.desktop"))
                } else {
                    None
                };
                let terminal = if let Some(path) = desktop_target {
                    let entry = super::desktop::read_at(&files, &path)?;
                    if super::desktop::file_field(&entry)?.is_some() {
                        super::session::validate_files(&runtime, &args)?;
                    }
                    entry.terminal
                } else {
                    false
                };
                let argv = super::desktop::resolve_at(&files, &target, &args)?;
                execution::execute(
                    reader,
                    wire,
                    runtime,
                    peer,
                    execution::ExecutionRequest {
                        argv,
                        environment,
                        terminal,
                        window: None,
                        detached: detach && !terminal,
                    },
                )?;
            }
            Operation::SyncExports { id } => {
                let config = self.config(&id)?;
                if !config.allows(peer.uid) {
                    return Err(Error::Invalid(
                        "子系统访问被拒绝 / subsystem access denied".into(),
                    ));
                }
                super::exports::sync(&self.store, &id, peer.uid)?;
                wire.send(&Event::Result {
                    statuses: vec![self.status(&id)?],
                })?;
            }
            Operation::Guest { .. } => {
                return Err(Error::Invalid(
                    "客体操作必须使用机器专属的客体套接字 / guest actions require the machine-specific guest socket".into(),
                ));
            }
        }
        Ok(())
    }
}
