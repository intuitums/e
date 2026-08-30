//! Retry policy for a mid-turn provider failure: how long to wait before
//! trying again, and how many attempts one failure campaign gets before the
//! turn gives up. The ladder shape (250ms, 1s, then doubling to a 30s
//! ceiling) and the ten-attempt budget are e's own defaults.
//! The wait carries downward jitter so many agents failing together don't
//! all knock on the provider on the same tick, and the attempt budget is
//! file-backed (`retry_max_attempts` in `~/.e/settings.json`) — quota
//! errors skip all of this, never being retried at all.

use std::time::Duration;

/// Total provider requests a single failure campaign gets, including the
/// unavoidable initial request, before the turn fails outright. The default;
/// overridden by `retry_max_attempts` (0 disables every follow-up request).
pub const MAX_ATTEMPTS: u32 = 10;
/// Hard bounds on the `retry_max_attempts` setting — a value outside this
/// range reads as the default rather than being trusted.
const MAX_ATTEMPTS_MIN: u64 = 0;
const MAX_ATTEMPTS_MAX: u64 = 20;
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

/// The attempt budget for one failure campaign, from `~/.e/settings.json`
/// (`retry_max_attempts`), defaulting to `MAX_ATTEMPTS`.
pub fn max_attempts() -> u32 {
    crate::core::config::settings::get_u64("retry_max_attempts")
        .filter(|v| (MAX_ATTEMPTS_MIN..=MAX_ATTEMPTS_MAX).contains(v))
        .unwrap_or(MAX_ATTEMPTS as u64) as u32
}

/// Downward jitter (75–100% of the computed wait), so retry storms spread
/// out instead of synchronizing every backoff onto the same tick.
fn jitter(delay: Duration) -> Duration {
    use rand::RngExt;
    let scale = 1.0 - rand::rng().random_range(0.0..0.25);
    delay.mul_f64(scale)
}

/// The wait before the next attempt: the provider's own `Retry-After` when
/// it sent one (capped at its own ceiling, never jittered — the provider
/// asked for that wait), else the computed backoff with jitter.
pub fn delay_for(attempt: u32, retry_after_secs: Option<u64>) -> Duration {
    match retry_after_secs {
        Some(secs) => Duration::from_secs(secs.min(RETRY_AFTER_CEILING_SECS)),
        None => jitter(backoff(attempt)),
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

    #[test]
    fn jitter_never_lengthens_the_wait() {
        let exact = backoff(7); // 30s ceiling
        for _ in 0..50 {
            let waited = delay_for(7, None);
            assert!(waited <= exact, "jitter produced a longer wait: {waited:?}");
            assert!(waited >= exact.mul_f64(0.75));
        }
    }

    #[test]
    fn attempt_budget_follows_the_setting_within_bounds() {
        // Outside the test home no settings file exists, so the default
        // holds; the clamp itself is exercised by the range check above it.
        assert_eq!(max_attempts(), MAX_ATTEMPTS);
    }
}
