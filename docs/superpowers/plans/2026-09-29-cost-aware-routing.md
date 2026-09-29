# Cost-Aware Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route every stage to the cheapest model that meets the tier Jev's answers require. Prices are fetched automatically. Every stage records its real cost. Provefab Pro reports costs.

**Architecture:**
- **New modules in the core.** `prices.rs` parses models.dev and LiteLLM, resolves a catalog entry's price, and loads the price table from a daily cache, falling back to a built-in snapshot. `routing.rs` sorts the catalog by the `[routing] prefer` policy and price, so the unchanged `select` picks the first usable model. `cost.rs` turns a stage's usage into dollars or quota units.
- **Jev.** The classification call gains two questions that lower plan and review tiers.
- **Workers.** They report cache tokens and the actual model; migration 0003 stores them.
- **Pro.** It adds a `costs` report over the core store.

**Tech Stack:**
- Rust 2024 (MSRV 1.96), sqlx 0.9 on SQLite;
- `reqwest` 0.13.5, already in the tree via the `jev` crate; `wiremock` 0.6.5 for tests;
- Astro 7.3.5 and Starlight 0.42.4 for the landing and docs.

**Spec:** `docs/specs/2026-09-29-cost-aware-routing-design.md` (D69 to D75).

## Global Constraints

- Each task ends green:
  - in `~/Projects/provefab/provefab` and `~/Projects/provefab/provefab-pro`: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings` and `cargo nextest run`;
  - in `~/Projects/provefab/landing`: `pnpm test`.
- No test touches the network; HTTP goes through wiremock. The only live test is `#[ignore]`.
- **Prices:**
  - USD per million tokens;
  - blended price `(3 × input + output) / 4`;
  - quota weight default = blended price ÷ cheapest blended price of the same vendor, rounded to 1 decimal, minimum 1.0.
- **Price sources:**
  - primary `https://models.dev/api.json`;
  - fallback `https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json`;
  - cache `$PROVEFAB_HOME/prices.json`, refreshed when older than 86 400 s.
- **Tier thresholds:**
  - `plan_depth < 1.5` → plan at the implement tier;
  - `review_risk < 1.5` → review at the implement tier;
  - `review_risk ≥ 3.0` → review at Frontier;
  - otherwise the previous rules apply.
- **Policy:** `[routing] prefer` ∈ `subscription` (default), `api_key`, `cheapest`. A model is never chosen below the required tier. Cross-review keeps priority over price.
- **Migrations:** 0001 and 0002 stay byte-for-byte frozen. 0003 is new, and its checksum is added to `migrations_are_frozen` once written.
- **Paid code:** cost reports live only in `provefab-pro`. The public tree gains no paid symbol, and the `no_paid_code` test and `tools/audit-public.sh` stay green.
- **Copy:** English in code and docs, no em-dashes in user-facing text.

## Review Focus

1. **A catalog model whose price cannot be resolved** (a typo, or a model models.dev does not list). It must still be usable, ranked after priced models, and `doctor` must say so. It must never be dropped or crash routing. Test: Task 1 `unknown_models_have_no_price`; Task 4 `unpriced_models_rank_last`.
2. **A malformed or truncated price feed** (HTML error page, JSON without costs). The loader must fall back to LiteLLM, then the cache, then the snapshot, and never write a broken cache. Test: Task 2 `a_broken_feed_falls_back_and_keeps_the_cache`.
3. **Old verdicts stored before this change,** which have no `plan_depth` or `review_risk`. They must still deserialize and give today's tiers. Test: Task 3 `old_verdicts_keep_todays_tiers`.
4. **A stage that reports no usage** (crash, timeout). Its cost must be recorded as 0 or absent, never negative or NaN, and totals must still add up. Test: Task 5 `a_stage_without_usage_costs_nothing`.
5. **Codex reports cached tokens inside `input_tokens`.** They must not be counted twice: cache is priced once, the rest at the input price. Test: Task 5 `codex_cached_input_is_not_counted_twice`.

---

### Task 1: Price parsing and resolution (`prices.rs`)

**Files:**
- Create: `crates/provefab/src/prices.rs`, `crates/provefab/tests/fixtures/prices/models_dev.json`, `crates/provefab/tests/fixtures/prices/litellm.json`
- Modify: `crates/provefab/src/lib.rs` (`pub mod prices;`), `crates/provefab/src/config.rs` (`ModelEntry` gains `price_id`, `price_in`, `price_out`, `price_cache_read`, `price_cache_write`, `quota_weight`, all `Option`, `#[serde(default)]`)
- Test: unit tests in `prices.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price { pub input: f64, pub output: f64, pub cache_read: Option<f64>, pub cache_write: Option<f64>, pub release_date: Option<String> }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PriceTable { pub source: String, pub fetched_at: i64, pub models: BTreeMap<String, Price> } // key "provider/model"
impl PriceTable {
    pub fn from_models_dev(json: &str, fetched_at: i64) -> Result<Self, String>;
    pub fn from_litellm(json: &str, fetched_at: i64) -> Result<Self, String>;
    pub fn key_for(&self, m: &ModelEntry) -> Option<String>;
}
pub fn blended(p: &Price) -> f64;
pub fn price_of(m: &ModelEntry, table: &PriceTable) -> Option<Price>;          // manual > price_id > rule
pub fn price_by_model_id(model: &str, table: &PriceTable) -> Option<Price>;    // actual model reported by a worker
pub fn quota_weight(m: &ModelEntry, catalog: &[ModelEntry], table: &PriceTable) -> f64;
pub fn is_api(m: &ModelEntry) -> bool;  // Pi, or auth = api_key
```

- [ ] **Step 1: Fixtures**

`tests/fixtures/prices/models_dev.json` (trimmed models.dev shape):

```json
{
  "anthropic": { "models": {
    "claude-opus-5-5":   { "cost": { "input": 4, "output": 20, "cache_read": 0.4, "cache_write": 5 }, "release_date": "2026-09-22" },
    "claude-opus-5":     { "cost": { "input": 5, "output": 25 }, "release_date": "2026-05-01" },
    "claude-sonnet-5-5": { "cost": { "input": 2, "output": 10, "cache_read": 0.2, "cache_write": 2.5 }, "release_date": "2026-09-28" },
    "claude-haiku-5":    { "cost": { "input": 0.5, "output": 2.5 }, "release_date": "2026-07-01" },
    "no-cost-model":     { "release_date": "2026-01-01" }
  }},
  "openai": { "models": {
    "gpt-6-sol":  { "cost": { "input": 3, "output": 12, "cache_read": 0.3 }, "release_date": "2026-09-22" },
    "gpt-6-luna": { "cost": { "input": 0.4, "output": 1.6 }, "release_date": "2026-09-23" }
  }},
  "deepinfra": { "models": { "x": { "cost": { "input": 1, "output": 1 } } } }
}
```

`tests/fixtures/prices/litellm.json`:

```json
{
  "claude-sonnet-5-5": { "input_cost_per_token": 2e-06, "output_cost_per_token": 1e-05, "cache_read_input_token_cost": 2e-07, "litellm_provider": "anthropic" },
  "gpt-6-luna": { "input_cost_per_token": 4e-07, "output_cost_per_token": 1.6e-06, "litellm_provider": "openai" },
  "sample_spec": { "input_cost_per_token": 0 }
}
```

- [ ] **Step 2: Write the failing tests**

In `prices.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Auth, ModelEntry, WorkerKind};
    use crate::task::Tier;

    fn md() -> PriceTable {
        PriceTable::from_models_dev(include_str!("../tests/fixtures/prices/models_dev.json"), 1).unwrap()
    }
    fn m(id: &str, worker: WorkerKind, model: &str, provider: &str, tier: Tier, auth: Auth) -> ModelEntry {
        ModelEntry { id: id.into(), worker, model: model.into(), provider: provider.into(), tier, max_concurrency: 1, auth,
            price_id: None, price_in: None, price_out: None, price_cache_read: None, price_cache_write: None, quota_weight: None }
    }

    #[test]
    fn models_dev_and_litellm_parse_to_the_same_prices() {
        let a = md();
        assert_eq!(a.models["anthropic/claude-sonnet-5-5"].input, 2.0);
        assert_eq!(a.models["anthropic/claude-sonnet-5-5"].cache_read, Some(0.2));
        assert!(!a.models.contains_key("anthropic/no-cost-model"));
        let l = PriceTable::from_litellm(include_str!("../tests/fixtures/prices/litellm.json"), 1).unwrap();
        assert!((l.models["anthropic/claude-sonnet-5-5"].output - 10.0).abs() < 1e-9);
        assert!((l.models["openai/gpt-6-luna"].input - 0.4).abs() < 1e-9);
        assert!(PriceTable::from_models_dev("<html>", 1).is_err());
        assert!(PriceTable::from_models_dev("{}", 1).is_err());
    }

    #[test]
    fn claude_aliases_resolve_to_the_newest_model_of_the_family() {
        let t = md();
        let opus = m("o", WorkerKind::ClaudeCode, "opus", "", Tier::Frontier, Auth::ApiKey);
        assert_eq!(t.key_for(&opus).as_deref(), Some("anthropic/claude-opus-5-5"));
        let full = m("s", WorkerKind::ClaudeCode, "claude-sonnet-5-5", "", Tier::Standard, Auth::ApiKey);
        assert_eq!(t.key_for(&full).as_deref(), Some("anthropic/claude-sonnet-5-5"));
        let codex = m("c", WorkerKind::Codex, "gpt-6-sol", "", Tier::Standard, Auth::ApiKey);
        assert_eq!(t.key_for(&codex).as_deref(), Some("openai/gpt-6-sol"));
        let pi = m("p", WorkerKind::Pi, "gpt-6-luna", "openai-codex", Tier::Fast, Auth::Subscription);
        assert_eq!(t.key_for(&pi).as_deref(), Some("openai/gpt-6-luna"));
    }

    #[test]
    fn overrides_win_price_id_then_manual() {
        let t = md();
        let mut e = m("o", WorkerKind::ClaudeCode, "opus", "", Tier::Frontier, Auth::ApiKey);
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
        let e = m("x", WorkerKind::Codex, "gpt-typo", "", Tier::Standard, Auth::ApiKey);
        assert_eq!(price_of(&e, &t), None);
    }

    #[test]
    fn quota_weight_is_relative_price_within_the_vendor() {
        let t = md();
        let cat = vec![
            m("s", WorkerKind::ClaudeCode, "sonnet", "", Tier::Standard, Auth::Subscription),
            m("o", WorkerKind::ClaudeCode, "opus", "", Tier::Frontier, Auth::Subscription),
        ];
        assert_eq!(quota_weight(&cat[0], &cat, &t), 1.0);
        assert_eq!(quota_weight(&cat[1], &cat, &t), 2.0); // (3*4+20)/4 = 8 vs (3*2+10)/4 = 4
        let mut o = cat[1].clone();
        o.quota_weight = Some(7.5);
        assert_eq!(quota_weight(&o, &cat, &t), 7.5);
        assert_eq!(blended(&t.models["anthropic/claude-opus-5-5"]), 8.0);
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo nextest run -p provefab prices`
Expected: compile errors (`prices` module and `ModelEntry` fields missing).

- [ ] **Step 4: Implement**

- **Config fields:** add the six `Option` fields to `ModelEntry` with `#[serde(default)]`. Then fix every `ModelEntry { .. }` literal: `agents.rs` tests, `router.rs` tests, and anything `cargo build --all-targets` reports. Add `price_id: None, price_in: None, price_out: None, price_cache_read: None, price_cache_write: None, quota_weight: None`.
- **`prices.rs`:**
  - `from_models_dev` parses `serde_json::Value`. For each provider object with a `models` map, keep entries that have `cost.input` and `cost.output`, with key `"{provider}/{model}"`. It errors when the JSON is not an object or yields no priced model.
  - `from_litellm` reads each entry that has `litellm_provider` and both `*_cost_per_token` fields. Key: `"{litellm_provider}/{name}"`. Prices: `per_token × 1e6`, and `cache_read_input_token_cost` / `cache_creation_input_token_cost` likewise.
  - `key_for`:
    - Claude Code with an alias `opus|sonnet|haiku`: among keys starting `anthropic/claude-{alias}-`, the one with the greatest `release_date`; ties go to the shortest key.
    - Claude Code otherwise: `anthropic/{model}`.
    - Codex: `openai/{model}`.
    - Pi: `{provider}/{model}` if present; otherwise the unique key ending with `/{model}`.
    - Return `None` when the key is absent.
  - `price_of`:
    - if `price_in` and `price_out` are set, `Price` from the manual fields (cache fields optional, `release_date: None`);
    - else `price_id` looked up;
    - else `key_for`.
  - `price_by_model_id(model)`: the unique key ending with `/{model}`.
  - `quota_weight`:
    - the manual value if set;
    - else blended price over the minimum blended price among catalog entries of the same vendor (`provider_key()`) that have a price, rounded with `(x * 10.0).round() / 10.0`, `max(1.0)`;
    - `1.0` when unpriced.
  - `is_api`: `m.worker == WorkerKind::Pi || m.auth == Auth::ApiKey`.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p provefab prices && cargo nextest run`
Expected: 5 new tests pass, and the whole suite is green.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: price table from models.dev and LiteLLM, price resolution and quota weights (D71, D72)"
```

---

### Task 2: Price loading, cache and snapshot

**Files:**
- Create: `crates/provefab/src/prices_snapshot.json`
- Modify: `crates/provefab/src/prices.rs` (loader), `crates/provefab/Cargo.toml` (`reqwest = { version = "0.13.5", features = ["json"] }`), `crates/provefab/src/paths.rs` (`pub fn prices(&self) -> PathBuf { self.home.join("prices.json") }`)
- Test: tests in `prices.rs`

**Interfaces:**
- Produces:

```rust
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
pub const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";
pub const MAX_AGE: i64 = 86_400;
pub fn snapshot() -> PriceTable;                       // built into the binary
pub fn cached(paths: &Paths) -> Option<PriceTable>;    // no network
pub async fn load(paths: &Paths, urls: [&str; 2], now: i64) -> PriceTable;
```

- [ ] **Step 1: Write the failing tests**

```rust
    #[tokio::test]
    async fn a_fresh_cache_is_used_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::new(dir.path());
        let t = PriceTable { source: "cache".into(), fetched_at: 1000, models: md().models };
        std::fs::write(paths.prices(), serde_json::to_string(&t).unwrap()).unwrap();
        let got = load(&paths, ["http://127.0.0.1:9/none", "http://127.0.0.1:9/none"], 1000 + 60).await;
        assert_eq!(got.source, "cache");
    }

    #[tokio::test]
    async fn a_stale_cache_is_refreshed_from_models_dev() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(include_str!("../tests/fixtures/prices/models_dev.json")))
            .mount(&server).await;
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
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_string("<html>")).mount(&broken).await;
        let lite = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(include_str!("../tests/fixtures/prices/litellm.json")))
            .mount(&lite).await;
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::new(dir.path());
        assert_eq!(load(&paths, [&broken.uri(), &lite.uri()], 1).await.source, "litellm");
        // Both broken: keep the (stale) cache, never overwrite it with junk.
        let stale = cached(&paths).unwrap();
        let got = load(&paths, [&broken.uri(), &broken.uri()], 10_000_000).await;
        assert_eq!(got, stale);
        assert_eq!(cached(&paths).unwrap(), stale);
        // No cache at all: the built-in snapshot.
        let empty = tempfile::tempdir().unwrap();
        let got = load(&crate::paths::Paths::new(empty.path()), [&broken.uri(), &broken.uri()], 1).await;
        assert_eq!(got.source, "snapshot");
        assert!(!got.models.is_empty());
    }

    /// Live, ignored by default: the real feed resolves opus and sonnet.
    #[tokio::test]
    #[ignore]
    async fn live_models_dev_resolves_claude_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let t = load(&crate::paths::Paths::new(dir.path()), [MODELS_DEV_URL, LITELLM_URL], crate::store::now()).await;
        assert_eq!(t.source, "models.dev");
        assert!(t.models.keys().any(|k| k.starts_with("anthropic/claude-opus-")));
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p provefab prices`
Expected: compile errors (`load`, `cached`, `Paths::prices` missing).

- [ ] **Step 3: Create the snapshot**

Filter the live models.dev feed to the `anthropic` and `openai` providers, in the same JSON shape:

```bash
cd ~/Projects/provefab/provefab
curl -s https://models.dev/api.json | python3 -c 'import json,sys; d=json.load(sys.stdin); json.dump({k: d[k] for k in ("anthropic","openai")}, open("crates/provefab/src/prices_snapshot.json","w"))'
```

Check that it is small (under 200 KB) and that it contains `claude-opus-5-5`.

- [ ] **Step 4: Implement the loader**

- `snapshot()`: `PriceTable::from_models_dev(include_str!("prices_snapshot.json"), 0)` with `source = "snapshot"`.
- `cached()`: read `paths.prices()` and deserialize; `None` on any error.
- `load()`:
  1. If `cached()` is fresh (`now - fetched_at < MAX_AGE`), return it.
  2. Otherwise GET `urls[0]` with a 10 s timeout (`reqwest::Client::builder().timeout(..)`) and parse it with `from_models_dev` (`source = "models.dev"`).
  3. On any error, GET `urls[1]` and parse it with `from_litellm` (`source = "litellm"`).
  4. On success, write the cache (`write` to `prices.json.tmp`, then `rename`) and return it.
  5. If both fail, return the stale cache if present, otherwise `snapshot()`.
  6. Log one line to stderr on fallback: `provefab: prices: <why>; using <source>`.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p provefab prices && cargo nextest run`
Expected: green; the live test is skipped.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: daily price cache with LiteLLM fallback and a built-in snapshot (D71)"
```

---

### Task 3: Jev questions and tier rules

**Files:**
- Modify: `crates/provefab/src/task.rs` (`Verdict` gains `#[serde(default)] pub plan_depth: Option<f64>, #[serde(default)] pub review_risk: Option<f64>`), `crates/provefab/src/jevq.rs` (`classify_questions`, `verdict_from`, test fixture), `crates/provefab/src/router.rs` (`stage_tiers`), every `Verdict {..}` literal (`testkit.rs`, `router.rs` tests, `jevq.rs` test, `tests/pipeline.rs` ×2)
- Test: `router.rs` and `jevq.rs` unit tests

**Interfaces:**
- Consumes: `Verdict`, `stage_tiers(&Verdict) -> StageTiers`.
- Produces: `Verdict.plan_depth` and `Verdict.review_risk` (0.0..=4.0, `None` for older verdicts or when Jev leaves them out).

- [ ] **Step 1: Write the failing tests**

In the `router.rs` tests, `verdict(difficulty, confidence, scope)` becomes the base; add:

```rust
    fn v2(difficulty: f64, plan: Option<f64>, risk: Option<f64>) -> Verdict {
        Verdict { plan_depth: plan, review_risk: risk, ..verdict(difficulty, 0.9, 0.5) }
    }

    #[test]
    fn simple_plans_and_low_risk_reviews_stay_at_the_implement_tier() {
        let t = stage_tiers(&v2(2.0, Some(0.5), Some(0.5)));
        assert_eq!((t.plan, t.implement, t.review), (Tier::Standard, Tier::Standard, Tier::Standard));
        assert!(t.reasons.iter().any(|r| r.contains("plan_depth")), "{:?}", t.reasons);
        let t = stage_tiers(&v2(2.0, Some(2.0), Some(3.5)));
        assert_eq!((t.plan, t.review), (Tier::Frontier, Tier::Frontier));
        let t = stage_tiers(&v2(2.0, Some(2.0), Some(2.0)));
        assert_eq!(t.review, Tier::Frontier); // implement Standard + 1
    }

    /// Plan review focus 3.
    #[test]
    fn old_verdicts_keep_todays_tiers() {
        let old: Verdict = serde_json::from_value(serde_json::json!({
            "task_kind": "feature", "difficulty": 2.0, "difficulty_confidence": 0.9,
            "scope": 0.5, "underspecified": 0.1, "jev_model": "jev-1.13.0"
        })).unwrap();
        assert_eq!((old.plan_depth, old.review_risk), (None, None));
        let t = stage_tiers(&old);
        assert_eq!((t.plan, t.implement, t.review), (Tier::Frontier, Tier::Standard, Tier::Frontier));
    }
```

In the `jevq.rs` test that builds a mocked classify answer, add `"plan_depth": {"type":"score","score":0.5,...}` and `"review_risk": {"type":"score","score":3.2,...}` to the answers, and assert the verdict carries `Some(0.5)` and `Some(3.2)`. Also assert `classify_questions()` has both ids.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p provefab router jevq`
Expected: compile errors (fields missing).

- [ ] **Step 3: Implement**

Add the two score questions to `classify_questions()`:

```rust
    q.insert("plan_depth".into(), Question::score(
        "Does planning this change need design decisions, or only finding where to change the code?",
        ["Only locate the code to change", "A small, obvious change once located", "Some choices between approaches",
         "Design across several parts", "Deep design or research"],
    ));
    q.insert("review_risk".into(), Question::score(
        "How costly would an unnoticed subtle mistake in this change be?",
        ["Cosmetic: text, formatting, docs", "Minor: easy to notice and fix", "Moderate: a wrong result in some cases",
         "High: data loss, security, concurrency or money", "Critical: silent corruption or a security hole in production"],
    ));
```

- `verdict_from`: `plan_depth: r.score("plan_depth").ok().map(|s| s.score)`, and the same for `review_risk` (missing answers stay `None`).
- `stage_tiers`: after computing `implement`:

```rust
    let mut plan = match v.plan_depth {
        Some(d) if d < 1.5 => { reasons.push(format!("plan_depth {d:.2} < 1.5 -> plan {implement:?}")); implement }
        _ => implement.up(),
    };
    let review = match v.review_risk {
        Some(r) if r < 1.5 => { reasons.push(format!("review_risk {r:.2} < 1.5 -> review {implement:?}")); implement }
        Some(r) if r >= 3.0 => { reasons.push(format!("review_risk {r:.2} >= 3.0 -> review Frontier")); Tier::Frontier }
        _ => implement.up(),
    };
```

  The architectural-scope rule then still forces `plan = Tier::Frontier`, as today.
- Fix every `Verdict {..}` literal with `plan_depth: None, review_risk: None`.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: Jev plan_depth and review_risk lower plan and review tiers (D70)"
```

---

### Task 4: `[routing]` policy and catalog order

**Files:**
- Create: `crates/provefab/src/routing.rs`
- Modify:
  - `crates/provefab/src/config.rs`: `Config` gains `#[serde(default)] pub routing: Routing`;
  - `crates/provefab/src/lib.rs`;
  - `crates/provefab/src/pipeline.rs`: field `pub prices: std::sync::RwLock<PriceTable>`; `pick` and `claim` select from the ordered catalog; `run_stage` records a `route` output;
  - `crates/provefab/src/app.rs`: builds `prices`;
  - `crates/provefab/src/testkit.rs`: `pipeline()` sets `prices: RwLock::new(PriceTable::from_models_dev(include_str!("../tests/fixtures/prices/models_dev.json"), 1).unwrap())`.
- Test: `routing.rs` unit tests; `tests/pipeline.rs`

**Interfaces:**
- Consumes: `price_of`, `quota_weight`, `blended`, `is_api` (Task 1).
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum Prefer { #[default] Subscription, ApiKey, Cheapest }
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct Routing { #[serde(default)] pub prefer: Prefer, #[serde(default)] pub prices_url: Option<String> }
pub fn ordered(catalog: &[ModelEntry], table: &PriceTable, prefer: Prefer) -> Vec<ModelEntry>;
pub fn why(m: &ModelEntry, catalog: &[ModelEntry], table: &PriceTable, prefer: Prefer) -> String;
```

- [ ] **Step 1: Write the failing tests**

In `routing.rs`, with the Task 1 fixture and helper `m(..)`:

```rust
    #[test]
    fn subscription_first_by_quota_then_api_by_price() {
        let t = table();
        let cat = vec![
            m("opus-key", WorkerKind::ClaudeCode, "opus", "", Tier::Standard, Auth::ApiKey),
            m("sonnet-key", WorkerKind::ClaudeCode, "sonnet", "", Tier::Standard, Auth::ApiKey),
            m("opus-sub", WorkerKind::ClaudeCode, "opus", "", Tier::Standard, Auth::Subscription),
            m("sonnet-sub", WorkerKind::ClaudeCode, "sonnet", "", Tier::Standard, Auth::Subscription),
        ];
        let ids = |v: Vec<ModelEntry>| v.into_iter().map(|m| m.id).collect::<Vec<_>>();
        assert_eq!(ids(ordered(&cat, &t, Prefer::Subscription)), ["sonnet-sub", "opus-sub", "sonnet-key", "opus-key"]);
        assert_eq!(ids(ordered(&cat, &t, Prefer::ApiKey)), ["sonnet-key", "opus-key", "sonnet-sub", "opus-sub"]);
        // cheapest: subscriptions count as 0, so they still come first, in catalog order.
        assert_eq!(ids(ordered(&cat, &t, Prefer::Cheapest))[..2], ["opus-sub", "sonnet-sub"]);
    }

    /// Plan review focus 1.
    #[test]
    fn unpriced_models_rank_last() {
        let t = table();
        let cat = vec![
            m("typo", WorkerKind::Codex, "gpt-typo", "", Tier::Standard, Auth::ApiKey),
            m("luna", WorkerKind::Codex, "gpt-6-luna", "", Tier::Standard, Auth::ApiKey),
        ];
        assert_eq!(ordered(&cat, &t, Prefer::ApiKey)[0].id, "luna");
        assert!(why(&cat[0], &cat, &t, Prefer::ApiKey).contains("no price"));
    }

    #[test]
    fn routing_parses_with_defaults() {
        let c = crate::config::Config::from_toml_str("models = []\n[jev]\nmodel = \"jev-1.13.0\"\n[routing]\nprefer = \"api_key\"\n").unwrap();
        assert_eq!(c.routing.prefer, Prefer::ApiKey);
        let c = crate::config::Config::from_toml_str("models = []\n[jev]\nmodel = \"jev-1.13.0\"\n").unwrap();
        assert_eq!(c.routing.prefer, Prefer::Subscription);
    }
```

In `tests/pipeline.rs`, a pipeline-level check that the order reaches `select` without breaking cross-review:

```rust
/// Cost order applies inside the tier, but review still avoids the implementer's family.
#[tokio::test]
async fn cheapest_first_but_cross_review_still_wins() {
    let mut f = fixture(&["test -f feature.txt"]);
    // std-claude and std-codex are both Standard; make codex the cheaper subscription.
    f.config.models.iter_mut().find(|m| m.id == "std-claude").unwrap().quota_weight = Some(5.0);
    f.config.models.iter_mut().find(|m| m.id == "std-codex").unwrap().quota_weight = Some(1.0);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let stages = p.runner.stages();
    let implement = stages.iter().find(|(_, s)| s == "implement").unwrap().0.clone();
    let review = stages.iter().find(|(_, s)| s == "review").unwrap().0.clone();
    assert_eq!(implement, "std-codex");
    assert_ne!(review, "std-codex");
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.contains("routes:"), "{log}");
}
```

This test assumes the testkit's FakeOracle returns `None`, so fallback tiers put every stage at Standard. If the fixture's tiers differ, set the verdict in `FakeOracle` so that implement and review are both Standard, and record that as a ruling.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p provefab routing cheapest_first`
Expected: compile errors.

- [ ] **Step 3: Implement**

- **`ordered`:** a stable sort of a clone by `(group, cost)`.
  - `group`: 0/1 by policy (`Subscription`: subscription 0, API 1; `ApiKey`: API 0, subscription 1; `Cheapest`: all 0). "Subscription" means `!is_api(m)`.
  - `cost`: for a subscription, `quota_weight(m, catalog, table)`, or `0.0` under `Cheapest`; for an API model, `price_of(..).map(|p| blended(&p)).unwrap_or(f64::MAX)`.
  - Compare with `partial_cmp().unwrap_or(Equal)`.
- **`why`:** for example `"subscription, quota weight 1.0"`, `"API key, $4.00/M blended (anthropic/claude-sonnet-5-5)"`, or `"API key, no price (set price_id or price_in/price_out)"`.
- **Pipeline:**
  - In `pick` and `claim`, replace `&self.config.models` with `&ordered_models`, where `let ordered_models = routing::ordered(&self.config.models, &self.prices.read().unwrap_or_else(PoisonError::into_inner), self.config.routing.prefer);`. Compute it before taking the cooldown guard in `claim`.
  - In `run_stage`, before running the worker: `self.store.record_output(task.id, "route", &json!({"stage": stage_name, "model": model.id, "why": routing::why(..)})).await?;`.
- **`commands::log`:** add a `routes:` section listing `stage -> model: why` for each `route` output (`recent_outputs(id, "route", 50)`).
- **`app.rs`:** before building the pipeline, `let prices = prices::load(&paths, [config.routing.prices_url.as_deref().unwrap_or(prices::MODELS_DEV_URL), prices::LITELLM_URL], store::now()).await;`, then `prices: std::sync::RwLock::new(prices)`.

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: [routing] prefer policy orders each tier by quota weight and price (D73)"
```

---

### Task 5: Usage with cache tokens, migration 0003, cost per stage

**Files:**
- Modify:
  - `crates/agent-workers/src/types.rs`: `Usage` gains `cache_read_tokens: u64, cache_write_tokens: u64`; `StageResult` gains `pub actual_model: Option<String>`;
  - `crates/agent-workers/src/claude.rs`, `codex.rs`, `pi.rs`: parsing;
  - every `StageResult {..}` literal (`testkit.rs` `done`/`exit`, worker tests).
- Create: `crates/provefab/migrations/0003_costs.sql`, `crates/provefab/src/cost.rs`
- Modify:
  - `crates/provefab/src/store.rs`: `StageRunRecord` fields, insert and select, the frozen-checksum test;
  - `crates/provefab/src/pipeline.rs`: record the cost; add the PR body cost line;
  - `crates/provefab/src/commands.rs`: cost in `log`.
- Test: worker unit tests, `cost.rs` unit tests, `tests/pipeline.rs`

**Interfaces:**
- Produces:

```rust
// cost.rs
pub struct StageCost { pub usd: Option<f64>, pub quota_units: Option<f64> }
pub fn stage_cost(m: &ModelEntry, catalog: &[ModelEntry], usage: &Usage, actual_model: Option<&str>, table: &PriceTable) -> StageCost;
// StageRunRecord gains: cache_read_tokens: u64, cache_write_tokens: u64, actual_model: Option<String>, cost_usd: Option<f64>, quota_units: Option<f64>
```

- [ ] **Step 1: Write the failing worker tests**

- **Claude:** feed `{"type":"system","subtype":"init","model":"claude-sonnet-5-5"}` and a result with `"usage":{"input_tokens":50,"cache_creation_input_tokens":1000,"cache_read_input_tokens":20000,"output_tokens":7}`. Expect `Usage { input_tokens: 50, output_tokens: 7, cache_read_tokens: 20000, cache_write_tokens: 1000 }` and `actual_model == Some("claude-sonnet-5-5")`.
- **Codex (review focus 5):** `turn.completed` with `"usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":7}`. Expect `input_tokens: 200, cache_read_tokens: 800`.
- **Pi:** a `message_end` whose usage is `{"input":100,"output":20,"cacheRead":300,"cacheWrite":40}`. Expect the cache fields summed.
  - Before writing this one, check the field names in Pi's JSON output. Run a real stage (`pi --mode json` in a scratch dir), or read pi-mono's `Usage` type through Context7. Record the result as a ruling if the names differ.

- [ ] **Step 2: Write the failing cost tests**

In `cost.rs`, with the Task 1 fixture:

```rust
    #[test]
    fn api_cost_prices_every_token_kind() {
        let t = table();
        let e = m("s", WorkerKind::ClaudeCode, "sonnet", "", Tier::Standard, Auth::ApiKey);
        let u = Usage { input_tokens: 1_000_000, output_tokens: 100_000, cache_read_tokens: 2_000_000, cache_write_tokens: 400_000 };
        let c = stage_cost(&e, &[e.clone()], &u, Some("claude-sonnet-5-5"), &t);
        // 1*2 + 0.1*10 + 2*0.2 + 0.4*2.5 = 4.4
        assert!((c.usd.unwrap() - 4.4).abs() < 1e-9);
        assert_eq!(c.quota_units, None);
    }

    #[test]
    fn subscription_cost_is_quota_units() {
        let t = table();
        let e = m("o", WorkerKind::ClaudeCode, "opus", "", Tier::Frontier, Auth::Subscription);
        let cat = vec![m("s", WorkerKind::ClaudeCode, "sonnet", "", Tier::Standard, Auth::Subscription), e.clone()];
        let u = Usage { input_tokens: 500_000, output_tokens: 500_000, cache_read_tokens: 0, cache_write_tokens: 0 };
        let c = stage_cost(&e, &cat, &u, None, &t);
        assert_eq!(c.usd, None);
        assert!((c.quota_units.unwrap() - 2.0).abs() < 1e-9); // 1M tokens x weight 2.0
    }

    /// Plan review focus 4.
    #[test]
    fn a_stage_without_usage_costs_nothing() {
        let t = table();
        let e = m("s", WorkerKind::ClaudeCode, "sonnet", "", Tier::Standard, Auth::ApiKey);
        let c = stage_cost(&e, &[e.clone()], &Usage::default(), None, &t);
        assert_eq!(c.usd, Some(0.0));
        let unpriced = m("x", WorkerKind::Codex, "gpt-typo", "", Tier::Standard, Auth::ApiKey);
        assert_eq!(stage_cost(&unpriced, &[unpriced.clone()], &Usage::default(), None, &t).usd, None);
    }
```

And in `tests/pipeline.rs`:

```rust
#[tokio::test]
async fn stage_costs_reach_the_log_and_the_pr_body() {
    let mut f = fixture(&["test -f feature.txt"]);
    for m in &mut f.config.models { m.auth = provefab::config::Auth::ApiKey; m.price_in = Some(1.0); m.price_out = Some(1.0); }
    let p = pipeline(&f, Box::new(happy_with_usage), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.contains("cost $"), "{log}");
    let body = p.hub.pr_bodies.lock().unwrap().last().cloned().unwrap();
    assert!(body.contains("Cost: $"), "{body}");
}
```

`happy_with_usage` is `happy` with `Usage { input_tokens: 1000, output_tokens: 100, .. }` in each result. Add it to `testkit.rs` next to `happy`. If `FakeHub` does not keep PR bodies, add `pub pr_bodies: Mutex<Vec<String>>`, pushed in its `pr_create`.

- [ ] **Step 3: Run them and watch them fail**

Run: `cargo nextest run`
Expected: compile errors.

- [ ] **Step 4: Implement**

- **Workers:**
  - Claude: read `cache_creation_input_tokens` and `cache_read_input_tokens` from `/usage`, and on the `system`/`init` event keep `model` into `actual_model`.
  - Codex: `cached = usage.cached_input_tokens`; `input_tokens += input - cached`; `cache_read_tokens += cached`.
  - Pi: the cache fields as verified in Step 1.
- **`0003_costs.sql`:**

```sql
-- Cost per stage (cost-aware routing, D74).
ALTER TABLE stage_runs ADD COLUMN cache_read_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stage_runs ADD COLUMN cache_write_tokens INTEGER NOT NULL DEFAULT 0;
ALTER TABLE stage_runs ADD COLUMN actual_model TEXT;
ALTER TABLE stage_runs ADD COLUMN cost_usd REAL;
ALTER TABLE stage_runs ADD COLUMN quota_units REAL;
```

- **Store:** `StageRunRecord`, insert and select gain the five fields. Then compute the checksum with `shasum -a 384 crates/provefab/migrations/0003_costs.sql`, and append it as the third element in `migrations_are_frozen`. After that, the file is frozen.
- **`stage_cost`:**
  - API model: the price is `actual_model.and_then(|a| price_by_model_id(a, t)).or_else(|| price_of(m, t))`. Cache prices default to the input price when missing. `usd = (in×p_in + out×p_out + cr×p_cr + cw×p_cw) / 1e6`.
  - Subscription model: `quota_units = (in + out + cr + cw) / 1e6 × quota_weight(m, catalog, t)`.
  - Guard both results: return `None` for any non-finite value.
- **Pipeline:** in the two `record_stage_run` calls, fill the new fields from `usage`, `result.actual_model` and `stage_cost(..)`, using the pipeline's price table. Gate runs (the second call) have no model: zero tokens, `None` costs.
- **PR body:** after the model list, `Cost: $X.XX API · Y.Y quota units`. Sum over the task's stage runs; omit a part whose sum is 0.
- **`log`:**
  - each stage line appends ` cost $0.0123` or ` quota 0.42` when present;
  - a final line `total: $X API, Y quota units`;
  - token display becomes `tokens in/out (+cache r/w)`.

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run` in both `crates/agent-workers` and `crates/provefab` (the workspace).
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: cache tokens, actual model and cost per stage in log and PR (D74)"
```

---

### Task 6: Daily refresh and `doctor` report

**Files:**
- Modify: `crates/provefab/src/pipeline.rs` (`pub async fn refresh_prices(&self)`), `crates/provefab/src/scheduler.rs` (call at the top of each `loop` iteration), `crates/provefab/src/commands.rs` (doctor price checks)
- Test: `commands.rs` doctor test, `tests/scheduler.rs`

**Interfaces:**
- Consumes: `prices::load`, `prices::cached`, `prices::snapshot`, `price_of`.
- Produces: doctor checks named `prices` and `price <model id>`.

- [ ] **Step 1: Write the failing tests**

A doctor test, using the fake tools from the existing doctor tests. Write a cache file into the temp home with `source: "models.dev"` and the fixture models. A catalog with one priced Claude model and one `gpt-typo` Codex model then gives:
- `prices` ok, with detail containing `models.dev` and an age;
- `price <claude id>` ok, with a `$` in its detail;
- `price typo` ok, with detail containing `no price` (a warning, not a failure: the model stays usable).

For the scheduler: after one `run(.., once)` with a fresh cache, `p.prices.read()` has the cache's source. A test can seed the cache before building the pipeline.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo nextest run -p provefab doctor scheduler`
Expected: FAIL (no `prices` check).

- [ ] **Step 3: Implement**

- **`refresh_prices`:** when `now - table.fetched_at >= prices::MAX_AGE`, call `prices::load` with the configured URLs and replace the table. The loop call is cheap when fresh, because only the timestamp is compared.
- **Doctor:** use `prices::cached(paths).unwrap_or_else(prices::snapshot)`; doctor does not fetch.
  - A `prices` line: `"{source}, {age}, {n} models"`, with the age in hours, or "built-in snapshot".
  - For each catalog model, a `price <id>` line: `"$in/$out per M (key)"`, or `"no price: ranked last; set price_id or price_in/price_out"` (`ok: true`).

- [ ] **Step 4: Run the tests**

Run: `cargo nextest run`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: refresh prices daily in the service; doctor reports prices per model"
```

---

### Task 7: Pro `costs` report

**Files (in `~/Projects/provefab/provefab-pro`):**
- Create: `crates/provefab-pro/src/costs.rs`
- Modify: `crates/provefab-pro/src/lib.rs` (`pub mod costs;`), `crates/provefab-pro/src/main.rs` (an `Extra` command `costs`, with `--days <N>`, default 30, 0 meaning all time), `README.md` (the command)
- Test: unit tests in `costs.rs`

**Interfaces:**
- Consumes: `provefab::store::{Store, NewIssue, StageRunRecord}`, `Store::add_issue`, `record_stage_run`, `set_pr`, `set_pr_state`, `tasks_in`, `stage_runs`, `TaskState::ALL`.
- Produces: `pub async fn report(store: &Store, since: Option<i64>) -> Result<String, provefab::store::StoreError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn costs_per_repo_model_and_merged_pr() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db")).await.unwrap();
    let id = store.add_issue(&NewIssue { repo: "o/r".into(), number: 1, url: "u".into(), title: "t".into(), author: "a".into() }).await.unwrap().unwrap();
    for (model, usd, quota) in [("sonnet-key", Some(0.40), None), ("opus-sub", None, Some(1.5))] {
        store.record_stage_run(&StageRunRecord { task_id: id, stage: "implement".into(), model_id: model.into(), exit: "completed".into(),
            turns: 1, input_tokens: 1, output_tokens: 1, cache_read_tokens: 0, cache_write_tokens: 0, actual_model: None,
            cost_usd: usd, quota_units: quota, session_dir: "/s".into(), gate_score: None, started_at: 100, finished_at: 200 }).await.unwrap();
    }
    store.set_pr(id, "https://github.com/o/r/pull/2", "open").await.unwrap();
    store.set_pr_state(id, "merged").await.unwrap();
    let out = report(&store, None).await.unwrap();
    assert!(out.contains("o/r: $0.40 API · 1.5 quota units · 1 merged PR · $0.40 and 1.5 quota units per merged PR"), "{out}");
    assert!(out.contains("sonnet-key: $0.40") && out.contains("opus-sub: 1.5 quota units"), "{out}");
    assert!(report(&store, Some(10_000)).await.unwrap().contains("no stage runs"));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo nextest run costs`
Expected: compile error.

- [ ] **Step 3: Implement**

- **`report`:**
  - walk `tasks_in(&TaskState::ALL)` and their `stage_runs` with `started_at >= since`;
  - sum `cost_usd` and `quota_units` per repository and per `model_id`;
  - count tasks whose `pr_state` is `merged` or `done`;
  - cost per merged PR = the repo's sums over its merged tasks ÷ that count;
  - print the lines shown in the test, with a `no stage runs` line when nothing matches.
- **`main.rs`:** an `Extra` named `costs`, with arg `--days` (`clap::Arg::new("days").long("days").value_parser(clap::value_parser!(u32)).default_value("30")`). It runs `report` on a current-thread runtime against `Paths::from_env().db()`, with `since = if days == 0 { None } else { Some(now - days × 86 400) }`.
- **README:** a "Cost reports" section.

- [ ] **Step 4: Run the tests and commit**

Run: `cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo nextest run`
Expected: green.

```bash
git add -A && git commit -m "provefab-pro: costs report per repository, model and merged PR (D75)"
```

---

### Task 8: Docs, example config and landing

**Files:**
- Core: `README.md`, `docs/guide/configuration.md`, `docs/guide/usage.md`, `docs/guide/operations.md`, `provefab.example.toml`
- Landing: `site/src/components/Hero.astro` (spec row), `site/src/components/Pricing.astro` (Pro list), `site/src/components/Faq.astro`, `vendor/provefab` (submodule bump)

- [ ] **Step 1: Core docs** (spec §8)
  - **`configuration.md`:**
    - a `[routing]` section (`prefer`, `prices_url`);
    - new `[[models]]` rows: `price_id`, `price_in`, `price_out`, `price_cache_read`, `price_cache_write`, `quota_weight`;
    - a subsection "How Provefab picks a model": tiers from Jev (including `plan_depth` and `review_risk`), cheapest usable in the tier by policy, never below the tier, cross-review first, and prices from models.dev cached daily (LiteLLM fallback, built-in snapshot).
  - **`usage.md`:** reading `routes:` and costs in `provefab log`, and the `Cost:` line in PRs.
  - **`operations.md`:**
    - `~/.provefab/prices.json` in "Where things are";
    - troubleshooting rows for `price <id>: no price` (set `price_id` or manual prices) and for an offline machine (snapshot or cache used).
  - **`README.md`:** one sentence on cost-aware routing in the intro.
  - **`provefab.example.toml`:** a commented `[routing]` block and commented price fields on one model.
  - Run `cargo nextest run the_example_config` (it must still load).
  - Commit: `docs: cost-aware routing`.
- [ ] **Step 2: Landing**
  - Hero spec rows: add `["routing", "cheapest model that fits"]` after `reviews`.
  - Pro list: add `"Cost reports: cost per merged PR, per model and repository"`.
  - FAQ: `["How does Provefab keep model costs down?", "Jev rates how hard each issue is, how much planning it needs and how risky a mistake would be. Provefab then runs each stage on the cheapest model that meets that level: your plan first, then your API keys by price. Prices update daily. A stage that fails moves one tier up."]`.
  - Bump the submodule: `git -C vendor/provefab pull origin main`.
  - Run `pnpm test` (the docs pages rebuild from the new guides), then commit and push the landing repo.

---

### Task 9: Real run on the sandbox

**Files:** none. The evidence goes into the ledger.

- [ ] **Step 1:** Rebuild and reinstall the service with the new `provefab-pro` (`cargo build --release` in `provefab-pro`, then `provefab-pro service install`). `provefab-pro doctor` shows the `prices` line and a price per model.
- [ ] **Step 2:** Create a sandbox issue with label `factory` (the existing sandbox authorization covers it), for example "Add a `median()` helper", and wait until it reaches `pr_open`.
- [ ] **Step 3:** Record the evidence:
  - `provefab log <id>` shows the Jev tiers with `plan_depth` and `review_risk` reasons, the `routes:` lines, and cost per stage;
  - the PR body shows the `Cost:` line;
  - `provefab-pro costs --days 1` shows the task.

  With subscription-only models, expect quota units and no dollars.
