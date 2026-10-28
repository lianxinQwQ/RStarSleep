# SSH 管理终端 / SSH management terminal

使用 `ssh -t -p 2222 apps@host` 进入管理 CLI。它管理当前子系统的配置和生命周期，
输入 `shell` 才进入子系统的 root 登录 shell。配置文件视图仍仅包括 `/build/` 和
`/script/`，不执行宿主 shell，也不支持管道、重定向和变量展开。

Use `ssh -t -p 2222 apps@host` to enter the management CLI. It manages the selected
subsystem's configuration and lifecycle; `shell` opens its root login shell. The
configuration view contains only `/build/` and `/script/`. Host shell execution,
pipes, redirection and expansion are unsupported.

本机连接示例：`ssh -t -p 2222 lin@localhost`。用户名是子系统 ID，端口取自
`config/ssh.toml`。如需指定客户端私钥，使用 `-i ~/.ssh/id_rsa`；对应的公钥应写入
`systems/lin/config/subsystem/authorized_keys`。支持 Ed25519、ECDSA 和 RSA SHA-2
公钥认证，包括 RSA-2048、RSA-4096 和 RSA-8196；RSA 模数最大为 65536 位。
本机连接同样需要认证。

Local connection example: `ssh -t -p 2222 lin@localhost`. The username is the
subsystem ID; the port comes from `config/ssh.toml`. To select a client private key,
add `-i ~/.ssh/id_rsa` and place its public key in
`systems/lin/config/subsystem/authorized_keys`. Ed25519, ECDSA and RSA SHA-2 public-key
authentication are supported, including RSA-2048, RSA-4096 and RSA-8196; RSA moduli
are limited to 65536 bits. Local connections also require authentication.

RSA 公钥行中的 `ssh-rsa` 是密钥格式名称；登录时同一把密钥可使用
`rsa-sha2-256` 或 `rsa-sha2-512` 签名，无需修改公钥行的前缀。

The `ssh-rsa` prefix on an RSA public-key line names its key format. The same key
can authenticate with `rsa-sha2-256` or `rsa-sha2-512` signatures; keep the public-key
line's prefix unchanged.

加载授权公钥或已有主机密钥时提前检查 RSA 位数，超限配置明确报错。
客户端提交超限公钥时，在认证失败前显示中英文提示，包含实际位数和 65536 位上限；
随后仍可尝试其他密钥。

RSA sizes are checked when loading authorized keys or an existing host key;
oversized configured keys produce an explicit error. If a client offers an
oversized public key, an authentication banner reports the actual size and the
65536-bit limit in Chinese and English before authentication fails. The client
can still try other keys.

服务端的 `early eof` 表示客户端在 SSH 数据包尚未读完时断开，单凭这条日志无法
判断是否为公钥问题。用 `ssh -vvv -p 2222 lin@localhost` 查看客户端失败阶段。
旧版本若接受 RSA 候选公钥后拒绝签名认证，需要更新管理器并重启 daemon。

The server's `early eof` means the client disconnected before a complete SSH packet
was read; that message alone does not identify an authorization problem. Use
`ssh -vvv -p 2222 lin@localhost` to inspect the failing client stage. Older builds
that accept an RSA key probe but reject signed authentication require a manager
update and daemon restart.

## CLI

| 操作 / Operation | 按键或命令 / Key or command |
| --- | --- |
| 清屏 / Clear screen | `clear`、`Ctrl+L` |
| 查看当前连接历史 / Show this connection's history | `history` |
| 移动光标 / Move cursor | 左右方向键 / Left, Right；`Ctrl+B/F` |
| 行首、行尾 / Start, end | `Home/End`、`Ctrl+A/E` |
| 删除字符 / Delete character | `Backspace`、`Delete`、`Ctrl+D` |
| 删除前部、后部、前一单词 / Delete prefix, suffix, previous word | `Ctrl+U/K/W` |
| 浏览历史并恢复草稿 / Browse history and restore draft | 上下方向键 / Up, Down |
| 命令、路径、选项补全 / Complete commands, paths, options | `Tab` |
| 取消当前输入 / Cancel input | `Ctrl+C` |
| 空行退出 / Exit on an empty line | `Ctrl+D`、`exit` |

补全识别引号、转义和光标位置，包含空格的文件名会安全转义。目录补全追加 `/`。
长命令使用单行水平视口；正文中的 `↑↓←→` 是普通字符。

Completion understands quotes, escapes and cursor position, and safely escapes
filenames containing spaces. Directories end in `/`. Long commands use a single-row
horizontal viewport; literal `↑↓←→` remain ordinary text.

CLI 的括号粘贴只接受单行、无控制字符的文本。多行粘贴或超过 32768 字节的命令会
明确报错，并使整条命令作废；按 Enter 或 Ctrl+C 后重新输入，不执行截断前缀。
历史最多保留当前连接的 200 条命令，断连后不保留。

CLI bracketed paste accepts single-line text without control characters. Multiline
paste and commands exceeding 32768 bytes invalidate the entire command with an
explicit error. Press Enter or Ctrl+C and re-enter it; a truncated prefix is never
executed. History retains up to 200 commands for the current connection only.

## 管理面板 / Dashboard

`build` 默认启动构建并实时输出。每系统在同一 daemon 内只能有一个 SSH 构建任务；`build status` 从头补齐
该任务的全部日志后继续跟随，结束后再执行则显示“当前系统不在构建”。`build stop`
请求停止并等待确认，CLI 构建输出中的 Ctrl+C 也会停止构建。关闭 SSH 连接只断开
查看，不终止构建；重新连接可用 `build status` 接回当前任务。

`build` starts a build and streams its output by default. Each daemon tracks one SSH build per
system. While it is active, `build status` replays the entire task log and follows new
output; after completion it reports that the system is not building. `build stop`
requests cancellation and waits for confirmation. Ctrl+C during CLI build output
also stops the build. Disconnecting SSH only detaches the viewer; reconnect with
`build status` to observe the active task again.

当前 SSH 构建状态保存在 daemon 的共享内存 map 中，不创建每系统 `build.lock`。
SSH 断开后任务及登记继续保留，任务结束或提交失败时清除；daemon 重启后此表为空，
历史任务及日志仍可通过 `jobs`、`logs build` 查看。独立 CLI 构建不纳入此表。

Current SSH build state lives in the daemon's shared in-memory map, without per-system `build.lock` files.
SSH disconnects retain the task and registration; completion or submission failure clears the registration.
Daemon restart starts an empty map; `jobs` and `logs build` retain history. Standalone CLI builds are not tracked in this map.

输入 `tui` 打开全屏面板。方向键选择，Enter 执行；`Tab` 切换操作与文件浏览。
子系统停止和重启需按 `y` 确认，其他键取消。操作成功和失败的输出都显示在结果区，
`PgUp/PgDn` 滚动，`Home/End` 跳到结果开头或结尾。

Enter `tui` to open the dashboard. Use arrows to select, Enter to execute and Tab to
switch between actions and files. Subsystem stop and restart require `y` confirmation; any
other key cancels. Successful output and errors appear in the results panel.
PgUp/PgDn scroll; Home/End jump to the beginning or end.

| 快捷键 / Shortcut | 操作 / Action |
| --- | --- |
| `s`、`v`、`j` | 状态、配置检查、任务 / Status, configuration check, jobs |
| `b`（或 `l`） | 打开构建页（不会创建任务） / Open build page without starting a task |
| 构建页 `n` / `n` on build page | 开始构建 / Start build |
| 构建页 `Ctrl+C` / `Ctrl+C` on build page | 停止构建 / Stop build |
| `u`、`d`、`r` | 启动、停止、重启子系统 / Start, stop, restart subsystem |
| `c`、`h` | 只读实时控制台、root shell / Read-only live console, root shell |
| `e` | 文件浏览 / File browser |
| `f` | 刷新文件列表 / Refresh files |
| `q`、`Esc` | 返回 CLI / Return to CLI |
| 控制台页 `Ctrl+C` / `Ctrl+C` on console page | 关闭视图，保留菜单 / Close view and keep menu |

文件浏览中 Enter 打开目录或编辑文件，Backspace 返回父目录。退出从面板打开的
编辑器后返回面板。构建和控制台均在面板内实时更新，菜单始终保留。日志按磁盘
偏移分页，不截取尾部；`Home` 回看开头，`PgUp/PgDn` 翻页，`End` 恢复跟随。
构建结束后保留本次日志并显示空闲状态，切换页面不会停止任务。普通操作的结果
最多收集 4 MiB，超限明确报错；只有 root shell 接管真实终端，结束后回到面板并
保留明确标注的最后 64 KiB 输出。
面板中的粘贴不会触发操作快捷键。

In the file browser, Enter opens a directory or edits a file; Backspace goes to the
parent. Leaving an editor opened from the dashboard returns to the dashboard.
Build and console output update inside the dashboard while its menu remains visible.
Logs are paged by disk offsets without discarding history: Home visits the beginning,
PgUp/PgDn scroll, and End resumes following. Completed builds retain their log with
an idle status. Switching pages does not stop tasks. Ordinary command output is
limited to 4 MiB with an explicit error on overflow. Only the root shell takes over
the terminal, then returns with an explicitly labelled last 64 KiB of output.
Pasting does not trigger actions.

## 编辑器与日志 / Editor and logs

编辑器仍支持 `i/a/o`、方向键、`h/j/k/l`、`:w`、`:q`、`:wq`、`:q!`、搜索和撤销。
括号粘贴在普通或插入模式都按字面插入，保留原模式，整次粘贴只占一次撤销；
箭头字符、换行和制表符保留原文。上限为 1 MiB，超限拒绝整次插入。
保存冲突及其他错误显示在编辑器状态区，不丢弃缓冲区。

The editor supports `i/a/o`, arrows, `h/j/k/l`, `:w`, `:q`, `:wq`, `:q!`, search and
undo. Bracketed paste inserts literal text in normal or insert mode while preserving
that mode, with one undo entry per paste. Arrow characters, newlines and tabs remain
unchanged. The limit is 1 MiB; oversized insertions are rejected atomically. Save
conflicts and other errors stay in the editor status area without discarding text.

传统 `logs --follow` 和 CLI 控制台中的 `Ctrl+C` 只取消查看；构建输出中的
`Ctrl+C` 会停止构建。跟随期间的预输入保留给 CLI。
非交互 `--follow` 遇到标准输入 EOF 仍等待实际任务结束并返回任务退出码。
关闭 SSH 通道会释放会话，已提交的后台任务继续运行。

Ctrl+C in legacy `logs --follow` and the CLI console only detaches the viewer;
Ctrl+C during build output stops the build. Type-ahead is
preserved for the CLI. Noninteractive `--follow` continues waiting after stdin EOF
and returns the actual job exit code. Closing the SSH channel releases the session;
already submitted jobs continue running.
