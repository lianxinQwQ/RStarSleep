//! Shared pre-build diagnostics. Checks never print countdowns or sleep.
//! 共享构建前诊断；检查本身不输出倒计时或等待。
pub(super) mod inheritance;
pub(super) mod source_root;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Diagnostic {
    pub system_id: String,
    pub code: &'static str,
    pub message: String,
    pub requires_wait: bool,
}

#[derive(Debug, Default)]
pub(super) struct Report {
    diagnostics: Vec<Diagnostic>,
}

impl Report {
    pub fn warn(&mut self, system_id: &str, code: &'static str, message: impl Into<String>) {
        self.add(system_id, code, message.into(), true);
    }

    /// Print advice immediately without adding it to the warning countdown.
    /// 立即输出建议，不将其计入警告倒计时。
    pub fn info(&mut self, system_id: &str, code: &'static str, message: impl Into<String>) {
        self.add(system_id, code, message.into(), false);
    }

    fn add(&mut self, system_id: &str, code: &'static str, message: String, requires_wait: bool) {
        let diagnostic = Diagnostic {
            system_id: system_id.into(),
            code,
            message,
            requires_wait,
        };
        if !self.diagnostics.contains(&diagnostic) {
            if !requires_wait {
                super::logging::diagnostic(format!("[{system_id}:{code}] {}", diagnostic.message));
            }
            self.diagnostics.push(diagnostic);
        }
    }

    pub fn messages(&self) -> Vec<String> {
        self.diagnostics
            .iter()
            .map(|d| format!("[{}:{}] {}", d.system_id, d.code, d.message))
            .collect()
    }

    /// Return only warnings that require the caller's existing countdown.
    /// 仅返回需要调用方执行既有倒计时的警告。
    pub fn warnings(&self) -> Vec<String> {
        self.diagnostics
            .iter()
            .filter(|d| d.requires_wait)
            .map(|d| format!("[{}:{}] {}", d.system_id, d.code, d.message))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deduplicates_findings_but_keeps_system_identity() {
        let mut report = Report::default();
        report.warn("one", "accounts", "risk");
        report.warn("one", "accounts", "risk");
        report.warn("two", "accounts", "risk");
        assert_eq!(report.messages().len(), 2);
    }

    /// Advice stays visible to configuration checks without requesting a wait.
    /// 建议对配置检查保持可见，但不要求等待。
    #[test]
    fn advice_does_not_enter_the_warning_countdown() {
        let mut report = Report::default();
        report.info("one", "mount-etc", "提示 / Advice");
        report.info("one", "mount-etc", "提示 / Advice");
        assert_eq!(report.messages().len(), 1);
        assert!(report.warnings().is_empty());
        report.warn("one", "accounts", "警告 / Warning");
        assert_eq!(report.messages().len(), 2);
        assert_eq!(report.warnings().len(), 1);
        assert!(report.warnings()[0].contains("accounts"));
    }
}
