//! Reconnect delays.
//!
//! Reconnecting immediately after a drop is what gets a client banned: when a
//! server is overloaded or has just k-lined a network, every client retrying
//! at once makes it worse. The delay doubles on each consecutive failure up to
//! a ceiling, and is jittered so that clients that lost the connection at the
//! same moment do not all come back at the same moment.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The smallest delay we will ever use, whatever the caller asks for. Keeps a
/// zero from becoming a busy loop.
const FLOOR: Duration = Duration::from_millis(10);

/// Exponential backoff with jitter.
#[derive(Debug, Clone)]
pub struct Backoff {
    min: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(min: Duration, max: Duration) -> Backoff {
        let min = min.max(FLOOR);
        Backoff {
            min,
            max: max.max(min),
            attempt: 0,
        }
    }

    /// The delay for the given attempt number, before jitter.
    pub fn delay_for(&self, attempt: u32) -> Duration {
        // Capping the shift keeps the multiplication from overflowing; the
        // ceiling has long since taken over by then.
        let factor = 1u32 << attempt.min(16);
        self.min.saturating_mul(factor).min(self.max)
    }

    /// The delay to wait now, advancing to the next attempt.
    pub fn next_delay(&mut self) -> Duration {
        let base = self.delay_for(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        jitter(base)
    }

    /// Forgets past failures. Call this after a connection that got as far as
    /// registering, so one bad night does not slow every later reconnect.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

/// Scales a delay by a factor in `[0.8, 1.2]`.
///
/// The clock's sub-second nanoseconds are a cheap, dependency-free source of
/// noise. It is not random in any cryptographic sense and does not need to be;
/// it only has to differ between clients.
fn jitter(base: Duration) -> Duration {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let unit = f64::from(nanos % 1000) / 1000.0;
    base.mul_f64(0.8 + 0.4 * unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn the_delay_doubles_until_it_hits_the_ceiling() {
        let b = Backoff::new(secs(2), secs(60));
        let delays: Vec<_> = (0..8).map(|i| b.delay_for(i)).collect();
        assert_eq!(
            delays,
            vec![secs(2), secs(4), secs(8), secs(16), secs(32), secs(60), secs(60), secs(60)]
        );
    }

    #[test]
    fn a_huge_attempt_number_does_not_overflow() {
        let b = Backoff::new(secs(2), secs(300));
        assert_eq!(b.delay_for(u32::MAX), secs(300));
    }

    #[test]
    fn jitter_stays_within_twenty_percent() {
        let mut b = Backoff::new(secs(10), secs(10));
        for _ in 0..200 {
            let d = b.next_delay();
            assert!(d >= secs(8) && d <= secs(12), "{d:?} out of range");
        }
    }

    #[test]
    fn reset_starts_over_from_the_minimum() {
        let mut b = Backoff::new(secs(2), secs(300));
        for _ in 0..5 {
            b.next_delay();
        }
        assert!(b.next_delay() > secs(30));
        b.reset();
        assert!(b.next_delay() < secs(3));
    }

    #[test]
    fn a_zero_minimum_is_raised_so_it_cannot_spin() {
        let b = Backoff::new(Duration::ZERO, Duration::ZERO);
        assert!(b.delay_for(0) >= FLOOR);
    }

    #[test]
    fn a_ceiling_below_the_minimum_is_corrected() {
        let b = Backoff::new(secs(5), secs(1));
        assert_eq!(b.delay_for(0), secs(5));
    }
}
