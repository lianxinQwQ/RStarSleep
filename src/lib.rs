//! Public library interfaces for RStarSleep.
//! RStarSleep 的公共库接口。

/// Command dispatch and independent subcommand implementations.
/// 命令分派及各个独立子命令的实现。
pub mod cli;

/// Workspace layout, initialization, and package configuration access.
/// 工作目录布局、初始化和软件包配置访问接口。
pub mod config;
