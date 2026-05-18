//! Test assertion macros and helpers for forge-runtime integration tests.
//!
//! Core assertion macros (`assert_ok!`, `assert_err!`, `assert_err_variant!`,
//! `assert_err_matches!`, `assert_job_dispatched!`, `assert_workflow_started!`)
//! are defined in `forge-core` and re-exported via `forge_core::testing`.
//! This module adds runtime-specific helpers that operate on `JobStatus` and
//! `WorkflowStatus`.

use forge_core::job::JobStatus;
use forge_core::workflow::WorkflowStatus;

/// Assert helper for job status.
#[allow(clippy::panic)]
pub fn assert_job_status(actual: Option<JobStatus>, expected: JobStatus) {
    match actual {
        Some(status) => assert_eq!(
            status, expected,
            "expected job status {:?}, got {:?}",
            expected, status
        ),
        None => panic!("expected job status {:?}, but job not found", expected),
    }
}

/// Assert helper for workflow status.
#[allow(clippy::panic)]
pub fn assert_workflow_status(actual: Option<WorkflowStatus>, expected: WorkflowStatus) {
    match actual {
        Some(status) => assert_eq!(
            status, expected,
            "expected workflow status {:?}, got {:?}",
            expected, status
        ),
        None => panic!(
            "expected workflow status {:?}, but workflow not found",
            expected
        ),
    }
}

/// Assert that a value matches a JSON pattern (partial matching).
///
/// The pattern only needs to contain the fields you want to verify.
/// Extra fields in the actual value are ignored.
pub fn assert_json_matches(actual: &serde_json::Value, pattern: &serde_json::Value) -> bool {
    match (actual, pattern) {
        (serde_json::Value::Object(a), serde_json::Value::Object(p)) => {
            for (key, expected_value) in p {
                match a.get(key) {
                    Some(actual_value) => {
                        if !assert_json_matches(actual_value, expected_value) {
                            return false;
                        }
                    }
                    None => return false,
                }
            }
            true
        }
        (serde_json::Value::Array(a), serde_json::Value::Array(p)) => {
            if a.len() != p.len() {
                return false;
            }
            a.iter()
                .zip(p.iter())
                .all(|(a, p)| assert_json_matches(a, p))
        }
        (a, p) => a == p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assert_job_status() {
        assert_job_status(Some(JobStatus::Completed), JobStatus::Completed);
    }

    #[test]
    #[should_panic(expected = "expected job status")]
    fn test_assert_job_status_mismatch() {
        assert_job_status(Some(JobStatus::Pending), JobStatus::Completed);
    }

    #[test]
    #[should_panic(expected = "job not found")]
    fn test_assert_job_status_not_found() {
        assert_job_status(None, JobStatus::Completed);
    }

    #[test]
    fn test_assert_job_status_cancelled() {
        assert_job_status(Some(JobStatus::Cancelled), JobStatus::Cancelled);
    }

    #[test]
    fn test_assert_json_matches() {
        let actual = serde_json::json!({
            "id": 123,
            "name": "Test",
            "nested": {
                "foo": "bar"
            }
        });

        // Partial match
        assert!(assert_json_matches(
            &actual,
            &serde_json::json!({"id": 123})
        ));
        assert!(assert_json_matches(
            &actual,
            &serde_json::json!({"name": "Test"})
        ));
        assert!(assert_json_matches(
            &actual,
            &serde_json::json!({"nested": {"foo": "bar"}})
        ));

        // Non-match
        assert!(!assert_json_matches(
            &actual,
            &serde_json::json!({"id": 456})
        ));
        assert!(!assert_json_matches(
            &actual,
            &serde_json::json!({"missing": true})
        ));
    }

    #[test]
    fn test_assert_json_matches_arrays() {
        let actual = serde_json::json!([1, 2, 3]);
        assert!(assert_json_matches(&actual, &serde_json::json!([1, 2, 3])));
        assert!(!assert_json_matches(&actual, &serde_json::json!([1, 2])));
        assert!(!assert_json_matches(&actual, &serde_json::json!([1, 2, 4])));
    }
}
