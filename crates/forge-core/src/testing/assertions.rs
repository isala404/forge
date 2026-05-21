//! Test assertion macros and helpers.
//!
//! Provides ergonomic assertion macros for common FORGE testing patterns.

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

/// Assert that an error matches a specific variant (pattern matching, not substring).
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

/// Assert that an error matches a pattern, with an optional guard expression.
///
/// Prefer this over `error_contains` or substring checks — guards can inspect
/// the inner value without forcing a string round-trip.
///
/// # Example
///
/// ```ignore
/// // Match variant only
/// assert_err_matches!(result, ForgeError::NotFound(_));
///
/// // Match variant with a guard on the inner value
/// assert_err_matches!(result, ForgeError::Validation(msg) if msg.contains("email"));
///
/// // Bind the inner value in the guard
/// assert_err_matches!(result, ForgeError::InvalidArgument(msg) if msg == "bad input");
/// ```
#[macro_export]
macro_rules! assert_err_matches {
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
    ($expr:expr, $variant:pat if $guard:expr) => {
        match &$expr {
            Err($variant) if $guard => (),
            Err($variant) => panic!(
                "assertion failed: pattern {} matched but guard failed for {:?}",
                stringify!($variant),
                $expr
            ),
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
    use super::{assert_contains, assert_json_matches};
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
    fn test_assert_err_variant() {
        let result: Result<(), ForgeError> = Err(ForgeError::NotFound("user".into()));
        assert_err_variant!(result, ForgeError::NotFound(_));
    }

    #[test]
    #[should_panic(expected = "expected ForgeError::Unauthorized(_)")]
    fn test_assert_err_variant_wrong_variant() {
        let result: Result<(), ForgeError> = Err(ForgeError::NotFound("user".into()));
        assert_err_variant!(result, ForgeError::Unauthorized(_));
    }

    #[test]
    fn test_assert_err_matches_no_guard() {
        let result: Result<(), ForgeError> =
            Err(ForgeError::Validation("email is required".into()));
        assert_err_matches!(result, ForgeError::Validation(_));
    }

    #[test]
    #[allow(unused_variables)]
    fn test_assert_err_matches_with_guard() {
        let result: Result<(), ForgeError> =
            Err(ForgeError::Validation("email is required".into()));
        assert_err_matches!(result, ForgeError::Validation(msg) if msg.contains("email"));
    }

    #[test]
    #[should_panic(expected = "guard failed")]
    #[allow(unused_variables)]
    fn test_assert_err_matches_guard_fails() {
        let result: Result<(), ForgeError> =
            Err(ForgeError::Validation("email is required".into()));
        assert_err_matches!(result, ForgeError::Validation(msg) if msg.contains("password"));
    }

    #[test]
    #[should_panic(expected = "expected ForgeError::Unauthorized(_)")]
    fn test_assert_err_matches_wrong_variant() {
        let result: Result<(), ForgeError> = Err(ForgeError::NotFound("user".into()));
        assert_err_matches!(result, ForgeError::Unauthorized(_));
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
