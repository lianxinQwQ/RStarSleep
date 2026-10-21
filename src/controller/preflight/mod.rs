//! Shared pre-build diagnostics. Checks never print countdowns or sleep.
pub(super) mod inheritance;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Diagnostic {
    pub system_id: String,
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Default)]
pub(super) struct Report {
    diagnostics: Vec<Diagnostic>,
}

impl Report {
    pub fn warn(&mut self, system_id: &str, code: &'static str, message: impl Into<String>) {
        let diagnostic = Diagnostic {
            system_id: system_id.into(),
            code,
            message: message.into(),
        };
        if !self.diagnostics.contains(&diagnostic) {
            self.diagnostics.push(diagnostic);
        }
    }

    pub fn messages(&self) -> Vec<String> {
        self.diagnostics
            .iter()
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
}
