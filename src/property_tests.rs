#![allow(clippy::panic, clippy::unwrap_used)]

use crate::blob::sign::{self, Method};
use crate::config::ForgeConfig;
use crate::schedule::cron::Cron;
use crate::{
    Bytes, Cursor, DequeueOpts, EnqueueOpts, Forge, ForgeError, JobId, Limit, NackOpts,
    TraceContext,
};
use proptest::prelude::*;
use std::time::{Duration, UNIX_EPOCH};

const MEMORY: &str = "[forge]\nmode = \"memory\"\nenvironment = \"test\"\n";

proptest! {
    #[test]
    fn arbitrary_config_text_never_panics(input in any::<String>()) {
        let result = ForgeConfig::from_toml_str(&input);
        if let Err(error) = result {
            prop_assert!(!error.is_retryable());
        }
    }

    #[test]
    fn opaque_cursor_roundtrips_without_decoding(input in any::<String>()) {
        let cursor = Cursor::from_token(input.clone());
        prop_assert_eq!(cursor.token(), input);
    }

    #[test]
    fn malformed_signatures_are_safe(
        signature in any::<String>(),
        key in any::<String>(),
        namespace in "[A-Za-z0-9._-]{0,32}",
    ) {
        let verified = sign::verify(
            b"property-secret",
            &namespace,
            Method::Get,
            &key,
            4_000_000_000,
            0,
            &signature,
        );
        if verified {
            let expected = sign::sign(
                b"property-secret",
                &namespace,
                Method::Get,
                &key,
                4_000_000_000,
                0,
            ).expect("HMAC accepts every key length");
            prop_assert_eq!(signature.to_ascii_lowercase(), expected);
        }
    }

    #[test]
    fn arbitrary_blob_keys_are_bounded(input in any::<String>()) {
        let result = crate::blob::common::check_key(&input);
        match result {
            Ok(()) => prop_assert!(!input.is_empty() && input.len() <= crate::blob::MAX_KEY_BYTES),
            Err(error) => prop_assert!(!error.is_retryable()),
        }
    }

    #[test]
    fn arbitrary_cron_is_never_transient(input in any::<String>()) {
        if let Err(error) = Cron::parse(&input) {
            prop_assert!(!error.is_retryable());
            prop_assert_eq!(error.code(), "INVALID");
        }
    }

    #[test]
    fn arbitrary_trace_payload_headers_are_never_transient(
        traceparent in any::<String>(),
        tracestate in prop::option::of(any::<String>()),
        baggage in prop::option::of(any::<String>()),
    ) {
        if let Err(error) = TraceContext::from_headers(traceparent, tracestate, baggage, &[]) {
            prop_assert!(!error.is_retryable());
            prop_assert_eq!(error.code(), "INVALID");
        }
    }

    #[test]
    fn error_context_conversion_preserves_classification(
        operation in any::<String>(),
        message in any::<String>(),
    ) {
        let error = ForgeError::invalid(message).with_context(operation.clone(), Some("memory".to_string()));
        prop_assert_eq!(error.code(), "INVALID");
        prop_assert!(!error.is_retryable());
        prop_assert_eq!(error.operation(), Some(operation.as_str()));
        prop_assert_eq!(error.backend_id(), Some("memory"));
    }

    #[test]
    fn distinct_namespaces_never_encode_the_same_scope(
        left in "[A-Za-z0-9._-]{1,32}",
        right in "[A-Za-z0-9._-]{1,32}",
        key in ".{0,64}",
    ) {
        prop_assume!(left != right);
        prop_assert_ne!(
            crate::util::namespaced(&left, &key),
            crate::util::namespaced(&right, &key),
        );
    }

    #[test]
    fn deterministic_ids_attempts_and_lease_fences_hold(retries in 1u32..20) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let forge = Forge::init_memory_for_testing(MEMORY, UNIX_EPOCH, 9).await.unwrap();
            let id = JobId::new();
            let options = EnqueueOpts::new().with_job_id(id).with_max_attempts(retries + 1);
            prop_assert_eq!(forge.queue().enqueue("jobs", Bytes::new(), options.clone()).await.unwrap(), id);
            prop_assert_eq!(forge.queue().enqueue("jobs", Bytes::new(), options).await.unwrap(), id);

            let dequeue = DequeueOpts::new().with_wait(Duration::ZERO);
            let mut last_attempt = 0;
            for _ in 0..retries {
                let job = forge.queue().dequeue("jobs", dequeue.clone()).await.unwrap().unwrap();
                prop_assert!(job.attempt > last_attempt);
                last_attempt = job.attempt;
                forge.queue().nack(&job, NackOpts::retry_in(Duration::ZERO)).await.unwrap();
                let stale = forge.queue().ack(&job).await.unwrap_err();
                prop_assert_eq!(stale.code(), "PRECONDITION");
            }
            Ok(())
        })?;
    }

    #[test]
    fn rate_limit_never_oversubscribes(max in 1u32..50, checks in 1u32..100) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        runtime.block_on(async move {
            let forge = Forge::init_memory_for_testing(MEMORY, UNIX_EPOCH, 11).await.unwrap();
            let limit = Limit::per_duration(max, Duration::from_secs(3600));
            let mut allowed = 0;
            let mut previous = max;
            for _ in 0..checks {
                let decision = forge.ratelimit().check("api", "subject", limit).await.unwrap();
                allowed += u32::from(decision.allowed);
                prop_assert!(decision.remaining <= previous);
                previous = decision.remaining;
            }
            prop_assert_eq!(allowed, checks.min(max));
            Ok(())
        })?;
    }
}
