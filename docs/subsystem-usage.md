# 子系统运行与应用集成

子系统使用构建模块发布的当前成功产物。构建失败保留上一引用；运行中的机器继续使用
原版本根，下一次冷启动或 `restart` 才消费新产物。启动前先完成该产物的延后 CoW
继承并验证提交记录，再直接从 `systems/<系统>/versions/<版本>/root` 引导 nspawn。
与主系统相同，运行修改直接保存在该版本根内。同版本重启或变更挂载、tmpfs、GPU
配置均保留这些修改；切换版本使用新版本根，旧版本及其修改保留到显式 `clean`。
跨版本共享的数据使用 Mount，需要随新构建复制的内容使用已有 CoW 继承配置。

## 安装和配置

宿主需要 Linux `openat2`、Btrfs、systemd/machined/nspawn 和 util-linux。
启用调用者会话资源时，还需要 `open_tree`、`move_mount`；只读资源另需
`mount_setattr`（Linux 5.12 起提供）。内核或权限不支持时会直接报错，不回退路径挂载
或可写绑定。
宿主及容器 systemd 需要支持 `ExitType=cgroup` 和 `systemd-run --expand-environment=no`
（systemd 254 或更新版本）。容器需具备可引导的 systemd、getent、useradd/groupadd、
install 和 `/bin/sh`；默认 isolated 会话还需要 `/usr/bin/dbus-daemon`。
native 模式额外需要宿主 `/usr/bin/xdg-dbus-proxy`；缺少代理不会回退直连宿主总线。

先把编译产物安装到稳定的管理员所有路径，例如：

```sh
cargo build --release
sudo install -m 0755 target/release/RStarSleep /usr/local/bin/RStarSleep
```

管理根、祖先目录、配置和插件资源须属于 root，不能允许普通用户写入。
只整理控制目录权限，运行根中的普通用户数据应保留原有属主。
在全局 `/rstarsleep/config/rstarsleep.toml` 中登记角色：

```toml
schema_version = 1
subsystems = ["apps"]
```

同一系统不能同时出现在 `main_system` 和 `subsystems`。每系统当前配置位于
`systems/apps/config/subsystem/config.toml`，旧的仅有 `schema_version=1` 的文件继续有效：

```toml
schema_version = 1
allowed_uids = [1000]
autostart = false
idle_timeout_seconds = 30
session_user = "caller"
session_mode = "isolated" # off / isolated / native
gpu = true                # 需要 GPU 时显式开放 /dev/dri

# 主机文件必须显式映射；不自动共享整个 home。
[[mounts]]
source = "/home/alice/Projects"
target = "/home/alice/Projects"
read_only = false

[exports]
packages = ["firefox"]
commands = ["/usr/bin/rg"]
desktop_files = []
```

本地 Unix socket 接口中，root 可以管理所有子系统；其他 UID 必须显式授权，只能以自己的 UID/GID 执行。
已有容器账户的 UID/GID 冲突会报错。授权对新请求立即生效；挂载、tmpfs 和 GPU
设置在下一次启动生效。初版共享宿主网络；nspawn 的 UID 也与宿主一致。

```sh
sudo RStarSleep build apps --root /rstarsleep
sudo RStarSleep daemon install --root /rstarsleep
# install 输出服务名；随后用它启用服务。 / Enable the service named by install.
sudo systemctl enable --now rstarsleep-subsystem-<管理根摘要>.service
```

`daemon install` 生成服务并重载 systemd，启用服务由上面的明确命令执行。
开发时可使用 `sudo RStarSleep daemon run --root /rstarsleep` 在前台运行。
daemon 诊断进入服务 journal；机器原始控制台、管理任务日志和继承错误保留在管理根。

### 控制台记录器监督与升级

每个子系统的控制台记录器由独立的 systemd 临时服务管理，名称为
`rstarsleep-console-<规范管理根与子系统标识的 SHA-256 摘要>.service`。
该服务在需要启动子系统时创建，无需单独安装或启用；daemon 重启不会停止它。
服务不自动重启，由 daemon 统一协调启动和重试。后台协调失败采用最长 60 秒的退避，
相同错误只在首次出现时输出，恢复成功后重置。

恢复时同时验证服务身份、本次 `InvocationID`、机器运行根和容器内执行能力；
服务显示 `active` 本身不代表子系统就绪。已有服务正在启动时继续等待，正在关机时
等待机器退出和控制台日志排空，然后才允许下一次启动。`capture.lock` 保留为防御保护，
不通过 PID 文件或删除锁文件判断实例是否存活。

`stop` 仍通过正常关机请求关闭机器，并等待记录器提交最后的输出。启动、关机或排空
超时会明确报错并保留实例，启动失败也不会无条件终止已有机器。可用下列命令查看
服务状态；具体失败信息会附上对应服务名，再用 `journalctl -u <服务名>` 查看诊断：

```sh
systemctl list-units --all 'rstarsleep-console-*.service'
```

机器尚未注册时发出的停止请求按 `InvocationID` 持久保存；即使等待超时或 daemon
重启，后台仍会继续完成该次停止。它不会影响下一次运行；`autostart=true` 的既有
自动启动策略仍然有效，需要持续停机时应关闭该配置。

从旧版脱离 daemon 的记录器升级时，应先使用旧版正常停止子系统，并确认旧记录器
已经退出，再替换程序和重启 daemon。新版不会接管没有服务身份的旧记录器；发现
旧实例持锁或机器运行但服务缺失时会报错，保留原日志。不要删除 `capture.lock` 来绕过检查。

开发验证中，`subsystem::supervision::systemd::tests::real_systemd_concurrent_start`
是需要 root 和系统 systemd 的手动测试（默认忽略）；它创建一个短生命周期的独立测试
服务，验证并发启动共享同一次 `InvocationID`，随后等待服务自然退出。该测试不启动
nspawn；真实子系统的启动、daemon 重启恢复和关机排空仍需在具备完整宿主条件的环境验证。

## SSH 监听与公钥认证

SSH 服务由同一 daemon 提供。管理员在管理根创建 `config/ssh.toml`，例如
`/rstarsleep/config/ssh.toml`：

```toml
schema_version = 1
listen = "127.0.0.1:2222"
host_key = "config/ssh_host_ed25519_key"
```

没有该文件时不启动 SSH。`schema_version` 必须为 `1`；`listen` 和 `host_key`
可省略，默认值如上。监听地址使用 IP 和非零端口；远程访问可将 `listen` 设置为
`0.0.0.0:2222`，客户端连接宿主的实际 IP 或域名。监听与主机密钥配置在 daemon
启动时读取，修改后重启对应的 daemon 服务。

相对 `host_key` 路径基于管理根解析，也可使用绝对路径。父目录须预先存在并受管理员
控制；首次启动时自动生成 Ed25519 主机私钥，后续复用。已有私钥必须仅属主可访问，
通常使用 root 所有及 `0600` 权限。SSH 配置、公钥文件或主机私钥无效时明确报错；
不跳过错误行，也不临时替换已有主机身份。

每个子系统的授权公钥集中写入：

```text
/rstarsleep/systems/apps/config/subsystem/authorized_keys
```

该文件由宿主管理员维护，属于 root，不能允许普通用户写入。每行一个 OpenSSH 公钥，
允许末尾说明、空行和以 `#` 开头的注释；不支持 `command=`、`from=` 等公钥选项。
同一文件可以包含多个公钥。文件缺失、为空或没有匹配公钥时拒绝登录；仅支持公钥认证。

SSH 用户名就是全局 `subsystems` 中登记的系统名称。例如：

```sh
ssh -p 2222 apps@127.0.0.1
ssh -p 2222 apps@host.example 'status'
```

SSH 公钥授权授予该子系统的配置编辑、构建、生命周期管理及 root shell 权限；
`allowed_uids` 用于本地宿主 UID 访问，不用于 SSH 公钥认证。每次认证重新读取角色和
公钥文件，删除公钥会拒绝后续认证，已经建立的连接不会因此自动断开。

## SSH 管理 CLI、编辑器与 TUI

交互登录直接显示类 shell 管理提示符；需要 PTY，从脚本发起交互会话时使用 `ssh -t`。
CLI 支持历史记录、Tab 补全、引号和转义；参数按字面值解析，不支持管道、重定向、
变量展开或命令替换。非交互 SSH 可执行单条管理命令；编辑器与 TUI 在交互管理 CLI 中使用。

CLI 与 SFTP 的虚拟根完全相同：

```text
/
├── build/     # 构建配置 / Build configuration
└── script/    # 构建脚本及资源 / Build scripts and resources
```

这里只显示当前登录子系统的配置。`subsystem/`、其他系统、宿主路径和运行根不在此
文件视图中；符号链接和特殊文件也不作为可访问的配置文件。

| 命令 | 用途 |
| --- | --- |
| `help`、`pwd`、`cd [路径]`、`ls [路径]` | 帮助与虚拟目录导航 |
| `cat 路径`、`mkdir 路径`、`rm 路径`、`rmdir 路径`、`mv 来源 目标` | 读取文本、创建目录、删除文件或空目录、重命名 |
| `edit 路径`、`vi 路径`、`vim 路径` | 打开同一个内置类 Vim 编辑器 |
| `check` | 检查配置、插件要求、脚本结构及已有继承来源；软件包解析和实际脚本结果在构建时检查 |
| `build`、`jobs` | 提交后台构建，查看持久任务状态、退出码和错误 |
| `status` | 查看机器状态及当前成功构建产物 |
| `start`、`stop`、`restart` | 提交生命周期任务；追加 `--follow` 等待并跟随输出 |
| `logs [boot\|shutdown\|build] [--follow] [--lines N] [--task ID]` | 查看启动、关机或构建日志；默认查看启动日志 |
| `console` | 从最近 100 行开始跟随机器原始控制台输出 |
| `shell`、`tui`、`exit` | 进入子系统 root shell、打开 TUI、退出管理会话 |

编辑器支持 UTF-8 文本，打开文件上限为 1 MiB。`i`／`a`／`o` 进入插入模式，
`Esc` 返回普通模式；方向键或 `h/j/k/l` 移动，`0`／`$` 到行首／行尾，`x` 删除，
`u` 撤销，`/文本` 搜索，`n` 查找下一处。使用 `:w` 保存、`:q` 退出、`:wq` 保存退出、
`:q!` 放弃修改。保存时比较原始内容，其他会话已修改同一文件时拒绝覆盖。

保存配置后显式构建，再启动或重启以使用成功产物，例如在管理 CLI 中执行：

```text
edit /build/config.conf
check
build
jobs
logs build --follow
restart --follow
```

软件包通过 `build/` 中 INI 的 `[Content] Packages` 声明；构建行为与资源通过
`script/` 管理，持久化和继承通过 `[Inheritance]` 配置。脚本目录的顶层条目遵循
[构建脚本格式](../src/config/layout.md#系统构建脚本)，资源应放在带 `main.sh` 的目录或资源归档中。
保存不会自动构建或重启；构建失败保留当前成功产物和正在运行的机器，成功产物在下次
冷启动或重启时生效。

输入 `tui` 打开全屏快捷键面板：`s` 状态、`b` 构建、`u` 启动、`d` 停止、`r` 重启、
`c` 控制台、`l` 构建日志、`h` root shell。`e` 返回 CLI 并预填 `edit `，随后输入文件路径；
`q`、`Esc` 或 `Ctrl+C` 返回 CLI。

## SFTP 配置访问与提交

使用同一个 SSH 地址、系统名称和公钥连接 SFTP：

```sh
sftp -P 2222 apps@host.example
```

例如在 SFTP 会话中传输配置和脚本：

```text
ls /
get /build/config.conf ./apps.conf
put ./apps.conf /build/config.conf
put ./01-configure.sh /script/01-configure.sh
```

SFTP 支持目录枚举、文件读取和上传、创建／删除目录、删除／重命名文件及受限属性修改。
上传文件上限为 256 MiB；编辑器之外的二进制脚本资源也可通过 SFTP 上传。配置须保持
管理员所有，不允许设置普通用户属主、组／其他用户可写权限或特殊权限位。

上传数据先写入独立暂存文件，成功关闭文件句柄后才原子发布；未关闭句柄就断开连接
会放弃该文件的暂存内容。编辑提交、SFTP 写入和构建输入冻结共用每系统 `config.lock`，
冲突时报错，请关闭上传并完成提交后重试构建。构建使用冻结输入，之后的编辑留待下次构建。

## root shell 与任务日志

在 SSH 管理 CLI 内输入 `shell`，或直接执行：

```sh
ssh -t -p 2222 apps@host.example shell
```

这会按需启动并进入该子系统 root 账户配置的登录 shell，使用子系统内的 home，支持
终端尺寸变化和双流、退出状态传递。远程 root shell 关闭桌面会话继承；退出后回到
交互管理 CLI，直接 SSH 执行 `shell` 时则结束该 SSH 命令。

宿主的 `RStarSleep subsystem shell apps --root /rstarsleep` 是本地调用者入口，
按调用者 UID/GID 运行 `/bin/sh -l`。以 root 调用可进入 root 身份；从管理根运行程序时
也可使用 `sudo /rstarsleep/RStarSleep subsystem shell apps`。

后台管理任务保存在 `systems/apps/runtime/jobs/`，每个任务有状态文件和日志；构建与
生命周期任务分别使用锁。同类任务并发提交会报错。任务独立于 SSH 连接运行，断连后
可重新登录，用 `jobs` 和 `logs build --task <任务ID>` 查看。`logs build --follow`
跟随对应构建任务日志；`start/stop/restart --follow` 跟随生命周期输出。

原始机器 PTY 输出追加到 `systems/apps/runtime/console/console.log`，启动和关机
偏移及退出结果保存在同目录的 `current.toml` 和 `boot-*.toml`。`console` 是输出订阅，
交互操作使用 `shell`。`Ctrl+C` 取消日志跟随或控制台订阅，不取消已提交的后台任务。
重新查询 `jobs` 时会核对未完成任务的工作进程；未记录完成且工作进程已经结束的任务显示为中断。

## 运行与应用

```sh
RStarSleep subsystem list --root /rstarsleep
RStarSleep subsystem status apps --root /rstarsleep
sudo RStarSleep subsystem start apps --root /rstarsleep
RStarSleep subsystem shell apps --root /rstarsleep
RStarSleep subsystem exec apps --root /rstarsleep -- /usr/bin/printf '%s\n' 'literal $HOME'
RStarSleep subsystem open apps firefox --root /rstarsleep
RStarSleep subsystem run-desktop apps /usr/share/applications/firefox.desktop --root /rstarsleep
sudo RStarSleep subsystem restart apps --root /rstarsleep
sudo RStarSleep subsystem stop apps --root /rstarsleep
```

`exec --pty` 和 `shell` 支持终端尺寸及信号转发，数值退出码和 stdout/stderr 分别传递。
`open` 默认分离图形应用，`open --wait` 等待应用完成；`Terminal=true` 的 desktop
始终等待终端程序。每个应用服务跟踪整个 cgroup，派生进程仍占用子系统。
显式启动和自启不会自动停止；按需启动在最后一个应用结束后等待闲置宽限时间再停止。
daemon 重启会恢复注册机器并检查残留应用服务。
闲置检查还覆盖私有总线激活的进程和客体用户应用 scope；最后一次占用结束后重新计算
宽限时间，探测失败时保留机器。

会话只接入认证用户的 Wayland、X11 认证文件及音频 socket。重登录时核对 socket
dev/inode 和容器可见挂载并重新绑定。isolated 使用容器私有用户总线；native 通过
降权过滤代理只开放 Fcitx/IBus 输入法服务，不开放宿主 portal 或文件管理器。
off 禁用这些资源。本地文件参数要求容器实际可见；主客路径不一致时提示使用客体路径。
当前本地 file URL 要求绝对未编码路径，portal 自动文件传递尚未支持。

会话资源以固定文件描述符跨挂载命名空间接入：先在宿主克隆分离挂载，再由重新执行
当前程序的独立辅助进程进入固定的容器命名空间，挂到固定目标。过程不经过 util-linux
对 `/proc/self/fd` 路径的解析，也不会将描述符还原成可被替换的路径。只读资源在挂入前
设置只读，不先暴露可写挂载；挂入后核对容器路径实际可见的 dev/inode。发布失败时
通过保留的挂载描述符清理本次挂载，避免误卸载原路径上的替换对象；清理失败也会报出。
错误包含操作、资源路径及失败的系统调用。卸载采用 `MNT_DETACH`，解除该挂载树的
命名空间可见性；已经打开的引用仍可使用旧对象，不保证立即撤销既有连接。

Wayland 与本地 X11/XWayland 可以同时接入，分别使用调用者的 `WAYLAND_DISPLAY`
和 `DISPLAY`。`DISPLAY` 不要求同时设置 `XAUTHORITY`；只有调用者显式提供非空
`XAUTHORITY` 时才校验并只读挂载该文件、设置容器内认证文件路径。显式提供的文件
无效时仍报错退出。不需要 cookie 文件的 XWayland 会话无需额外设置认证路径。

管理员声明的持久 Mount 由 nspawn 注入。运行时通过独立只读 fstab 覆盖去掉管理器
挂载块，保留受支持的 guest tmpfs/bind 条目；不改写共享 `/etc` 或持久 fstab。
需要宿主设备或文件系统的其他 fstab 条目会报错，须改为显式 runtime mounts。

## 用户级入口

```sh
# 以目标普通用户执行，而非 sudo：
RStarSleep subsystem sync-exports apps --root /rstarsleep
```

命令生成到 `~/.local/bin/apps-<命令>`，desktop 和图标生成到用户 XDG 数据目录并使用
系统前缀。确保 `~/.local/bin` 在 PATH 中。保留 desktop 的本地化名称、类别、MIME 类型、
Terminal 等主组信息；Exec 字段按参数展开，不交给 shell 解析。
同名命令使用系统前缀避免冲突，已有外部入口或用户修改过的入口会保留。

按包导出需要插件声明 `query_package_files=true` 并提供同名操作。Arch 已实现；
构建生成 `application-files.tsv` 候选索引，失败不发布当前产物。索引记录构建时所属关系，
继承可能改变实际文件；导出和运行仍通过容器根内文件描述符验证文件类型和可见性。
已有运行实例使用其运行版本的索引，冷实例使用当前产物。首次显式同步后，产物或运行态
切换会刷新已登记用户；移出角色或撤销 UID 授权时只撤销未修改的自有入口。
降权导出 helper 的总执行时限为 30 秒，超时会清理其子进程并返回错误。

## 从旧 guest 控制方式迁移

`guest` 子命令及其 install、rebuild、restart、add-copy、add-mount、migrate 附加机制
已移除，容器不再获得专用 guest socket 或管理器程序注入。配置管理统一使用 SSH CLI／TUI
和 SFTP：编辑 Packages 或构建脚本，检查并提交构建，随后显式重启。
持久数据和继承使用现有 build 配置机制，来源选择遵循构建继承语义；不再提供旧 guest
捕获、持久迁移收据和运行时热绑定流程。继承说明见
[配置目录说明](../src/config/layout.md#继承配置与版本数据)。

## 恢复和验证范围

`build-current.toml` 保存当前成功构建引用；`runtime/current.toml` 使用格式 2，保存
已准备的版本和机器身份，直接解析到该版本的 `root`。机器就绪由单独的启动来源记录
提交；引导失败不声称机器运行，也不把已完成的继承改回待办。版本清理保护当前构建、
已登记运行版本、实际运行根、挂载及待办继承来源，不在新机器启动后自动删除旧版本。

升级前的格式 1 运行记录仍可用于恢复、停止已有机器。下一次冷启动或重启改用版本根；
旧 `runtime/generations/<代>/root` 中的修改保留原处，不自动合并到版本根，也不自动
回收已提交的旧运行快照。旧存储准备事务仍按原恢复规则收尾，新启动不创建运行代或存储准备事务。

自动化验证覆盖直接版本根启动、配置变更保留修改、一次性继承、失败引用保持、持久 fstab、版本切换及
清理保护，另外用真实 Unix socket/子进程验证流、PTY、信号、退出状态、固定 socket
inode 和所有权协调。SSH/SFTP 测试覆盖真实回环握手、公钥认证与撤销、命令执行和
二进制上传；工作区与终端测试覆盖路径边界、事务写入、并发保存、UTF-8 编辑和 TUI 返回。
真实 Btrfs+nspawn 引导、远程 root shell、重建重启闭环、图形出窗、音频与重登录
仍需在可执行特权操作的隔离环境完成验收。测试结果以实际命令输出为准。

`cargo test --test session_mounts -- --include-ignored` 在独立 user/mount 命名空间中
验证真实 Unix socket 跨命名空间接入、源路径替换、只读绑定和固定挂载清理；需要启用
非特权用户命名空间，并提供 `unshare` 和 `mount`。默认测试会跳过这项环境相关验证。
