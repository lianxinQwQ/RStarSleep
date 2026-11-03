# 子系统固定代理出口

状态：已实现可选的固定代理运行网络；省略 `network` 时继续共享宿主网络。
配置及依赖见[使用说明](subsystem-usage.md#固定-socks5http-代理出口--fixed-socks5http-proxy-egress)。
真实内核数据路径已通过隔离测试，完整 systemd+nspawn 部署仍需专用环境验收。

## 1. 出口语义

为需要特殊网络处理的子系统配置一个固定 SOCKS5 或 HTTP CONNECT 代理。应用发起普通
网络连接，由宿主透明接管，不要求应用支持代理，不在客体中设置代理环境变量或桌面系统代理。
第一版以宿主独立 Xray 实例完成透明入站与固定代理出站，不修改宿主已有 Xray/dae 的配置。

每个子系统只有一个配置的代理出口，不做目标分流、出口选择、负载均衡或直连回退。
代理地址可以指向宿主现有代理服务，也可以指向外部代理服务器。

不主动探测远端代理，不按在线状态切换规则，也不以远端可达作为子系统启动条件。
无监听者、连接超时、认证失败等由实际代理连接自然返回失败；恢复后新连接继续使用同一出口。
本地网络资源和透明接管程序仍必须准备成功，配置与依赖错误不能留到应用联网时才发现。

“自然失败”成立的前提是网络拓扑只有这条出口路径：子系统不能在透明接管失效后直接转发
到宿主外网。禁止直接转发是固定的数据路径约束，不依赖探活结果，不随远端状态开关。

## 2. 网络拓扑与接管

数据路径：子系统独立网络命名空间 → 专属 veth → 宿主透明接管 → 固定代理出站 → 配置的代理服务器。

运行模块预建网络命名空间、veth、地址及路由，nspawn 使用 `--network-namespace-path=` 加入。
此模式不同时传入 `--private-network`、`--network-veth` 等其他 nspawn 网络选项。
由宿主完成网络准备，不要求客体或宿主使用特定发行版的网络管理器，也不依赖客体 DHCP 启动。

宿主按 veth 接口识别来源，通过专属 nftables 规则和策略路由交给 Xray 透明入站。
这里的策略路由只负责把包送入本地透明代理，不用于选择多个出口。
支持的业务连接统一指向唯一代理出站；未支持的协议明确阻断。
规则匹配原始目的地址，不能仅把代理端口当作默认网关，也不能只把普通 TCP 包重定向到 SOCKS/HTTP 端口。

宿主规则对该 veth 不开放通往外网或其他子系统的普通转发，客体到宿主的访问仅开放规定的
透明接管和 DNS 路径；必要的地址解析、邻居发现、返回流量和错误报文单独处理。
IPv4、IPv6 采用一致约束，不能只接管一种地址族而让另一种沿宿主默认路由转发。
透明接管进程退出或监听端口消失后，规则仍保留，连接失败，不增加其他出口。

代理服务器的地址是透明接管进程新建连接的目的地。宿主已有透明分流必须排除专属 veth 的
原始报文（包括早于 nft 的 TC/eBPF 接管）和这些出口连接，
避免再次接管和循环。需要时使用独立的 socket mark/掩码与宿主约定；该标记只用于出口排除，
不作为交给 dae 选择代理的接口。远端域名的初始解析同样需要明确的宿主解析路径。
因此固定代理保证业务连接只交给指定代理；绕过宿主既有透明处理仍需宿主遵守这项排除约定。

## 3. 协议与 DNS

| 项目 | 第一版行为 |
| --- | --- |
| SOCKS5 TCP/UDP | 透明接管后通过固定 SOCKS5 出站；UDP 按协议协商 relay |
| HTTP CONNECT TCP | 透明接管后通过固定 HTTP CONNECT 出站 |
| 普通 UDP、QUIC | SOCKS5 使用 UDP relay；普通 HTTP CONNECT 模式阻断 |
| 常规 UDP/TCP DNS | 宿主专用 DNS 转发入口接收，经固定代理转发到配置的 TCP DNS 上游 |
| 应用自带 DoH/DoT | 按相应 TCP/UDP 能力使用固定代理；HTTP/3 形式遵循 UDP 限制 |
| ICMP 等其他协议 | 不声明代理支持；必要的网络错误报文保留，普通探测不能改走直连 |

普通 HTTP CONNECT 不提供任意 UDP 能力。SOCKS5 的 UDP ASSOCIATE 属于实际协议握手，
不需要提前探测；远端拒绝协商或不能转发时自然失败，不改走直连。
协商得到的 UDP relay 属于同一个固定代理会话，不是另一个可选出口。
两种代理均提供 TCP 和常规 DNS；SOCKS5 另外提供 UDP，不能因协议能力差异增加隐含直连行为。
普通 DNS 的 UDP 入口需要由本地 DNS 转发器转换为 TCP 查询，再通过同一个固定代理；
不能直接把 UDP 查询交给仅支持 TCP 的代理出站。

DNS 上游使用明确的 IP 和端口，通过代理建立 TCP 连接。客体运行时注入仅指向宿主专用入口的
`resolv.conf`，不复制宿主回环地址，也不修改构建配置中的代理设置。
DNS 转发失败自然返回错误或超时，不尝试宿主默认 DNS 或直连公共 DNS。
这一 DNS 上游处理业务查询，与宿主为代理服务器域名执行的初始解析分别定义。

## 4. 配置

沿用每系统 `config/subsystem/config.toml`。
不含 `network` 的现有配置继续使用共享宿主网络；显式选择固定代理后，缺项或错误直接报错。

```toml
# 固定代理运行网络；下一次启动生效。 / Fixed-proxy runtime network; effective on next boot.
[network]
mode = "proxy"
# 可选的宿主出口排除标记。 / Optional host egress exclusion mark.
# outbound_mark = 255
# 默认地址冲突时可显式配置。 / Explicit ranges can resolve default address conflicts.
# ipv4_subnet = "10.97.0.0/30"
# ipv6_subnet = "fd97::/64"

[network.proxy]
# 首版支持 socks5 或 http。 / Initially supports socks5 or http.
protocol = "socks5"
# 由宿主接管进程连接；回环地址指宿主。 / Connected by the host relay; loopback refers to the host.
address = "127.0.0.1"
port = 1080
# 可选认证，不记录到日志或命令行。 / Optional credentials; exclude from logs and command lines.
# username = "example"
# password = "replace-me"

[network.dns]
# 示例上游，通过上述代理连接，不直连。 / Example upstream, reached through the proxy without direct access.
address = "9.9.9.9"
port = 53
```

不提供远端探活、自动回退、不可达时直连等开关。固定代理模式自身定义失败行为。
代理认证材料只保留在管理员可读的配置及必要运行文件中，状态展示隐藏认证内容。

在版本继承收尾、创建网络资源、启动客体之前，完成所有只依赖配置、现有环境和来源系统的检查：
模式与字段组合、协议、端口、地址、认证、DNS 上游、工具依赖、Xray 配置语法和已知资源冲突。
资源创建后才能确定的命名空间、网卡与本地接管状态，则在引导客体前检查。
以上检查不连接远端代理，不能把远端离线视为配置错误。

## 5. 生命周期与所有权

网络资源按规范管理根与子系统标识派生稳定身份。网卡名满足 Linux 长度限制，并显式检查
冲突；不能仅截断现有机器名，也不能删除或接管其他程序创建的同名资源。
内部地址、专用路由表及监听资源统一登记，检查跨子系统和宿主冲突。入站策略路由按 veth
匹配，不设置入站 mark；可选的出站 socket mark 由管理员与宿主分流规则约定。

启动顺序为：提前校验 → 创建隔离网络及禁止旁路的规则 → 准备本地透明代理和 DNS 服务 →
完成本地接管 → 引导 nspawn。创建中的资源不开放普通转发，远端在线与否不参与这一步骤。

每个子系统的接管实例使用独立配置和 systemd 运行单元，独立于 daemon 服务生命周期。
daemon 重启时按资源身份恢复登记，不中断仍在运行的子系统，也不临时开放直连。
恢复核对命名空间 inode、网卡 epoch alias、防火墙内容、策略路由、代理配置及运行程序身份，
不以远端代理可达判断网络是否属于当前实例。发现漂移时先关闭可核实归属的 veth 再报错。

停止时先等待客体及其应用退出，再停止接管服务并清理自有网络资源。启动失败仅回收本次
创建且已确认归属的资源；网络清理不能早于客体退出，不能清空宿主公共防火墙或路由表。
接管进程在运行中退出时，原有阻断约束继续生效；本地服务可以按自身重启策略恢复，
无需以远端状态触发服务重启或改变客体生命周期。

代理地址、认证和 DNS 设置在下一次启动或重启时生效。已有运行实例保留启动时的配置，
不在读取新配置时中途切换出口。此策略只作用于子系统运行网络，不改变构建、下载阶段网络。

## 6. 实现边界与验收

Rust 核心维护配置语义、资源身份、准备/恢复/清理顺序和错误上下文；具体透明接管后端
独立封装。首版明确依赖宿主 Xray、nftables 和网络管理能力，不写死当前开发机的
发行版、网卡、网段、networkd/NetworkManager 状态或 Xray 安装路径。
新增错误、状态输出和代码注释使用中英文，不把代理凭据写入诊断。

验收需要覆盖实际数据路径，不能只检查生成参数：

- 不设置客体代理环境变量，任意目标 TCP 连接均到达配置的代理。
- SOCKS5/HTTP CONNECT 代理无监听、拒绝认证、超时或中途断开时，连接失败且没有直连。
- 远端离线时仍能启动子系统；远端恢复后新连接自然恢复，不需要重新探测或重启客体。
- 本地接管进程退出后，IPv4/IPv6 连接均不能沿普通转发路径离开。
- 常规 DNS 经同一固定代理转发；阻断 DNS 上游后不出现其他解析路径。
- SOCKS5 UDP/QUIC 经协商的 relay 转发；UDP ASSOCIATE 被拒绝或 relay 不可达时不能直连。
- 普通 HTTP CONNECT 的 UDP/QUIC 明确失败，不能因协议不支持而绕过代理。
- 多子系统地址与规则互不混淆，daemon 重启和客体重启不清除仍在使用的阻断约束。
- 宿主同时运行既有 Xray/dae 时，确认代理出口排除约定生效且没有二次接管循环。

当前单元与注入后端测试覆盖配置校验、凭据脱敏和快照冻结。真实 Xray+nftables 数据路径在
可丢弃的 user/mount/network/PID 命名空间中验证，受控 SOCKS5/HTTP 服务记录目标请求，
目标地址上的直连哨兵监听确认没有旁路。已覆盖 IPv4/IPv6 TCP、SOCKS5 UDP、HTTP UDP 阻断、
UDP/TCP DNS 的 A/AAAA/TXT/HTTPS 查询、上游断开、本地接管退出及保守清理。
上述验收矩阵中的完整 nspawn 引导、systemd 服务恢复、认证拒绝/超时、UDP relay 拒绝和现有
宿主 Xray/dae 集成仍需专用环境补充验收；不将生成配置测试等同于完整部署验证。

参考：[systemd-nspawn 网络选项](https://www.freedesktop.org/software/systemd/man/latest/systemd-nspawn.html)、
[Xray 透明入站](https://github.com/XTLS/Xray-docs-next/blob/main/docs/config/inbounds/tunnel.md)、
[Xray 路由](https://github.com/XTLS/Xray-docs-next/blob/main/docs/config/routing.md)。
