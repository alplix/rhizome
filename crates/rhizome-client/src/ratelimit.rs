//! Keeping outgoing traffic under the server's flood limit.
//!
//! Servers disconnect clients that send too much too fast ("Excess Flood").
//! The usual defence is a token bucket: a short burst is allowed, after which
//! messages are released at a steady rate. A pasted block of text is the case
//! this exists for — without it, twenty lines go out at once and the
//! connection is killed.
//!
//! Time is passed in rather than read, so the behaviour is testable without
//! sleeping.

use std::time::{Duration, Instant};

use rhizome_proto::Message;

/// A token bucket.
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    tokens: f64,
    per_second: f64,
    last: Instant,
}

impl TokenBucket {
    /// A bucket that starts full, holds at most `burst` tokens, and refills at
    /// `per_second` tokens per second.
    pub fn new(burst: u32, per_second: f64, now: Instant) -> TokenBucket {
        let capacity = f64::from(burst.max(1));
        TokenBucket {
            capacity,
            tokens: capacity,
            // A zero or negative rate would make the wait infinite.
            per_second: per_second.max(0.001),
            last: now,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        self.last = now;
    }

    /// Takes one token if one is available.
    pub fn try_take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// How long until a token will be available; zero if one is now.
    pub fn wait_time(&mut self, now: Instant) -> Duration {
        self.refill(now);
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.per_second)
        }
    }
}

/// Whether a message skips the rate limit.
///
/// Anything the connection cannot work without must not queue behind chat: a
/// `PONG` that waits behind a paste gets the client dropped for ping timeout,
/// which is the very failure the limiter exists to prevent. `QUIT` is
/// deliberately *not* exempt, so that messages sent just before quitting are
/// delivered first.
pub fn is_exempt(message: &Message) -> bool {
    matches!(
        message.command.to_string().as_str(),
        "PONG" | "PING" | "CAP" | "AUTHENTICATE" | "PASS" | "NICK" | "USER"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_is_allowed_then_it_runs_dry() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(3, 1.0, t0);
        assert!(b.try_take(t0));
        assert!(b.try_take(t0));
        assert!(b.try_take(t0));
        assert!(!b.try_take(t0), "the fourth message in the same instant waits");
    }

    #[test]
    fn tokens_return_at_the_configured_rate() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(2, 2.0, t0);
        assert!(b.try_take(t0));
        assert!(b.try_take(t0));
        assert!(!b.try_take(t0));
        // Half a second at 2/s is one token.
        assert!(b.try_take(t0 + Duration::from_millis(500)));
        assert!(!b.try_take(t0 + Duration::from_millis(500)));
    }

    #[test]
    fn the_bucket_never_holds_more_than_its_capacity() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(2, 10.0, t0);
        // A long idle spell must not bank an unlimited burst.
        let later = t0 + Duration::from_secs(3600);
        assert!(b.try_take(later));
        assert!(b.try_take(later));
        assert!(!b.try_take(later));
    }

    #[test]
    fn wait_time_says_when_the_next_token_arrives() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(1, 1.0, t0);
        assert_eq!(b.wait_time(t0), Duration::ZERO);
        assert!(b.try_take(t0));
        let wait = b.wait_time(t0);
        assert!(wait > Duration::from_millis(990) && wait <= Duration::from_secs(1));
        // After that long, the wait is over.
        assert_eq!(b.wait_time(t0 + Duration::from_millis(1001)), Duration::ZERO);
    }

    #[test]
    fn a_nonsense_rate_cannot_produce_an_infinite_wait() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(1, 0.0, t0);
        assert!(b.try_take(t0));
        assert!(b.wait_time(t0) < Duration::from_secs(10_000_000));
        let mut b = TokenBucket::new(0, 1.0, t0);
        assert!(b.try_take(t0), "a zero burst still allows one message");
    }

    #[test]
    fn connection_essentials_bypass_the_limit_but_chat_does_not() {
        for cmd in ["PONG", "PING", "CAP", "AUTHENTICATE", "PASS", "NICK", "USER"] {
            assert!(is_exempt(&Message::new(cmd, ["x"])), "{cmd} should be exempt");
        }
        for cmd in ["PRIVMSG", "NOTICE", "JOIN", "PART", "MODE", "QUIT"] {
            assert!(!is_exempt(&Message::new(cmd, ["x"])), "{cmd} should be limited");
        }
    }
}
