//! Bilingual build progress kept separate from query and command output.
//! 与查询及命令输出分离的双语构建进度。
use super::Result;
use std::{fmt::Display, time::Instant};

/// Immediately identify the current system and work on the diagnostic stream.
/// 在诊断流中即时标明当前系统和正在执行的工作。
pub(crate) fn message(system: &str, chinese: impl Display, english: impl Display) {
    eprintln!("==> [RStarSleep][{system}] {chinese} / {english}");
}

/// Report a step before execution, then its actual result and elapsed time.
/// 执行前报告步骤，随后报告实际结果及耗时。
pub(super) fn run<T>(
    system: &str,
    chinese: impl Display,
    english: impl Display,
    action: impl FnOnce() -> Result<T>,
) -> Result<T> {
    message(
        system,
        format!("开始：{chinese}"),
        format!("Starting: {english}"),
    );
    let started = Instant::now();
    let result = action();
    let seconds = started.elapsed().as_secs_f64();
    let (zh, en) = if result.is_ok() {
        ("完成", "Completed")
    } else {
        ("失败", "Failed")
    };
    message(
        system,
        format!("{zh}：{chinese}（{seconds:.1} 秒）"),
        format!("{en}: {english} ({seconds:.1} s)"),
    );
    result
}

/// Describe mutating distribution operations without exposing their arguments.
/// 描述会修改系统的发行版操作，不输出其参数。
pub(super) fn operation(name: &str) -> (&'static str, &'static str) {
    match name {
        "bootstrap" => (
            "下载、验签并展开基础安装文件",
            "Download, verify and extract bootstrap files",
        ),
        "base_install" => ("安装基础系统", "Install the base system"),
        "system_init" => (
            "初始化系统身份和密钥环",
            "Initialize system identity and keyring",
        ),
        "update" => ("更新系统软件", "Update system packages"),
        "predownload_install" => (
            "安装预下载和 AUR 构建工具",
            "Install predownload and AUR build tools",
        ),
        "predownload_update" => (
            "维护预下载和 AUR 构建工具",
            "Maintain predownload and AUR build tools",
        ),
        "predownload" => (
            "预下载软件并构建 AUR 包",
            "Predownload packages and build AUR packages",
        ),
        "install" => ("安装所需软件包", "Install required packages"),
        "maintain_packages" => ("清理并维护目标软件", "Reconcile and update target packages"),
        "post_maintenance" => ("执行软件维护后行为", "Run post-maintenance behavior"),
        "remove" => ("移除软件包", "Remove packages"),
        "mark_reason" => (
            "整理软件包安装原因",
            "Reconcile package installation reasons",
        ),
        "remove_orphans" => ("清理孤儿软件包", "Remove orphan packages"),
        "mount_inheritance" => ("写入继承挂载配置", "Write inherited mount configuration"),
        _ => ("执行插件任务", "Run plugin operation"),
    }
}
