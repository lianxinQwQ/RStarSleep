# 账户继承与统一构建预检

账户继承由发行版插件显式注册。`[account_inheritance]` 仅开启账户操作调用；
具体配置项仍由统一的 `[config]` 元数据声明及约束负责。
未注册能力时不运行账户操作、不解释账户配置的协议语义，也不自动套用 passwd/shadow 规则；
显式配置的字段仍须经过元数据校验。

系统配置可写在 `config/build/accounts.conf` 或其他构建 INI 文件中：

```ini
[AccountInheritance]
Users=alice
Users=bob
Groups=wheel
DriftPolicy=warn
```

Users、Groups 支持重复赋值、多行值和空列表；重复名称去重。DriftPolicy 为单项，
只接受 `silent`、`warn`、`error`、`assess`，默认 `warn`。Arch 插件通过以下元数据声明接受它们：

```toml
[config]
required = []
optional = ["RSTAR_ACCOUNTINHERITANCE_USERS", "RSTAR_ACCOUNTINHERITANCE_GROUPS", "RSTAR_ACCOUNTINHERITANCE_DRIFTPOLICY"]

[config.constraints.RSTAR_ACCOUNTINHERITANCE_USERS]
kind = "free-list"

[config.constraints.RSTAR_ACCOUNTINHERITANCE_GROUPS]
kind = "free-list"

[config.constraints.RSTAR_ACCOUNTINHERITANCE_DRIFTPOLICY]
kind = "enum-single"
values = ["silent", "warn", "error", "assess"]
```

以上仅展示账户字段；完整插件还须声明其他接受的构建参数。
未声明字段、缺失必填项、违反单项或枚举约束都在任何插件调用及版本分配前报错。
控制器从校验后的参数提取标准账户请求，并负责漂移策略的执行；
账户字段同时作为普通配置环境变量及 `RSTAR_PARAMETERS_JSON` 传给插件。
插件可声明额外的 `[AccountInheritance]` 字段，控制器不解释这些扩展字段，由插件自行读取。
Arch 未把账户字段加入基础、软件缓存选择器，因此这些字段不使这两个阶段缓存失效。
不根据 UID/GID 范围猜测账户来源，不自动合并全部运行期账户修改。
账户来源根遵循构建的逐系统自动选择：主系统在运行根属于自身某版本时使用运行根，
其余情况使用该系统最新完成开机复制的版本根。没有来源时，在构建前三秒警告后跳过继承。
`--inheritance-root <绝对路径>` 仅在显式指定单个构建系统时覆盖来源，与选择管理存储的
`--root` 独立。例如 `RStarSleep build desktop --inheritance-root /mnt/source-system`
从该根内的 `/etc/passwd`、`/etc/shadow`、`/etc/group`、`/etc/gshadow` 固定 old 输入；
这些路径仍是来源系统内的绝对路径。来源无效或读取失败不回退当前系统根。

## 预检与执行时机

构建先锁定系统、冻结配置并加载插件，再完成当前配置和来源视图能够确定的检查。
`controller::preflight::Report` 汇总通用继承、插件发现、账户选择和账户覆盖风险，
按系统及诊断类型去重。检查本身不倒计时，CLI 在全部系统预检后集中展示并只等待一次三秒；
取消时尚未分配版本、清理目标缓存或执行正式软件构建。任一所选系统的配置或来源预检
发生阻断错误时，直接报错退出；多系统汇总本批预检错误，不进入三秒倒计时或构建其他系统。

账户预检运行插件的 `account_check --preflight`。它能验证来源文件及声明的用户、组，
但不能知道新软件最终创建什么身份。Arch 将未选择的来源身份列为**可能不保留**，
合并到预检警告；构建仍可能提供同名身份，不能提前认定它们一定被删除。
最终的语义比较和 UID/GID 漂移检查在新系统产物生成后进行。

执行顺序为：

```text
预检并固定 old 输入 → 一次三秒警告 → 软件构建
→ 捕获 new 输入 → CopyOnWriteBuild
→ account_prepare → account_check → Rust 策略处理 → account_apply
→ post_maintenance（Arch 的 RootPassword）→ 用户脚本 → 发布
```

配置了非空 RootPassword 时，它覆盖账户继承的 root 密码。用户脚本仍在后面执行。
CopyOnWriteBoot 或 Mount 覆盖账户路径（包括其父目录，如 `/etc/`）时，预检明确警告并
纳入同一次三秒等待；其中 `Mount=/etc/` 使用单独的即时提示，不触发三秒等待。
它可能遮蔽未来 build 自动创建的用户；运行时新增的用户也可能只保存在持久 `/etc` 中，
后续 build 从裸版本根继承时找不到这些用户。来源版本留档配置包含 `Mount=/etc/`，
且账户预检报告所选用户不存在时，错误会追加该原因提示，在版本分配和构建前退出。
该诊断不从持久目录补读账户，也不改变原有继承来源。

最佳实践是只对 `/etc` 中明确需要保留的内容做写时复制继承。必须继承整个 `/etc/`
时，使用命名 CoW 分节，并通过 `Preserve` 排除旧账户文件，保留新构建的账户结果：

```ini
[Inheritance.CopyOnWriteBuild.etc]
Path=/etc/
# Keep newly built account files instead of overwriting them from the old /etc.
# 保留新构建的账户文件，排除旧 /etc 对它们的覆盖。
Preserve=passwd
Preserve=group
Preserve=shadow
Preserve=gshadow
```

首次启动时复制可改用 `[Inheritance.CopyOnWriteBoot.etc]`，采用相同的相对例外路径。
普通继承语义保持不变，不在首次启动重跑账户插件或重新设置密码。
首次启动执行器与待办清单协议没有为账户继承增加特殊处理。

## 插件接口

```toml
[account_inheritance]
workdir = "account-inheritance"
paths = ["/etc/passwd", "/etc/group", "/etc/shadow", "/etc/gshadow"]
tools = ["python3"]
```

workdir 是 `RSTAR_PLUGIN_TMP` 下的规范相对路径，paths 是目标根内绝对文件/目录路径，
不能重叠。核心保留逻辑路径结构，捕获普通文件和目录；入口路径按所属系统根解析，
声明目录内部不跟随符号链接。来源缺项保留缺失，由插件判断是否允许。
工具是账户操作单独声明的宿主依赖，预检即检查，不要求目标系统已有这些工具。

三个入口都在宿主运行，以支持目标根生成前的检查和不同发行版的写入方式：

| 入口 | 职责 |
| --- | --- |
| `account_prepare --workspace <绝对路径>` | 读取 old/new 和 request.json，在 output 生成候选，不改目标根 |
| `account_check --workspace <绝对路径> [--preflight]` | 仅返回语义检查结果；预检只读取 old 和 request |
| `account_apply --workspace <绝对路径>` | Rust 策略允许后，按发行版规则将 output 应用到 RSTAR_ROOT |

入口支持 `.sh` 或无后缀可执行文件，遵循现有插件查找规则。提供宿主路径的
RSTAR_PLUGIN_ROOT、RSTAR_PLUGIN_TMP、RSTAR_ROOT，以及冻结配置和环境。
账户操作不挂载或提交插件共享持久目录。RSTAR_ROOT 在预检时为来源 `/`，
在构建操作时为新系统的暂存根。插件工作区包含：

```text
<workdir>/
├── old/              # 预检时固定的来源账户文件
├── new/              # 普通构建继承前的原始新系统账户文件
├── output/           # 初始为空，插件生成候选
└── request.json      # {"schema_version":1,"users":[...],"groups":[...]}
```

工作区入口为 0700，含凭据的文件不写入诊断；随插件临时会话清理。
old 输入在预检和实际拟继承之间保持固定，不在构建中途重新读取来源账户。

检查结果通过 stdout 返回 JSON，诊断使用 stderr：

```json
{
  "schema_version": 1,
  "consistent": true,
  "added_users": [],
  "added_groups": [],
  "dropped_users": [],
  "dropped_groups": [],
  "drifts": [],
  "differences": []
}
```

除 schema_version、consistent 外列表可省略。consistent 表示除允许的构建新增身份外一致。
预检还可返回 `missing_users` 列表，配合 `consistent=false` 和 `differences` 说明缺失的
所选用户；核心据此结合来源版本配置提供挂载提示，不解析发行版账户文件或匹配错误文本。
漂移项形如 `{"kind":"uid","name":"alice","old_id":1000,"new_id":2000,"new_name":"alice"}`；
kind 为 uid/gid，new_id、new_name 可为 null；编号被不同身份复用时 new_name 表示新身份。
预检中的 dropped 列表是可能舍弃项，最终检查中的列表才是实际结果。
多个事实可以同时存在，不使用互斥的单个状态码。differences 表示无法接受的其他
语义差异或无效输入，Rust 阻止发布；插件不自行倒计时或根据漂移策略退出。
正常发现漂移时插件退出 0；非零退出表示解析或执行失败。JSON 不包含密码哈希等凭据。

## Arch 语义

继承所选用户的 passwd 和 shadow 记录，所选组的 group 和 gshadow 记录。
所选用户的本地主组作为依赖一并继承。保留构建创建的其他账户、组和成员关系；
继承所选用户在目标已有组中的附加成员/管理员关系，以及所选组中目标存在用户的关系。
不会因为继承组而创建它的全部旧成员；新系统不存在的成员不会形成悬空引用。
组与 gshadow 成员关系统一。可选 shadow/gshadow 缺失时不凭空生成空数据库。

新构建已有同名用户/组时，保留其构建分配的 UID/GID；所选用户的主组按旧组名关联到
目标组编号。只有目标不存在的身份才带入来源编号。这样不改变新系统文件依赖的身份编号，
旧数据中的编号差异交给漂移策略处理，不自动修复。

按账户字段和成员集合比较，忽略记录顺序。所选记录损坏或没有主组等问题在预检失败；
所选凭据未保留、候选产生未知身份等问题阻止发布。UID/GID 变化或旧编号被其他身份复用
只报告，绝不重分配编号或 chown 数据。

subuid/subgid、SSH 密钥、sudoers、PAM、家目录和应用数据不在 Arch 首版账户协议范围内，
需要另行声明数据继承或后续扩展插件。

## 漂移策略与评估范围

- silent：继续，不显示漂移警告。
- warn：警告并继续。
- error：阻止发布最终根。
- assess：报告影响并继续，不修复。

评估只扫描 CopyOnWriteBuild、CopyOnWriteBoot、Mount 项及递归内容；已有 Mount 持久对象
优先于它的初始来源。检查属主、属组及 POSIX access/default ACL，不扫描新构建系统，
不进入其他文件系统，但遍历同一 Btrfs 文件系统的内部子卷。递归不跟随符号链接。
报告受影响对象数量和最多 100 个示例路径；读取失败明确报告评估不完整。
文件内容、数据库里的数字 ID 不解析；没有权限引用不能保证程序完全不受影响。
首次启动项评估的是当前来源视图，不保证覆盖实际首次启动前的新变化。

固定的 old 内容摘要参与 script 阶段缓存输入；账户配置和插件内容也受现有摘要约束。
assess 会读取仍可能变化的启动/Mount 来源，因此不直接复用 script 结果，基础和软件阶段
仍按原策略复用。其余策略可复用输入未变的完整 script 产物，预检仍然执行。
