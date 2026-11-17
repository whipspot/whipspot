//! The rate gate (decision D14). After the first 429 the Web API client stops: it remembers when
//! the cooldown ends and every later call fails fast, without any network I/O, until then. There
//! is no sleep, no retry loop and no background timer here: the user re-triggers manually.

use std::time::Instant;
use std::{sync::Mutex, time::Duration};

use crate::web_auth::parse_retry_after;

/// Assumed cooldown when `Retry-After` is missing, unparseable or zero (fail closed).
pub const DEFAULT_COOLDOWN_SECS: u64 = 30;
/// Longest cooldown we accept from a header.
pub const MAX_COOLDOWN_SECS: u64 = 86_400;

/// Cooldown in seconds for a `Retry-After` header value: plain ASCII digits only, clamped to
/// `[1, 86400]`; anything else (absent, `abc`, `-5`, `1.5`, `0`, ` 7`, 11+ digits) is 30.
pub fn cooldown_secs(retry_after: Option<&str>) -> u64 {
    match retry_after.and_then(parse_retry_after) {
        Some(n) if n >= 1 => n.min(MAX_COOLDOWN_SECS),
        _ => DEFAULT_COOLDOWN_SECS,
    }
}

#[derive(Debug, Default)]
pub struct RateGate {
    blocked_until: Mutex<Option<Instant>>,
}

impl RateGate {
    /// `Ok(())` if a request may be sent at `now`; else `Err(seconds remaining)` (at least 1).
    pub fn check(&self, now: Instant) -> Result<(), u64> {
        let mut g = self.blocked_until.lock().unwrap();
        match *g {
            Some(until) if until > now => {
                let left = until - now;
                Err(left.as_secs() + u64::from(left.subsec_nanos() > 0))
            }
            Some(_) => {
                *g = None;
                Ok(())
            }
            None => Ok(()),
        }
    }

    /// Start (or extend, never shorten) a cooldown of `secs` from `now`.
    pub fn block(&self, now: Instant, secs: u64) {
        let until = now + Duration::from_secs(secs);
        let mut g = self.blocked_until.lock().unwrap();
        if g.is_none_or(|cur| until > cur) {
            *g = Some(until);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_table() {
        let t = [
            (Some("26"), 26),
            (Some("1"), 1),
            (Some("86400"), 86_400),
            (Some("9999999999"), 86_400), // 10 digits: clamped
            (Some("99999999999"), 30),    // 11 digits: unparseable
            (Some("0"), 30),
            (Some("abc"), 30),
            (Some("-5"), 30),
            (Some("+5"), 30),
            (Some("1.5"), 30),
            (Some(" 7"), 30),
            (Some("7 "), 30),
            (Some(""), 30),
            (Some("٣"), 30), // non-ASCII digit
            (None, 30),
        ];
        for (input, want) in t {
            assert_eq!(cooldown_secs(input), want, "{input:?}");
        }
    }

    #[test]
    fn blocks_until_the_deadline_then_opens() {
        let g = RateGate::default();
        let t0 = Instant::now();
        assert_eq!(g.check(t0), Ok(()));
        g.block(t0, 26);
        assert_eq!(g.check(t0), Err(26));
        assert_eq!(g.check(t0 + Duration::from_millis(25_500)), Err(1));
        assert_eq!(g.check(t0 + Duration::from_secs(26)), Ok(()));
        assert_eq!(
            g.check(t0),
            Ok(()),
            "the gate stays open once it has opened"
        );
    }

    #[test]
    fn a_shorter_cooldown_never_shortens_a_longer_one() {
        let g = RateGate::default();
        let t0 = Instant::now();
        g.block(t0, 60);
        g.block(t0, 5);
        assert_eq!(g.check(t0 + Duration::from_secs(10)), Err(50));
    }
}
