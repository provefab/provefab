# Cost-aware routing: Design

- Date: 2026-09-29
- Status: draft, awaiting review
- Path: architectural (brainstorming → spec → implementation plan)
- Decisions continue at D69 (D1 to D68 are in the private design archive).

## 1. Intent

**Outcome.** Provefab spends as little as it can for each stage without going below the capability the task needs. It saves dollars on models signed in by API key and quota on models signed in by subscription. Every stage records what it really cost, so that routing can later learn from history.

**Stated by the user (2026-09-29):**
- Jev should drive the choice of model to save tokens and money.
- Save both, depending on each model's sign-in mode: dollars for API keys, quota for subscriptions.
- Approach A now, C later:
  - **A:** Jev estimates what a task needs, and a deterministic rule picks the cheapest capable model.
  - **C:** learn the best model per task from recorded history.
- Prices are fetched automatically, because they change.

**Not chosen, and why.** Jev naming the model directly (approach B):
- Jev is a classifier that does not know new models.
- Issue text can steer it (prompt injection), towards the most expensive model or the weakest.
- The choice would be hard to test or explain.

**Success criteria.**
1. With Jev available, plan and review run at the implementation tier when Jev rates planning simple or review risk low. They run at the frontier tier when review risk is high.
2. Within a tier, the chosen model follows the `[routing] prefer` policy:
   - subscription models first, lowest quota weight first;
   - then API-key models, lowest blended price first.
   The cross-review rule keeps its priority.
3. A model is never chosen below the tier the stage requires.
4. Prices refresh by themselves at most once a day from models.dev, with LiteLLM as fallback. They survive offline use through a local cache and a snapshot built into the binary. A price set by hand in `provefab.toml` always wins.
5. Every stage run records its tokens (including cache reads and writes), the model actually used, and its cost:
   - dollars for an API key;
   - quota units for a subscription.

   `provefab log`, the PR body and `provefab stats` show them.
6. No test touches the network. One ignored test fetches real prices.

**Out of scope:**
- learning from history (approach C: this design only records its data);
- per-token budgets or dollar caps (the daily stage-run budget, D53, still applies);
- currencies other than USD.

## 2. Prices

**Source.**
- `https://models.dev/api.json`, keyed by provider, then model, with `cost.input`, `cost.output`, `cost.cache_read` and `cost.cache_write` in USD per million tokens, and a `release_date`.
- Fallback: LiteLLM's `model_prices_and_context_window.json` (per-token USD costs).
- Both were checked on 2026-09-28, and they agreed: `claude-opus-5-5` at 4/20 and `claude-sonnet-5-5` at 2/10.

**Refresh.**
- The service refreshes prices when the cache is older than 24 hours, and at most once a day. The request carries no user data.
- Result: `$PROVEFAB_HOME/prices.json`, holding the source, the fetch time and the per-model prices for the providers in use.
- If the fetch fails: keep the cache. Without a cache: use the snapshot compiled into the binary, updated each release.
- `doctor` reports the source and age that apply.

**Matching a catalog entry to a price** (`price_key`):

| Worker | Rule |
|---|---|
| Claude Code | the alias (`opus`, `sonnet`, `haiku`) resolves to the newest `anthropic` model of that family by `release_date`; a full id (`claude-sonnet-5-5`) is used as is |
| Codex | `openai/<model>` |
| Pi | `<provider>/<model>` |

- The `price_id = "anthropic/claude-opus-5-5"` field overrides the rule.
- `price_in` and `price_out` (and optionally `price_cache_read` and `price_cache_write`) override the fetched prices.
- A model with no price is ranked after the priced ones, and `doctor` warns about it.

**Quota weight** (subscription models).
- Default: the model's blended price divided by the blended price of the cheapest priced model of the same vendor, rounded to one decimal, never below 1.
- This is a proxy: vendors meter subscription quota by compute, and compute follows price.
- `quota_weight` in `[[models]]` overrides it.

**Blended price** (used to compare): `(3 × input + output) / 4`, because agents read far more than they write.

## 3. Choosing tiers: two new Jev questions

Both questions are added to the existing classification call. That means one call, the same 2-second deadline and the same fallback.

- **`plan_depth`** (score 0 to 4). "Does planning this change need design decisions, or only finding where to change the code?" Anchors run from "only locate the code" to "deep design".
- **`review_risk`** (score 0 to 4). "How costly would an unnoticed subtle mistake in this change be?" Anchors run from "cosmetic" to "security, data loss, concurrency or money".

**Tier rules**, extending `stage_tiers`:
- **Implement:** unchanged, from difficulty and its confidence.
- **Plan:**
  - `plan_depth < 1.5` → implement tier;
  - otherwise implement + 1, as today;
  - architectural scope still forces Frontier.
- **Review:**
  - `review_risk < 1.5` → implement tier;
  - `review_risk ≥ 3.0` → Frontier;
  - otherwise implement + 1, as today.
- **Unchanged:**
  - the escalation ladder (retry, then one tier up);
  - the D45 rule (second correction round → one tier up);
  - the D50 rule (boosted pass at Frontier);
  - Pro's frontier second reviewer.

Jev only moves tiers inside these bounds. At worst, a crafted issue causes an under-tiered stage, which fails and escalates, or an over-tiered one, which the daily budget caps.

## 4. Choosing the model inside a tier

`select` keeps its signature and its cross-review behaviour (avoid the providers in `avoid`, relaxing from the least important). It changes only the order of candidates in the resolved tier:

1. `[routing] prefer`:
   - `"subscription"` (default): subscription models before API-key models;
   - `"api_key"`: the reverse;
   - `"cheapest"`: one list, with subscription models ranked as cost 0.
2. Subscription models: lower quota weight first.
3. API-key models: lower blended price first.
4. Ties: catalog order, as today.

Paused models (cooldowns keyed by family and sign-in mode) are skipped before ordering, as today. A tier with no usable model resolves to the nearest configured tier as today, stronger first, never weaker, unless the catalog has nothing stronger.

**Why each choice was made** is appended to the routing reasons stored with the task. For example: `review: Standard (review_risk 0.80 < 1.5)`, then `review -> sonnet-sub: subscription, quota weight 1.0, cheapest usable in Standard`.

## 5. Recording cost

- **Worker usage** gains `cache_read_tokens` and `cache_write_tokens`, and the model actually used.
  - Claude Code: from the `system/init` event (`model`) and from the result's `usage`, whose `input_tokens` excludes cache tokens. That is why today's records understate Claude's input.
  - Codex: from its usage events.
  - Pi: from its usage records.
- **Migration `0003`** adds columns to `stage_runs`: `cache_read_tokens`, `cache_write_tokens`, `actual_model`, `cost_usd` (nullable), `quota_units` (nullable). Migrations 0001 and 0002 stay byte-for-byte frozen, as a test enforces.
- **Cost of a stage:**
  - API key: `cost_usd = (input × p_in + output × p_out + cache_read × p_cr + cache_write × p_cw) / 1e6`. It is priced by the actual model when it is known, otherwise by the catalog entry.
  - Subscription: `quota_units = (total tokens / 1e6) × quota weight`.
- **Where it shows:**
  - `provefab log <task>` shows each stage's cost and the task total.
  - The PR body's "Routing" section gains `Cost: $X API · Y quota units`.
  - `provefab stats` adds, per repository and per model, dollars, quota units, and cost per merged PR.

## 6. Configuration

```toml
[routing]
prefer = "subscription"          # or "api_key", "cheapest"
# prices_url = "https://models.dev/api.json"   # override for tests or mirrors

[[models]]
id = "sonnet-key"
worker = "claude-code"
model = "sonnet"
tier = "standard"
auth = "api_key"
# price_id = "anthropic/claude-sonnet-5-5"     # when the alias match is wrong
# price_in = 2.0                                # USD per million tokens, overrides the fetch
# price_out = 10.0
# quota_weight = 1.0                            # subscription models only
```

All new fields are optional. A config without them keeps working and gets fetched prices.

## 7. Testing

- **Parsing:** a trimmed models.dev fixture and a LiteLLM fixture.
- **Price resolution:** alias → newest family model; `price_id` override; manual price override; unknown model ranked last.
- **Cache:** fresh cache used without fetching; stale cache refreshed; fetch failure → cache; no cache → built-in snapshot. HTTP goes through wiremock.
- **Tiers:** `plan_depth` and `review_risk` thresholds; Jev unavailable → fallback tiers.
- **Selection:**
  - each policy's order;
  - never below the required tier;
  - cross-review still wins over price;
  - a paused subscription falls through to the API key.
- **Cost:** Claude result with cache tokens → exact dollars; subscription → quota units; `provefab log`, PR body and `stats` lines.
- **Migration:** the checksum test covers 0003 once applied, and 0001/0002 stay frozen.
- **Live, and ignored by default:** fetch models.dev and resolve `opus` and `sonnet`.
- **Real run:** one sandbox issue whose `provefab log` shows the chosen tiers, the reasons and the stage costs.

## 8. Decisions

| ID | Decision | Why | Re-open trigger |
|---|---|---|---|
| D69 | Jev estimates needs; a deterministic rule picks the cheapest capable model (approach A); history-based routing (C) comes later on recorded data | User's choice; bounded by rules, testable, explainable, not steerable to arbitrary models by issue text | Enough recorded runs to compare success per dollar by model and task kind |
| D70 | Two Jev questions in the existing classification call: `plan_depth`, `review_risk` | Plan and review always ran one tier up; these are the largest savings; no extra call or latency | Review misses rise when review runs at the implement tier |
| D71 | Prices from models.dev (LiteLLM fallback), cached daily, snapshot in the binary, manual override wins | User asked for automatic prices; both sources are public and agreed on 2026-09-28 | A vendor publishes an official pricing API |
| D72 | Subscription quota weight defaults to relative price within the vendor | Vendors meter quota by compute; no published per-model quota figures | A vendor publishes per-model quota consumption |
| D73 | `[routing] prefer = "subscription"` by default | Subscriptions are already paid; API keys take over when a plan is paused (BYOK cooldowns) | Users on API keys only complain about the default |
| D74 | Record cache tokens, the actual model, and cost per stage in a new migration | Claude's `input_tokens` excludes cache tokens, so costs were understated; approach C needs exact data | none |
