//! Test assertion macros and helpers.

use forge_core::error::ForgeError;
use forge_core::job::JobStatus;
use forge_core::workflow::WorkflowStatus;

/// Assert that a result is Ok.
#[macro_export]
macro_rules! assert_ok {
    ($expr:expr) => {
        match &$expr {
            Ok(_) => (),
            Err(e) => panic!("assertion failed: expected Ok, got Err({:?})", e),
        }
    };
    ($expr:expr, $($arg:tt)+) => {
        match &$expr {
            Ok(_) => (),
            Err(e) => panic!("assertion failed: {}: expected Ok, got Err({:?})", format_args!($($arg)+), e),
        }
    };
}

/// Assert that a result is Err.
#[macro_export]
macro_rules! assert_err {
    ($expr:expr) => {
        match &$expr {
            Err(_) => (),
            Ok(v) => panic!("assertion failed: expected Err, got Ok({:?})", v),
        }
    };
    ($expr:expr, $($arg:tt)+) => {
        match &$expr {
            Err(_) => (),
            Ok(v) => panic!("assertion failed: {}: expected Err, got Ok({:?})", format_args!($($arg)+), v),
        }
    };
}

/// Assert that an error matches a specific variant.
#[macro_export]
macro_rules! assert_err_variant {
    ($expr:expr, $variant:pat) => {
        match &$expr {
            Err($variant) => (),
            Err(e) => panic!(
                "assertion failed: expected {}, got {:?}",
                stringify!($variant),
                e
            ),
            Ok(v) => panic!(
                "assertion failed: expected Err({}), got Ok({:?})",
                stringify!($variant),
                v
            ),
        }
    };
}

/// Assert that a job was dispatched.
#[macro_export]
macro_rules! assert_job_dispatched {
    ($ctx:expr, $job_type:expr) => {
        assert!(
            $ctx.job_dispatched($job_type),
            "assertion failed: job '{}' was not dispatched",
            $job_type
        );
    };
    ($ctx:expr, $job_type:expr, $predicate:expr) => {
        let jobs = $ctx
            .dispatched_jobs()
            .iter()
            .filter(|j| j.job_type == $job_type)
            .collect::<Vec<_>>();
        assert!(
            jobs.iter().any(|j| $predicate(&j.input)),
            "assertion failed: no job '{}' matching predicate was dispatched",
            $job_type
        );
    };
}

/// Assert that a workflow was started.
#[macro_export]
macro_rules! assert_workflow_started {
    ($ctx:expr, $workflow_name:expr) => {
        assert!(
            $ctx.started_workflows()
                .iter()
                .any(|w| w.workflow_name == $workflow_name),
            "assertion failed: workflow '{}' was not started",
            $workflow_name
        );
    };
}

/// Check if an error message contains a substring.
pub fn error_contains(error: &ForgeError, substring: &str) -> bool {
    error.to_string().contains(substring)
}

/// Check if a validation error contains specific field.
pub fn validation_error_for_field(error: &ForgeError, field: &str) -> bool {
    match error {
        ForgeError::Validation(msg) => msg.contains(field),
        _ => false,
    }
}

/// Assert helper for job status.
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

/// Assert that a value matches a JSON pattern.
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
    fn test_assert_ok_macro() {
        let result: Result<i32, String> = Ok(42);
        assert_ok!(result);
    }

    #[test]
    #[should_panic(expected = "expected Ok")]
    fn test_assert_ok_macro_fails() {
        let result: Result<i32, String> = Err("error".to_string());
        assert_ok!(result);
    }

    #[test]
    fn test_assert_err_macro() {
        let result: Result<i32, String> = Err("error".to_string());
        assert_err!(result);
    }

    #[test]
    #[should_panic(expected = "expected Err")]
    fn test_assert_err_macro_fails() {
        let result: Result<i32, String> = Ok(42);
        assert_err!(result);
    }

    #[test]
    fn test_error_contains() {
        let error = ForgeError::Validation("email is required".to_string());
        assert!(error_contains(&error, "email"));
        assert!(error_contains(&error, "required"));
        assert!(!error_contains(&error, "password"));
    }

    #[test]
    fn test_validation_error_for_field() {
        let error = ForgeError::Validation("email: is invalid".to_string());
        assert!(validation_error_for_field(&error, "email"));
        assert!(!validation_error_for_field(&error, "password"));

        let other_error = ForgeError::Internal("internal error".to_string());
        assert!(!validation_error_for_field(&other_error, "email"));
    }

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
