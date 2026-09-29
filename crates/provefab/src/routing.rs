//! `[routing]`: the order models are tried in inside a tier (D73). `select`
//! keeps its rules (tier, cooldowns, cross-review); this only sorts the catalog.

use serde::Deserialize;

use std::cmp::Ordering;

use crate::config::ModelEntry;
use crate::prices::{PriceTable, blended, is_api, price_of, quota_weight};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Prefer {
    /// Subscriptions first (already paid), lowest quota weight first; then API keys.
    #[default]
    Subscription,
    /// API keys first, cheapest first; then subscriptions.
    ApiKey,
    /// One list: subscriptions count as free.
    Cheapest,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Routing {
    #[serde(default)]
    pub prefer: Prefer,
    /// Overrides the models.dev URL (tests, mirrors).
    #[serde(default)]
    pub prices_url: Option<String>,
    /// Overrides the LiteLLM fallback URL (tests, mirrors).
    #[serde(default)]
    pub litellm_url: Option<String>,
}

impl Routing {
    /// models.dev, then LiteLLM (D71).
    pub fn price_urls(&self) -> [&str; 2] {
        [
            self.prices_url
                .as_deref()
                .unwrap_or(crate::prices::MODELS_DEV_URL),
            self.litellm_url
                .as_deref()
                .unwrap_or(crate::prices::LITELLM_URL),
        ]
    }
}

/// The catalog sorted by `prefer`; ties keep catalog order.
pub fn ordered(catalog: &[ModelEntry], table: &PriceTable, prefer: Prefer) -> Vec<ModelEntry> {
    let unpriced_sub = |m: &ModelEntry| m.quota_weight.is_none() && price_of(m, table).is_none();
    let rank = |m: &ModelEntry| -> (u8, f64) {
        let api = is_api(m);
        let group = match prefer {
            Prefer::Subscription => u8::from(api),
            Prefer::ApiKey => u8::from(!api),
            Prefer::Cheapest => 0,
        };
        let cost = if !api {
            match prefer {
                Prefer::Cheapest => 0.0,
                // Unpriced subscriptions rank after every priced one.
                _ if unpriced_sub(m) => f64::MAX,
                // Weights are ordered per vendor below (D76).
                _ => 0.0,
            }
        } else {
            // Unpriced API models rank after every priced one.
            price_of(m, table).map_or(f64::MAX, |p| blended(&p))
        };
        (group, cost)
    };
    let mut ranked: Vec<((u8, f64), &ModelEntry)> = catalog.iter().map(|m| (rank(m), m)).collect();
    // Stable: ties keep catalog order.
    ranked.sort_by(|(a, _), (b, _)| {
        a.0.cmp(&b.0)
            .then(a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal))
    });
    let mut out: Vec<&ModelEntry> = ranked.into_iter().map(|(_, m)| m).collect();
    if prefer != Prefer::Cheapest {
        // D76: a quota weight is relative to its own vendor, so it only orders
        // that vendor's priced subscriptions, inside the slots they already hold.
        let subs: Vec<usize> = (0..out.len())
            .filter(|&i| !is_api(out[i]) && !unpriced_sub(out[i]))
            .collect();
        let mut vendors: Vec<String> = subs.iter().map(|&i| out[i].provider_key()).collect();
        vendors.dedup();
        for v in vendors {
            let slots: Vec<usize> = subs
                .iter()
                .copied()
                .filter(|&i| out[i].provider_key() == v)
                .collect();
            let mut models: Vec<&ModelEntry> = slots.iter().map(|&i| out[i]).collect();
            models.sort_by(|a, b| {
                quota_weight(a, catalog, table)
                    .partial_cmp(&quota_weight(b, catalog, table))
                    .unwrap_or(Ordering::Equal)
            });
            for (slot, m) in slots.into_iter().zip(models) {
                out[slot] = m;
            }
        }
    }
    out.into_iter().cloned().collect()
}

/// Why `m` sits where it does in the order, for the routing log.
pub fn why(m: &ModelEntry, catalog: &[ModelEntry], table: &PriceTable, prefer: Prefer) -> String {
    let policy = match prefer {
        Prefer::Subscription => "prefer subscription",
        Prefer::ApiKey => "prefer api_key",
        Prefer::Cheapest => "prefer cheapest",
    };
    if !is_api(m) {
        if m.quota_weight.is_none() && price_of(m, table).is_none() {
            return format!(
                "subscription, no price (ranked last; set price_id or quota_weight; {policy})"
            );
        }
        return format!(
            "subscription, quota weight {:.1} ({policy})",
            quota_weight(m, catalog, table)
        );
    }
    match price_of(m, table) {
        Some(p) => {
            let source = if m.price_in.is_some() && m.price_out.is_some() {
                "set in provefab.toml".to_string()
            } else {
                m.price_id
                    .clone()
                    .or_else(|| table.key_for(m))
                    .unwrap_or_default()
            };
            format!(
                "API key, ${:.2}/M blended ({source}; {policy})",
                blended(&p)
            )
        }
        None => format!("API key, no price (set price_id or price_in/price_out; {policy})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Auth, WorkerKind};
    use crate::prices::tests::{m, md as table};
    use crate::task::Tier;

    #[test]
    fn subscription_first_by_quota_then_api_by_price() {
        let t = table();
        let cat = vec![
            m(
                "opus-key",
                WorkerKind::ClaudeCode,
                "opus",
                "",
                Tier::Standard,
                Auth::ApiKey,
            ),
            m(
                "sonnet-key",
                WorkerKind::ClaudeCode,
                "sonnet",
                "",
                Tier::Standard,
                Auth::ApiKey,
            ),
            m(
                "opus-sub",
                WorkerKind::ClaudeCode,
                "opus",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
            m(
                "sonnet-sub",
                WorkerKind::ClaudeCode,
                "sonnet",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
        ];
        let ids = |v: Vec<ModelEntry>| v.into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(
            ids(ordered(&cat, &t, Prefer::Subscription)),
            ["sonnet-sub", "opus-sub", "sonnet-key", "opus-key"]
        );
        assert_eq!(
            ids(ordered(&cat, &t, Prefer::ApiKey)),
            ["sonnet-key", "opus-key", "sonnet-sub", "opus-sub"]
        );
        // cheapest: subscriptions count as 0, so they still come first, in catalog order.
        assert_eq!(
            ids(ordered(&cat, &t, Prefer::Cheapest))[..2],
            ["opus-sub", "sonnet-sub"]
        );
        assert!(why(&cat[3], &cat, &t, Prefer::Subscription).contains("quota weight 1.0"));
        assert!(why(&cat[1], &cat, &t, Prefer::Subscription).contains("$4.00/M"));
    }

    /// Plan review focus 1.
    #[test]
    fn unpriced_models_rank_last() {
        let t = table();
        let cat = vec![
            m(
                "typo",
                WorkerKind::Codex,
                "gpt-typo",
                "",
                Tier::Standard,
                Auth::ApiKey,
            ),
            m(
                "luna",
                WorkerKind::Codex,
                "gpt-6-luna",
                "",
                Tier::Standard,
                Auth::ApiKey,
            ),
        ];
        assert_eq!(ordered(&cat, &t, Prefer::ApiKey)[0].id, "luna");
        assert!(why(&cat[0], &cat, &t, Prefer::ApiKey).contains("no price"));
    }

    #[test]
    fn routing_parses_with_defaults() {
        let c = crate::config::Config::from_toml_str(
            "models = []\n[jev]\nmodel = \"jev-1.13.0\"\n[routing]\nprefer = \"api_key\"\n",
        )
        .unwrap();
        assert_eq!(c.routing.prefer, Prefer::ApiKey);
        let c =
            crate::config::Config::from_toml_str("models = []\n[jev]\nmodel = \"jev-1.13.0\"\n")
                .unwrap();
        assert_eq!(c.routing.prefer, Prefer::Subscription);
    }

    /// Review finding 2: an unpriced subscription must not jump the queue.
    #[test]
    fn unpriced_subscriptions_rank_after_priced_ones() {
        let t = table();
        let cat = vec![
            m(
                "mystery",
                WorkerKind::Codex,
                "gpt-typo",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
            m(
                "opus-sub",
                WorkerKind::ClaudeCode,
                "opus",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
        ];
        assert_eq!(ordered(&cat, &t, Prefer::Subscription)[0].id, "opus-sub");
        assert!(why(&cat[0], &cat, &t, Prefer::Subscription).contains("no price"));
    }

    /// D76: quota weights are only comparable within a vendor. Across vendors,
    /// file order decides, and a model in another tier changes nothing.
    #[test]
    fn quota_weight_orders_within_a_vendor_only() {
        let t = table();
        let ids = |v: Vec<ModelEntry>| v.into_iter().map(|m| m.id).collect::<Vec<_>>();
        let cat = vec![
            m(
                "opus-sub",
                WorkerKind::ClaudeCode,
                "opus",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
            m(
                "sol-sub",
                WorkerKind::Codex,
                "gpt-6-sol",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
            m(
                "sonnet-sub",
                WorkerKind::ClaudeCode,
                "sonnet",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
            // Another tier: makes Claude weights 4.0 and 8.0, Codex stays 1.0.
            m(
                "haiku-sub",
                WorkerKind::ClaudeCode,
                "haiku",
                "",
                Tier::Fast,
                Auth::Subscription,
            ),
        ];
        // Claude keeps its file slots (0, 2, 3), sorted by weight inside them.
        assert_eq!(
            ids(ordered(&cat, &t, Prefer::Subscription)),
            ["haiku-sub", "sol-sub", "sonnet-sub", "opus-sub"]
        );
    }
}
