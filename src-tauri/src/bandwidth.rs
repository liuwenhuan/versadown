use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

/// A shared, blocking pace maker for streaming transports. A zero limit means
/// unlimited; otherwise every reader using the same limiter shares one cap.
pub(crate) struct BandwidthLimiter {
    state: Mutex<BandwidthLimiterState>,
}

#[derive(Debug)]
struct BandwidthLimiterState {
    bytes_per_second: u64,
    next_available: Instant,
}

impl BandwidthLimiter {
    pub(crate) fn new(bytes_per_second: u64) -> Self {
        Self {
            state: Mutex::new(BandwidthLimiterState {
                bytes_per_second,
                next_available: Instant::now(),
            }),
        }
    }

    pub(crate) fn set_limit(&self, bytes_per_second: u64) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.bytes_per_second = bytes_per_second;
        state.next_available = Instant::now();
    }

    pub(crate) fn consume_blocking(&self, bytes: usize) {
        if let Some(delay) = self.reserve(bytes) {
            std::thread::sleep(delay);
        }
    }

    pub(crate) async fn consume_async(&self, bytes: usize) {
        if let Some(delay) = self.reserve(bytes) {
            tokio::time::sleep(delay).await;
        }
    }

    fn reserve(&self, bytes: usize) -> Option<Duration> {
        if bytes == 0 {
            return None;
        }
        let deadline = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state.bytes_per_second == 0 {
                return None;
            }
            let now = Instant::now();
            let seconds = bytes as f64 / state.bytes_per_second as f64;
            let duration = Duration::from_secs_f64(seconds);
            let start = state.next_available.max(now);
            let deadline = start + duration;
            state.next_available = deadline;
            deadline
        };
        deadline.checked_duration_since(Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_limit_does_not_delay_reads() {
        let limiter = BandwidthLimiter::new(0);
        let started = Instant::now();
        limiter.consume_blocking(1_024 * 1_024);
        assert!(started.elapsed() < Duration::from_millis(25));
    }

    #[test]
    fn shared_limit_paces_consecutive_reads() {
        let limiter = BandwidthLimiter::new(1_000);
        let started = Instant::now();
        limiter.consume_blocking(50);
        limiter.consume_blocking(50);
        assert!(started.elapsed() >= Duration::from_millis(90));
    }
}
