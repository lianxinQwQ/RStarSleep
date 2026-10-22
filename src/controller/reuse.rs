//! Time-bounded execution receipts, independent of maintenance compatibility.
//! 独立于维护兼容性的限时执行凭据。
use super::{BuildContext, Error, Result, cache::Cache};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// The original successful execution time is preserved through cache hits.
/// 缓存命中始终保留最初成功执行的时间。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Receipt {
    /// Original successful execution time in Unix seconds.
    /// 最初成功执行的 Unix 秒数。
    pub executed_at_unix: u64,
    /// Effective input fingerprint including upstream content identity.
    /// 包含上游内容标识的生效输入指纹。
    pub inputs_sha256: String,
}

impl Receipt {
    /// Record a completed execution without inheriting a donor's age.
    /// 记录已完成执行，不继承来源的年龄。
    /// # Arguments
    /// - `inputs_sha256`: Effective stage input digest. 生效阶段输入摘要。
    pub(super) fn executed(inputs_sha256: String) -> Result<Self> {
        Ok(Self {
            executed_at_unix: now()?,
            inputs_sha256,
        })
    }

    /// Require a well-formed execution input digest.
    /// 要求执行输入摘要格式正确。
    /// # Arguments
    /// - `self`: Decoded completion receipt. 已解码完成凭据。
    pub(super) fn validate(&self) -> Result<()> {
        if self.inputs_sha256.len() != 64
            || !self.inputs_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Invalid(
                "缓存执行输入摘要无效 / Invalid cache execution-input digest".into(),
            ));
        }
        Ok(())
    }

    /// Reject disabled, expired, overflowing and future-dated lifetimes.
    /// 拒绝禁用、过期、溢出或来自未来的有效期。
    /// # Arguments
    /// - `self`: Original execution receipt. 原执行凭据。
    /// - `minutes`: Configured maximum lifetime. 配置的最长分钟数。
    /// - `current`: Current Unix seconds. 当前 Unix 秒数。
    fn fresh_at(&self, minutes: u64, current: u64) -> bool {
        minutes.checked_mul(60).is_some_and(|limit| {
            limit > 0
                && current
                    .checked_sub(self.executed_at_unix)
                    .is_some_and(|age| age < limit)
        })
    }
}

/// Read wall time for durable cross-process lifetimes.
/// 读取墙钟时间以持久化跨进程有效期。
/// # Arguments
/// No parameters. 无参数。
pub(super) fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|e| Error::Invalid(format!("系统时间无效 / Invalid system time: {e}")))
}

/// Require matching plugin content and a still-fresh execution receipt.
/// 要求插件内容一致且执行凭据仍有效。
/// # Arguments
/// - `cache`: Validated previous stage. 经校验的过往阶段。
/// - `minutes`: Maximum execution age. 最长执行年龄分钟数。
/// - `plugin`: Current frozen plugin digest. 当前冻结插件摘要。
pub(super) fn eligible(cache: Option<&Cache>, minutes: u64, plugin: &str) -> Result<bool> {
    let current = now()?;
    Ok(match cache {
        Some(cache) if cache.record.plugin_sha256 == plugin => cache
            .record
            .reuse
            .as_ref()
            .is_some_and(|receipt| receipt.fresh_at(minutes, current)),
        _ => false,
    })
}

/// Match both execution lifetime and all declared stage inputs.
/// 同时匹配执行有效期及全部声明的阶段输入。
/// # Arguments
/// - `cache`: Validated previous stage. 经校验的过往阶段。
/// - `minutes`: Maximum execution age. 最长执行年龄分钟数。
/// - `plugin`: Current frozen plugin digest. 当前冻结插件摘要。
/// - `inputs`: Current stage input digest. 当前阶段输入摘要。
pub(super) fn matches(
    cache: Option<&Cache>,
    minutes: u64,
    plugin: &str,
    inputs: &str,
) -> Result<bool> {
    Ok(eligible(cache, minutes, plugin)?
        && cache
            .unwrap()
            .record
            .reuse
            .as_ref()
            .is_some_and(|receipt| receipt.inputs_sha256 == inputs))
}

/// Fingerprint explicit stage dependencies and the actual resolved execution environment.
/// 计算阶段显式依赖和实际解析后执行环境的摘要。
/// # Arguments
/// - `stage`: Stable stage name. 稳定阶段名称。
/// - `configuration`: Stage-specific configuration digest. 阶段配置摘要。
/// - `context`: Frozen identity and resolved environment. 冻结身份及解析后的环境。
/// - `upstream`: Completed upstream content identity, when required. 按需提供的上游已完成内容身份。
/// - `extra`: Ordered package targets or inherited-content digest. 有序软件目标或继承内容摘要。
pub(super) fn inputs(
    stage: &str,
    configuration: &str,
    context: &BuildContext,
    upstream: Option<&Cache>,
    extra: &[u8],
) -> String {
    let mut digest = Sha256::new();
    field(&mut digest, b"rstarsleep-stage-reuse-v1");
    for value in [
        stage,
        configuration,
        &context.system_id,
        &context.architecture,
    ] {
        field(&mut digest, value.as_bytes());
    }
    field(
        &mut digest,
        upstream
            .map(|c| c.record.origin_version.as_bytes())
            .unwrap_or_default(),
    );
    digest.update((context.environment.len() as u64).to_le_bytes());
    for (name, value) in &context.environment {
        field(&mut digest, name.as_bytes());
        digest.update([u8::from(value.is_some())]);
        field(
            &mut digest,
            value
                .as_ref()
                .map(|v| v.as_encoded_bytes())
                .unwrap_or_default(),
        );
    }
    field(&mut digest, extra);
    format!("{:x}", digest.finalize())
}

/// Append one field with an explicit boundary.
/// 追加带明确边界的字段。
/// # Arguments
/// - `digest`: Accumulated hash state. 累积摘要状态。
/// - `value`: Raw field bytes. 字段原始字节。
fn field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exercise exact expiry, disabled reuse and backwards wall-clock movement.
    /// 验证精确过期边界、禁用复用及墙钟倒退。
    /// # Arguments
    /// No parameters. 无参数。
    #[test]
    fn age_is_bounded_and_never_extended_or_wrapped() {
        let receipt = Receipt {
            executed_at_unix: 100,
            inputs_sha256: "a".repeat(64),
        };
        assert!(!receipt.fresh_at(0, 100));
        assert!(!receipt.fresh_at(1, 99));
        assert!(receipt.fresh_at(1, 159));
        assert!(!receipt.fresh_at(1, 160));
        assert!(!receipt.fresh_at(u64::MAX, 101));
    }
}
