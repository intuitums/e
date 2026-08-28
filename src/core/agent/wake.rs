//! System-sleep detection by clock divergence.
//!
//! Monotonic time (`Instant`) pauses while the process is suspended; wall
//! time (`SystemTime`) does not. A one-second heartbeat therefore tells
//! sleep from ordinary elapsed time: when the wall clock ran far ahead of
//! the monotonic wait, the process was suspended in between. No platform
//! APIs, works everywhere; the clock reads go through [`Beat::now`] and the
//! comparison lives in pure [`observe`], so the policy is testable without
//! putting the machine to sleep.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// Wall-clock excess (over the monotonic wait) that counts as a suspend —
/// comfortably above NTP corrections and timer coalescing.
const MIN_GAP: Duration = Duration::from_secs(5);

/// One detected suspension: how long the wall clock says the machine was
/// gone, and when — monotonically — it came back.
#[derive(Clone, Copy, Debug)]
pub struct SleepGap {
    pub duration: Duration,
    pub woke_at: Instant,
}

impl SleepGap {
    /// Duration in the compact activity-row grammar (`4s`, `3m 20s`).
    pub fn label(&self) -> String {
        crate::core::output::format_elapsed(self.duration.as_secs())
    }
}

#[derive(Clone, Copy, Debug)]
struct Beat {
    wall: SystemTime,
    mono: Instant,
}

impl Beat {
    fn now() -> Self {
        Self {
            wall: SystemTime::now(),
            mono: Instant::now(),
        }
    }
}

/// The sleep one heartbeat tick observed: the wall clock outpaced the
/// monotonic wait by more than [`MIN_GAP`].
fn observe(last: Beat, now: Beat) -> Option<SleepGap> {
    let mono_wait = now.mono.duration_since(last.mono);
    let wall_wait = now.wall.duration_since(last.wall).unwrap_or_default();
    let extra = wall_wait.checked_sub(mono_wait)?;
    (extra >= MIN_GAP).then_some(SleepGap {
        duration: extra,
        woke_at: now.mono,
    })
}

/// The latest gap, shared between the heartbeat task and the turn loop.
pub type Shared = Arc<Mutex<Option<SleepGap>>>;

pub fn shared() -> Shared {
    Arc::new(Mutex::new(None))
}

/// The heartbeat: one tick per `interval`, recording a gap whenever the
/// wall clock outpaced the monotonic wait by more than [`MIN_GAP`]. Runs
/// on a spawned task for one turn's life; `stop` ends it promptly.
pub async fn heartbeat(gaps: Shared, stop: Arc<std::sync::atomic::AtomicBool>, interval: Duration) {
    use std::sync::atomic::Ordering;
    let mut last = Beat::now();
    loop {
        tokio::time::sleep(interval).await;
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let now = Beat::now();
        if let Some(gap) = observe(last, now) {
            let mut slot = gaps.lock().unwrap_or_else(|e| e.into_inner());
            // Keep only the latest gap — an earlier one is history, and
            // its window verdict has already been applied.
            *slot = Some(gap);
        }
        last = now;
    }
}

/// The gap the process woke from while `since` was in flight — a gap
/// recorded before `since` cannot be this attempt's loss, and a fresh
/// request after wake correctly owns whatever happens to it next.
pub fn gap_since(gaps: &Shared, since: Instant) -> Option<SleepGap> {
    let slot = gaps.lock().unwrap_or_else(|e| e.into_inner());
    let gap = *slot;
    gap.filter(|gap| gap.woke_at >= since)
}

/// Resume policy knobs, read from `~/.e/settings.json` with built-in
/// defaults: how long a sleep the turn will ride out, and how many
/// mid-reply continuations one turn may chain.
pub mod policy {
    use crate::core::config::settings;

    pub const DEFAULT_WINDOW_SECS: u64 = 300;
    pub const DEFAULT_CONTINUATIONS: u32 = 3;

    pub fn window_secs() -> u64 {
        settings::get_u64("sleep_window_secs").unwrap_or(DEFAULT_WINDOW_SECS)
    }

    pub fn max_continuations() -> u32 {
        settings::get_u64("sleep_continuations").unwrap_or(DEFAULT_CONTINUATIONS as u64) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beats(wall_excess_ms: u64, mono_wait_ms: u64) -> (Beat, Beat) {
        let now = Beat::now();
        // The story of one tick, reconstructed: `last` sits mono_wait
        // before `now` monotonically, and the wall clock covered the same
        // wait plus the excess — the excess is the observed jump.
        let last = Beat {
            wall: now.wall - Duration::from_millis(mono_wait_ms + wall_excess_ms),
            mono: now.mono - Duration::from_millis(mono_wait_ms),
        };
        (last, now)
    }

    #[test]
    fn a_wall_jump_beyond_the_monotonic_wait_is_a_gap() {
        let (last, now) = beats(8_000, 1_000);
        let gap = observe(last, now).expect("a 7s unexplained jump is a suspend");
        assert!(gap.duration >= Duration::from_secs(5));
    }

    #[test]
    fn ordinary_elapsed_time_is_not_a_gap() {
        let (last, now) = beats(1_000, 1_000);
        assert!(observe(last, now).is_none());
    }

    #[test]
    fn ntp_sized_noise_is_not_a_gap() {
        let (last, now) = beats(2_000, 1_000);
        assert!(observe(last, now).is_none());
    }

    #[test]
    fn a_gap_only_attributable_to_attempts_in_flight() {
        let gaps = shared();
        let (last, now) = beats(8_000, 1_000);
        if let Some(gap) = observe(last, now) {
            *gaps.lock().unwrap_or_else(|e| e.into_inner()) = Some(gap);
        }
        // The attempt was running when the gap ended (started before the
        // wake) — attributable.
        assert!(gap_since(&gaps, now.mono - Duration::from_secs(1)).is_some());
        // A fresh attempt started after the wake owns its own fate.
        assert!(gap_since(&gaps, now.mono + Duration::from_secs(1)).is_none());
    }
}
