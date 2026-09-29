//! Routing policy (spec §4.2). Pure: Jev verdict + catalog + availability in,
//! a model out. Every tier decision carries a readable reason for the audit log.

use std::collections::HashMap;
use std::time::SystemTime;

use crate::config::ModelEntry;
use crate::task::{Tier, Verdict};

/// Below this Jev confidence on `difficulty`, go one tier up: wasting some
/// quota is cheaper than a failed run.
pub const LOW_CONFIDENCE: f64 = 0.5;
/// `scope` at or above this rounds to level 3, "architectural".
pub const ARCHITECTURAL_SCOPE: f64 = 2.5;

#[derive(Debug, Clone, PartialEq)]
pub struct StageTiers {
    pub plan: Tier,
    pub implement: Tier,
    pub review: Tier,
    pub reasons: Vec<String>,
}

pub fn stage_tiers(v: &Verdict) -> StageTiers {
    let mut reasons = Vec::new();
    let mut implement = if v.difficulty < 1.5 {
        Tier::Fast
    } else if v.difficulty < 3.0 {
        Tier::Standard
    } else {
        Tier::Frontier
    };
    reasons.push(format!(
        "difficulty {:.2} -> implement {implement:?}",
        v.difficulty
    ));
    if v.difficulty_confidence < LOW_CONFIDENCE {
        implement = implement.up();
        reasons.push(format!(
            "difficulty confidence {:.2} < {LOW_CONFIDENCE} -> implement {implement:?}",
            v.difficulty_confidence
        ));
    }
    let mut plan = implement.up();
    let review = implement.up();
    if v.scope >= ARCHITECTURAL_SCOPE {
        plan = Tier::Frontier;
        reasons.push(format!(
            "scope {:.2} is architectural -> plan Frontier",
            v.scope
        ));
    }
    StageTiers {
        plan,
        implement,
        review,
        reasons,
    }
}

/// Used when Jev is unavailable (spec §4.1 fallback).
pub fn fallback_tiers() -> StageTiers {
    StageTiers {
        plan: Tier::Standard,
        implement: Tier::Standard,
        review: Tier::Standard,
        reasons: vec!["jev_unavailable -> every stage Standard".to_string()],
    }
}

#[derive(Debug, Clone, Default)]
pub struct Availability {
    /// Keyed by `ModelEntry::cooldown_key()`: family and sign-in mode.
    pub cooling_until: HashMap<String, SystemTime>,
    /// Running stages, keyed by model id.
    pub running: HashMap<String, u32>,
}

impl Availability {
    fn usable(&self, m: &ModelEntry, now: SystemTime) -> bool {
        let cooling = self
            .cooling_until
            .get(&m.cooldown_key())
            .is_some_and(|until| *until > now);
        let running = self.running.get(&m.id).copied().unwrap_or(0);
        !cooling && running < m.max_concurrency
    }
}

/// Tiers to try when the wanted tier has no model in the catalog at all:
/// stronger first, then weaker.
fn tier_preference(t: Tier) -> [Tier; 3] {
    match t {
        Tier::Fast => [Tier::Fast, Tier::Standard, Tier::Frontier],
        Tier::Standard => [Tier::Standard, Tier::Frontier, Tier::Fast],
        Tier::Frontier => [Tier::Frontier, Tier::Standard, Tier::Fast],
    }
}

/// The tier `select` will actually use for `tier`: the nearest tier that has
/// a model in the catalog at all (`None` if the catalog is empty). Callers
/// that escalate a tier (`Tier::up`) must resolve first, so they escalate
/// above the tier that actually ran rather than above a tier the catalog
/// never had a model for.
pub fn resolve_tier(tier: Tier, catalog: &[ModelEntry]) -> Option<Tier> {
    tier_preference(tier)
        .into_iter()
        .find(|t| catalog.iter().any(|m| m.tier == *t))
}

/// Picks a model for one stage. `None` means every model in the tier is
/// busy or cooling down: the task goes to `Waiting`, not `Failed`.
///
/// A tier missing from the catalog entirely falls back to the nearest
/// configured tier. Cooling models never cause a tier hop.
///
/// `avoid` lists providers to avoid, most important first (e.g. `[first
/// approver, implementer]` for a second review, spec §3.2). `select` tries to
/// avoid the whole list, then progressively shorter prefixes, then falls
/// back to any usable model.
pub fn select<'a>(
    tier: Tier,
    catalog: &'a [ModelEntry],
    avail: &Availability,
    now: SystemTime,
    avoid: &[String],
) -> Option<&'a ModelEntry> {
    let tier = resolve_tier(tier, catalog)?;
    let usable: Vec<&ModelEntry> = catalog
        .iter()
        .filter(|m| m.tier == tier && avail.usable(m, now))
        .collect();
    (0..=avoid.len()).rev().find_map(|n| {
        usable
            .iter()
            .find(|m| !avoid[..n].contains(&m.provider_key()))
            .copied()
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::config::WorkerKind;
    use crate::task::TaskKind;

    fn verdict(difficulty: f64, confidence: f64, scope: f64) -> Verdict {
        Verdict {
            task_kind: TaskKind::Bugfix,
            difficulty,
            difficulty_confidence: confidence,
            scope,
            underspecified: 0.1,
            jev_model: "jev-1.13.0".into(),
        }
    }

    #[test]
    fn stage_tiers_table() {
        use Tier::*;
        // (difficulty, confidence, scope) -> (plan, implement, review)
        let cases = [
            ((0.2, 0.9, 0.0), (Standard, Fast, Standard)),
            ((1.49, 0.9, 1.0), (Standard, Fast, Standard)),
            ((1.5, 0.9, 1.0), (Frontier, Standard, Frontier)),
            ((2.99, 0.9, 1.0), (Frontier, Standard, Frontier)),
            ((3.0, 0.9, 1.0), (Frontier, Frontier, Frontier)),
            ((0.2, 0.3, 0.0), (Frontier, Standard, Frontier)),
            ((0.2, 0.9, 2.5), (Frontier, Fast, Standard)),
            ((4.0, 0.1, 3.0), (Frontier, Frontier, Frontier)),
        ];
        for ((d, c, s), (plan, implement, review)) in cases {
            let t = stage_tiers(&verdict(d, c, s));
            assert_eq!(
                (t.plan, t.implement, t.review),
                (plan, implement, review),
                "d={d} c={c} s={s}"
            );
            assert!(!t.reasons.is_empty());
        }
    }

    #[test]
    fn fallback_is_all_standard() {
        let t = fallback_tiers();
        assert_eq!(
            (t.plan, t.implement, t.review),
            (Tier::Standard, Tier::Standard, Tier::Standard)
        );
    }

    fn model(id: &str, worker: WorkerKind, provider: &str, tier: Tier) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            worker,
            model: id.into(),
            provider: provider.into(),
            tier,
            max_concurrency: 1,
            auth: crate::config::Auth::Subscription,
        }
    }

    /// BYOK: when the subscription is paused by a limit, the same vendor's API
    /// key model stays usable.
    #[test]
    fn a_paused_subscription_leaves_the_api_key_model_usable() {
        let sub = model("sub", WorkerKind::ClaudeCode, "", Tier::Standard);
        let key = ModelEntry {
            auth: crate::config::Auth::ApiKey,
            ..model("key", WorkerKind::ClaudeCode, "", Tier::Standard)
        };
        let now = SystemTime::now();
        let mut avail = Availability::default();
        avail
            .cooling_until
            .insert(sub.cooldown_key(), now + Duration::from_secs(600));
        let catalog = vec![sub, key];
        let picked = select(Tier::Standard, &catalog, &avail, now, &[]).unwrap();
        assert_eq!(picked.id, "key");
    }

    fn catalog() -> Vec<ModelEntry> {
        vec![
            model("opus", WorkerKind::ClaudeCode, "", Tier::Frontier),
            model("codex-hi", WorkerKind::Pi, "openai-codex", Tier::Frontier),
            model("sonnet", WorkerKind::ClaudeCode, "", Tier::Standard),
        ]
    }

    #[test]
    fn picks_first_usable_in_catalog_order() {
        let c = catalog();
        let now = SystemTime::now();
        let m = select(Tier::Frontier, &c, &Availability::default(), now, &[]).unwrap();
        assert_eq!(m.id, "opus");
    }

    #[test]
    fn skips_cooling_provider_and_full_models() {
        let c = catalog();
        let now = SystemTime::now();
        let mut a = Availability::default();
        a.cooling_until
            .insert("claude-code".into(), now + Duration::from_secs(60));
        assert_eq!(
            select(Tier::Frontier, &c, &a, now, &[]).unwrap().id,
            "codex-hi"
        );

        let mut a = Availability::default();
        a.running.insert("opus".into(), 1);
        assert_eq!(
            select(Tier::Frontier, &c, &a, now, &[]).unwrap().id,
            "codex-hi"
        );
    }

    #[test]
    fn expired_cooldown_is_ignored() {
        let c = catalog();
        let now = SystemTime::now();
        let mut a = Availability::default();
        a.cooling_until
            .insert("claude-code".into(), now - Duration::from_secs(1));
        assert_eq!(select(Tier::Frontier, &c, &a, now, &[]).unwrap().id, "opus");
    }

    #[test]
    fn everything_cooling_means_wait_not_tier_hop() {
        let c = catalog();
        let now = SystemTime::now();
        let mut a = Availability::default();
        a.cooling_until
            .insert("claude-code".into(), now + Duration::from_secs(60));
        a.cooling_until
            .insert("pi:openai-codex".into(), now + Duration::from_secs(60));
        assert_eq!(select(Tier::Frontier, &c, &a, now, &[]), None);
    }

    #[test]
    fn missing_tier_falls_back_to_nearest_configured_tier() {
        let c = catalog(); // no Fast models
        let now = SystemTime::now();
        assert_eq!(
            select(Tier::Fast, &c, &Availability::default(), now, &[])
                .unwrap()
                .id,
            "sonnet"
        );
        let only_fast = vec![model("mini", WorkerKind::Pi, "openai-codex", Tier::Fast)];
        assert_eq!(
            select(
                Tier::Frontier,
                &only_fast,
                &Availability::default(),
                now,
                &[]
            )
            .unwrap()
            .id,
            "mini"
        );
        assert_eq!(
            select(Tier::Fast, &[], &Availability::default(), now, &[]),
            None
        );
    }

    #[test]
    fn resolve_tier_falls_back_to_nearest_configured_tier() {
        let c = catalog(); // no Fast models
        assert_eq!(resolve_tier(Tier::Fast, &c), Some(Tier::Standard));
    }

    #[test]
    fn review_prefers_a_different_provider_but_does_not_require_one() {
        let c = catalog();
        let now = SystemTime::now();
        let a = Availability::default();
        assert_eq!(
            select(Tier::Frontier, &c, &a, now, &["claude-code".to_string()])
                .unwrap()
                .id,
            "codex-hi"
        );
        assert_eq!(
            select(Tier::Standard, &c, &a, now, &["claude-code".to_string()])
                .unwrap()
                .id,
            "sonnet"
        );
    }

    #[test]
    fn avoid_list_tries_progressively_shorter_prefixes() {
        // Three models, three distinct providers, same tier.
        let c = vec![
            model("claude", WorkerKind::ClaudeCode, "", Tier::Frontier),
            model("codex", WorkerKind::Codex, "", Tier::Frontier),
            model("pi", WorkerKind::Pi, "x", Tier::Frontier),
        ];
        let now = SystemTime::now();
        let a = Availability::default();

        // Avoiding two providers where a third model is free picks that model.
        assert_eq!(
            select(
                Tier::Frontier,
                &c,
                &a,
                now,
                &["claude-code".to_string(), "codex".to_string()],
            )
            .unwrap()
            .id,
            "pi"
        );

        // Avoiding all three, in order, falls back to the model that avoids the
        // most important ones: it avoids "claude-code" and "codex" (the first
        // two, most important) but not "pi:x" (the least important), since no
        // model can avoid all three.
        assert_eq!(
            select(
                Tier::Frontier,
                &c,
                &a,
                now,
                &[
                    "claude-code".to_string(),
                    "codex".to_string(),
                    "pi:x".to_string(),
                ],
            )
            .unwrap()
            .id,
            "pi"
        );

        // Avoiding providers that cover the whole catalog still returns a
        // model (the "else any" fallback), rather than None.
        let one = vec![model("claude", WorkerKind::ClaudeCode, "", Tier::Frontier)];
        assert_eq!(
            select(
                Tier::Frontier,
                &one,
                &a,
                now,
                &["claude-code".to_string(), "claude-code".to_string()],
            )
            .unwrap()
            .id,
            "claude"
        );
    }
}
