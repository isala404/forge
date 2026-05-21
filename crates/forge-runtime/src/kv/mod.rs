//! PostgreSQL-backed key-value store for framework internals.

mod store;

pub use store::KvStore;

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use forge_core::error::Result;
use forge_core::function::KvHandle;

impl KvHandle for KvStore {
    fn get<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>>> + Send + 'a>> {
        Box::pin(self.get(key))
    }

    fn set<'a>(
        &'a self,
        key: &'a str,
        value: &'a [u8],
        ttl: Option<Duration>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(self.set(key, value, ttl))
    }

    fn set_if_absent<'a>(
        &'a self,
        key: &'a str,
        value: &'a [u8],
        ttl: Option<Duration>,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(self.set_if_absent(key, value, ttl))
    }

    fn delete<'a>(
        &'a self,
        key: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(self.delete(key))
    }

    fn increment<'a>(
        &'a self,
        key: &'a str,
        delta: i64,
        ttl: Option<Duration>,
    ) -> Pin<Box<dyn Future<Output = Result<i64>> + Send + 'a>> {
        Box::pin(self.increment(key, delta, ttl))
    }
}
