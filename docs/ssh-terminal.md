# SSH 管理终端 / SSH management terminal

使用 `ssh -t -p 2222 apps@host` 进入管理 CLI。它管理当前子系统的配置和生命周期，
输入 `shell` 才进入子系统的 root 登录 shell。配置文件视图仍仅包括 `/build/` 和
`/script/`，不执行宿主 shell，也不支持管道、重定向和变量展开。

Use `ssh -t -p 2222 apps@host` to enter the management CLI. It manages the selected
subsystem's configuration and lifecycle; `shell` opens its root login shell. The
configuration view contains only `/build/` and `/script/`. Host shell execution,
pipes, redirection and expansion are unsupported.

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

输入 `tui` 打开全屏面板。方向键选择，Enter 执行；`Tab` 切换操作与文件浏览。
停止和重启需按 `y` 确认，其他键取消。操作成功和失败的输出都显示在结果区，
`PgUp/PgDn` 滚动，`Home/End` 跳到结果开头或结尾。

Enter `tui` to open the dashboard. Use arrows to select, Enter to execute and Tab to
switch between actions and files. Stop and restart require `y` confirmation; any
other key cancels. Successful output and errors appear in the results panel.
PgUp/PgDn scroll; Home/End jump to the beginning or end.

| 快捷键 / Shortcut | 操作 / Action |
| --- | --- |
| `s`、`v`、`j` | 状态、配置检查、任务 / Status, configuration check, jobs |
| `b`、`u`、`d`、`r` | 构建、启动、停止、重启 / Build, start, stop, restart |
| `l`、`c`、`h` | 构建日志、实时控制台、root shell / Build log, live console, root shell |
| `e` | 文件浏览 / File browser |
| `f` | 刷新文件列表 / Refresh files |
| `q`、`Esc`、`Ctrl+C` | 返回 CLI / Return to CLI |

文件浏览中 Enter 打开目录或编辑文件，Backspace 返回父目录。退出从面板打开的
编辑器后返回面板。普通操作的结果最多收集 4 MiB，超限明确报错；实时控制台和
root shell 使用真实终端，结束后回到面板并保留明确标注的最后 64 KiB 输出。
面板中的粘贴不会触发操作快捷键。

In the file browser, Enter opens a directory or edits a file; Backspace goes to the
parent. Leaving an editor opened from the dashboard returns to the dashboard.
Ordinary command output is limited to 4 MiB with an explicit error on overflow.
Live console and root shell use the real terminal, then return to the dashboard
with an explicitly labelled last 64 KiB of output. Pasting does not trigger actions.

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

`Ctrl+C` 仅取消日志跟随，不取消后台任务；跟随期间的预输入保留给 CLI。
非交互 `--follow` 遇到标准输入 EOF 仍等待实际任务结束并返回任务退出码。
关闭 SSH 通道会释放会话，已提交的后台任务继续运行。

Ctrl+C cancels log following without cancelling the background job. Type-ahead is
preserved for the CLI. Noninteractive `--follow` continues waiting after stdin EOF
and returns the actual job exit code. Closing the SSH channel releases the session;
already submitted jobs continue running.
