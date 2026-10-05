# RStarSleep 工作目录

RStarSleep 的工作目录属于整个管理体系，不依附任何被管理系统的 `/etc`。
在 Btrfs 顶层（subvolid=5）创建专用子卷 `@rstarsleep`，建议运行时挂载到
`/rstarsleep`。存储路径与挂载路径指向同一份数据；初始化接口不执行挂载。

## 目录树

```text
Btrfs 顶层/
└── @rstarsleep/                         # 管理器专用子卷
    ├── config/
    │   └── rstarsleep.toml              # 全局配置
    └── systems/
        └── <系统标识>/                  # 系统身份不随角色变更
            ├── config/                 # 当前可编辑配置
            │   ├── build/
            │   │   └── config.toml     # 构建配置，当前仅读取 packages
            │   └── subsystem/
            │       └── config.toml     # 子系统运行配置，暂留模板
            ├── versions/
            │   └── <版本标识>/
            │       ├── config/         # 构建时配置留档，结构同当前 config/
            │       └── root/           # 该版本系统根，由构建模块创建子卷
            ├── cache/
            │   └── <版本标识>/
            │       └── <流程标识>/     # 各版本、各流程构建缓存
            └── persistent/             # 此系统跨版本共享的持久化目录和文件
```

初始化管理器时只创建 `@rstarsleep` 子卷、`config/`、`systems/` 和全局配置。
初始化一个被管理系统时创建其当前配置、版本、缓存和持久化目录及默认配置。
这些系统目录目前是普通目录；具体版本根、阶段缓存是否使用子卷由对应功能实现。
初始化不会生成系统版本、配置留档或修改主/子系统角色，也不会覆盖已有配置。
子卷创建失败时返回错误，不使用普通目录代替；既有 `@rstarsleep` 必须为真实子卷。

## 系统归属与角色

任意被管理系统都可以由全局配置指定为主系统。全局配置单独指定子系统名单，
其余系统仅保留为被管理对象。子系统运行配置属于对应系统，只有它被显式指定为
子系统时才应用，包括向当前主系统导出应用入口。

角色变更不移动目录。移出子系统名单时，后续运行模块负责撤销已建立的集成。
持久化数据默认跨同一系统的版本共享，删除版本不应删除它；其快照与回滚策略
由持久化功能实现。缓存独立于已完成版本，可以按版本和构建流程清理。

当前配置用于下一次构建。后续构建模块在开始构建时保存配置留档，并使用留档
完成构建，避免期间编辑当前配置影响版本归属。既有版本留档不应覆盖。
全局角色配置不归属于某个系统版本。

## 默认配置

全局配置 `config/rstarsleep.toml`：

```toml
schema_version = 1
# Optional main system ID; omit this setting until a main system is selected.
# 可选的主系统标识；选定主系统前省略此项。
# main_system = "desktop"
# Only explicitly listed systems receive the subsystem role.
# 只有显式列出的系统才具有子系统角色。
subsystems = []
```

未指定主系统时，省略 `main_system`。该文件目前仅生成，不实现角色配置读取。

系统构建配置 `systems/<系统标识>/config/build/config.toml`：

```toml
schema_version = 1
packages = []
```

软件包配置示例：

```toml
schema_version = 1
packages = ["linux", "systemd", "vim"]
```

系统运行配置 `systems/<系统标识>/config/subsystem/config.toml` 目前仅包含
`schema_version = 1` 和说明注释。其字段随子系统运行功能确定。

## 当前接口

仓库内的 `src/config/` 是 Rust 源码模块，由 `src/lib.rs` 导出为
`rstarsleep::config`，与运行时工作目录内的 `config/` 区分。

源码注释使用英文在上、中文在下的对应段落。模块使用 `//!`，类型、字段和接口
使用 `///`，关键实现理由使用 `//`。所有函数（包括私有辅助函数、trait 声明及其
实现、测试函数）都必须写明函数作用，并在 `# Arguments` 章节逐项解释参数。
方法还需解释 `self`，未使用的参数需注明，无参数函数显式写明“无参数”。参数名称
应与签名一致，并说明路径、标识等参数的具体含义和约束。

公开接口另需说明前置条件、读写行为及错误条件；Rustdoc 使用标准的 `# Errors`
章节。默认配置模板和本文的代码示例采用相同的双语注释顺序。

```rust,no_run
use rstarsleep::config::{ConfigStore, initialize};

# /// Demonstrate workspace initialization and package configuration access.
# /// 演示工作目录初始化和软件包配置访问。
# ///
# /// # Arguments
# ///
# /// No parameters.
# /// 无参数。
# fn main() -> Result<(), Box<dyn std::error::Error>> {
// Use the mounted Btrfs top level with permission to create subvolumes and write files.
// 使用已挂载的 Btrfs 顶层；调用者应具有创建子卷、写文件的权限。
let store = initialize("/mnt/btrfs")?;
store.initialize_system("desktop")?;

// The runtime mount point can also be used directly for reading.
// 挂载后的入口也可直接用于读取。
let store = ConfigStore::new("/rstarsleep")?;
let config = store.read_packages("desktop")?;
assert!(config.packages.is_empty());

// Read a version's package configuration after the build module creates its record.
// 后续构建模块创建留档后，可读取某个版本的软件包配置。
let recorded = store.read_version_packages("desktop", "v1")?;
let cache = store.build_cache_path("desktop", "v1", "packages")?;
# Ok(())
# }
```

读取接口只解释 `packages`，保留列表顺序和重复项，不查询软件包仓库或安装软件。
其他构建字段暂不解释；全局配置和子系统运行配置也不会被软件包接口读取。
缺失文件、缺失 `packages`、错误类型、无效 TOML 或空白包名均返回错误，不静默
替换成空列表。错误包含相关路径或校验原因。`schema_version` 目前作为格式标记
保留，其兼容性策略在扩展配置读取时实现。

系统、版本和流程标识必须以 ASCII 字母或数字开头，其余字符只能使用字母、数字、
点、下划线和连字符，避免标识被当成路径使用。

## 命令行

```sh
# Show the init subcommand's argument requirements and usage.
# 查看 init 子命令的参数要求和使用方式。
cargo run -- init --help

# /mnt/btrfs must be the mounted Btrfs top level; the command requires write permission.
# /mnt/btrfs 必须是已挂载的 Btrfs 顶层，命令需要相应写权限。
cargo run -- init /mnt/btrfs

# Access the created subvolume directly or use the mounted /rstarsleep workspace.
# 可直接访问已创建子卷，也可使用已挂载的 /rstarsleep。
cargo run -- init-system /mnt/btrfs/@rstarsleep desktop
cargo run -- packages /mnt/btrfs/@rstarsleep desktop
```

本模块不自动编辑 `/etc/fstab`、执行挂载或导出应用入口。

`init` 子命令直接调用 `config::initialize()`。目录参数缺失或过多时显示具体错误和
子命令用法；初始化失败时输出错误原因并返回非零退出码。

命令分派位于 `src/cli/mod.rs`，初始化命令的参数处理和执行位于 `src/cli/init.rs`。
其他子命令也使用独立模块；`src/main.rs` 仅保留进程入口、错误输出和退出码处理。
