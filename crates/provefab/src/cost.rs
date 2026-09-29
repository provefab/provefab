//! What a stage cost (D74): dollars for a model signed in by API key, quota
//! units for a subscription.

use agent_workers::Usage;

use crate::config::ModelEntry;
use crate::prices::{PriceTable, is_api, price_by_model_id, price_of, quota_weight};
use crate::store::StageRunRecord;

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StageCost {
    pub usd: Option<f64>,
    pub quota_units: Option<f64>,
}

pub fn stage_cost(
    m: &ModelEntry,
    catalog: &[ModelEntry],
    usage: &Usage,
    actual_model: Option<&str>,
    table: &PriceTable,
) -> StageCost {
    let finite = |x: f64| x.is_finite().then_some(x);
    if !is_api(m) {
        let tokens = (usage.input_tokens
            + usage.output_tokens
            + usage.cache_read_tokens
            + usage.cache_write_tokens) as f64;
        return StageCost {
            usd: None,
            quota_units: finite(tokens / 1e6 * quota_weight(m, catalog, table)),
        };
    }
    // Prices set in provefab.toml win (D71); otherwise the model the CLI
    // reports it ran, then the catalog entry.
    let explicit = m.price_id.is_some() || (m.price_in.is_some() && m.price_out.is_some());
    let price = if explicit {
        price_of(m, table)
    } else {
        actual_model
            .and_then(|a| price_by_model_id(a, table))
            .or_else(|| price_of(m, table))
    };
    let usd = price.and_then(|p| {
        finite(
            (usage.input_tokens as f64 * p.input
                + usage.output_tokens as f64 * p.output
                + usage.cache_read_tokens as f64 * p.cache_read.unwrap_or(p.input)
                + usage.cache_write_tokens as f64 * p.cache_write.unwrap_or(p.input))
                / 1e6,
        )
    });
    StageCost {
        usd,
        quota_units: None,
    }
}

/// `$0.0123` under a dollar, `$4.40` above.
pub fn usd(x: f64) -> String {
    if x < 1.0 {
        format!("${x:.4}")
    } else {
        format!("${x:.2}")
    }
}

/// A task's total: `$X API · Y quota units`, leaving out a zero part; `None`
/// when nothing was priced.
pub fn summary(runs: &[StageRunRecord]) -> Option<String> {
    let dollars: f64 = runs.iter().filter_map(|r| r.cost_usd).sum();
    let quota: f64 = runs.iter().filter_map(|r| r.quota_units).sum();
    let mut parts = Vec::new();
    if dollars > 0.0 {
        parts.push(format!("{} API", usd(dollars)));
    }
    if quota > 0.0 {
        parts.push(format!("{quota:.2} quota units"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Auth, WorkerKind};
    use crate::prices::tests::{m, md as table};
    use crate::task::Tier;

    #[test]
    fn api_cost_prices_every_token_kind() {
        let t = table();
        let e = m(
            "s",
            WorkerKind::ClaudeCode,
            "sonnet",
            "",
            Tier::Standard,
            Auth::ApiKey,
        );
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_read_tokens: 2_000_000,
            cache_write_tokens: 400_000,
        };
        let c = stage_cost(
            &e,
            std::slice::from_ref(&e),
            &u,
            Some("claude-sonnet-5-5"),
            &t,
        );
        // 1*2 + 0.1*10 + 2*0.2 + 0.4*2.5 = 4.4
        assert!((c.usd.unwrap() - 4.4).abs() < 1e-9);
        assert_eq!(c.quota_units, None);
        // The actual model wins over the catalog entry's alias.
        let c = stage_cost(
            &e,
            std::slice::from_ref(&e),
            &u,
            Some("claude-opus-5-5"),
            &t,
        );
        // 1*4 + 0.1*20 + 2*0.4 + 0.4*5 = 8.8
        assert!((c.usd.unwrap() - 8.8).abs() < 1e-9);
    }

    #[test]
    fn subscription_cost_is_quota_units() {
        let t = table();
        let e = m(
            "o",
            WorkerKind::ClaudeCode,
            "opus",
            "",
            Tier::Frontier,
            Auth::Subscription,
        );
        let cat = vec![
            m(
                "s",
                WorkerKind::ClaudeCode,
                "sonnet",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
            e.clone(),
        ];
        let u = Usage {
            input_tokens: 500_000,
            output_tokens: 500_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let c = stage_cost(&e, &cat, &u, None, &t);
        assert_eq!(c.usd, None);
        assert!((c.quota_units.unwrap() - 2.0).abs() < 1e-9); // 1M tokens x weight 2.0
    }

    /// Plan review focus 4.
    #[test]
    fn a_stage_without_usage_costs_nothing() {
        let t = table();
        let e = m(
            "s",
            WorkerKind::ClaudeCode,
            "sonnet",
            "",
            Tier::Standard,
            Auth::ApiKey,
        );
        let c = stage_cost(&e, std::slice::from_ref(&e), &Usage::default(), None, &t);
        assert_eq!(c.usd, Some(0.0));
        let unpriced = m(
            "x",
            WorkerKind::Codex,
            "gpt-typo",
            "",
            Tier::Standard,
            Auth::ApiKey,
        );
        assert_eq!(
            stage_cost(
                &unpriced,
                std::slice::from_ref(&unpriced),
                &Usage::default(),
                None,
                &t
            )
            .usd,
            None
        );
    }
}
