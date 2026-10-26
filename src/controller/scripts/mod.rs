//! Apply frozen build scripts after software installation on an independent root.
//! 在独立系统根上，于软件安装后应用冻结构建脚本。
mod inputs;
mod runner;
mod workflow;
pub(super) use runner::run;
pub(super) use workflow::apply;

/// Validate every frozen script resource before executing build stages.
/// 执行构建阶段前验证全部冻结脚本资源。
/// # Arguments
/// - `config`: Frozen configuration root. 冻结配置根。
pub(super) fn validate(config: &std::path::Path) -> crate::controller::Result<()> {
    inputs::load(&config.join("script")).map(|_| ())
}
