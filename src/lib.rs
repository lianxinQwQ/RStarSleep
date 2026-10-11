//! Public library interfaces for RStarSleep.
//! RStarSleep 的公共库接口。

/// Command dispatch and independent subcommand implementations.
/// 命令分派及各个独立子命令的实现。
pub mod cli;

/// Workspace layout, initialization, and package configuration access.
/// 工作目录布局、初始化和软件包配置访问接口。
pub mod config;

/// Build orchestration infrastructure, including distribution plugin loading.
/// 构建编排基础设施，包括发行版插件加载。
pub mod controller;

/// Bootstrap and maintenance of the protected base system.
/// 受保护基础系统的安装和维护。
pub mod base_system;

/// Configured package reconciliation and orphan cleanup.
/// 配置包协调和孤儿清理。
pub mod software;

/// Persistent subsystem preparation, privileged lifecycle and application integration.
/// 持久子系统准备、特权生命周期与应用集成。
pub mod subsystem;
