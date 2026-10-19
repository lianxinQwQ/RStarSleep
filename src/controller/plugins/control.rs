//! Invocation-local save requests, serviced on the transaction's owning thread.
use super::{Error, Result, io_error};
use serde::Deserialize;
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

const GUEST: &str = "/run/rstarsleep/control";
const LIMIT: usize = 4096;
const VARIABLES: &[&str] = &["RSTAR_CONTROL_SOCKET", "RSTAR_SDK_DIR", "RSTAR_SAVE"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    id: u64,
    operation: String,
}

struct Client {
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    written: usize,
    started: Instant,
}

pub(super) struct Server {
    directory: tempfile::TempDir,
    listener: UnixListener,
    clients: Vec<Client>,
    failure: Option<String>,
}

impl Server {
    pub(super) fn new() -> Result<Self> {
        // Keep AF_UNIX addresses short even with long TMPDIR or manager paths.
        let directory = tempfile::Builder::new()
            .prefix("rstar-ctl-")
            .tempdir_in("/tmp")
            .map_err(|e| io_error(Path::new("/tmp"), e))?;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))
            .map_err(|e| io_error(directory.path(), e))?;
        for (name, content, mode) in [
            ("rstar.py", include_str!("sdk/rstar.py"), 0o644),
            ("rstar-save", include_str!("sdk/rstar-save"), 0o755),
        ] {
            let path = directory.path().join(name);
            fs::write(&path, content).map_err(|e| io_error(&path, e))?;
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .map_err(|e| io_error(&path, e))?;
        }
        let socket = directory.path().join("control.sock");
        let listener = UnixListener::bind(&socket).map_err(|e| io_error(&socket, e))?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))
            .map_err(|e| io_error(&socket, e))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| io_error(&socket, e))?;
        Ok(Self {
            directory,
            listener,
            clients: Vec::new(),
            failure: None,
        })
    }

    pub(super) fn clear_environment(command: &mut Command) {
        for name in VARIABLES {
            command.env_remove(name);
        }
    }

    /// Call before the nspawn command delimiter; the tool probe gets no channel.
    pub(super) fn configure(&self, command: &mut Command, container: bool) -> Result<()> {
        let visible = if container {
            Path::new(GUEST)
        } else {
            self.directory.path()
        };
        if container {
            command.arg(
                crate::controller::container::bind("--bind-ro=", self.directory.path(), GUEST)
                    .map_err(|e| Error::Invalid(e.to_string()))?,
            );
        }
        for (name, value) in [
            ("RSTAR_CONTROL_SOCKET", visible.join("control.sock")),
            ("RSTAR_SDK_DIR", visible.to_owned()),
            ("RSTAR_SAVE", visible.join("rstar-save")),
        ] {
            if container {
                command.arg(format!("--setenv={name}={}", value.display()));
            } else {
                command.env(name, value);
            }
        }
        Ok(())
    }

    pub(super) fn check(&self) -> Result<()> {
        match &self.failure {
            Some(error) => Err(Error::Invalid(format!("插件持久卷保存失败: {error}"))),
            None => Ok(()),
        }
    }

    /// Nonblocking clients cannot hold up a save, output draining, or child exit.
    pub(super) fn poll(
        &mut self,
        save: &mut dyn FnMut() -> crate::controller::Result<()>,
    ) -> io::Result<()> {
        for _ in self.clients.len()..16 {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(true)?;
                    self.clients.push(Client {
                        stream,
                        input: Vec::new(),
                        output: Vec::new(),
                        written: 0,
                        started: Instant::now(),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        let failure = &mut self.failure;
        self.clients.retain_mut(|client| {
            if client.started.elapsed() > Duration::from_secs(5) { return false; }
            if client.output.is_empty() {
                let mut buffer = [0; 1024];
                loop {
                    match client.stream.read(&mut buffer) {
                        Ok(0) => return false,
                        Ok(count) => client.input.extend_from_slice(&buffer[..count]),
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => return false,
                    }
                    if client.input.len() > LIMIT || client.input.contains(&b'\n') { break; }
                }
                if client.input.len() > LIMIT { return false; }
                if let Some(end) = client.input.iter().position(|b| *b == b'\n') {
                    let request = serde_json::from_slice::<Request>(&client.input[..end]);
                    let (id, result) = match request {
                        Ok(request) if request.version == 1 && request.operation == "save" => {
                            let result = if let Some(error) = failure.as_ref() {
                                Err(error.clone())
                            } else {
                                save().map_err(|error| {
                                    let error = error.to_string();
                                    *failure = Some(error.clone());
                                    error
                                })
                            };
                            (Some(request.id), result)
                        }
                        Ok(request) => (Some(request.id), Err("Unsupported control request".into())),
                        Err(_) => (None, Err("Invalid control request".into())),
                    };
                    let response = match result {
                        Ok(()) => serde_json::json!({"id":id,"ok":true}),
                        Err(error) => serde_json::json!({"id":id,"ok":false,"error":error.chars().take(1024).collect::<String>()}),
                    };
                    client.output = serde_json::to_vec(&response).expect("JSON response");
                    client.output.push(b'\n');
                    client.started = Instant::now();
                }
            }
            while client.written < client.output.len() {
                match client.stream.write(&client.output[client.written..]) {
                    Ok(0) => return false,
                    Ok(count) => client.written += count,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => return true,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => return false,
                }
            }
            client.output.is_empty()
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, io::BufRead, rc::Rc, thread};

    fn exchange(
        server: &mut Server,
        request: &[u8],
        save: &mut dyn FnMut() -> crate::controller::Result<()>,
    ) -> serde_json::Value {
        let mut client = UnixStream::connect(server.directory.path().join("control.sock")).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        client.write_all(request).unwrap();
        server.poll(save).unwrap();
        let mut response = String::new();
        io::BufReader::new(client).read_line(&mut response).unwrap();
        serde_json::from_str(&response).unwrap()
    }

    #[test]
    fn save_is_acknowledged_after_callback_on_owning_thread() {
        let mut server = Server::new().unwrap();
        let called = Rc::new(Cell::new(0));
        let owner = thread::current().id();
        let mut save = || {
            assert_eq!(owner, thread::current().id());
            called.set(called.get() + 1);
            Ok(())
        };
        let response = exchange(
            &mut server,
            b"{\"version\":1,\"id\":7,\"operation\":\"save\"}\n",
            &mut save,
        );
        assert_eq!(called.get(), 1);
        assert_eq!(response, serde_json::json!({"id":7,"ok":true}));
        assert!(server.check().is_ok());
    }

    #[test]
    fn invalid_and_partial_clients_do_not_block_valid_save() {
        let mut server = Server::new().unwrap();
        let mut stalled =
            UnixStream::connect(server.directory.path().join("control.sock")).unwrap();
        stalled.write_all(b"{\"version\":").unwrap();
        let called = Cell::new(0);
        let mut save = || {
            called.set(called.get() + 1);
            Ok(())
        };
        for request in [
            b"{}\n".as_slice(),
            b"{\"version\":1,\"id\":1,\"operation\":\"save\",\"path\":\"/tmp\"}\n",
            b"{\"version\":2,\"id\":1,\"operation\":\"save\"}\n",
            b"{\"version\":1,\"id\":1,\"operation\":\"delete\"}\n",
        ] {
            assert_eq!(exchange(&mut server, request, &mut save)["ok"], false);
        }
        let mut oversized =
            UnixStream::connect(server.directory.path().join("control.sock")).unwrap();
        oversized.write_all(&vec![b'x'; LIMIT + 1]).unwrap();
        let response = exchange(
            &mut server,
            b"{\"version\":1,\"id\":1,\"operation\":\"save\"}\n",
            &mut save,
        );
        assert_eq!(response["ok"], true);
        assert_eq!(called.get(), 1);
    }

    #[test]
    fn first_save_failure_poisoning_cannot_be_hidden_by_later_requests() {
        let mut server = Server::new().unwrap();
        let called = Cell::new(0);
        let mut save = || {
            called.set(called.get() + 1);
            Err(crate::controller::Error::Invalid("disk full".into()))
        };
        for _ in 0..2 {
            let response = exchange(
                &mut server,
                b"{\"version\":1,\"id\":1,\"operation\":\"save\"}\n",
                &mut save,
            );
            assert_eq!(response["ok"], false);
            assert!(response["error"].as_str().unwrap().contains("disk full"));
        }
        assert_eq!(called.get(), 1);
        assert!(server.check().is_err());
    }

    #[test]
    fn container_mount_and_environment_are_invocation_local() {
        let server = Server::new().unwrap();
        let mut command = Command::new("systemd-nspawn");
        server.configure(&mut command, true).unwrap();
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(
            args.iter()
                .any(|a| a.starts_with("--bind-ro=") && a.ends_with(GUEST))
        );
        assert!(
            args.contains(&"--setenv=RSTAR_CONTROL_SOCKET=/run/rstarsleep/control/control.sock")
        );
        assert!(args.contains(&"--setenv=RSTAR_SAVE=/run/rstarsleep/control/rstar-save"));
        let address = server.directory.path().join("control.sock");
        assert_eq!(
            fs::metadata(&address).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(server);
        assert!(!address.exists());
    }
}
