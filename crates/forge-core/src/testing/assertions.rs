//! Test assertion macros and helpers.
//!
//! Provides ergonomic assertion macros for common FORGE testing patterns.

use crate::error::ForgeError;

/// Assert that a result is Ok.
///
/// # Example
///
/// ```ignore
/// let result = some_operation();
/// assert_ok!(result);
/// assert_ok!(result, "Operation should succeed");
/// ```
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
///
/// # Example
///
/// ```ignore
/// let result: Result<(), ForgeError> = Err(ForgeError::Unauthorized);
/// assert_err!(result);
/// ```
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
///
/// # Example
///
/// ```ignore
/// let result: Result<(), ForgeError> = Err(ForgeError::NotFound("user".into()));
/// assert_err_variant!(result, ForgeError::NotFound(_));
/// ```
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
///
/// # Example
///
/// ```ignore
/// assert_job_dispatched!(ctx, "send_email");
/// assert_job_dispatched!(ctx, "send_email", |args| args["to"] == "test@example.com");
/// ```
#[macro_export]
macro_rules! assert_job_dispatched {
    ($ctx:expr, $job_type:expr) => {
        $ctx.job_dispatch().assert_dispatched($job_type);
    };
    ($ctx:expr, $job_type:expr, $predicate:expr) => {
        $ctx.job_dispatch()
            .assert_dispatched_with($job_type, $predicate);
    };
}

/// Assert that a job was not dispatched.
///
/// # Example
///
/// ```ignore
/// assert_job_not_dispatched!(ctx, "send_email");
/// ```
#[macro_export]
macro_rules! assert_job_not_dispatched {
    ($ctx:expr, $job_type:expr) => {
        $ctx.job_dispatch().assert_not_dispatched($job_type);
    };
}

/// Assert that a workflow was started.
///
/// # Example
///
/// ```ignore
/// assert_workflow_started!(ctx, "onboarding");
/// assert_workflow_started!(ctx, "onboarding", |input| input["user_id"] == "123");
/// ```
#[macro_export]
macro_rules! assert_workflow_started {
    ($ctx:expr, $workflow_name:expr) => {
        $ctx.workflow_dispatch().assert_started($workflow_name);
    };
    ($ctx:expr, $workflow_name:expr, $predicate:expr) => {
        $ctx.workflow_dispatch()
            .assert_started_with($workflow_name, $predicate);
    };
}

/// Assert that a workflow was not started.
///
/// # Example
///
/// ```ignore
/// assert_workflow_not_started!(ctx, "onboarding");
/// ```
#[macro_export]
macro_rules! assert_workflow_not_started {
    ($ctx:expr, $workflow_name:expr) => {
        $ctx.workflow_dispatch().assert_not_started($workflow_name);
    };
}

/// Assert that an HTTP call was made.
///
/// # Example
///
/// ```ignore
/// assert_http_called!(ctx, "https://api.example.com/*");
/// ```
#[macro_export]
macro_rules! assert_http_called {
    ($ctx:expr, $pattern:expr) => {
        $ctx.http().assert_called($pattern);
    };
}

/// Assert that an HTTP call was not made.
///
/// # Example
///
/// ```ignore
/// assert_http_not_called!(ctx, "https://api.example.com/*");
/// ```
#[macro_export]
macro_rules! assert_http_not_called {
    ($ctx:expr, $pattern:expr) => {
        $ctx.http().assert_not_called($pattern);
    };
}

// =========================================================================
// HELPER FUNCTIONS
// =========================================================================

/// Check if an error message contains a substring.
pub fn error_contains(error: &ForgeError, substring: &str) -> bool {
    error.to_string().contains(substring)
}

/// Check if a validation error contains a specific field.
pub fn validation_error_for_field(error: &ForgeError, field: &str) -> bool {
    match error {
        ForgeError::Validation(msg) => msg.contains(field),
        _ => false,
    }
}

/// Assert that a value matches a JSON pattern (partial matching).
///
/// The pattern only needs to contain the fields you want to verify.
/// Extra fields in the actual value are ignored.
///
/// # Example
///
/// ```ignore
/// let actual = json!({"id": 123, "name": "Test", "extra": "ignored"});
/// assert!(assert_json_matches(&actual, &json!({"id": 123})));
/// assert!(assert_json_matches(&actual, &json!({"name": "Test"})));
/// ```
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

/// Assert that an array contains an element matching a predicate.
pub fn assert_contains<T, F>(items: &[T], predicate: F) -> bool
where
    F: Fn(&T) -> bool,
{
    items.iter().any(predicate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ForgeError;

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

    #[test]
    fn test_assert_contains() {
        let items = vec![1, 2, 3, 4, 5];
        assert!(assert_contains(&items, |x| *x == 3));
        assert!(!assert_contains(&items, |x| *x == 6));
    }
}
