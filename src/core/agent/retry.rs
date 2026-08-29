//! Retry policy for a mid-turn provider failure: how long to wait before
//! trying again, and how many attempts one failure campaign gets before the
//! turn gives up. The ladder shape (250ms, 1s, then doubling to a 30s
//! ceiling) and the ten-attempt budget follow the reference client.

use std::time::Duration;

/// Total provider requests a single failure campaign gets, including the
/// initial request, before the turn fails outright.
pub const MAX_ATTEMPTS: u32 = 10;
/// The computed backoff ladder never waits longer than this.
const CEILING_SECS: u64 = 30;
/// A provider's own `Retry-After` is honored up to this higher bound:
/// clamping a requested 60s to 30s just burned attempts on requests the
/// provider already said would fail. Still capped — a provider cannot buy
/// an unbounded pause.
const RETRY_AFTER_CEILING_SECS: u64 = 60;

/// Delay before retry attempt `n` (1-indexed; 0 means "no wait"): 250ms, 1s,
/// 2s, 4s, 8s, 16s, then flat at the ceiling for every attempt after.
pub fn backoff(attempt: u32) -> Duration {
    match attempt {
        0 => Duration::ZERO,
        1 => Duration::from_millis(250),
        n => Duration::from_secs(2u64.saturating_pow(n - 2).min(CEILING_SECS)),
    }
}

/// The wait before the next attempt: the provider's own `Retry-After` when
/// it sent one (capped at its own ceiling), else the computed backoff.
pub fn delay_for(attempt: u32, retry_after_secs: Option<u64>) -> Duration {
    match retry_after_secs {
        Some(secs) => Duration::from_secs(secs.min(RETRY_AFTER_CEILING_SECS)),
        None => backoff(attempt),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_matches_the_reference_shape() {
        let expected_ms = [0, 250, 1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000];
        for (attempt, &ms) in expected_ms.iter().enumerate() {
            assert_eq!(
                backoff(attempt as u32).as_millis(),
                ms as u128,
                "attempt {attempt}"
            );
        }
    }

    #[test]
    fn retry_after_is_capped_at_its_ceiling_and_skips_the_ladder() {
        assert_eq!(
            delay_for(1, Some(999)),
            Duration::from_secs(RETRY_AFTER_CEILING_SECS)
        );
        assert_eq!(delay_for(1, Some(5)), Duration::from_secs(5));
        assert_eq!(delay_for(1, Some(60)), Duration::from_secs(60));
        assert_eq!(delay_for(7, Some(2)), Duration::from_secs(2));
    }
}
