//! Apply frozen build scripts after software installation on an independent root.
//! 在独立系统根上，于软件安装后应用冻结构建脚本。
mod inputs;
mod runner;
mod workflow;
pub(super) use runner::run;
pub(super) use workflow::apply;
