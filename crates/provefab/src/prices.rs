//! Model prices (cost-aware routing, D71, D72): parsed from models.dev or
//! LiteLLM, resolved per catalog entry, turned into subscription quota weights.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Auth, ModelEntry, WorkerKind};
use crate::paths::Paths;

pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
pub const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
/// Prices are refreshed at most once a day (D71).
pub const MAX_AGE: i64 = 86_400;
const FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// USD per million tokens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub release_date: Option<String>,
}

/// Prices keyed by `provider/model`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PriceTable {
    pub source: String,
    pub fetched_at: i64,
    pub models: BTreeMap<String, Price>,
}

const CLAUDE_ALIASES: [&str; 3] = ["opus", "sonnet", "haiku"];

impl PriceTable {
    /// `https://models.dev/api.json`: `{provider: {models: {id: {cost: {input, output, cache_read?, cache_write?}, release_date?}}}}`.
    pub fn from_models_dev(json: &str, fetched_at: i64) -> Result<Self, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| format!("models.dev: {e}"))?;
        let providers = v.as_object().ok_or("models.dev: not an object")?;
        let mut models = BTreeMap::new();
        for (provider, p) in providers {
            let Some(ms) = p.get("models").and_then(Value::as_object) else {
                continue;
            };
            for (id, m) in ms {
                let cost = &m["cost"];
                let (Some(input), Some(output)) = (cost["input"].as_f64(), cost["output"].as_f64())
                else {
                    continue;
                };
                models.insert(
                    format!("{provider}/{id}"),
                    Price {
                        input,
                        output,
                        cache_read: cost["cache_read"].as_f64(),
                        cache_write: cost["cache_write"].as_f64(),
                        release_date: m["release_date"].as_str().map(str::to_string),
                    },
                );
            }
        }
        if models.is_empty() {
            return Err("models.dev: no priced model".into());
        }
        Ok(Self {
            source: "models.dev".into(),
            fetched_at,
            models,
        })
    }

    /// LiteLLM's `model_prices_and_context_window.json`: USD per token.
    pub fn from_litellm(json: &str, fetched_at: i64) -> Result<Self, String> {
        let v: Value = serde_json::from_str(json).map_err(|e| format!("litellm: {e}"))?;
        let entries = v.as_object().ok_or("litellm: not an object")?;
        let per_m = |x: &Value| x.as_f64().map(|p| p * 1e6);
        let mut models = BTreeMap::new();
        for (id, m) in entries {
            let (Some(provider), Some(input), Some(output)) = (
                m["litellm_provider"].as_str(),
                per_m(&m["input_cost_per_token"]),
                per_m(&m["output_cost_per_token"]),
            ) else {
                continue;
            };
            models.insert(
                format!("{provider}/{id}"),
                Price {
                    input,
                    output,
                    cache_read: per_m(&m["cache_read_input_token_cost"]),
                    cache_write: per_m(&m["cache_creation_input_token_cost"]),
                    release_date: None,
                },
            );
        }
        if models.is_empty() {
            return Err("litellm: no priced model".into());
        }
        Ok(Self {
            source: "litellm".into(),
            fetched_at,
            models,
        })
    }

    /// The unique key ending with `/<model>`.
    fn by_suffix(&self, model: &str) -> Option<String> {
        let suffix = format!("/{model}");
        let mut hits = self.models.keys().filter(|k| k.ends_with(&suffix));
        match (hits.next(), hits.next()) {
            (Some(k), None) => Some(k.clone()),
            _ => None,
        }
    }

    /// The price-table key for a catalog entry, by worker (spec §2).
    pub fn key_for(&self, m: &ModelEntry) -> Option<String> {
        let key = match m.worker {
            WorkerKind::ClaudeCode if CLAUDE_ALIASES.contains(&m.model.as_str()) => {
                let prefix = format!("anthropic/claude-{}-", m.model);
                return self
                    .models
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .max_by(|(ka, a), (kb, b)| {
                        a.release_date
                            .cmp(&b.release_date)
                            .then(kb.len().cmp(&ka.len()))
                    })
                    .map(|(k, _)| k.clone());
            }
            WorkerKind::ClaudeCode => format!("anthropic/{}", m.model),
            WorkerKind::Codex => format!("openai/{}", m.model),
            WorkerKind::Pi => {
                let direct = format!("{}/{}", m.provider, m.model);
                if self.models.contains_key(&direct) {
                    direct
                } else {
                    return self.by_suffix(&m.model);
                }
            }
        };
        self.models.contains_key(&key).then_some(key)
    }
}

/// `(3 × input + output) / 4`: agents read far more than they write.
/// models.dev (anthropic and openai) at build time: the last resort offline.
pub fn snapshot() -> PriceTable {
    let mut t = PriceTable::from_models_dev(include_str!("prices_snapshot.json"), 0)
        .expect("the built-in price snapshot parses");
    t.source = "snapshot".into();
    t
}

/// The cached table in `$PROVEFAB_HOME/prices.json`, if readable.
pub fn cached(paths: &Paths) -> Option<PriceTable> {
    let raw = std::fs::read_to_string(paths.prices()).ok()?;
    serde_json::from_str(&raw).ok()
}

/// A fresh cache as is; otherwise models.dev, then LiteLLM (`urls`), written
/// back to the cache; on failure the stale cache, then the built-in snapshot.
/// Never fails: routing must work offline.
pub async fn load(paths: &Paths, urls: [&str; 2], now: i64) -> PriceTable {
    let cache = cached(paths);
    if let Some(c) = &cache
        && now - c.fetched_at < MAX_AGE
    {
        return c.clone();
    }
    match fetch(urls, now).await {
        Ok(t) => {
            if let Err(e) = write_cache(paths, &t) {
                eprintln!("provefab: prices: cannot write the cache: {e}");
            }
            t
        }
        Err(e) => {
            let fallback = cache.unwrap_or_else(snapshot);
            eprintln!(
                "provefab: prices: {e}; using the {} prices",
                fallback.source
            );
            fallback
        }
    }
}

async fn fetch(urls: [&str; 2], now: i64) -> Result<PriceTable, String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let get = async |url: &str| -> Result<String, String> {
        let r = client.get(url).send().await.map_err(|e| e.to_string())?;
        let r = r.error_for_status().map_err(|e| e.to_string())?;
        r.text().await.map_err(|e| e.to_string())
    };
    let first = match get(urls[0]).await {
        Ok(body) => PriceTable::from_models_dev(&body, now),
        Err(e) => Err(format!("models.dev: {e}")),
    };
    match first {
        Ok(t) => Ok(t),
        Err(e1) => {
            let second = match get(urls[1]).await {
                Ok(body) => PriceTable::from_litellm(&body, now),
                Err(e) => Err(format!("litellm: {e}")),
            };
            second.map_err(|e2| format!("{e1}; {e2}"))
        }
    }
}

/// Atomic: a temporary file renamed over the cache, so a crash never leaves junk.
fn write_cache(paths: &Paths, t: &PriceTable) -> std::io::Result<()> {
    let path = paths.prices();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(t)?)?;
    std::fs::rename(tmp, path)
}

pub fn blended(p: &Price) -> f64 {
    (3.0 * p.input + p.output) / 4.0
}

/// Manual prices, else `price_id`, else the automatic match.
pub fn price_of(m: &ModelEntry, table: &PriceTable) -> Option<Price> {
    if let (Some(input), Some(output)) = (m.price_in, m.price_out) {
        return Some(Price {
            input,
            output,
            cache_read: m.price_cache_read,
            cache_write: m.price_cache_write,
            release_date: None,
        });
    }
    let key = match &m.price_id {
        Some(id) => id.clone(),
        None => table.key_for(m)?,
    };
    table.models.get(&key).cloned()
}

/// The price of the model a worker reported using (for example
/// `claude-sonnet-5-5` from Claude Code's init event).
pub fn price_by_model_id(model: &str, table: &PriceTable) -> Option<Price> {
    table
        .by_suffix(model)
        .and_then(|k| table.models.get(&k).cloned())
}

/// Runs on an API key (Pi always does), not on a subscription.
pub fn is_api(m: &ModelEntry) -> bool {
    m.worker == WorkerKind::Pi || m.auth == Auth::ApiKey
}

/// Subscription quota weight: manual, else blended price over the vendor's
/// cheapest priced catalog model, one decimal, at least 1 (D72).
pub fn quota_weight(m: &ModelEntry, catalog: &[ModelEntry], table: &PriceTable) -> f64 {
    if let Some(w) = m.quota_weight {
        return w;
    }
    let Some(own) = price_of(m, table).map(|p| blended(&p)) else {
        return 1.0;
    };
    let family = m.provider_key();
    let cheapest = catalog
        .iter()
        .filter(|c| c.provider_key() == family)
        .filter_map(|c| price_of(c, table).map(|p| blended(&p)))
        .fold(f64::INFINITY, f64::min);
    if !cheapest.is_finite() || cheapest <= 0.0 {
        return 1.0;
    }
    (((own / cheapest) * 10.0).round() / 10.0).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Auth, ModelEntry, WorkerKind};
    use crate::task::Tier;

    fn md() -> PriceTable {
        PriceTable::from_models_dev(include_str!("../tests/fixtures/prices/models_dev.json"), 1)
            .unwrap()
    }

    fn m(
        id: &str,
        worker: WorkerKind,
        model: &str,
        provider: &str,
        tier: Tier,
        auth: Auth,
    ) -> ModelEntry {
        ModelEntry {
            id: id.into(),
            worker,
            model: model.into(),
            provider: provider.into(),
            tier,
            max_concurrency: 1,
            auth,
            price_id: None,
            price_in: None,
            price_out: None,
            price_cache_read: None,
            price_cache_write: None,
            quota_weight: None,
        }
    }

    #[test]
    fn models_dev_and_litellm_parse_to_the_same_prices() {
        let a = md();
        assert_eq!(a.models["anthropic/claude-sonnet-5-5"].input, 2.0);
        assert_eq!(
            a.models["anthropic/claude-sonnet-5-5"].cache_read,
            Some(0.2)
        );
        assert!(!a.models.contains_key("anthropic/no-cost-model"));
        let l = PriceTable::from_litellm(include_str!("../tests/fixtures/prices/litellm.json"), 1)
            .unwrap();
        assert!((l.models["anthropic/claude-sonnet-5-5"].output - 10.0).abs() < 1e-9);
        assert!((l.models["openai/gpt-6-luna"].input - 0.4).abs() < 1e-9);
        assert!(PriceTable::from_models_dev("<html>", 1).is_err());
        assert!(PriceTable::from_models_dev("{}", 1).is_err());
    }

    #[test]
    fn claude_aliases_resolve_to_the_newest_model_of_the_family() {
        let t = md();
        let opus = m(
            "o",
            WorkerKind::ClaudeCode,
            "opus",
            "",
            Tier::Frontier,
            Auth::ApiKey,
        );
        assert_eq!(
            t.key_for(&opus).as_deref(),
            Some("anthropic/claude-opus-5-5")
        );
        let full = m(
            "s",
            WorkerKind::ClaudeCode,
            "claude-sonnet-5-5",
            "",
            Tier::Standard,
            Auth::ApiKey,
        );
        assert_eq!(
            t.key_for(&full).as_deref(),
            Some("anthropic/claude-sonnet-5-5")
        );
        let codex = m(
            "c",
            WorkerKind::Codex,
            "gpt-6-sol",
            "",
            Tier::Standard,
            Auth::ApiKey,
        );
        assert_eq!(t.key_for(&codex).as_deref(), Some("openai/gpt-6-sol"));
        let pi = m(
            "p",
            WorkerKind::Pi,
            "gpt-6-luna",
            "openai-codex",
            Tier::Fast,
            Auth::Subscription,
        );
        assert_eq!(t.key_for(&pi).as_deref(), Some("openai/gpt-6-luna"));
    }

    #[test]
    fn overrides_win_price_id_then_manual() {
        let t = md();
        let mut e = m(
            "o",
            WorkerKind::ClaudeCode,
            "opus",
            "",
            Tier::Frontier,
            Auth::ApiKey,
        );
        e.price_id = Some("anthropic/claude-opus-5".into());
        assert_eq!(price_of(&e, &t).unwrap().input, 5.0);
        e.price_in = Some(1.0);
        e.price_out = Some(2.0);
        let p = price_of(&e, &t).unwrap();
        assert_eq!((p.input, p.output), (1.0, 2.0));
    }

    /// Plan review focus 1.
    #[test]
    fn unknown_models_have_no_price() {
        let t = md();
        let e = m(
            "x",
            WorkerKind::Codex,
            "gpt-typo",
            "",
            Tier::Standard,
            Auth::ApiKey,
        );
        assert_eq!(price_of(&e, &t), None);
    }

    #[test]
    fn quota_weight_is_relative_price_within_the_vendor() {
        let t = md();
        let cat = vec![
            m(
                "s",
                WorkerKind::ClaudeCode,
                "sonnet",
                "",
                Tier::Standard,
                Auth::Subscription,
            ),
            m(
                "o",
                WorkerKind::ClaudeCode,
                "opus",
                "",
                Tier::Frontier,
                Auth::Subscription,
            ),
        ];
        assert_eq!(quota_weight(&cat[0], &cat, &t), 1.0);
        // (3*4+20)/4 = 8 against (3*2+10)/4 = 4.
        assert_eq!(quota_weight(&cat[1], &cat, &t), 2.0);
        let mut o = cat[1].clone();
        o.quota_weight = Some(7.5);
        assert_eq!(quota_weight(&o, &cat, &t), 7.5);
        assert_eq!(blended(&t.models["anthropic/claude-opus-5-5"]), 8.0);
    }

    #[tokio::test]
    async fn a_fresh_cache_is_used_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::new(dir.path());
        let t = PriceTable {
            source: "cache".into(),
            fetched_at: 1000,
            models: md().models,
        };
        std::fs::write(paths.prices(), serde_json::to_string(&t).unwrap()).unwrap();
        let got = load(
            &paths,
            ["http://127.0.0.1:9/none", "http://127.0.0.1:9/none"],
            1000 + 60,
        )
        .await;
        assert_eq!(got.source, "cache");
    }

    #[tokio::test]
    async fn a_stale_cache_is_refreshed_from_models_dev() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../tests/fixtures/prices/models_dev.json")),
            )
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::new(dir.path());
        let got = load(&paths, [&server.uri(), "http://127.0.0.1:9/none"], 100_000).await;
        assert_eq!(got.source, "models.dev");
        assert_eq!(cached(&paths).unwrap().fetched_at, 100_000);
    }

    /// Plan review focus 2.
    #[tokio::test]
    async fn a_broken_feed_falls_back_and_keeps_the_cache() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let broken = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .mount(&broken)
            .await;
        let lite = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(include_str!("../tests/fixtures/prices/litellm.json")),
            )
            .mount(&lite)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::new(dir.path());
        assert_eq!(
            load(&paths, [&broken.uri(), &lite.uri()], 1).await.source,
            "litellm"
        );
        // Both broken: keep the (stale) cache, never overwrite it with junk.
        let stale = cached(&paths).unwrap();
        let got = load(&paths, [&broken.uri(), &broken.uri()], 10_000_000).await;
        assert_eq!(got, stale);
        assert_eq!(cached(&paths).unwrap(), stale);
        // No cache at all: the built-in snapshot.
        let empty = tempfile::tempdir().unwrap();
        let got = load(
            &crate::paths::Paths::new(empty.path()),
            [&broken.uri(), &broken.uri()],
            1,
        )
        .await;
        assert_eq!(got.source, "snapshot");
        assert!(!got.models.is_empty());
    }

    /// Live, ignored by default: the real feed resolves opus and sonnet.
    #[tokio::test]
    #[ignore]
    async fn live_models_dev_resolves_claude_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let t = load(
            &crate::paths::Paths::new(dir.path()),
            [MODELS_DEV_URL, LITELLM_URL],
            crate::store::now(),
        )
        .await;
        assert_eq!(t.source, "models.dev");
        assert!(
            t.models
                .keys()
                .any(|k| k.starts_with("anthropic/claude-opus-"))
        );
    }
}
