# Risk-Aware Policy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Classify every change into risk categories by its changed paths (built-ins plus `[repos.risk]`), add the categories' checks, force a frontier reviewer, explain it in the PR, label the issue, record it; in Pro (separate repository), refuse auto-merge for risky changes unless the repository allows the category.

**Architecture:** A pure core module `risk.rs` (glob matcher, built-in categories, typed `[repos.risk]` config, resolution, `classify`). The pipeline classifies in `gate()` right after it commits the round (the diff exists only then), runs the categories' checks as a second gates pass, writes a `risk_classified` record event, relabels the issue, and `tier_for` forces frontier review from that event. Pro reclassifies the exact head at merge time with the same core functions.

**Tech Stack:** Rust 2024, sqlx/SQLite (record events, no migration), serde/toml, clap, `cargo nextest`.

**Spec:** `docs/specs/2026-09-30-risk-policy-design.md`.

## Global Constraints

- Core repo `/Users/antoinehoriot/Projects/provefab/provefab`, branch `feature/risk-policy` (from main). Pro repo `../provefab-pro`, branch `feature/risk-policy` (from main; it already uses a temporary path dependency on the local core). Landing `../landing` main. Local commits only; never push, tag or deploy.
- Core: exactly ONE new module `crates/provefab/src/risk.rs`; no migration; no new `Hub` method (use existing `ensure_label`, `edit_labels` through `relabel`). Pro: no new module. Anything more is a STOP.
- No new dependency for glob matching.
- Category names match `[a-z0-9-]+`; `unknown` is reserved; `reviewer_tier` is `standard` or `frontier` (default `frontier`).
- Built-in categories and paths exactly as spec §3.
- Label name: `<repo.label>:risk-<category>`, color `b60205`, description `Provefab: change touches <category>`. Labels go on the ISSUE, like every Provefab label (spec amendment in Task 5).
- Record event `risk_classified`, source `fact`, schema_version 1, payload `{ pass, round, categories: [{ name, paths }] }`.
- Stage name for the extra checks: `risk-gates`.
- Wording: English; no em-dashes; never "secure"/"safe"/"proof"/"correct" guarantees.
- Commit trailer: blank line then `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Checks: core `cargo fmt -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --all-features`; Pro `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run`; landing `cd site && pnpm test`.

## Review Focus

1. **A rename out of a risky path** (`git mv migrations/0001.sql docs/old.sql`): the old path still triggers `migrations` (both sides of a rename count). Test in Task 2.
2. **A risk check that is also one of the repo gates**: it runs once, not twice (dedupe against `repo.gates`). Test in Task 2.
3. **A later round drops a category**: its issue label is removed and the frontier rule no longer applies to that round. Test in Task 2.
4. **A path with a space or unicode** (`docs/réunion notes.sql`): matched literally, no panic. Test in Task 1.
5. **`[repos.risk]` absent**: built-ins apply and behaviour for non-risky changes is byte-identical to before (same gates, same PR body sections). Test in Task 3.

---

### Task 1: `risk.rs`: matcher, built-ins, config, resolution, classification

**Files:**
- Create: `crates/provefab/src/risk.rs`
- Modify: `crates/provefab/src/lib.rs` (`pub mod risk;`), `crates/provefab/src/config.rs` (field + validation)

**Interfaces (produced):**

```rust
pub const UNKNOWN: &str = "unknown";
pub fn glob_match(pattern: &str, path: &str) -> bool;

#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskConfig {
    #[serde(default)] pub disable: Vec<String>,
    #[serde(default)] pub categories: std::collections::BTreeMap<String, CategoryConfig>,
}
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CategoryConfig {
    #[serde(default)] pub paths: Vec<String>,
    #[serde(default)] pub checks: Vec<String>,
    #[serde(default)] pub reviewer_tier: Option<String>,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Category { pub name: String, pub paths: Vec<String>, pub checks: Vec<String>, pub frontier: bool }
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Policy { pub categories: Vec<Category> }
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Detected { pub name: String, pub paths: Vec<String> }

pub fn builtins() -> Vec<Category>;
pub fn resolve(cfg: Option<&RiskConfig>) -> Result<Policy, String>;
pub fn classify(policy: &Policy, paths: &[String]) -> Vec<Detected>;
pub fn unknown() -> Vec<Detected>;                       // [Detected { name: "unknown", paths: [] }]
impl Policy {
    pub fn checks(&self, detected: &[Detected]) -> Vec<String>;      // union, category order, deduped
    pub fn needs_frontier(&self, detected: &[Detected]) -> bool;     // any detected frontier, or unknown
}
```

`RepoConfig` gains `#[serde(default)] pub risk: Option<crate::risk::RiskConfig>`. `ConfigError` gains `Risk(String /*slug*/, String /*reason*/)` with message `"{slug}: [repos.risk]: {reason}"`. `Config::validate` calls `crate::risk::resolve(r.risk.as_ref()).map_err(|e| ConfigError::Risk(r.slug.clone(), e))?` in the per-repo loop.

Rules:
- Matcher (spec §5): split pattern and path on `/`; `**` segment matches zero or more segments; within a segment `*` matches any run of characters (not `/`); everything else literal and case-sensitive; every pattern anchored at the root.

```rust
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    segs(&p, &s)
}
fn segs(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => (0..=s.len()).any(|i| segs(&p[1..], &s[i..])),
        Some(seg) => !s.is_empty() && seg_match(seg.as_bytes(), s[0].as_bytes()) && segs(&p[1..], &s[1..]),
    }
}
fn seg_match(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') => (0..=s.len()).any(|i| seg_match(&p[1..], &s[i..])),
        Some(c) => s.first() == Some(c) && seg_match(&p[1..], &s[1..]),
    }
}
```

- `builtins()`: the five spec §3 categories in table order (`ci`, `dependencies`, `migrations`, `infrastructure`, `secrets-config`), exact paths, `checks: vec![]`, `frontier: true`.
- `resolve`: start from built-ins; drop names in `disable` (unknown name → `Err("unknown category in disable: <name>")`); for each `categories` entry in name order: validate the name (`[a-z0-9-]+`, not `unknown` → `Err("invalid category name: <name>")`), non-empty paths/checks strings (`Err("<name>: empty path")` / `Err("<name>: empty check")`), `reviewer_tier` in `standard|frontier` (`Err("<name>: reviewer_tier must be standard or frontier")`); if it names a built-in that is still enabled, append its paths, set its checks, and set `frontier` from `reviewer_tier` when given; if it names a disabled built-in, `Err("<name> is disabled")`; otherwise it is a new category and must have at least one path (`Err("<name>: no paths")`), appended after the built-ins in name order.
- `classify`: for each category in policy order, the input paths (in input order, deduped) matching any of its patterns; categories with no match are omitted.

- [ ] **Step 1: Failing tests** in `risk.rs` `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn globs_match_like_the_docs_say() {
        for (p, s) in [
            ("**/migrations/**", "db/migrations/0005.sql"),
            ("**/migrations/**", "migrations/0005.sql"),
            ("**/*.sql", "schema.sql"),
            ("**/.env*", "web/.env.local"),
            ("**/*lock*.json", "web/package-lock.json"),
            ("src/auth/**", "src/auth/mod.rs"),
            (".github/**", ".github/workflows/ci.yml"),
            ("**/*.sql", "docs/réunion notes.sql"),
        ] {
            assert!(glob_match(p, s), "{p} should match {s}");
        }
        for (p, s) in [
            ("src/auth/**", "src/authz.rs"),
            (".gitlab-ci.yml", "ci/.gitlab-ci.yml"),
            ("**/*.sql", "schema.sqlx"),
            ("k8s/**", "deploy/k8s/app.yaml"),
        ] {
            assert!(!glob_match(p, s), "{p} should not match {s}");
        }
    }

    #[test]
    fn resolution_extends_disables_and_validates() {
        let cfg: RiskConfig = toml::from_str(r#"
            disable = ["dependencies"]
            [categories.auth]
            paths = ["src/auth/**"]
            checks = ["cargo test auth"]
            [categories.migrations]
            paths = ["db/schema/**"]
            checks = ["./check.sh"]
            reviewer_tier = "standard"
        "#).unwrap();
        let p = resolve(Some(&cfg)).unwrap();
        let names: Vec<_> = p.categories.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["ci", "migrations", "infrastructure", "secrets-config", "auth"]);
        let m = &p.categories[1];
        assert!(m.paths.contains(&"db/schema/**".to_string()) && m.paths.contains(&"**/*.sql".to_string()));
        assert_eq!((m.checks.clone(), m.frontier), (vec!["./check.sh".to_string()], false));
        for bad in [
            r#"disable = ["nope"]"#,
            "[categories.unknown]\npaths = [\"x\"]",
            "[categories.Auth]\npaths = [\"x\"]",
            "[categories.auth]\npaths = []",
            "[categories.auth]\npaths = [\"\"]",
            "[categories.auth]\npaths = [\"x\"]\nchecks = [\" \"]",
            "[categories.auth]\npaths = [\"x\"]\nreviewer_tier = \"fast\"",
            "disable = [\"ci\"]\n[categories.ci]\npaths = [\"x\"]",
        ] {
            let c: RiskConfig = toml::from_str(bad).unwrap();
            assert!(resolve(Some(&c)).is_err(), "{bad}");
        }
        assert!(toml::from_str::<RiskConfig>("typo = 1").is_err());
        assert_eq!(resolve(None).unwrap().categories, builtins());
    }

    #[test]
    fn classification_lists_the_matching_paths_per_category() {
        let p = resolve(None).unwrap();
        let paths: Vec<String> = ["src/lib.rs", "migrations/0005.sql", "Cargo.toml", ".github/workflows/ci.yml", "migrations/0005.sql"]
            .iter().map(|s| s.to_string()).collect();
        let d = classify(&p, &paths);
        assert_eq!(d.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["ci", "dependencies", "migrations"]);
        assert_eq!(d[2].paths, ["migrations/0005.sql"]);
        assert!(classify(&p, &["src/lib.rs".to_string()]).is_empty());
        assert!(p.needs_frontier(&d));
        assert!(p.needs_frontier(&unknown()));
        assert!(!p.needs_frontier(&[]));
    }

    #[test]
    fn checks_are_the_deduped_union_in_category_order() {
        let cfg: RiskConfig = toml::from_str("[categories.migrations]\nchecks = [\"a\", \"b\"]\n[categories.auth]\npaths = [\"src/auth/**\"]\nchecks = [\"b\", \"c\"]").unwrap();
        let p = resolve(Some(&cfg)).unwrap();
        let d = classify(&p, &["migrations/1.sql".to_string(), "src/auth/x.rs".to_string()]);
        assert_eq!(p.checks(&d), ["a", "b", "c"]);
    }
```

Config test in `config.rs` tests: a repo with `[repos.risk]\ndisable = ["nope"]` gives `Err(ConfigError::Risk(_, _))`; a repo without it parses with `risk == None`.

Run → FAIL (module missing).

- [ ] **Step 2: Implement** per the rules and code above.
- [ ] **Step 3: Run** full core checks → PASS.
- [ ] **Step 4: Commit** `risk: path matcher, built-in categories, [repos.risk] resolution and classification`.

---

### Task 2: Pipeline: classify each round, extra checks, frontier reviewer, labels, record

**Files:**
- Modify: `crates/provefab/src/record.rs` (event), `crates/provefab/src/commands.rs` (`event_summary` arm), `crates/provefab/src/pipeline.rs`
- Test: `crates/provefab/tests/risk.rs` (create)

**Interfaces:**
- Consumes: `risk::{resolve, classify, unknown, Detected, Policy}` (Task 1); `Git::changed_files`; `Store::write_with_events`, `Write::Nothing`; `give_up`; `relabel`; `hub.ensure_label`; `tier_for`; `pass_of`.
- Produces: `Event::RiskClassified { pass: u32, round: u32, categories: Vec<crate::risk::Detected> }` (kind `risk_classified`, source fact); `pub(crate) async fn risk_of_round(&self, task: &TaskRow) -> Result<Option<Vec<Detected>>, PipelineError>` returning the categories of the latest `risk_classified` event whose `pass == pass_of(task)` and `round == task.review_rounds`.

Flow inside `gate()` (after `commit_all` and the existing `changed_files` call, before `enter(Reviewing)`):
1. `let policy = risk::resolve(repo.risk.as_ref()).map_err(...)?` (config was validated at load; map an error to `give_up(NeedsYou)` with the message).
2. Paths: from the `changed_files` result already computed there: every `c.path` plus every `c.from`. If `changed_files` returned an error in that code path, use `risk::unknown()` (do not fail the round for the classification itself).
3. `detected = risk::classify(&policy, &paths)`.
4. Write `Event::RiskClassified { pass: pass_of(task), round: task.review_rounds, categories: detected.clone() }` with `write_with_events(task.id, Write::Nothing, ...)` (always, also when empty: an empty list records "classified, nothing detected").
5. Extra checks: `extra = policy.checks(&detected)` minus commands already in `Self::gate_commands(repo, plan)`. If non-empty, run `self.gates(task, &wt, &extra, "risk-gates")`; on failure, go through the same `failed_attempt(...)` path the regular gates use (same message shape, naming the failing command).
6. Frontier: if `policy.needs_frontier(&detected)` and no frontier model exists (`!self.config.models.iter().any(|m| m.tier == Tier::Frontier)`), `give_up(task, Some(repo), TaskState::NeedsYou, "a risk category requires a frontier reviewer and none is configured", <detail listing categories>)`.
7. Labels: for each detected category `name` (including `unknown`), `hub.ensure_label(slug, "<label>:risk-<name>", "b60205", "Provefab: change touches <name>")` (log and continue on error); then `relabel(task.id, slug, issue, add = current risk labels, remove = risk labels of the previous `risk_classified` event of this task that are not current)`. Skip the relabel call when both lists are empty.

`tier_for`: add before `Ok(tier)`:

```rust
        // A risky change is reviewed on the frontier tier (risk policy §7).
        if stage == Stage::Review
            && let Some(detected) = self.risk_of_round(task).await?
            && let Some(repo) = self.repo(task)
            && crate::risk::resolve(repo.risk.as_ref()).map(|p| p.needs_frontier(&detected)).unwrap_or(true)
        {
            tier = resolve(Tier::Frontier);
        }
```

- [ ] **Step 1: Failing tests** `crates/provefab/tests/risk.rs` (`#![cfg(feature = "testkit")]`, `use provefab::testkit::*;`). Write a script that, in `implement`, writes `migrations/0005.sql` (create the dir) and `feature.txt`, and approves in review:

```rust
fn risky(m: &provefab::config::ModelEntry, req: &agent_workers::StageRequest, tx: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>) -> Option<agent_workers::StageResult> {
    match stage_of(&req.prompt) {
        "implement" => {
            std::fs::create_dir_all(req.cwd.join("migrations")).unwrap();
            std::fs::write(req.cwd.join("migrations/0005.sql"), "create table t(x int);\n").unwrap();
            std::fs::write(req.cwd.join("feature.txt"), "done\n").unwrap();
            done(None)
        }
        _ => happy(m, req, tx),
    }
}
```

Tests:
- `a_migration_is_classified_checked_reviewed_on_frontier_and_labelled`: fixture gates `["test -f feature.txt"]`, `f.config.repos[0].risk = Some(toml::from_str("[categories.migrations]\nchecks = [\"test -f migrations/0005.sql\"]").unwrap())`; drive to `PrOpen`; assert: a `risk_classified` event with `categories[0].name == "migrations"` and paths `["migrations/0005.sql"]`; a stage run with stage `risk-gates`; the review stage ran on `top-claude` (`p.runner.stages()` contains `("top-claude", "review")`); `p.hub.ensured` contains `"<label>:risk-migrations"` (use the fixture's label; read it from `f.config.repos[0].label`) and a label edit added it.
- `a_failing_risk_check_goes_back_to_implementation`: checks = `["false"]`; drive; assert the task did not reach `PrOpen` on the first gate attempt (attempts > 0 or state `Implementing` after one gate; use the same assertion pattern tests/pipeline.rs uses for a failing gate) and the failure mentions `false`.
- `no_frontier_model_parks_the_task`: `f.config.models.retain(|m| m.tier != provefab::task::Tier::Frontier)`; drive; state `NeedsYou`; posted comment contains "requires a frontier reviewer".
- `a_rename_out_of_a_risky_path_still_counts` (Review Focus 1): script renames an existing committed `migrations/0001.sql` (create it in the fixture's local repo and push it first) to `docs/old.sql` via `std::fs::rename` in implement; the event lists `migrations` with path `migrations/0001.sql`.
- `a_risk_check_already_in_gates_runs_once` (Review Focus 2): gates `["test -f feature.txt"]`, migrations checks `["test -f feature.txt"]`; no `risk-gates` stage run recorded.
- `a_later_round_without_the_category_removes_its_label` (Review Focus 3): round 0 implement writes the migration, review asks for changes; round 1 implement deletes it (`std::fs::remove_file`) and review approves; the last label edit removes `<label>:risk-migrations`; the round-1 review ran on a standard model.

Run → FAIL.

- [ ] **Step 2: Implement** event variant (+ `event_summary` arm in commands.rs printing `risk_classified <names or "none">`), `risk_of_round`, the `gate()` flow, the `tier_for` rule.
- [ ] **Step 3: Run** full core checks → PASS.
- [ ] **Step 4: Commit** `risk: classify each round, run category checks, frontier review, issue labels, record event`.

---

### Task 3: PR Risk section, `log`, `doctor`

**Files:**
- Modify: `crates/provefab/src/pipeline.rs` (`pr_body`), `crates/provefab/src/commands.rs` (`doctor`)
- Test: `crates/provefab/tests/risk.rs`

**Interfaces:**
- Consumes: `risk_of_round` (Task 2), `risk::resolve`.
- Produces: `fn risk_section(policy: &Policy, detected: &[Detected], frontier: bool) -> Option<String>` (private, pure) in pipeline.rs.

Format (spec §7.3), inserted after `## Checks` and before `## Test changes to check`:

```text
## Risk

- migrations: `migrations/0005.sql`, `db/schema/users.sql` · checks added: `./scripts/check-migration.sh` · reviewer: frontier
- ci: `.github/workflows/ci.yml` · reviewer: frontier
```

Paths: up to 5 in backticks, then `and N more`. `checks added:` only when the category has checks. `reviewer: frontier` when the category (or `unknown`) forces it, else `reviewer: standard`. `unknown` line: `- unknown: the changed files could not be computed · reviewer: frontier`. No section when `detected` is empty or there is no event for the round.

`log`: the `event_summary` arm from Task 2 already shows it; add nothing else unless the arm prints only names: extend it to `risk_classified migrations (1 path), ci (1 path)`.

`doctor`: per repo push `Check { name: format!("risk {}", repo.slug), ok: true, detail }` where detail is `"<n> categories: ci, dependencies, ... ; checks: <m>"` from the resolved policy (resolution errors cannot happen: config validation already refused them).

- [ ] **Step 1: Failing tests**: unit test of `risk_section` (5+ paths truncation, checks shown only when present, unknown line, None when empty); integration: the Task 2 migration test's PR body contains `## Risk` and `- migrations: \`migrations/0005.sql\``; `pr_body_is_unchanged_without_risk` (Review Focus 5): a `happy` run (only `feature.txt`) has no `## Risk` and the same section headers as before (assert the ordered list of `## ` headers equals `["## Plan", "## Routing", "## Checks"]` or whatever the current happy PR body produces: capture it from main first and pin it); doctor test lists `risk o/r` with `5 categories`.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Run** → PASS.
- [ ] **Step 4: Commit** `risk: PR Risk section, log summary, doctor line`.

---

### Task 4: Provefab Pro part

Specified and planned in the Pro repository (private): `docs/specs/2026-09-30-risk-policy-pro-design.md` and `docs/superpowers/plans/2026-09-30-risk-policy-pro.md`. It consumes `risk::{resolve, classify}` and `RepoConfig.risk` from Task 1.

---

### Task 5: Docs, example config, landing, spec amendment

**Files:**
- Core: `docs/guide/configuration.md`, `docs/guide/usage.md`, `README.md`, `provefab.example.toml`, `docs/specs/2026-09-30-risk-policy-design.md`
- Pro: `README.md`
- Landing: `site/src/components/Pricing.astro`, `site/src/components/Faq.astro`

- [ ] **Step 1: Core docs** (match the code as built): `configuration.md` gets a `[repos.risk]` section (built-ins table, `disable`, categories, `checks`, `reviewer_tier`, pattern rules, validation errors); `usage.md` explains the per-round classification, extra checks (`risk-gates`), frontier reviewer, `needs_you` without a frontier model, the Risk section and `<label>:risk-<category>` issue labels; README one sentence; `provefab.example.toml` a commented `[repos.risk]` example.
- [ ] **Step 2: Pro README**: see the Pro plan.
- [ ] **Step 3: Landing**: Free list line `"Risk-aware checks: migrations, CI, dependencies and your own sensitive paths get stricter review"`; the Pro merge-policies line gains "risky changes never merge without a person unless you allow them"; one FAQ entry "What happens when a change touches something sensitive?" answering from the docs as built. `pnpm test` passes. Terms unchanged.
- [ ] **Step 4: Spec amendments** (marked "(amended 2026-09-30 in the plan)"): §6 classification runs in the gate step right after the round is committed (the diff exists only then) and the extra checks run as a second gates pass `risk-gates`; §7.4 labels go on the issue like every Provefab label, created on demand with `ensure_label`.
- [ ] **Step 5: Checks and commits**: core, Pro and landing checks; one commit per repo (core on `feature/risk-policy`, Pro on `feature/risk-policy`, landing main).
