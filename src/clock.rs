use rand_core::{OsRng, RngCore};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
    fn elapsed(&self) -> Duration;
}

pub(crate) struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub(crate) fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn elapsed(&self) -> Duration {
        self.origin.elapsed()
    }
}

pub(crate) struct ManualClock {
    start: SystemTime,
    elapsed_ms: AtomicU64,
}

impl ManualClock {
    pub(crate) fn new(start: SystemTime) -> Self {
        Self {
            start,
            elapsed_ms: AtomicU64::new(0),
        }
    }

    pub(crate) fn advance(&self, duration: Duration) {
        let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let _ = self
            .elapsed_ms
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(current.saturating_add(millis))
            });
    }

    fn offset(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms.load(Ordering::SeqCst))
    }
}

impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        self.start + self.offset()
    }

    fn elapsed(&self) -> Duration {
        self.offset()
    }
}

pub(crate) trait RandomSource: Send + Sync {
    fn fill(&self, bytes: &mut [u8]);
}

pub(crate) struct SystemRandom;

impl RandomSource for SystemRandom {
    fn fill(&self, bytes: &mut [u8]) {
        OsRng.fill_bytes(bytes);
    }
}

pub(crate) struct SeededRandom {
    state: Mutex<u64>,
}

impl SeededRandom {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            state: Mutex::new(seed.max(1)),
        }
    }
}

impl RandomSource for SeededRandom {
    fn fill(&self, bytes: &mut [u8]) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        for chunk in bytes.chunks_mut(8) {
            let mut value = *state;
            value ^= value >> 12;
            value ^= value << 25;
            value ^= value >> 27;
            *state = value;
            let random = value.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes();
            for (target, source) in chunk.iter_mut().zip(random) {
                *target = source;
            }
        }
    }
}
