use crate::effects::Effect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Ask,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Low => "LOW",
            Self::Medium => "MEDIUM",
            Self::High => "HIGH",
        })
    }
}

pub fn risk_level(effects: &[Effect]) -> RiskLevel {
    if effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::PrivilegeChange
                | Effect::SensitivePathWrite(_)
                | Effect::UntrustedPipelineExecution(_)
        )
    }) {
        RiskLevel::High
    } else if effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::FilesystemDelete(_)
                | Effect::FilesystemWrite(_)
                | Effect::SensitivePathRead(_)
                | Effect::NetworkAccess
                | Effect::PackageInstallation(_)
                | Effect::ProcessKill(_)
        )
    }) {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    }
}

impl std::fmt::Display for PolicyDecision {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Allow => "ALLOW",
            Self::Ask => "ASK",
            Self::Block => "BLOCK",
        })
    }
}

pub fn evaluate(effects: &[Effect]) -> PolicyDecision {
    if effects
        .iter()
        .any(|effect| matches!(effect, Effect::PrivilegeChange))
    {
        return PolicyDecision::Block;
    }

    if effects
        .iter()
        .any(|effect| matches!(effect, Effect::SensitivePathWrite(_)))
    {
        return PolicyDecision::Block;
    }

    if effects.iter().any(|effect| {
        matches!(
            effect,
            Effect::FilesystemDelete(_)
                | Effect::FilesystemWrite(_)
                | Effect::SensitivePathRead(_)
                | Effect::NetworkAccess
                | Effect::UntrustedPipelineExecution(_)
                | Effect::PackageInstallation(_)
                | Effect::ProcessKill(_)
        )
    }) {
        return PolicyDecision::Ask;
    }

    PolicyDecision::Allow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_privilege_changes() {
        assert_eq!(evaluate(&[Effect::PrivilegeChange]), PolicyDecision::Block);
    }

    #[test]
    fn classifies_effect_risk_levels() {
        assert_eq!(risk_level(&[Effect::ProcessCreation]), RiskLevel::Low);
        assert_eq!(risk_level(&[Effect::NetworkAccess]), RiskLevel::Medium);
        assert_eq!(risk_level(&[Effect::PrivilegeChange]), RiskLevel::High);
    }

    #[test]
    fn asks_for_destructive_or_external_effects() {
        assert_eq!(
            evaluate(&[Effect::FilesystemDelete("build".into())]),
            PolicyDecision::Ask
        );
        assert_eq!(evaluate(&[Effect::NetworkAccess]), PolicyDecision::Ask);
    }

    #[test]
    fn allows_process_creation() {
        assert_eq!(evaluate(&[Effect::ProcessCreation]), PolicyDecision::Allow);
    }

    #[test]
    fn blocks_sensitive_path_writes_and_asks_for_reads() {
        assert_eq!(
            evaluate(&[Effect::SensitivePathWrite("/etc/hosts".into())]),
            PolicyDecision::Block
        );
        assert_eq!(
            evaluate(&[Effect::SensitivePathRead("/root/.ssh/config".into())]),
            PolicyDecision::Ask
        );
    }

    #[test]
    fn classifies_untrusted_pipeline_and_package_actions() {
        assert_eq!(
            risk_level(&[Effect::UntrustedPipelineExecution("bash".into())]),
            RiskLevel::High
        );
        assert_eq!(
            evaluate(&[Effect::UntrustedPipelineExecution("bash".into())]),
            PolicyDecision::Ask
        );
        assert_eq!(
            risk_level(&[Effect::PackageInstallation("curl".into())]),
            RiskLevel::Medium
        );
        assert_eq!(
            evaluate(&[Effect::PackageInstallation("curl".into())]),
            PolicyDecision::Ask
        );
        assert_eq!(
            risk_level(&[Effect::ProcessKill("123".into())]),
            RiskLevel::Medium
        );
        assert_eq!(
            evaluate(&[Effect::ProcessKill("123".into())]),
            PolicyDecision::Ask
        );
    }
}
