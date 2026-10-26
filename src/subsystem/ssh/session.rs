//! Management command dispatch and lossless nested root-shell terminal bridging.
//! 管理命令分派及无损嵌套 root shell 终端桥接。
use super::super::{
    Error, Result, admin, client, console,
    management::{
        Workspace,
        terminal::{Action, HELP, Terminal, parse},
    },
    protocol::{self, ClientMessage, Event, Operation, WindowSize},
};
use crate::{config::ConfigStore, controller};
use russh::{Channel, ChannelId, server::{Msg, Handle}};

struct Remote { handle: Handle, channel: ChannelId }
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    sync::mpsc,
};

/// Serve one interactive shell or noninteractive management command channel.
/// 服务一个交互式 shell 或非交互式管理命令通道。
/// # Arguments
/// - `channel`, `store`, `id`, `pty`, `sizes`, `command`: SSH channel, fixed identity, terminal metadata, resize events and optional exec. SSH 通道、固定身份、终端元数据、尺寸事件及可选执行命令。
pub(super) async fn run(
    channel: Channel<Msg>,
    store: ConfigStore,
    id: String,
    pty: Option<(String, u32, u32)>,
    mut sizes: mpsc::Receiver<(u32, u32)>,
    command: Option<Vec<u8>>,
    handle: Handle,
) -> Result<u32> {
    let workspace = Workspace::new(store, id.as_str())?;
    let remote = Remote { handle, channel: channel.id() };
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
            Vec::new(),
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
    loop {
        let update = if !next.is_empty() {
            let data = std::mem::take(&mut next);
            terminal.input(&data, &workspace)
        } else {
            tokio::select! {
                n=input.read(&mut bytes)=>{let n=n?;if n==0{break;}terminal.input(&bytes[..n],&workspace)},
                Some((cols,rows))=sizes.recv()=>{dimensions.1=cols;dimensions.2=rows;output.write_all(&terminal.resize(cols,rows)).await?;continue;},
                _=tokio::time::sleep(Duration::from_millis(40))=>terminal.flush_escape(&workspace)
            }
        };
        let update = match update {
            Ok(update) => update,
            Err(error) => {
                terminal.take_pending();
                output
                    .write_all(&terminal.finish(&format!("操作失败 / operation failed: {error}")))
                    .await?;
                continue;
            }
        };
        output.write_all(&update.output).await?;
        output.flush().await?;
        if let Some(action) = update.action {
            let pending = terminal.take_pending();
            let words = match action {
                Action::Exit => break,
                Action::Shell => vec!["shell".into()],
                Action::Command(words) => words,
            };
            let shell = words.first().is_some_and(|s| s == "shell");
            let result = execute(
                &workspace,
                words,
                &mut input,
                &mut output,
                &mut sizes,
                &mut dimensions,
                true,
                if shell { pending.clone() } else { Vec::new() },
                &remote,
            )
            .await;
            let text = match result {
                Ok(0) => String::new(),
                Ok(code) => format!("退出码 {code} / exit code {code}"),
                Err(error) => format!("操作失败 / operation failed: {error}"),
            };
            output.write_all(b"\x1b[0m\x1b[r\x1b[?25h\r\n").await?;
            output.write_all(&terminal.finish(&text)).await?;
            if !shell {
                next = pending;
            }
        }
    }
    output.write_all(b"\x1b[?2004l\x1b[?25h\x1b[0m").await?;
    Ok(0)
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
    sizes: &mut mpsc::Receiver<(u32, u32)>,
    dimensions: &mut (String, u32, u32),
    pty: bool,
    pending: Vec<u8>,
    remote: &Remote,
) -> Result<u32> {
    super::super::SubsystemConfig::read(&workspace.store, &workspace.id)?;
    let args = words.iter().map(String::as_str).collect::<Vec<_>>();
    let mut text = String::new();
    match args.as_slice(){
        []=>{},["help"]=>text=HELP.into(),["pwd"]=>text="/".into(),["ls"]=>text=workspace.list("/")?.join("\n"),["ls",path]=>text=workspace.list(path)?.join("\n"),
        ["cat",path]=>{output.write_all(&workspace.read(path)?).await?;return Ok(0);},
        [operation@("mkdir"|"rm"|"rmdir"),path]=>workspace.mutate(operation,path,None)?,["mv",source,target]=>workspace.mutate("mv",source,Some(target))?,
        ["check"]=>{let store=workspace.store.clone();let id=workspace.id.clone();let warnings=tokio::task::spawn_blocking(move||controller::check_configuration(&store,&id)).await.map_err(|e|Error::Invalid(format!("检查任务失败 / check task failed: {e}")))??;text=format!("配置及已有来源检查通过；软件包解析与脚本执行结果在构建时检查。\nConfiguration and existing-source checks passed; package resolution and script execution are checked during build.\n{}",warnings.join("\n"));},
        ["status"]=>{let store=workspace.store.clone();let id=workspace.id.clone();let statuses=tokio::task::spawn_blocking(move||client::request(&client::socket_for_root(store.root())?,Operation::Status{id})).await.map_err(|e|Error::Invalid(format!("状态任务失败 / status task failed: {e}")))??;text=serde_json::to_string_pretty(&statuses).map_err(|e|Error::Protocol(format!("状态编码失败 / status encoding failed: {e}")))?;let current=controller::read_current_build(&workspace.store,&workspace.id)?;text.push_str(&format!("\n当前成功产物 / current successful artifact: {}",current.version));},
        [operation@("start"|"stop"|"restart"|"build")] | [operation@("start"|"stop"|"restart"|"build"),"--follow"]=>{
            let offset=fs::metadata(console::directory(&workspace.store,&workspace.id)?.join("console.log")).map(|m|m.len()).or_else(|e|if e.kind()==std::io::ErrorKind::NotFound{Ok(0)}else{Err(e)})?;
            let store=workspace.store.clone();let id=workspace.id.clone();let operation=operation.to_string();
            let job=tokio::task::spawn_blocking(move||admin::submit(&store,&id,&operation)).await.map_err(|e|Error::Invalid(format!("任务提交失败 / job submission failed: {e}")))??;
            output.write_all(format!("任务已提交 / task submitted: {}\r\n",job.task).as_bytes()).await?;
            if args.len()==2{return follow(workspace,input,output,Some(job.task),offset,None).await;}
        },
        ["jobs"]=>text=serde_json::to_string_pretty(&admin::jobs(&workspace.store,&workspace.id)?).map_err(|e|Error::Protocol(format!("任务编码失败 / job encoding failed: {e}")))?,
        ["console"]=>{let offset=tail_offset(&console::directory(&workspace.store,&workspace.id)?.join("console.log"),100)?;console::current(&workspace.store,&workspace.id)?;return follow(workspace,input,output,None,offset,None).await;},
        ["shell"]=>return shell(workspace,input,output,sizes,dimensions,pty,pending,remote).await,
        ["logs",rest@..]=>{
            let mut kind="boot";let mut follow_output=false;let mut lines=None;let mut task=None;let mut i=0;
            if rest.first().is_some_and(|s|matches!(*s,"boot"|"shutdown"|"build")){kind=rest[0];i=1;}
            while i<rest.len(){match rest[i]{"--follow"=>follow_output=true,"--lines"=>{i+=1;let value=rest.get(i).ok_or_else(||Error::Invalid("--lines 缺少数量 / --lines requires a count".into()))?.parse::<usize>().map_err(|_|Error::Invalid("日志行数无效 / invalid log line count".into()))?;if value>100000{return Err(Error::Invalid("日志行数超出限制 / log line count exceeds limit".into()));}lines=Some(value);},"--task"=>{i+=1;task=Some(rest.get(i).ok_or_else(||Error::Invalid("--task 缺少标识 / --task requires an identity".into()))?.to_string());},_=>return Err(Error::Invalid("日志参数无效 / invalid log arguments".into()))}i+=1;}
            if kind=="build"{
                let jobs=admin::jobs(&workspace.store,&workspace.id)?;let job=jobs.iter().rev().find(|job|job.operation=="build"&&task.as_ref().is_none_or(|task|task==&job.task)).ok_or_else(||Error::Invalid("没有对应构建任务 / no matching build job".into()))?;
                let path=admin::directory(&workspace.store,&workspace.id)?.join(format!("{}.log",job.task));let offset=match lines{Some(lines)=>tail_offset(&path,lines)?,None=>0};
                if follow_output{return follow(workspace,input,output,Some(job.task.clone()),offset,Some(path)).await;}read_file(&path,offset,output).await?;
            }else{
                let directory=console::directory(&workspace.store,&workspace.id)?;
                let run=if let Some(task)=task{if task.contains('/')||task.contains("..")||!task.starts_with("boot-"){return Err(Error::Invalid("控制台任务标识无效 / invalid console task identity".into()));}super::super::read_record::<console::Run>(&directory.join(format!("{task}.toml")))?}else{console::current(&workspace.store,&workspace.id)?};
                let offset=if kind=="shutdown"{run.shutdown_offset.ok_or_else(||Error::Invalid("此启动尚无关机记录 / this boot has no shutdown record".into()))?}else{run.offset};
                let offset=if let Some(lines)=lines{offset.max(tail_offset(&directory.join("console.log"),lines)?)}else{offset};
                if follow_output{return follow(workspace,input,output,None,offset,None).await;}
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

/// Follow a recorder or durable task while concurrently accepting cancellation.
/// 跟随记录器或持久任务，同时接受取消。
/// # Arguments
/// - `workspace`, `input`, `output`, `task`, `offset`, `job_log`: Fixed view, SSH streams, optional job and source cursor. 固定视图、SSH 流、可选任务及来源游标。
async fn follow<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    workspace: &Workspace,
    input: &mut R,
    output: &mut W,
    task: Option<String>,
    mut offset: u64,
    job_log: Option<PathBuf>,
) -> Result<u32> {
    let mut bytes = [0; 8192];
    let mut lifecycle_log_offset = 0;
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            n=input.read(&mut bytes)=>{let n=n?;if n==0||bytes[..n].contains(&3){return Ok(0);}},
            _=tick.tick()=>{
                let path=job_log.clone().unwrap_or(console::directory(&workspace.store,&workspace.id)?.join("console.log"));
                if path.try_exists()?{let mut file=fs::File::open(&path)?;file.seek(SeekFrom::Start(offset))?;let n=file.read(&mut bytes)?;if n>0{output.write_all(&bytes[..n]).await?;offset+=n as u64;continue;}}
                if let Some(task)=&task{let jobs=admin::jobs(&workspace.store,&workspace.id)?;let job=jobs.iter().find(|job|&job.task==task).ok_or_else(||Error::Invalid("任务丢失 / task was lost".into()))?;
                    if job_log.is_none(){let path=admin::directory(&workspace.store,&workspace.id)?.join(format!("{task}.log"));let mut file=fs::File::open(&path)?;file.seek(SeekFrom::Start(lifecycle_log_offset))?;let n=file.read(&mut bytes)?;if n>0{output.write_all(&bytes[..n]).await?;lifecycle_log_offset+=n as u64;}}
                    if !matches!(job.state.as_str(),"queued"|"running"){if let Some(error)=&job.error{output.write_all(format!("\r\n{error}\r\n").as_bytes()).await?;}return Ok(job.exit_code.unwrap_or(1).clamp(0,255) as u32);}
                }else if console::current(&workspace.store,&workspace.id)?.finished{return Ok(0);}
            }
        }
    }
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
    sizes: &mut mpsc::Receiver<(u32, u32)>,
    dimensions: &mut (String, u32, u32),
    pty: bool,
    pending: Vec<u8>,
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
    if pty { output.write_all(b"\x1b[?2004l").await?; }
    let (mut ready, mut input_open) = (false, true);
    let mut frame = Vec::new();
    let mut bytes = [0; 8192];
    let mut pending = Some(pending);
    loop {
        tokio::select! {
            result=reader.read_until(b'\n',&mut frame)=>{if result?==0{return Err(Error::Protocol("root shell 未返回退出状态 / root shell ended without an exit status".into()));}if frame.len()>protocol::MAX_FRAME_BYTES{return Err(Error::Protocol("shell 消息超出限制 / shell message exceeds limit".into()));}let event:Event=serde_json::from_slice(&frame).map_err(|e|Error::Protocol(format!("shell 消息无效 / invalid shell message: {e}")))?;frame.clear();match event{
                Event::Ready{..}=>{ready=true;if let Some(bytes)=pending.take(){if !bytes.is_empty(){send(&mut writer,&ClientMessage::Input{bytes}).await?;}}},
                Event::Stdout{bytes}=>output.write_all(&bytes).await?,Event::Stderr{bytes}=>{remote.handle.extended_data(remote.channel,1,bytes).await.map_err(|_|Error::Invalid("SSH 标准错误通道已关闭 / SSH stderr channel closed".into()))?;},Event::Exit{code}=>return Ok(code.clamp(0,255) as u32),Event::Error{message}=>return Err(Error::Invalid(message)),Event::Message{text}=>{output.write_all(text.as_bytes()).await?;},_=>{}
            }},
            n=input.read(&mut bytes),if ready&&input_open=>{let n=n?;if n==0{input_open=false;send(&mut writer,&ClientMessage::Eof).await?;}else{send(&mut writer,&ClientMessage::Input{bytes:bytes[..n].into()}).await?;}},
            Some((cols,rows))=sizes.recv()=>{dimensions.1=cols;dimensions.2=rows;if ready&&pty{send(&mut writer,&ClientMessage::Resize{window:WindowSize{cols:cols as u16,rows:rows as u16}}).await?;}}
        }
    }
}
