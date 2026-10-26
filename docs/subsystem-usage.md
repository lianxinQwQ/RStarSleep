# 子系统运行与应用集成

子系统使用构建模块发布的当前成功产物。构建失败保留上一引用；运行中的机器继续使用
原运行根，下一次冷启动或 `restart` 才消费新产物。启动前先完成该产物的延后 CoW
继承并验证提交记录，再创建可写运行代和引导 nspawn。同产物重启复用运行根；切换
产物或挂载配置时重新创建运行根，不扫描修改、不询问丢弃修改，也没有版本选择参数。

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

root 可以管理所有子系统；其他 UID 必须显式授权，只能以自己的 UID/GID 执行。
已有容器账户的 UID/GID 冲突会报错。授权对新请求立即生效；挂载、tmpfs 和 GPU
设置在下一次启动生效。初版共享宿主网络；nspawn 的 UID 也与宿主一致。

```sh
sudo RStarSleep build apps --root /rstarsleep
sudo RStarSleep daemon install --root /rstarsleep
# install 输出服务名；随后用它启用服务：
sudo systemctl enable --now rstarsleep-subsystem-<管理根摘要>.service
```

`daemon install` 生成服务并重载 systemd，启用服务由上面的明确命令执行。
开发时可使用 `sudo RStarSleep daemon run --root /rstarsleep` 在前台运行。
机器引导日志进入 daemon 的 journal；构建状态、继承错误及 guest 构建日志保留在管理根。

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
daemon 重启会恢复注册机器、检查残留应用服务并修复 guest socket。
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

## 容器内管理

容器只获得 `/run/rstarsleep/guest.sock` 和只读管理器程序，不能读取整个管理根。
guest 操作要求容器内 root；守护进程核对内核 UID 和机器挂载命名空间，客户端没有
指定其他系统或宿主路径的参数：

```sh
sudo /run/rstarsleep/RStarSleep guest install firefox
sudo /run/rstarsleep/RStarSleep guest rebuild
sudo /run/rstarsleep/RStarSleep guest restart
sudo /run/rstarsleep/RStarSleep guest add-copy /etc/myapp.conf
sudo /run/rstarsleep/RStarSleep guest add-mount /srv/data
sudo /run/rstarsleep/RStarSleep guest migrate /srv/data
```

install 写入受控 `build/guest-packages.conf`，保留其他赋值文件，并安排异步构建。
旧 TOML 构建配置需要先迁移为 INI。响应输出宿主日志位置；构建失败不会影响运行机器，
成功产物在下一次启动/重启生效。restart 先确认请求再异步重启，原会话会断开。

add-copy 保存捕获资源及构建脚本，后续每次构建应用；add-mount 只登记待迁移项目，
未迁移时仍可重启。migrate 在互斥锁内复制到暂存 Btrfs 卷，同步并原子发布，随后激活
运行挂载并尝试热绑定。绑定失败保留已发布数据，重试不覆盖。选择路径及其祖先不能是
符号链接，内部数据链接按数据保留；控制路径、伪文件系统及特殊节点被拒绝。

捕获与迁移要求退出所有管理器应用/shell 租约，并停止其他可能写入的 guest 服务。
可显式启动机器后，在宿主使用 `sudo machinectl shell root@<status 输出的机器名>`
执行这些操作；普通 `subsystem shell` 自身也持有租约。复制期间拒绝新应用请求。

## 恢复和验证范围

`build-current.toml` 保存当前成功构建引用；`runtime/current.toml` 保存已经同步的可写
运行代。`transaction.toml` 保护未完成的存储准备，机器就绪由单独的启动来源记录提交。
引导失败不声称机器运行，也不把已经完成的继承改回待办。新机器确认就绪后才回收旧代，
仍被挂载或待办来源引用的根会保留。版本清理保护当前构建、运行版本及切换引用。
回收时会确认旧运行根确实消失后再删除代目录；失败会报告实际路径，元数据检查错误会
直接中止检查。新机器就绪后的回收失败会记录日志并结束本轮回收，机器保持运行。

自动化验证覆盖模拟 CoW 启动、一次性继承、失败引用保持、持久 fstab、版本切换及
清理保护，另外用真实 Unix socket/子进程验证流、PTY、信号、退出状态、固定 socket
inode、所有权协调和迁移发布重试。真实 Btrfs+nspawn 引导、图形出窗、音频与重登录
仍需在可执行特权操作的隔离环境完成验收；当前开发环境没有免密 sudo，未安装或启动宿主服务。

`cargo test --test session_mounts -- --include-ignored` 在独立 user/mount 命名空间中
验证真实 Unix socket 跨命名空间接入、源路径替换、只读绑定和固定挂载清理；需要启用
非特权用户命名空间，并提供 `unshare` 和 `mount`。默认测试会跳过这项环境相关验证。
