use std::str::FromStr;

/// Step execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum StepStatus {
    /// Step not yet started.
    Pending,
    /// Step currently running.
    Running,
    /// Step completed successfully.
    Completed,
    /// Step failed.
    Failed,
    /// Step compensation ran.
    Compensated,
    /// Step compensation handler ran but failed; manual remediation may be
    /// required for any side effects of the original step.
    CompensationFailed,
    /// Step was skipped.
    Skipped,
    /// Step is waiting (suspended).
    Waiting,
}

impl StepStatus {
    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Compensated => "compensated",
            Self::CompensationFailed => "compensation_failed",
            Self::Skipped => "skipped",
            Self::Waiting => "waiting",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseStepStatusError(pub String);

impl std::fmt::Display for ParseStepStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid step status: '{}'", self.0)
    }
}

impl std::error::Error for ParseStepStatusError {}

impl FromStr for StepStatus {
    type Err = ParseStepStatusError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "compensated" => Ok(Self::Compensated),
            "compensation_failed" => Ok(Self::CompensationFailed),
            "skipped" => Ok(Self::Skipped),
            "waiting" => Ok(Self::Waiting),
            _ => Err(ParseStepStatusError(s.to_string())),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn step_status_parse_roundtrips_every_variant() {
        // StepStatus is persisted to and read back from the DB (executor.rs,
        // state.rs), so as_str() and FromStr must stay inverses for every variant.
        for status in [
            StepStatus::Pending,
            StepStatus::Running,
            StepStatus::Completed,
            StepStatus::Failed,
            StepStatus::Compensated,
            StepStatus::CompensationFailed,
            StepStatus::Skipped,
            StepStatus::Waiting,
        ] {
            let s = status.as_str();
            let parsed: StepStatus = s.parse().unwrap();
            assert_eq!(parsed, status, "{s} did not round-trip");
        }
    }

    #[test]
    fn step_status_parse_rejects_unknown() {
        let err = "garbage".parse::<StepStatus>().unwrap_err();
        assert_eq!(err.0, "garbage");
        // Display must echo the bad value so logs pinpoint the typo.
        assert!(err.to_string().contains("garbage"));
    }
}
