//! Per-account cooldowns after rate limits (spec §4.2): 15 minutes, doubling
//! on each consecutive limit, capped at 4 hours; a successful stage clears it.
//! Kept in memory: a restarted provefab simply tries again (D36).

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use crate::router::Availability;

pub const FIRST: Duration = Duration::from_secs(15 * 60);
pub const MAX: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Debug, Default)]
pub struct Cooldowns {
    strikes: HashMap<String, u32>,
    until: HashMap<String, SystemTime>,
    struck_at: HashMap<String, SystemTime>,
    running: HashMap<String, u32>,
    /// Claimed runs whose `stage_run` is not yet in the store: counted against
    /// the daily budget alongside the recorded rows, so parallel workers
    /// cannot both pass the check before either is recorded (issue #12).
    unrecorded: u32,
}

impl Cooldowns {
    /// The provider hit a rate limit; returns when it may be used again.
    pub fn strike(&mut self, provider_key: &str, now: SystemTime) -> SystemTime {
        let n = self.strikes.entry(provider_key.to_string()).or_insert(0);
        let wait = FIRST.saturating_mul(1u32 << (*n).min(8)).min(MAX);
        *n += 1;
        let until = now + wait;
        self.until.insert(provider_key.to_string(), until);
        self.struck_at.insert(provider_key.to_string(), now);
        until
    }

    /// A stage on this provider finished without a rate limit, having started
    /// at `started_at`. A strike made after the stage started is kept: it
    /// belongs to a different, more recent run and must not be cancelled.
    pub fn clear(&mut self, provider_key: &str, started_at: SystemTime) {
        if let Some(t) = self.struck_at.get(provider_key)
            && *t >= started_at
        {
            return;
        }
        self.strikes.remove(provider_key);
        self.until.remove(provider_key);
        self.struck_at.remove(provider_key);
    }

    pub fn start(&mut self, model_id: &str) {
        *self.running.entry(model_id.to_string()).or_insert(0) += 1;
        self.unrecorded += 1;
    }

    pub fn finish(&mut self, model_id: &str) {
        if let Some(n) = self.running.get_mut(model_id) {
            *n = n.saturating_sub(1);
        }
    }

    /// Claimed runs not yet reflected in the store's `stage_runs` count.
    pub fn unrecorded(&self) -> u32 {
        self.unrecorded
    }

    /// A claimed run's `stage_run` has been recorded; give its budget back.
    pub fn recorded(&mut self) {
        self.unrecorded = self.unrecorded.saturating_sub(1);
    }

    /// Earliest time any cooled-down provider becomes usable again.
    pub fn next_expiry(&self, now: SystemTime) -> Option<SystemTime> {
        self.until.values().copied().filter(|t| *t > now).min()
    }

    pub fn availability(&self) -> Availability {
        Availability {
            cooling_until: self.until.clone(),
            running: self.running.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_up_to_four_hours_and_clears() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut c = Cooldowns::default();
        let waits: Vec<u64> = (0..6)
            .map(|_| {
                c.strike("codex", now)
                    .duration_since(now)
                    .unwrap()
                    .as_secs()
                    / 60
            })
            .collect();
        assert_eq!(waits, vec![15, 30, 60, 120, 240, 240]);
        assert!(c.availability().cooling_until.contains_key("codex"));
        c.clear("codex", now + Duration::from_secs(1));
        assert_eq!(c.strike("codex", now).duration_since(now).unwrap(), FIRST);
    }

    #[test]
    fn clear_keeps_a_strike_newer_than_the_stage() {
        let t = |s| SystemTime::UNIX_EPOCH + Duration::from_secs(s);
        let mut c = Cooldowns::default();
        c.strike("codex", t(10));

        c.clear("codex", t(5));
        assert!(c.availability().cooling_until.contains_key("codex"));

        c.clear("codex", t(15));
        assert!(!c.availability().cooling_until.contains_key("codex"));
        assert_eq!(
            c.strike("codex", t(20)).duration_since(t(20)).unwrap(),
            FIRST
        );
    }

    #[test]
    fn running_counts_feed_availability() {
        let mut c = Cooldowns::default();
        c.start("opus");
        c.start("opus");
        c.finish("opus");
        assert_eq!(c.availability().running.get("opus"), Some(&1));
        let now = SystemTime::now();
        assert_eq!(c.next_expiry(now), None);
        let until = c.strike("claude-code", now);
        assert_eq!(c.next_expiry(now), Some(until));
    }

    #[test]
    fn unrecorded_tracks_claims_not_yet_recorded() {
        let mut c = Cooldowns::default();
        assert_eq!(c.unrecorded(), 0);
        c.start("opus");
        c.start("sonnet");
        assert_eq!(c.unrecorded(), 2);
        c.recorded();
        assert_eq!(c.unrecorded(), 1);
        // A saturating decrement: never goes negative.
        c.recorded();
        c.recorded();
        assert_eq!(c.unrecorded(), 0);
    }
}
