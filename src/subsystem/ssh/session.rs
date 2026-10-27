//! Management command dispatch and lossless nested root-shell terminal bridging.
//! 管理命令分派及无损嵌套 root shell 终端桥接。
use super::super::{
    Error, Result, admin, client, console,
    management::{
        Workspace,
        terminal::{Action, HELP, LiveView, Terminal, parse},
    },
    protocol::{self, ClientMessage, Event, Operation, WindowSize},
};
use crate::{config::ConfigStore, controller};
use russh::{
    Channel, ChannelId,
    server::{Handle, Msg},
};

struct Remote {
    handle: Handle,
    channel: ChannelId,
}
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

/// Capture ordinary dashboard output; root shells retain an explicitly labelled tail.
/// 收集普通面板输出；root shell 保留明确标注的尾部。
struct ViewOutput<'a, W> {
    wire: &'a mut W,
    capture: bool,
    tail: bool,
    bytes: Vec<u8>,
    omitted: bool,
}
impl<'a, W> ViewOutput<'a, W> {
    fn new(wire: &'a mut W, capture: bool, tail: bool) -> Self {
        Self {
            wire,
            capture,
            tail,
            bytes: Vec::new(),
            omitted: false,
        }
    }
    fn text(&self) -> String {
        let bytes = if self.omitted {
            let start = self
                .bytes
                .iter()
                .take(4)
                .position(|b| b & 0xc0 != 0x80)
                .unwrap_or(0);
            &self.bytes[start..]
        } else {
            &self.bytes
        };
        match std::str::from_utf8(bytes) {
            Ok(text) => format!("{}{}", if self.omitted { "仅保留最后 64 KiB 输出 / Only the last 64 KiB of output is retained\n" } else { "" }, text.replace("\r\n", "\n")),
            Err(_) => "输出不是 UTF-8 文本，请使用非交互 SSH 读取原始数据 / output is not UTF-8 text; use noninteractive SSH for raw data".into(),
        }
    }
}
impl<W: AsyncWrite + Unpin> AsyncWrite for ViewOutput<'_, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.capture {
            if this.bytes.len() + bytes.len() > 4 * 1024 * 1024 {
                return Poll::Ready(Err(std::io::Error::other(
                    "面板输出超出 4 MiB，请缩小日志范围 / dashboard output exceeds 4 MiB; narrow the log range",
                )));
            }
            this.bytes.extend_from_slice(bytes);
            return Poll::Ready(Ok(bytes.len()));
        }
        match Pin::new(&mut *this.wire).poll_write(cx, bytes) {
            Poll::Ready(Ok(n)) => {
                if this.tail {
                    this.bytes.extend_from_slice(&bytes[..n]);
                    if this.bytes.len() > 65536 {
                        this.bytes.drain(..this.bytes.len() - 65536);
                        this.omitted = true;
                    }
                }
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.capture {
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut *this.wire).poll_flush(cx)
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.capture {
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut *this.wire).poll_shutdown(cx)
        }
    }
}
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::watch,
};
#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;

/// Coalesce resize bursts to the latest size without dropping the final window event.
/// 将连续尺寸变化合并为最新尺寸，不丢弃最终窗口事件。
async fn window(sizes: &mut watch::Receiver<(u32, u32)>) -> (u32, u32) {
    if sizes.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
    *sizes.borrow_and_update()
}

/// Serve one interactive shell or noninteractive management command channel.
/// 服务一个交互式 shell 或非交互式管理命令通道。
/// # Arguments
/// - `channel`, `store`, `id`, `pty`, `sizes`, `command`: SSH channel, fixed identity, terminal metadata, resize events and optional exec. SSH 通道、固定身份、终端元数据、尺寸事件及可选执行命令。
pub(super) async fn run(
    channel: Channel<Msg>,
    store: ConfigStore,
    id: String,
    pty: Option<(String, u32, u32)>,
    mut sizes: watch::Receiver<(u32, u32)>,
    command: Option<Vec<u8>>,
    handle: Handle,
) -> Result<u32> {
    let workspace = Workspace::new(store, id.as_str())?;
    let remote = Remote {
        handle,
        channel: channel.id(),
    };
    let stream = channel.into_stream();
    let (mut input, mut output) = tokio::io::split(stream);
    let mut dimensions = pty.clone().unwrap_or(("dumb".into(), 80, 24));
    if let Some(command) = command {
        let command = std::str::from_utf8(&command)
            .map_err(|_| Error::Invalid("命令必须为 UTF-8 / command must be UTF-8".into()))?;
        return execute(
            &workspace,
            parse(command)?,
            &mut input,
            &mut output,
            &mut sizes,
            &mut dimensions,
            pty.is_some(),
            &mut Vec::new(),
            &remote,
        )
        .await;
    }
    if pty.is_none() {
        return Err(Error::Invalid("交互管理终端需要 PTY，请使用 ssh -t / interactive management terminal requires a PTY; use ssh -t".into()));
    }
    let mut terminal = Terminal::new(&id);
    terminal.resize(dimensions.1, dimensions.2);
    output.write_all(&terminal.welcome()).await?;
    let mut bytes = [0; 8192];
    let mut next = Vec::new();
    let mut refresh = tokio::time::interval(Duration::from_millis(100));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let update = if !next.is_empty() {
            let data = std::mem::take(&mut next);
            terminal.input(&data, &workspace)
        } else {
            tokio::select! {
                n=input.read(&mut bytes)=>{let n=n?;if n==0{break;}terminal.input(&bytes[..n],&workspace)},
                (cols,rows)=window(&mut sizes)=>{dimensions.1=cols;dimensions.2=rows;output.write_all(&terminal.resize(cols,rows)).await?;continue;},
                _=refresh.tick(), if terminal.live_view().is_some()=>{
                    let update = refresh_dashboard(&workspace, &mut terminal)
                        .unwrap_or_else(|error| terminal.error(&format!("实时视图失败 / live view failed: {error}")));
                    output.write_all(&update).await?;
                    continue;
                },
                _=tokio::time::sleep(Duration::from_millis(40))=>terminal.flush_escape(&workspace)
            }
        };
        let update = match update {
            Ok(update) => update,
            Err(error) => {
                terminal.take_pending();
                output
                    .write_all(&terminal.error(&format!("操作失败 / operation failed: {error}")))
                    .await?;
                continue;
            }
        };
        output.write_all(&update.output).await?;
        output.flush().await?;
        if let Some(action) = update.action {
            let mut pending = terminal.take_pending();
            let words = match action {
                Action::Exit => break,
                Action::Shell => vec!["shell".into()],
                Action::Command(words) => words,
            };
            let tui = terminal.is_tui();
            if tui && matches!(words.first().map(String::as_str), Some("build" | "console")) {
                let result = dashboard_command(&workspace, &words).await;
                match result {
                    Ok(()) => {
                        let update =
                            refresh_dashboard(&workspace, &mut terminal).unwrap_or_else(|error| {
                                terminal.error(&format!("实时视图失败 / live view failed: {error}"))
                            });
                        output.write_all(&update).await?;
                    }
                    Err(error) => {
                        output
                            .write_all(
                                &terminal.error(&format!("操作失败 / operation failed: {error}")),
                            )
                            .await?
                    }
                }
                next = pending;
                continue;
            }
            let live = words
                .first()
                .is_some_and(|s| s == "shell" || s == "console")
                || words.iter().any(|s| s == "--follow");
            if tui && live {
                output.write_all(b"\x1b[?1049l\x1b[?25h\r\n").await?;
            }
            let mut capture = ViewOutput::new(&mut output, tui && !live, tui && live);
            let result = execute(
                &workspace,
                words,
                &mut input,
                &mut capture,
                &mut sizes,
                &mut dimensions,
                true,
                &mut pending,
                &remote,
            )
            .await;
            let mut text = capture.text();
            match result {
                Ok(0) => {}
                Ok(code) => text.push_str(&format!("\n退出码 {code} / exit code {code}")),
                Err(error) => text.push_str(&format!("\n操作失败 / operation failed: {error}")),
            }
            terminal.set_size(dimensions.1, dimensions.2);
            if tui {
                output.write_all(b"\x1b[?1049h").await?;
            } else if live {
                // Resetting margins homes the cursor; preserve its position across that reset.
                // 重置滚动边界会把光标移到原点；重置前后必须保留位置。
                output
                    .write_all(b"\x1b7\x1b[0m\x1b[r\x1b8\x1b[?25h\r\n")
                    .await?;
            } else {
                output.write_all(b"\x1b[0m\x1b[?25h\r\n").await?;
            }
            output.write_all(&terminal.finish(&text)).await?;
            next = pending;
        }
    }
    output.write_all(b"\x1b[?2004l\x1b[?25h\x1b[0m").await?;
    Ok(0)
}

/// Keep dashboard live pages inside the dashboard rather than handing off its terminal.
/// 将面板实时页保留在面板内，不交出终端控制权。
async fn dashboard_command(workspace: &Workspace, words: &[String]) -> Result<()> {
    let args: Vec<_> = words.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["build"] => {
            let store = workspace.store.clone();
            let id = workspace.id.clone();
            tokio::task::spawn_blocking(move || admin::submit(&store, &id, "build"))
                .await
                .map_err(|e| {
                    Error::Invalid(format!("任务提交失败 / job submission failed: {e}"))
                })??;
        }
        ["build", "stop"] => {
            admin::stop_build(&workspace.store, &workspace.id)?;
        }
        ["build", "status"] | ["console"] => {}
        _ => {
            return Err(Error::Invalid(
                "实时页命令无效 / invalid live page command".into(),
            ));
        }
    }
    Ok(())
}

/// A stop command confirms the durable result instead of reporting a queued request as success.
/// 停止命令确认持久结果，不将排队请求误报为停止成功。
async fn wait_build_stop(workspace: &Workspace, task: &str) -> Result<String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let job = admin::jobs(&workspace.store, &workspace.id)?
            .into_iter()
            .find(|j| j.task == task)
            .ok_or_else(|| Error::Invalid("构建任务丢失 / build job was lost".into()))?;
        if let Some(error) = job
            .error
            .as_ref()
            .filter(|_| matches!(job.state.as_str(), "queued" | "running"))
        {
            return Err(Error::Invalid(error.clone()));
        }
        match job.state.as_str() {
            "cancelled" => {
                return Ok(
                    "构建已停止；当前系统不在构建 / build stopped; this system is not building"
                        .into(),
                );
            }
            "complete" => return Ok("构建已结束 / build already finished".into()),
            "queued" | "running" => {}
            _ => {
                return Err(Error::Invalid(format!(
                    "构建停止未成功确认 / build stop was not confirmed: {}",
                    job.error.unwrap_or(job.state)
                )));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Invalid("等待停止构建超时；请求仍保留 / timed out waiting for build stop; the request is retained".into()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Refresh only the selected page, including historical log data from byte zero.
/// 只刷新选中页面，构建历史从第零字节开始读取。
fn refresh_dashboard(workspace: &Workspace, terminal: &mut Terminal) -> Result<Vec<u8>> {
    match terminal.live_view() {
        Some(LiveView::Build) => {
            let jobs = admin::jobs(&workspace.store, &workspace.id)?;
            let active = jobs.iter().rev().find(|j| {
                j.operation == "build" && matches!(j.state.as_str(), "queued" | "running")
            });
            let job = active.or_else(|| jobs.iter().rev().find(|j| j.operation == "build"));
            let directory = admin::directory(&workspace.store, &workspace.id)?;
            let status = if let Some(job) = active {
                if directory
                    .join(format!("{}.cancel", job.task))
                    .try_exists()?
                {
                    format!("正在停止构建 / Stopping build: {}", job.task)
                } else {
                    format!("正在构建 / Building: {}", job.task)
                }
            } else if let Some(job) = job {
                format!(
                    "当前系统不在构建 / This system is not building · {} · exit {}",
                    job.state,
                    job.exit_code.unwrap_or(1)
                )
            } else {
                "当前系统不在构建 / This system is not building".into()
            };
            let status = if let Some(error) = job.and_then(|j| j.error.as_ref()) {
                format!("{status} · {error}")
            } else {
                status
            };
            terminal.update_live(
                job.map(|j| (directory.join(format!("{}.log", j.task)), 0)),
                &status,
            )
        }
        Some(LiveView::Console) => {
            let directory = console::directory(&workspace.store, &workspace.id)?;
            if !directory.join("current.toml").try_exists()? {
                return terminal.update_live(None, "暂无控制台 / No console yet");
            }
            let run = console::current(&workspace.store, &workspace.id)?;
            let status = if run.finished {
                "控制台已结束（只读） / Console ended (read-only)"
            } else {
                "实时输出（只读） / Live output (read-only)"
            };
            terminal.update_live(
                Some((directory.join("console.log"), run.offset)),
                &format!("{status}: {}", run.task),
            )
        }
        None => Ok(Vec::new()),
    }
}

/// Execute only fixed management operations for the authenticated subsystem.
/// 仅对已认证子系统执行固定管理操作。
/// # Arguments
/// - `workspace`, `words`, `input`, `output`, `sizes`, `dimensions`, `pty`, `pending`: Fixed workspace, literal command and owned terminal streams/events. 固定工作区、字面命令及所属终端流和事件。
async fn execute<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    workspace: &Workspace,
    words: Vec<String>,
    input: &mut R,
    output: &mut W,
    sizes: &mut watch::Receiver<(u32, u32)>,
    dimensions: &mut (String, u32, u32),
    pty: bool,
    pending: &mut Vec<u8>,
    remote: &Remote,
) -> Result<u32> {
    super::super::SubsystemConfig::read(&workspace.store, &workspace.id)?;
    let args = words.iter().map(String::as_str).collect::<Vec<_>>();
    let mut text = String::new();
    match args.as_slice(){
        []=>{},["help"]=>text=HELP.into(),["pwd"]=>text="/".into(),["ls"]=>text=workspace.list("/")?.join("\n"),["ls",path]=>text=workspace.list(path)?.join("\n"),
        ["clear"]=>{if !pty{return Err(Error::Invalid("clear 需要 PTY / clear requires a PTY".into()));}output.write_all(b"\x1b[2J\x1b[H").await?;return Ok(0);},
        ["cat",path]=>{output.write_all(&workspace.read(path)?).await?;return Ok(0);},
        [operation@("mkdir"|"rm"|"rmdir"),path]=>workspace.mutate(operation,path,None)?,["mv",source,target]=>workspace.mutate("mv",source,Some(target))?,
        ["check"]=>{let store=workspace.store.clone();let id=workspace.id.clone();let warnings=tokio::task::spawn_blocking(move||controller::check_configuration(&store,&id)).await.map_err(|e|Error::Invalid(format!("检查任务失败 / check task failed: {e}")))??;text=format!("配置及已有来源检查通过；软件包解析与脚本执行结果在构建时检查。\nConfiguration and existing-source checks passed; package resolution and script execution are checked during build.\n{}",warnings.join("\n"));},
        ["status"]=>{let store=workspace.store.clone();let id=workspace.id.clone();let statuses=tokio::task::spawn_blocking(move||client::request(&client::socket_for_root(store.root())?,Operation::Status{id})).await.map_err(|e|Error::Invalid(format!("状态任务失败 / status task failed: {e}")))??;text=serde_json::to_string_pretty(&statuses).map_err(|e|Error::Protocol(format!("状态编码失败 / status encoding failed: {e}")))?;let reference=workspace.store.system_layout(&workspace.id)?.root.join("build-current.toml");if reference.try_exists()?{let current=controller::read_current_build(&workspace.store,&workspace.id)?;text.push_str(&format!("\n当前成功产物 / current successful artifact: {}",current.version));}else{text.push_str("\n尚无成功构建产物 / no successful build artifact yet");}},
        ["build", "status"] => {
            if let Some(job) = admin::active_build(&workspace.store, &workspace.id)? {
                return follow_build(workspace, input, output, job, pending).await;
            }
            text = "当前系统不在构建 / this system is not building".into();
        },
        ["build", "stop"] => {
            text = if let Some(job) = admin::stop_build(&workspace.store, &workspace.id)? {
                wait_build_stop(workspace, &job.task).await?
            } else { "当前系统不在构建 / this system is not building".into() };
        },
        ["build"] | ["build", "--follow"] => {
            let store=workspace.store.clone(); let id=workspace.id.clone();
            let job=tokio::task::spawn_blocking(move||admin::submit(&store,&id,"build")).await
                .map_err(|e|Error::Invalid(format!("任务提交失败 / job submission failed: {e}")))??;
            return follow_build(workspace, input, output, job, pending).await;
        },
        [operation@("start"|"stop"|"restart")] | [operation@("start"|"stop"|"restart"),"--follow"]=>{
            let offset=fs::metadata(console::directory(&workspace.store,&workspace.id)?.join("console.log")).map(|m|m.len()).or_else(|e|if e.kind()==std::io::ErrorKind::NotFound{Ok(0)}else{Err(e)})?;
            let store=workspace.store.clone();let id=workspace.id.clone();let operation=operation.to_string();
            let job=tokio::task::spawn_blocking(move||admin::submit(&store,&id,&operation)).await.map_err(|e|Error::Invalid(format!("任务提交失败 / job submission failed: {e}")))??;
            output.write_all(format!("任务已提交 / task submitted: {}\r\n",job.task).as_bytes()).await?;
            if args.len()==2{return follow(workspace,input,output,Some(job.task.clone()),offset,None,pending).await;}
        },
        ["jobs"]=>text=serde_json::to_string_pretty(&admin::jobs(&workspace.store,&workspace.id)?).map_err(|e|Error::Protocol(format!("任务编码失败 / job encoding failed: {e}")))?,
        ["console"]=>{let offset=tail_offset(&console::directory(&workspace.store,&workspace.id)?.join("console.log"),100)?;console::current(&workspace.store,&workspace.id)?;return follow(workspace,input,output,None,offset,None,pending).await;},
        ["shell"]=>return shell(workspace,input,output,sizes,dimensions,pty,pending,remote).await,
        ["logs",rest@..]=>{
            let mut kind="boot";let mut follow_output=false;let mut lines=None;let mut task=None;let mut i=0;
            if rest.first().is_some_and(|s|matches!(*s,"boot"|"shutdown"|"build")){kind=rest[0];i=1;}
            while i<rest.len(){match rest[i]{"--follow"=>follow_output=true,"--lines"=>{i+=1;let value=rest.get(i).ok_or_else(||Error::Invalid("--lines 缺少数量 / --lines requires a count".into()))?.parse::<usize>().map_err(|_|Error::Invalid("日志行数无效 / invalid log line count".into()))?;if value>100000{return Err(Error::Invalid("日志行数超出限制 / log line count exceeds limit".into()));}lines=Some(value);},"--task"=>{i+=1;task=Some(rest.get(i).ok_or_else(||Error::Invalid("--task 缺少标识 / --task requires an identity".into()))?.to_string());},_=>return Err(Error::Invalid("日志参数无效 / invalid log arguments".into()))}i+=1;}
            if kind=="build"{
                let jobs=admin::jobs(&workspace.store,&workspace.id)?;let job=jobs.iter().rev().find(|job|job.operation=="build"&&task.as_ref().is_none_or(|task|task==&job.task)).ok_or_else(||Error::Invalid("没有对应构建任务 / no matching build job".into()))?;
                let path=admin::directory(&workspace.store,&workspace.id)?.join(format!("{}.log",job.task));let offset=match lines{Some(lines)=>tail_offset(&path,lines)?,None=>0};
                if follow_output{return follow(workspace,input,output,Some(job.task.clone()),offset,Some(path),pending).await;}read_file(&path,offset,output).await?;
            }else{
                let directory=console::directory(&workspace.store,&workspace.id)?;
                let run=if let Some(task)=task{if task.contains('/')||task.contains("..")||!task.starts_with("boot-"){return Err(Error::Invalid("控制台任务标识无效 / invalid console task identity".into()));}super::super::read_record::<console::Run>(&directory.join(format!("{task}.toml")))?}else{console::current(&workspace.store,&workspace.id)?};
                let offset=if kind=="shutdown"{run.shutdown_offset.ok_or_else(||Error::Invalid("此启动尚无关机记录 / this boot has no shutdown record".into()))?}else{run.offset};
                let offset=if let Some(lines)=lines{offset.max(tail_offset(&directory.join("console.log"),lines)?)}else{offset};
                if follow_output{return follow(workspace,input,output,None,offset,None,pending).await;}
                read_file(&directory.join("console.log"),offset,output).await?;
            }return Ok(0);
        },
        _=>return Err(Error::Invalid("未知命令或参数；输入 help 查看支持的命令 / unknown command or arguments; type help for supported commands".into()))
    }
    if !text.is_empty() {
        output
            .write_all(
                text.replace('\n', if pty { "\r\n" } else { "\n" })
                    .as_bytes(),
            )
            .await?;
        output.write_all(if pty { b"\r\n" } else { b"\n" }).await?;
    }
    Ok(0)
}

/// Read a bounded log tail by bytes without assuming UTF-8.
/// 按字节读取有界日志尾部，不假定 UTF-8。
/// # Arguments
/// - `path`, `lines`: Log path and desired trailing line count. 日志路径及所需尾部行数。
fn tail_offset(path: &PathBuf, lines: usize) -> Result<u64> {
    let mut file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    let offset = length.saturating_sub(4 * 1024 * 1024);
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    if lines == 0 {
        return Ok(length);
    }
    let mut count = 0;
    for (i, byte) in bytes.iter().enumerate().rev() {
        if *byte == b'\n' && i + 1 < bytes.len() {
            count += 1;
            if count == lines {
                return Ok(offset + i as u64 + 1);
            }
        }
    }
    Ok(offset)
}

/// Stream an existing log in bounded chunks.
/// 按有界块传输已有日志。
/// # Arguments
/// - `path`, `offset`, `output`: Log, byte cursor and SSH output. 日志、字节游标及 SSH 输出。
async fn read_file<W: AsyncWrite + Unpin>(
    path: &PathBuf,
    offset: u64,
    output: &mut W,
) -> Result<()> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = [0; 8192];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        output.write_all(&bytes[..n]).await?;
    }
    Ok(())
}

/// Preserve type-ahead, consuming only a real cancellation key outside bracketed paste.
/// 保留预输入，只消费括号粘贴之外的真实取消键。
#[derive(Default)]
struct FollowInput {
    scanned: usize,
    paste: bool,
    eof: bool,
}
impl FollowInput {
    fn cancelled(&mut self, pending: &mut Vec<u8>) -> bool {
        const START: &[u8] = b"\x1b[200~";
        const END: &[u8] = b"\x1b[201~";
        while self.scanned < pending.len() {
            let tail = &pending[self.scanned..];
            if tail.starts_with(START) {
                self.paste = true;
                self.scanned += START.len();
            } else if tail.starts_with(END) {
                self.paste = false;
                self.scanned += END.len();
            } else if tail[0] == 27 && (START.starts_with(tail) || END.starts_with(tail)) {
                break;
            } else if tail[0] == 3 && !self.paste {
                pending.remove(self.scanned);
                return true;
            } else {
                self.scanned += 1;
            }
        }
        false
    }
}

/// Follow durable files fairly; stdin EOF does not mean the remote job has succeeded.
/// 公平跟随持久文件；标准输入 EOF 不代表远程任务成功。
async fn follow<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    workspace: &Workspace,
    input: &mut R,
    output: &mut W,
    task: Option<String>,
    offset: u64,
    job_log: Option<PathBuf>,
    pending: &mut Vec<u8>,
) -> Result<u32> {
    follow_inner(
        workspace, input, output, task, offset, job_log, pending, false,
    )
    .await
}

async fn follow_build<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    workspace: &Workspace,
    input: &mut R,
    output: &mut W,
    job: admin::Job,
    pending: &mut Vec<u8>,
) -> Result<u32> {
    let path = admin::directory(&workspace.store, &workspace.id)?.join(format!("{}.log", job.task));
    output
        .write_all(format!("构建 / Build: {}\r\n", job.task).as_bytes())
        .await?;
    let code = follow_inner(
        workspace,
        input,
        output,
        Some(job.task),
        0,
        Some(path),
        pending,
        true,
    )
    .await?;
    let status = if admin::active_build(&workspace.store, &workspace.id)?.is_none() {
        "当前系统不在构建 / this system is not building"
    } else {
        "本次构建已结束 / this build has finished"
    };
    output
        .write_all(format!("\r\n{status} · exit {code}\r\n").as_bytes())
        .await?;
    Ok(code)
}

async fn follow_inner<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    workspace: &Workspace,
    input: &mut R,
    output: &mut W,
    task: Option<String>,
    mut offset: u64,
    job_log: Option<PathBuf>,
    pending: &mut Vec<u8>,
    stop_on_cancel: bool,
) -> Result<u32> {
    let path = job_log
        .clone()
        .unwrap_or(console::directory(&workspace.store, &workspace.id)?.join("console.log"));
    let task_path = task
        .as_ref()
        .filter(|_| job_log.is_none())
        .map(|task| {
            admin::directory(&workspace.store, &workspace.id).map(|d| d.join(format!("{task}.log")))
        })
        .transpose()?;
    let mut task_offset = 0;
    let mut control = FollowInput::default();
    let mut input_bytes = [0; 8192];
    let mut queued = Vec::new();
    let mut written = 0;
    let mut completed: Option<(u32, Option<String>, u64, u64)> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if control.cancelled(pending) {
            if stop_on_cancel {
                if let Some(job) = admin::jobs(&workspace.store, &workspace.id)?
                    .into_iter()
                    .find(|job| {
                        Some(&job.task) == task.as_ref()
                            && matches!(job.state.as_str(), "queued" | "running")
                    })
                {
                    admin::request_stop(&workspace.store, &workspace.id, &job)?;
                }
            } else {
                return Ok(0);
            }
        }
        if completed
            .as_ref()
            .is_some_and(|(_, _, end, task_end)| offset >= *end && task_offset >= *task_end)
            && written == queued.len()
        {
            let (code, error, _, _) = completed.as_mut().unwrap();
            if let Some(error) = error.take() {
                queued = format!("\r\n{error}\r\n").into_bytes();
                written = 0;
                continue;
            }
            return Ok(*code);
        }
        // Replay available history at the writer's speed instead of one chunk per timer tick.
        // 按输出端速度补齐已有历史，而不是每次定时器触发只读一块。
        if written == queued.len() {
            queued.clear();
            written = 0;
            read_chunk(
                &path,
                &mut offset,
                completed.as_ref().map(|v| v.2),
                &mut queued,
            )?;
            if let Some(path) = &task_path {
                read_chunk(
                    path,
                    &mut task_offset,
                    completed.as_ref().map(|v| v.3),
                    &mut queued,
                )?;
            }
        }
        tokio::select! {
            biased;
            n=input.read(&mut input_bytes), if !control.eof => {
                let n=n?;
                if n==0 { control.eof=true; }
                else {
                    if pending.len()+n > 65536 { return Err(Error::Invalid("跟随期间预输入超出限制 / type-ahead during follow exceeds limit".into())); }
                    pending.extend_from_slice(&input_bytes[..n]);
                }
            },
            _=tick.tick()=>{
                if completed.is_none() {
                    let status = if let Some(task)=&task {
                        let jobs=admin::jobs(&workspace.store,&workspace.id)?;
                        let job=jobs.into_iter().find(|job| &job.task==task).ok_or_else(||Error::Invalid("任务丢失 / task was lost".into()))?;
                        if matches!(job.state.as_str(),"queued"|"running") {
                            if let Some(error) = job.error { return Err(Error::Invalid(error)); }
                            None
                        }
                        else { Some((job.exit_code.unwrap_or(1).clamp(0,255) as u32,job.error)) }
                    } else {
                        let run=console::current(&workspace.store,&workspace.id)?;
                        run.finished.then_some((run.exit_code.unwrap_or(0).clamp(0,255) as u32,run.error))
                    };
                    if let Some((code,error))=status {
                        completed=Some((code,error,file_length(&path)?,task_path.as_ref().map(file_length).transpose()?.unwrap_or(0)));
                    }
                }
            },
            result=output.write(&queued[written..]), if written<queued.len() => {
                let n=result?;
                if n==0 { return Err(Error::Invalid("SSH 输出已关闭 / SSH output closed".into())); }
                written+=n;
            },
        }
    }
}
fn file_length(path: &PathBuf) -> Result<u64> {
    match fs::metadata(path) {
        Ok(m) => Ok(m.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e.into()),
    }
}
fn read_chunk(path: &PathBuf, offset: &mut u64, end: Option<u64>, out: &mut Vec<u8>) -> Result<()> {
    let length = end.unwrap_or(file_length(path)?);
    if length < *offset {
        return Err(Error::Invalid("日志被截断 / log was truncated".into()));
    }
    if length == *offset {
        return Ok(());
    }
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(*offset))?;
    let before = out.len();
    file.take((length - *offset).min(8192)).read_to_end(out)?;
    *offset += (out.len() - before) as u64;
    Ok(())
}

/// Encode one complete local protocol frame with a single writer.
/// 使用唯一写入者编码完整本地协议帧。
/// # Arguments
/// - `writer`, `message`: Local stream and message. 本地流及消息。
async fn send<W: AsyncWrite + Unpin>(writer: &mut W, message: &ClientMessage) -> Result<()> {
    let mut bytes = serde_json::to_vec(message)
        .map_err(|e| Error::Protocol(format!("请求编码失败 / request encoding failed: {e}")))?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    Ok(())
}

/// Bridge the actual root login shell, preserving PTY bytes, size and exit status.
/// 桥接真实 root 登录 shell，保留 PTY 字节、尺寸及退出状态。
/// # Arguments
/// - `workspace`, `input`, `output`, `sizes`, `dimensions`, `pty`, `pending`: Fixed identity and terminal ownership. 固定身份及终端所有权。
async fn shell<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    workspace: &Workspace,
    input: &mut R,
    output: &mut W,
    sizes: &mut watch::Receiver<(u32, u32)>,
    dimensions: &mut (String, u32, u32),
    pty: bool,
    pending: &mut Vec<u8>,
    remote: &Remote,
) -> Result<u32> {
    let socket = client::socket_for_root(workspace.store.root())?;
    let stream = client::connect(&socket)?;
    stream.set_nonblocking(true)?;
    let stream = tokio::net::UnixStream::from_std(stream)?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let operation = Operation::RootShell {
        id: workspace.id.clone(),
        pty,
        window: pty.then_some(WindowSize {
            cols: dimensions.1 as u16,
            rows: dimensions.2 as u16,
        }),
        term: dimensions.0.clone(),
    };
    protocol::validate_request(&protocol::Request {
        version: protocol::PROTOCOL_VERSION,
        operation: operation.clone(),
    })?;
    send(&mut writer, &protocol::request(operation)).await?;
    if pty {
        output.write_all(b"\x1b[?2004l").await?;
    }
    let (mut ready, mut input_open) = (false, true);
    let mut frame = Vec::new();
    let mut bytes = [0; 8192];
    let mut pending = Some(std::mem::take(pending));
    loop {
        tokio::select! {
            result=reader.read_until(b'\n',&mut frame)=>{if result?==0{return Err(Error::Protocol("root shell 未返回退出状态 / root shell ended without an exit status".into()));}if frame.len()>protocol::MAX_FRAME_BYTES{return Err(Error::Protocol("shell 消息超出限制 / shell message exceeds limit".into()));}let event:Event=serde_json::from_slice(&frame).map_err(|e|Error::Protocol(format!("shell 消息无效 / invalid shell message: {e}")))?;frame.clear();match event{
                Event::Ready{..}=>{ready=true;if pty{send(&mut writer,&ClientMessage::Resize{window:WindowSize{cols:dimensions.1 as u16,rows:dimensions.2 as u16}}).await?;}if let Some(bytes)=pending.take(){if !bytes.is_empty(){send(&mut writer,&ClientMessage::Input{bytes}).await?;}}},
                Event::Stdout{bytes}=>output.write_all(&bytes).await?,Event::Stderr{bytes}=>{remote.handle.extended_data(remote.channel,1,bytes).await.map_err(|_|Error::Invalid("SSH 标准错误通道已关闭 / SSH stderr channel closed".into()))?;},Event::Exit{code}=>return Ok(code.clamp(0,255) as u32),Event::Error{message}=>return Err(Error::Invalid(message)),Event::Message{text}=>{output.write_all(text.as_bytes()).await?;},_=>{}
            }},
            n=input.read(&mut bytes),if ready&&input_open=>{let n=n?;if n==0{input_open=false;send(&mut writer,&ClientMessage::Eof).await?;}else{send(&mut writer,&ClientMessage::Input{bytes:bytes[..n].into()}).await?;}},
            (cols,rows)=window(sizes)=>{dimensions.1=cols;dimensions.2=rows;if ready&&pty{send(&mut writer,&ClientMessage::Resize{window:WindowSize{cols:cols as u16,rows:rows as u16}}).await?;}}
        }
    }
}
