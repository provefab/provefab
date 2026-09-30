# Evidence and Decision Record Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record every Provefab change as an append-only, source-labelled event log plus addressable findings, with human `/provefab Fn <disposition>` commands, labelled inferences, and `log` / `export` / `prune` CLI access.

**Architecture:** A new module `crates/provefab/src/record.rs` holds the typed `Event` enum, findings/command types, the command parser and export redaction. `store.rs` gains a generic transactional writer (`write_with_events`) so every existing write and its event commit together, plus findings, dispositions and inference queries. The pipeline swaps its existing writes for the transactional variant at each capture point.

**Tech Stack:** Rust 2024, sqlx 0.9 (SQLite, checksummed migrations), clap derive, tokio, `cargo nextest`. New dependency: `sha2` (export hashing).

**Spec:** `docs/specs/2026-09-30-evidence-record-design.md`. Section numbers (§3.3, §5...) refer to it.

## Global Constraints

- Work in `provefab/` (core repo), crate `crates/provefab`. Run commands from `provefab/`. Branch `feature/evidence-record` from `main`.
- Exactly ONE new migration: `crates/provefab/migrations/0005_record.sql`. Never edit 0001-0004 (published). Append its checksum to `migrations_are_frozen` (store.rs). A second migration is a STOP: ask the owner.
- Exactly ONE new module: `crates/provefab/src/record.rs`. No new `Hub` trait method. Either is a STOP.
- Events are written in the SAME SQLite transaction as the write they describe (`BEGIN IMMEDIATE`, as `transition_and` does). If either fails, both roll back.
- `change_events` is append-only: no UPDATE ever; DELETE only in `prune`.
- `seq` is gap-free per task: `COALESCE(MAX(seq), 0) + 1` inside the same immediate transaction.
- Sources are exactly `fact`, `claim`, `human`, `inferred`. Every event kind is `schema_version` 1 in v1.
- Inference rule names: `unaddressed_at_merge`, `followed_by_revert`, `followed_by_reopen`; `RULE_VERSION = 1`. At most one inferred event per (finding, rule, rule_version).
- Wording: never "proof" or "correct" in `log`, export or PR text; passing checks are "checks passed".
- No em-dashes in any user-facing text (PR body, CLI output, docs). Use `·` separators as the spec shows.
- Export default redacts free text (plan summary/steps/risks, finding text, disposition reason, ignored command line) to `{"redacted": true, "len": <chars>, "sha256": "<hex>"}`. Command output is never exported.
- Authorization for commands = the existing rule: comment author == issue author, or association in `OWNER`, `MEMBER`, `COLLABORATOR`.
- Commits end with a blank line then `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Checks before any task is done: `cargo fmt -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --all-features`.
- Recorded deviation from spec §3.3/§5: `Comment` has no URL field; the command's identity is `"<author>@<created_at>"` (field `comment`), unique per GitHub comment. Adding a URL would touch every `Comment` constructor for no v1 benefit. The spec is amended in Task 7.
- Recorded addition to spec §3.2: `findings.pass` (the task's pass number, `reopen_count + 1`) so inferences target the PR that was merged, reverted or reopened. And one event kind `command_ignored` (source `human`) to show ignored commands in `log` (spec §5 "logged locally and shown by `provefab log`").

## Review Focus

1. **A reviewer that returns zero findings** (approve with no notes): no findings rows, a `review` event with an empty key list, and no "Review notes" section in the PR. Test in Task 3.
2. **Commands in a comment that also contains prose or code** (e.g. a quoted `/provefab` inside a code fence, or `/provefab` mid-sentence): only lines that START with `/provefab` (after trimming) are commands. Test in Task 5.
3. **The same disposition command posted twice by different people, or edited later to another disposition**: each distinct comment counts once; the latest (by `created_at`) is current. Test in Task 5.
4. **`provefab export` on a database with no events** (fresh install): exits 0, prints nothing. Test in Task 6.
5. **`prune --before` with a malformed date**: a clear error, exit non-zero, nothing deleted. Test in Task 6.

---

## File map

- Create `crates/provefab/migrations/0005_record.sql`: `change_events`, `findings`.
- Create `crates/provefab/src/record.rs`: `Event`, `Source`, `Disposition`, `MergedBy`, `GateEntry`, `Rule`, `StoredEvent`, `FindingRow`, `Command`, `parse_commands`, `redact_event`, `redact_text`, `parse_date`, `RULE_VERSION`, and the `impl Pipeline` block applying commands.
- Modify `crates/provefab/src/lib.rs`: `pub mod record;`.
- Modify `crates/provefab/Cargo.toml`: `sha2 = "0.10"`.
- Modify `crates/provefab/src/store.rs`: `Write`, `write_with_events`, existing writes routed through it, review/findings/dispositions/inference/export/prune queries, `advance_post_merge` emits `post_merge`.
- Modify `crates/provefab/src/pipeline.rs`: capture points, PR body keys, command reading, closed-PR exclusion, hourly post-merge PR read.
- Modify `crates/provefab/src/commands.rs` and `crates/provefab/src/app.rs`: `log` record section, `export`, `prune`.
- Create `crates/provefab/tests/record.rs`.
- Docs: `docs/guide/usage.md`, `docs/guide/operations.md`, `README.md`, spec amendment.

---

### Task 1: Schema, event types and transactional writer

**Files:**
- Create: `crates/provefab/migrations/0005_record.sql`, `crates/provefab/src/record.rs`
- Modify: `crates/provefab/src/lib.rs`, `crates/provefab/Cargo.toml`, `crates/provefab/src/store.rs`

**Interfaces:**
- Produces (`provefab::record`): `Event` (serde-tagged enum, section below), `Event::{kind, source, schema_version}`, `Source`, `Disposition`, `MergedBy`, `GateEntry`, `Rule`, `RULE_VERSION`, `StoredEvent { id, task_id, seq, kind, source, schema_version, payload: Value, at }` with `fn typed(&self) -> Option<Event>`.
- Produces (`provefab::store`): `Write<'a>` enum, `Store::write_with_events(&self, task_id: i64, write: Write<'_>, events: &[Event]) -> Result<Vec<i64>, StoreError>`, `Store::events(&self, task_id: i64) -> Result<Vec<StoredEvent>, StoreError>`. Existing `record_stage_run`, `record_output`, `record_routing`, `set_pr`, `set_pr_state` keep their signatures and delegate to the shared SQL.

- [ ] **Step 1: Migration**

`crates/provefab/migrations/0005_record.sql`:

```sql
-- Evidence and decision record (docs/specs/2026-09-30-evidence-record-design.md, section 3).
-- change_events is append-only: rows are only deleted by `provefab prune`.
CREATE TABLE change_events (
    id             INTEGER PRIMARY KEY,
    task_id        INTEGER NOT NULL REFERENCES tasks (id),
    seq            INTEGER NOT NULL,
    kind           TEXT    NOT NULL,
    source         TEXT    NOT NULL,
    schema_version INTEGER NOT NULL,
    payload        TEXT    NOT NULL,
    at             INTEGER NOT NULL,
    UNIQUE (task_id, seq)
);
CREATE INDEX change_events_kind ON change_events (task_id, kind);

CREATE TABLE findings (
    id             INTEGER PRIMARY KEY,
    task_id        INTEGER NOT NULL REFERENCES tasks (id),
    key            TEXT    NOT NULL,
    pass           INTEGER NOT NULL,
    round          INTEGER NOT NULL,
    reviewer_model TEXT    NOT NULL,
    severity       TEXT    NOT NULL,
    file           TEXT    NOT NULL,
    line           INTEGER,
    text           TEXT    NOT NULL,
    event_id       INTEGER NOT NULL REFERENCES change_events (id),
    UNIQUE (task_id, key)
);
```

- [ ] **Step 2: Types**

Add `sha2 = "0.10"` under `[dependencies]` in `crates/provefab/Cargo.toml` and `pub mod record;` in `lib.rs` (keep the list sorted).

Create `crates/provefab/src/record.rs`:

```rust
//! The evidence and decision record
//! (docs/specs/2026-09-30-evidence-record-design.md): what Provefab observed,
//! what models claimed, what humans decided and what Provefab inferred.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Inference rules carry their version so a changed rule never mixes with old results.
pub const RULE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Fact,
    Claim,
    Human,
    Inferred,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Claim => "claim",
            Self::Human => "human",
            Self::Inferred => "inferred",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Accepted,
    Rejected,
    Fixed,
    Waived,
}

impl Disposition {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "accepted" => Some(Self::Accepted),
            "rejected" => Some(Self::Rejected),
            "fixed" => Some(Self::Fixed),
            "waived" => Some(Self::Waived),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Fixed => "fixed",
            Self::Waived => "waived",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergedBy {
    Auto,
    Human,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    UnaddressedAtMerge,
    FollowedByRevert,
    FollowedByReopen,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnaddressedAtMerge => "unaddressed_at_merge",
            Self::FollowedByRevert => "followed_by_revert",
            Self::FollowedByReopen => "followed_by_reopen",
        }
    }
}

/// One gate command's result. `output_ref` is the local session directory,
/// never the output itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateEntry {
    pub command: String,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub passed: bool,
    pub output_ref: String,
}

/// Every event kind of spec section 3.3 (schema_version 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    // fact
    Routed { tiers: Value, jev_model: Option<String>, fallback: bool },
    StageRun {
        stage: String,
        model_id: String,
        actual_model: Option<String>,
        provider: Option<String>,
        input_tokens: u64,
        output_tokens: u64,
        cache_read_tokens: u64,
        cache_write_tokens: u64,
        cost_usd: Option<f64>,
        exit: String,
    },
    GatesRun { stage: String, round: u32, results: Vec<GateEntry> },
    Reproduction { command: String, failed_before_fix: bool },
    PrOpened { url: String, head: Option<String>, base: String, pass: u32 },
    Merged { sha: Option<String>, base: Option<String>, by: MergedBy, pass: u32 },
    PostMerge { check_id: i64, state: String, failure_kind: Option<String> },
    IssueReopened { previous_pass: u32 },
    // claim
    Plan { pass: u32, summary: String, steps: Vec<String>, risks: Vec<String> },
    Review { reviewer_model: String, pass: u32, round: u32, verdict: String, findings: Vec<String> },
    // human
    FindingDisposition {
        finding: String,
        disposition: Disposition,
        reason: Option<String>,
        login: String,
        association: String,
        comment: String,
    },
    CommandIgnored { comment: String, login: String, line: String, why: String },
    // inferred
    FindingInferred { finding: String, rule: Rule, rule_version: u32 },
}

impl Event {
    /// The `kind` column. Inferred events are stored under their rule's
    /// spec name (`finding_unaddressed_at_merge`, ...).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Routed { .. } => "routed",
            Self::StageRun { .. } => "stage_run",
            Self::GatesRun { .. } => "gates_run",
            Self::Reproduction { .. } => "reproduction",
            Self::PrOpened { .. } => "pr_opened",
            Self::Merged { .. } => "merged",
            Self::PostMerge { .. } => "post_merge",
            Self::IssueReopened { .. } => "issue_reopened",
            Self::Plan { .. } => "plan",
            Self::Review { .. } => "review",
            Self::FindingDisposition { .. } => "finding_disposition",
            Self::CommandIgnored { .. } => "command_ignored",
            Self::FindingInferred { rule, .. } => match rule {
                Rule::UnaddressedAtMerge => "finding_unaddressed_at_merge",
                Rule::FollowedByRevert => "finding_followed_by_revert",
                Rule::FollowedByReopen => "finding_followed_by_reopen",
            },
        }
    }

    pub fn source(&self) -> Source {
        match self {
            Self::Plan { .. } | Self::Review { .. } => Source::Claim,
            Self::FindingDisposition { .. } | Self::CommandIgnored { .. } => Source::Human,
            Self::FindingInferred { .. } => Source::Inferred,
            _ => Source::Fact,
        }
    }

    pub fn schema_version(&self) -> u32 {
        1
    }
}

/// A row of `change_events`. `payload` is kept as JSON so a reader never
/// fails on a kind or version it does not know.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub id: i64,
    pub task_id: i64,
    pub seq: i64,
    pub kind: String,
    pub source: String,
    pub schema_version: u32,
    pub payload: Value,
    pub at: i64,
}

impl StoredEvent {
    /// `None` for an unknown kind or version: report it, never fail.
    pub fn typed(&self) -> Option<Event> {
        if self.schema_version != 1 {
            return None;
        }
        serde_json::from_value(self.payload.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_with_its_source() {
        let events = [
            Event::Plan { pass: 1, summary: "s".into(), steps: vec![], risks: vec![] },
            Event::FindingDisposition {
                finding: "F1".into(),
                disposition: Disposition::Rejected,
                reason: None,
                login: "alice".into(),
                association: "OWNER".into(),
                comment: "alice@2026-09-30T10:00:00Z".into(),
            },
            Event::FindingInferred { finding: "F1".into(), rule: Rule::FollowedByRevert, rule_version: RULE_VERSION },
            Event::IssueReopened { previous_pass: 1 },
        ];
        let sources = [Source::Claim, Source::Human, Source::Inferred, Source::Fact];
        for (e, s) in events.iter().zip(sources) {
            assert_eq!(e.source(), s);
            let v = serde_json::to_value(e).unwrap();
            assert_eq!(v["kind"], serde_json::Value::String(match e { Event::FindingInferred { .. } => "finding_inferred".into(), _ => e.kind().into() }));
            let back = StoredEvent { id: 1, task_id: 1, seq: 1, kind: e.kind().into(), source: s.as_str().into(), schema_version: 1, payload: v, at: 0 };
            assert_eq!(back.typed().as_ref(), Some(e));
        }
        assert_eq!(events[2].kind(), "finding_followed_by_revert");
    }

    #[test]
    fn an_unknown_kind_or_version_reads_as_none() {
        let e = StoredEvent { id: 1, task_id: 1, seq: 1, kind: "future".into(), source: "fact".into(), schema_version: 1, payload: serde_json::json!({"kind": "future"}), at: 0 };
        assert_eq!(e.typed(), None);
        let e = StoredEvent { schema_version: 2, payload: serde_json::json!({"kind": "issue_reopened", "previous_pass": 1}), ..e };
        assert_eq!(e.typed(), None);
    }
}
```

The payload stored in `change_events.payload` is `serde_json::to_value(event)` (it contains the serde tag `"kind"`; for inferred events the tag is `finding_inferred` while the column holds the rule-specific kind, which is what the test above asserts).

- [ ] **Step 3: Store writer, failing tests first**

In `store.rs` `mod tests`, add:

```rust
    use crate::record::{Event, MergedBy};

    #[tokio::test]
    async fn events_share_the_write_transaction_and_seq_is_gap_free() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(1)).await.unwrap().unwrap();
        let ids = s
            .write_with_events(id, Write::SetPr { url: "u", state: "open" }, &[Event::PrOpened { url: "u".into(), head: None, base: "main".into(), pass: 1 }])
            .await
            .unwrap();
        assert_eq!(ids.len(), 1);
        s.write_with_events(id, Write::Nothing, &[Event::IssueReopened { previous_pass: 1 }, Event::Merged { sha: None, base: None, by: MergedBy::Human, pass: 1 }])
            .await
            .unwrap();
        let ev = s.events(id).await.unwrap();
        assert_eq!(ev.iter().map(|e| e.seq).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(ev[0].kind, "pr_opened");
        assert_eq!(ev[0].source, "fact");
        assert_eq!(s.task(id).await.unwrap().unwrap().pr_url.as_deref(), Some("u"));
    }

    #[tokio::test]
    async fn a_failing_write_rolls_its_events_back() {
        let (_d, s) = store().await;
        // Unknown task: the UPDATE matches nothing, so the write fails.
        let r = s.write_with_events(999, Write::SetPr { url: "u", state: "open" }, &[Event::IssueReopened { previous_pass: 1 }]).await;
        assert!(matches!(r, Err(StoreError::UnknownTask(999))));
        assert!(s.events(999).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn existing_writers_still_write_without_events() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(1)).await.unwrap().unwrap();
        s.record_output(id, "plan", &json!({"a": 1})).await.unwrap();
        s.set_pr(id, "u", "open").await.unwrap();
        assert!(s.events(id).await.unwrap().is_empty());
        assert_eq!(s.last_output(id, "plan").await.unwrap(), Some(json!({"a": 1})));
    }
```

Update `migrations_are_frozen`: run it, copy the printed fifth checksum into the array.

Run: `cargo nextest run --all-features -p provefab store::tests record::tests`
Expected: FAIL (`Write`, `write_with_events`, `events` missing).

- [ ] **Step 4: Implement the writer**

In `store.rs`:

```rust
use crate::record::{Event, StoredEvent};

/// One existing write, run in the same transaction as its record events.
pub enum Write<'a> {
    Nothing,
    StageRun(&'a StageRunRecord),
    Output { kind: &'a str, value: &'a Value },
    Routing { jev_model: Option<&'a str>, verdict: Option<&'a Value>, tiers: &'a Value, reasons: &'a [String] },
    SetPr { url: &'a str, state: &'a str },
    SetPrState(&'a str),
}
```

Move the SQL bodies of `record_stage_run`, `record_output`, `record_routing`, `set_pr` and `set_pr_state` into one private `async fn exec_write(conn: &mut SqliteConnection, task_id: i64, write: &Write<'_>) -> Result<(), StoreError>` (same statements, same binds; `SetPr`/`SetPrState` return `UnknownTask(task_id)` when 0 rows change, as `update()` does). Then:

```rust
    /// Runs `write` and appends `events` in one `BEGIN IMMEDIATE`
    /// transaction (spec section 4): the record never disagrees with the
    /// tables the pipeline reads. Returns the new event ids, in order.
    pub async fn write_with_events(
        &self,
        task_id: i64,
        write: Write<'_>,
        events: &[Event],
    ) -> Result<Vec<i64>, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        exec_write(&mut tx, task_id, &write).await?;
        let mut ids = Vec::with_capacity(events.len());
        for e in events {
            ids.push(append_event(&mut tx, task_id, e).await?);
        }
        tx.commit().await?;
        Ok(ids)
    }

    pub async fn events(&self, task_id: i64) -> Result<Vec<StoredEvent>, StoreError> {
        let rows = sqlx::query("SELECT * FROM change_events WHERE task_id = ? ORDER BY seq")
            .bind(task_id)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(stored_event).collect()
    }
```

```rust
async fn append_event(conn: &mut SqliteConnection, task_id: i64, e: &Event) -> Result<i64, StoreError> {
    let payload = serde_json::to_string(e).map_err(|x| StoreError::Corrupt(x.to_string()))?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO change_events (task_id, seq, kind, source, schema_version, payload, at) \
         VALUES (?, (SELECT COALESCE(MAX(seq), 0) + 1 FROM change_events WHERE task_id = ?), ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(task_id)
    .bind(task_id)
    .bind(e.kind())
    .bind(e.source().as_str())
    .bind(e.schema_version())
    .bind(payload)
    .bind(now())
    .fetch_one(&mut *conn)
    .await?;
    Ok(id)
}

fn stored_event(r: &SqliteRow) -> Result<StoredEvent, StoreError> {
    let payload: String = r.get("payload");
    Ok(StoredEvent {
        id: r.get("id"),
        task_id: r.get("task_id"),
        seq: r.get("seq"),
        kind: r.get("kind"),
        source: r.get("source"),
        schema_version: r.get::<i64, _>("schema_version") as u32,
        payload: serde_json::from_str(&payload).map_err(|x| StoreError::Corrupt(x.to_string()))?,
        at: r.get("at"),
    })
}
```

The existing public writers become one-liners, e.g. `pub async fn record_output(&self, id: i64, kind: &str, value: &Value) -> Result<(), StoreError> { self.write_with_events(id, Write::Output { kind, value }, &[]).await.map(|_| ()) }`. `add_issue`, `transition_and` and post-merge functions are untouched in this task. Use the connection type the existing `transition_and` uses for `&mut *tx` (`SqliteConnection`); a transaction derefs to it.

- [ ] **Step 5: Run**

Run: `cargo nextest run --all-features` (whole suite: the delegation must keep every existing test green).
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/provefab/migrations/0005_record.sql crates/provefab/src/record.rs crates/provefab/src/lib.rs crates/provefab/Cargo.toml Cargo.lock crates/provefab/src/store.rs
git commit -m "record: change_events and findings tables, typed events, transactional writer

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Capture routing, stage runs, gates, reproduction, plan

**Files:**
- Modify: `crates/provefab/src/pipeline.rs`
- Test: `crates/provefab/tests/record.rs` (create)

**Interfaces:**
- Consumes: `Store::write_with_events`, `Write`, `Event::{Routed, StageRun, GatesRun, Reproduction, Plan}`, `GateEntry` (Task 1).
- Produces: `fn pass_of(task: &TaskRow) -> u32 { task.reopen_count + 1 }` in pipeline.rs (`pub(crate)`), used by Tasks 3-5.

Capture points (from the code map; confirm each by reading the surrounding function):

| Point | Current call | Replace with |
|---|---|---|
| classify, `pipeline.rs` ~702 | `self.store.record_routing(task.id, jev, verdict_json.as_ref(), &tiers_json(&tiers), &tiers.reasons)` | `write_with_events(task.id, Write::Routing { jev_model: jev, verdict: verdict_json.as_ref(), tiers: &tiers_json(&tiers), reasons: &tiers.reasons }, &[Event::Routed { tiers: tiers_json(&tiers), jev_model: jev.map(str::to_string), fallback: verdict.is_none() }])` |
| `run_stage` ~1648 | `self.store.record_stage_run(&run)` | `write_with_events(task.id, Write::StageRun(&run), &[Event::StageRun { stage, model_id, actual_model, provider: Some(model.provider_key().to_string()), tokens..., cost_usd, exit }])` (fields copied from `run`; keep it inside the existing `budget` lock) |
| `gates()` ~2001 | `self.store.record_stage_run(&run)` | `write_with_events(task.id, Write::StageRun(&run), &[Event::GatesRun { stage: stage.into(), round: task.review_rounds, results: report.results.iter().map(|r| GateEntry { command: r.command.clone(), exit: r.exit, timed_out: r.timed_out, passed: r.passed, output_ref: scratch.display().to_string() }).collect() }])` |
| `plan_ready` repro ~1947 | `record_output(task.id, "repro_result", &json!({...}))` | same output via `Write::Output` plus `Event::Reproduction { command: cmd.to_string(), failed_before_fix: genuine }` |
| plan ~1907 | `record_output(task.id, "plan", &value)` | `Write::Output { kind: "plan", value: &value }` plus `Event::Plan { pass: pass_of(task), summary: plan.summary.clone(), steps: plan.steps.clone(), risks: plan.risks.clone() }` |

If `model.provider_key()` returns `String` or `&str`, adapt the conversion; if the model entry is not in scope in `run_stage`, set `provider: None` and note it in the report.

- [ ] **Step 1: Failing test**

Create `crates/provefab/tests/record.rs`:

```rust
#![cfg(feature = "testkit")]
//! Evidence and decision record (docs/specs/2026-09-30-evidence-record-design.md).

use provefab::testkit::*;

fn kinds(events: &[provefab::record::StoredEvent]) -> Vec<String> {
    events.iter().map(|e| e.kind.clone()).collect()
}

#[tokio::test]
async fn a_pass_records_routing_stage_runs_gates_and_the_plan_in_order() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let ev = p.store.events(id).await.unwrap();
    let k = kinds(&ev);
    assert_eq!(k[0], "routed");
    for want in ["stage_run", "plan", "gates_run", "review"] {
        assert!(k.contains(&want.to_string()), "{want} missing from {k:?}");
    }
    // Every stage_runs row has exactly one stage_run or gates_run event.
    let runs = p.store.stage_runs(id).await.unwrap().len();
    let run_events = k.iter().filter(|x| *x == "stage_run" || *x == "gates_run").count();
    assert_eq!(runs, run_events);
    // seq is 1..=n.
    assert_eq!(ev.iter().map(|e| e.seq).collect::<Vec<_>>(), (1..=ev.len() as i64).collect::<Vec<_>>());
    let plan = ev.iter().find(|e| e.kind == "plan").unwrap();
    assert_eq!(plan.source, "claim");
    let gates = ev.iter().find(|e| e.kind == "gates_run").unwrap();
    assert_eq!(gates.payload["results"][0]["command"], "test -f feature.txt");
    assert_eq!(gates.payload["results"][0]["passed"], true);
}

fn bugfix(
    _: &provefab::config::ModelEntry,
    req: &agent_workers::StageRequest,
    _: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>,
) -> Option<agent_workers::StageResult> {
    match stage_of(&req.prompt) {
        "plan" => done(Some(plan_json(Some("test -f fixed.txt")))),
        "implement" => {
            std::fs::write(req.cwd.join("fixed.txt"), "ok\n").unwrap();
            done(None)
        }
        _ => done(Some(approve())),
    }
}

#[tokio::test]
async fn a_bugfix_records_whether_its_reproduction_failed_before_the_fix() {
    let f = fixture(&["test -f fixed.txt"]);
    // A bugfix verdict: build the oracle exactly as the bugfix tests in
    // tests/pipeline.rs do (search `TaskKind::Bugfix` there).
    let p = pipeline(&f, Box::new(bugfix), bugfix_oracle(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    p.drive(id).await.unwrap();
    let ev = p.store.events(id).await.unwrap();
    let r = ev.iter().find(|e| e.kind == "reproduction").expect("reproduction event");
    assert_eq!(r.payload["failed_before_fix"], true);
}
```

`review` will only appear after Task 3; for this task assert the other four kinds and add `"review"` in Task 3 (write the loop without `"review"` now). `bugfix_oracle()` is a local fn returning the `FakeOracle` the bugfix tests in `tests/pipeline.rs` construct for a `TaskKind::Bugfix` verdict (copy that construction; `testkit::verdict(TaskKind::Bugfix, 0.0)` builds the verdict).

Run: `cargo nextest run --all-features --test record`
Expected: FAIL (no events recorded).

- [ ] **Step 2: Implement the five capture points** (table above).

- [ ] **Step 3: Run**

Run: `cargo nextest run --all-features` → PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/provefab/src/pipeline.rs crates/provefab/tests/record.rs
git commit -m "record: capture routing, stage runs, gates, reproduction and plan

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Reviews, finding keys, PR body

**Files:**
- Modify: `crates/provefab/src/store.rs`, `crates/provefab/src/pipeline.rs`, `crates/provefab/src/record.rs`
- Test: `crates/provefab/tests/record.rs`

**Interfaces:**
- Produces (`record`): `FindingRow { id, task_id, key, pass: u32, round: u32, reviewer_model, severity, file, line: Option<u32>, text, event_id }`.
- Produces (`store`): `Store::record_review(&self, task_id: i64, value: &Value, reviewer_model: &str, pass: u32, round: u32, verdict: &str, findings: &[crate::stage::Finding]) -> Result<Vec<String>, StoreError>` (returns the new keys in order); `Store::findings(&self, task_id: i64) -> Result<Vec<FindingRow>, StoreError>`; `Store::review_keys(&self, event_id: i64) -> ...` is NOT needed: `pr_body` uses the keys returned for the approving review.

- [ ] **Step 1: Failing tests** (append to `tests/record.rs`)

These helpers are reused by Tasks 4 to 6; keep their names and signatures.

```rust
type P = provefab::pipeline::Pipeline<FakeRunner, FakeOracle, FakeHub>;

/// Approves with two minor findings; F1's text carries a sentinel secret for
/// the export redaction test.
fn approve_with_findings(
    m: &provefab::config::ModelEntry,
    req: &agent_workers::StageRequest,
    tx: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>,
) -> Option<agent_workers::StageResult> {
    match stage_of(&req.prompt) {
        "review" => done(Some(serde_json::json!({"verdict": "approve", "findings": [
            {"file": "src/a.rs", "line": 4, "severity": "minor", "text": "typo SENTINEL_SECRET_42"},
            {"file": "src/b.rs", "line": null, "severity": "minor", "text": "naming"}
        ]}))),
        _ => happy(m, req, tx),
    }
}

/// A task driven to PrOpen with findings F1 and F2 (pass 1).
async fn open_task_with_findings() -> (Fixture, P, i64) {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(&f, Box::new(approve_with_findings), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    (f, p, id)
}

/// `open_task_with_findings`, then merged by a person: README.md becomes
/// "broken" in one commit on origin/main (so `grep -q hello README.md`
/// post-merge checks fail and a revert is prepared).
async fn merged_task(checks: &[&str]) -> (Fixture, P, i64) {
    let mut f = fixture(&["test -f feature.txt"]);
    f.config.repos[0].post_merge_checks = checks.iter().map(|c| c.to_string()).collect();
    let p = pipeline(&f, Box::new(approve_with_findings), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let repo = f.config.repos[0].path_in(&f.home);
    std::fs::write(repo.join("README.md"), "broken\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-qm", "squash merge"]);
    git(&repo, &["push", "-q", "origin", "main"]);
    let sha = git(&repo, &["rev-parse", "HEAD"]);
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: Some("pr-head".into()),
        merge_sha: Some(sha),
        base_ref: Some("main".into()),
        commit_count: Some(1),
    };
    p.watch_pr(id).await.unwrap();
    (f, p, id)
}

fn review_with(findings: serde_json::Value, verdict: &str) -> serde_json::Value {
    serde_json::json!({"verdict": verdict, "findings": findings})
}

#[tokio::test]
async fn review_findings_get_stable_keys_across_rounds() {
    // Round 1 asks for changes with two findings, round 2 approves with one minor note.
    let f = fixture(&["test -f feature.txt"]);
    let rounds = std::sync::Mutex::new(0);
    let script = move |m: &provefab::config::ModelEntry, req: &agent_workers::StageRequest, tx: &tokio::sync::mpsc::UnboundedSender<agent_workers::WorkerEvent>| {
        match stage_of(&req.prompt) {
            "review" => {
                let mut n = rounds.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    done(Some(review_with(serde_json::json!([
                        {"file": "src/a.rs", "line": 3, "severity": "blocking", "text": "off by one"},
                        {"file": "src/b.rs", "line": null, "severity": "minor", "text": "naming"}
                    ]), "changes")))
                } else {
                    done(Some(review_with(serde_json::json!([
                        {"file": "src/a.rs", "line": 4, "severity": "minor", "text": "comment typo"}
                    ]), "approve")))
                }
            }
            _ => happy(m, req, tx),
        }
    };
    let p = pipeline(&f, Box::new(script), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let fs = p.store.findings(id).await.unwrap();
    assert_eq!(fs.iter().map(|x| x.key.as_str()).collect::<Vec<_>>(), ["F1", "F2", "F3"]);
    assert_eq!((fs[0].round, fs[2].round), (0, 1));
    assert!(fs.iter().all(|x| x.pass == 1));
    let reviews: Vec<_> = p.store.events(id).await.unwrap().into_iter().filter(|e| e.kind == "review").collect();
    assert_eq!(reviews[0].payload["findings"], serde_json::json!(["F1", "F2"]));
    assert_eq!(reviews[1].payload["findings"], serde_json::json!(["F3"]));
    let body = &p.hub.prs.lock().unwrap()[0].3;
    assert!(body.contains("F3 · minor · `src/a.rs:4` · comment typo"), "{body}");
    assert!(body.contains("Reply `/provefab F3 rejected`"), "{body}");
    assert!(!body.contains('\u{2014}'));
}

#[tokio::test]
async fn an_approval_without_findings_has_no_review_notes() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    p.drive(id).await.unwrap();
    assert!(p.store.findings(id).await.unwrap().is_empty());
    let review = p.store.events(id).await.unwrap().into_iter().find(|e| e.kind == "review").unwrap();
    assert_eq!(review.payload["findings"], serde_json::json!([]));
    assert!(!p.hub.prs.lock().unwrap()[0].3.contains("Review notes"));
}
```

If `happy`'s signature differs from `(m, req, tx)`, call it the way `tests/pipeline.rs` composes scripts; if round numbering starts at 1 in `review_rounds`, fix the asserted rounds to match the stored `task.review_rounds` at review time. Also add `"review"` to the kinds loop of Task 2's first test.

Run: `cargo nextest run --all-features --test record` → FAIL.

- [ ] **Step 2: Store**

```rust
    /// Records a review output, its `review` event and its findings with
    /// new keys, in one transaction (spec sections 3.2 and 4).
    #[allow(clippy::too_many_arguments)]
    pub async fn record_review(
        &self,
        task_id: i64,
        value: &Value,
        reviewer_model: &str,
        pass: u32,
        round: u32,
        verdict: &str,
        findings: &[crate::stage::Finding],
    ) -> Result<Vec<String>, StoreError> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        exec_write(&mut tx, task_id, &Write::Output { kind: "review", value }).await?;
        let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM findings WHERE task_id = ?")
            .bind(task_id)
            .fetch_one(&mut *tx)
            .await?;
        let keys: Vec<String> = (0..findings.len()).map(|i| format!("F{}", taken + 1 + i as i64)).collect();
        let event_id = append_event(
            &mut tx,
            task_id,
            &Event::Review { reviewer_model: reviewer_model.into(), pass, round, verdict: verdict.into(), findings: keys.clone() },
        )
        .await?;
        for (k, f) in keys.iter().zip(findings) {
            sqlx::query(
                "INSERT INTO findings (task_id, key, pass, round, reviewer_model, severity, file, line, text, event_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(task_id)
            .bind(k)
            .bind(pass)
            .bind(round)
            .bind(reviewer_model)
            .bind(match f.severity { crate::stage::Severity::Blocking => "blocking", crate::stage::Severity::Minor => "minor" })
            .bind(&f.file)
            .bind(f.line)
            .bind(&f.text)
            .bind(event_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(keys)
    }

    pub async fn findings(&self, task_id: i64) -> Result<Vec<FindingRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM findings WHERE task_id = ? ORDER BY id")
            .bind(task_id)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .iter()
            .map(|r| FindingRow {
                id: r.get("id"),
                task_id: r.get("task_id"),
                key: r.get("key"),
                pass: r.get::<i64, _>("pass") as u32,
                round: r.get::<i64, _>("round") as u32,
                reviewer_model: r.get("reviewer_model"),
                severity: r.get("severity"),
                file: r.get("file"),
                line: r.get::<Option<i64>, _>("line").map(|l| l as u32),
                text: r.get("text"),
                event_id: r.get("event_id"),
            })
            .collect())
    }
```

Add `FindingRow` to `record.rs` (derive `Debug, Clone, PartialEq, Serialize`).

- [ ] **Step 3: Pipeline**

In `review()` (~2403) replace `self.store.record_output(task.id, "review", &value)` with:

```rust
        let keys = self
            .store
            .record_review(
                task.id,
                &value,
                &model.id,
                pass_of(task),
                task.review_rounds,
                match out.verdict { ReviewVerdict::Approve => "approve", ReviewVerdict::Changes => "changes" },
                &out.findings,
            )
            .await?;
```

(`out` is the parsed `ReviewOutput`; use the real variable name.) Keep `keys` and pass them to `open_pr` / `pr_body` so "Review notes" lists `F<n>`. Change `findings_text` to take the keys:

```rust
fn findings_text(r: &ReviewOutput, keys: &[String]) -> String {
    r.findings
        .iter()
        .zip(keys)
        .map(|(f, k)| {
            let at = f.line.map(|l| format!(":{l}")).unwrap_or_default();
            let sev = match f.severity { Severity::Blocking => "blocking", Severity::Minor => "minor" };
            format!("- {k} · {sev} · `{}{at}` · {}", f.file, f.text)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

and in `pr_body` after the list:

```rust
            b.push_str(&format!(
                "\n## Review notes\n\n{}\n\nReply `/provefab {} rejected` (or accepted, fixed, waived), optionally followed by a reason, to record what you decided.\n",
                findings_text(review, keys),
                keys.first().map(String::as_str).unwrap_or("F1")
            ));
```

Other callers of `findings_text` (e.g. the implement prompt) pass keys too, or keep a keyless variant if they have none: read each call site; the `start_pass` external findings (PR comments) are NOT recorded as findings (they are human text, not model claims).

The approving review's keys must reach `pr_body`: `resume_approved` may open a PR without re-reviewing; in that path load the keys of the latest `review` event from `p.store.events(task.id)` (`payload["findings"]`).

- [ ] **Step 4: Run** `cargo nextest run --all-features` → PASS (update existing PR-body assertions in `tests/pipeline.rs` that match the old `- file:line [minor] text` format).

- [ ] **Step 5: Commit**

```bash
git add crates/provefab/src/store.rs crates/provefab/src/record.rs crates/provefab/src/pipeline.rs crates/provefab/tests/record.rs crates/provefab/tests/pipeline.rs
git commit -m "record: review events with stable finding keys, keys in the PR review notes

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: PR opened, merged, post-merge, reopen, and inferences

**Files:**
- Modify: `crates/provefab/src/store.rs`, `crates/provefab/src/pipeline.rs`
- Test: `crates/provefab/tests/record.rs`

**Interfaces:**
- Consumes: `write_with_events`, `Event::{PrOpened, Merged, PostMerge, IssueReopened, FindingInferred}`, `Rule`, `RULE_VERSION`, `pass_of`.
- Produces (`store`): `Store::write_with_inference(&self, task_id: i64, write: Write<'_>, events: &[Event], infer: Option<(Rule, u32)>) -> Result<Vec<i64>, StoreError>` (`infer` = rule and pass whose findings it applies to); `write_with_events` delegates to it with `None`.

Inference rules (inside the same transaction, after the events):
- `UnaddressedAtMerge` for pass `p`: every finding of pass `p` with no `finding_disposition` event naming it.
- `FollowedByRevert` for pass `p`: every finding of pass `p`.
- `FollowedByReopen` for pass `p`: every finding of pass `p`.
- Idempotent: skip a finding when an event exists with the rule's kind and `json_extract(payload, '$.finding') = key` and `json_extract(payload, '$.rule_version') = RULE_VERSION`.

- [ ] **Step 1: Failing tests**

Use `merged_task` and `open_task_with_findings` from Task 3.

Tests:

```rust
#[tokio::test]
async fn opening_and_merging_a_pr_are_facts_and_open_findings_are_inferred_unaddressed() {
    let (_f, p, id) = merged_task(&[]).await;
    let ev = p.store.events(id).await.unwrap();
    let opened = ev.iter().find(|e| e.kind == "pr_opened").unwrap();
    assert_eq!(opened.payload["pass"], 1);
    let merged = ev.iter().find(|e| e.kind == "merged").unwrap();
    assert_eq!(merged.payload["by"], "human");
    let inferred: Vec<_> = ev.iter().filter(|e| e.kind == "finding_unaddressed_at_merge").collect();
    assert_eq!(inferred.len(), 2);
    assert_eq!(inferred[0].source, "inferred");
    assert_eq!(inferred[0].payload["finding"], "F1");
    assert_eq!(inferred[0].payload["rule_version"], 1);
    // Replaying the merge write infers nothing new.
    p.store
        .write_with_inference(id, provefab::store::Write::Nothing, &[], Some((provefab::record::Rule::UnaddressedAtMerge, 1)))
        .await
        .unwrap();
    assert_eq!(p.store.events(id).await.unwrap().iter().filter(|e| e.kind == "finding_unaddressed_at_merge").count(), 2);
}

#[tokio::test]
async fn a_post_merge_revert_marks_the_merged_findings() {
    // post_merge_checks that fail on the merge and pass on the revert, as in
    // tests/post_merge.rs `setup(&["grep -q hello README.md"], "broken\n")`,
    // but with a finding-bearing review; drive the check to revert_open.
    let (_f, p, id) = merged_task(&["grep -q hello README.md"]).await;
    for _ in 0..12 {
        let _ = p.process_post_merge(id).await;
    }
    let ev = p.store.events(id).await.unwrap();
    assert!(ev.iter().any(|e| e.kind == "post_merge" && e.payload["state"] == "revert_open"));
    assert_eq!(ev.iter().filter(|e| e.kind == "finding_followed_by_revert").count(), 2);
}

#[tokio::test]
async fn a_reopened_issue_marks_the_previous_pass_findings() {
    let (_f, p, id) = merged_task(&[]).await;
    // Drive the reopen path the way tests/pipeline.rs does (issue seen closed
    // → pr_state done, then open again → watch_merged reopens).
    p.store.set_pr_state(id, "done").await.unwrap();
    p.hub.issue_is_open.store(true, std::sync::atomic::Ordering::SeqCst);
    p.watch_pr(id).await.unwrap();
    let ev = p.store.events(id).await.unwrap();
    let reopened = ev.iter().find(|e| e.kind == "issue_reopened").unwrap();
    assert_eq!(reopened.payload["previous_pass"], 1);
    assert_eq!(ev.iter().filter(|e| e.kind == "finding_followed_by_reopen").count(), 2);
}
```

The hourly `watched_at` throttle lives in the scheduler, not in `watch_pr`, so calling `watch_pr` reaches `watch_merged` directly; read `watch_merged` to confirm the `done` then issue-open condition.

Run: `cargo nextest run --all-features --test record` → FAIL.

- [ ] **Step 2: Store**

Rename the body of `write_with_events` into `write_with_inference` (adding the `infer` step before commit) and make `write_with_events` call it with `None`:

```rust
async fn infer(conn: &mut SqliteConnection, task_id: i64, rule: Rule, pass: u32) -> Result<(), StoreError> {
    let kind = Event::FindingInferred { finding: String::new(), rule, rule_version: RULE_VERSION }.kind();
    let keys: Vec<String> = match rule {
        Rule::UnaddressedAtMerge => sqlx::query_scalar(
            "SELECT key FROM findings f WHERE task_id = ? AND pass = ? AND NOT EXISTS (\
               SELECT 1 FROM change_events e WHERE e.task_id = f.task_id AND e.kind = 'finding_disposition' \
               AND json_extract(e.payload, '$.finding') = f.key) ORDER BY id",
        )
        .bind(task_id)
        .bind(pass)
        .fetch_all(&mut *conn)
        .await?,
        _ => sqlx::query_scalar("SELECT key FROM findings WHERE task_id = ? AND pass = ? ORDER BY id")
            .bind(task_id)
            .bind(pass)
            .fetch_all(&mut *conn)
            .await?,
    };
    for key in keys {
        let seen: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM change_events WHERE task_id = ? AND kind = ? \
             AND json_extract(payload, '$.finding') = ? AND json_extract(payload, '$.rule_version') = ?",
        )
        .bind(task_id)
        .bind(kind)
        .bind(&key)
        .bind(RULE_VERSION)
        .fetch_optional(&mut *conn)
        .await?;
        if seen.is_none() {
            append_event(conn, task_id, &Event::FindingInferred { finding: key, rule, rule_version: RULE_VERSION }).await?;
        }
    }
    Ok(())
}
```

`advance_post_merge`: run its UPDATE inside a `BEGIN IMMEDIATE` transaction; when it changed one row and `to.is_terminal()`, append `Event::PostMerge { check_id: id, state: to.as_str().into(), failure_kind: patch.failure_kind.map(|k| k.as_str().into()) }` (read the stored `failure_kind` back from the row if the patch has none), and when `to == CheckState::RevertOpen`, infer `FollowedByRevert` for the pass of the task's latest `merged` event (`SELECT json_extract(payload, '$.pass') FROM change_events WHERE task_id = ? AND kind = 'merged' ORDER BY seq DESC LIMIT 1`; skip when absent). The post-merge task id is the check's `task_id` (select it in the transaction).

- [ ] **Step 3: Pipeline**

- `open_pr` (~2681): replace `set_pr(task.id, &url, "open")` with `write_with_events(task.id, Write::SetPr { url: &url, state: "open" }, &[Event::PrOpened { url: url.clone(), head: self.git.head(wt).await.ok(), base: repo.base.clone(), pass: pass_of(task) }])`.
- `record_merge` (~1246): replace `set_pr_state(task.id, "merged")` with `write_with_inference(task.id, Write::SetPrState("merged"), &[Event::Merged { sha: merge_sha.map(str::to_string), base: base.map(str::to_string), by: if auto_merged { MergedBy::Auto } else { MergedBy::Human }, pass: pass_of(task) }], Some((Rule::UnaddressedAtMerge, pass_of(task))))`. `auto_merged` is the value `record_merge` already computes; when post-merge checks are off, compute it the same way (`last_output("auto_merged")["head"] == head`).
- Reopen in `watch_merged`: capture `let previous = pass_of(&task);` BEFORE `auto_pass(...)`, then replace `set_pr_state(task.id, "reopened")` with `write_with_inference(task.id, Write::SetPrState("reopened"), &[Event::IssueReopened { previous_pass: previous }], Some((Rule::FollowedByReopen, previous)))`.

- [ ] **Step 4: Run** `cargo nextest run --all-features` → PASS (post-merge suite included).

- [ ] **Step 5: Commit**

```bash
git add crates/provefab/src/store.rs crates/provefab/src/pipeline.rs crates/provefab/tests/record.rs
git commit -m "record: PR opened, merged, post-merge and reopen facts; labelled inferences

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Human dispositions from PR comments

**Files:**
- Modify: `crates/provefab/src/record.rs`, `crates/provefab/src/store.rs`, `crates/provefab/src/pipeline.rs`
- Test: `crates/provefab/tests/record.rs`, unit tests in `record.rs`

**Interfaces:**
- Produces (`record`): `Command { key: String, disposition: Disposition, reason: Option<String> }`; `parse_commands(body: &str) -> Vec<Result<Command, String>>` (`Err(line)` for a `/provefab` line that does not parse); `impl Pipeline { pub(crate) async fn apply_finding_commands(&self, task: &TaskRow, comments: &[Comment]) -> Result<(), PipelineError> }`.
- Produces (`store`): `Store::record_human(&self, task_id: i64, event: &Event) -> Result<bool, StoreError>` (false when already recorded: same `comment` and same `finding`/`line`); `Store::finding_keys(&self, task_id: i64) -> Result<HashSet<String>, StoreError>`; `Store::current_dispositions(&self, task_id: i64) -> Result<HashMap<String, Disposition>, StoreError>` (latest by `payload.comment` time order = event `seq`; ties impossible since each comment is one event per key).

Rules (spec section 5):
- A command is a line that, after trimming, starts with `/provefab ` (case-insensitive prefix). Grammar: `/provefab <key> <disposition>[:| ]<reason>`; key like `F12` (case-insensitive, stored upper-case); disposition one of accepted, rejected, fixed, waived; reason optional, trimmed, `None` when empty.
- Lines inside fenced code blocks (between lines starting with ```) are not commands.
- Authorized = `c.author == task.author || association ∈ {OWNER, MEMBER, COLLABORATOR}`. Unauthorized, unknown key or unparsable line → one `CommandIgnored { comment, login, line, why }` event (`why` = `"not authorized"`, `"unknown finding"`, `"not a command"`), deduplicated like dispositions.
- `comment` identity = `format!("{}@{}", c.author, c.created_at)`.
- Bot comments (`is_bot_comment`) are skipped entirely.

- [ ] **Step 1: Unit tests for the parser (record.rs)**

```rust
    #[test]
    fn commands_are_whole_lines_outside_code_fences() {
        let body = "Thanks!\n/provefab F2 rejected: the value is bounded above\n/PROVEFAB f3 Fixed\nsee /provefab F9 accepted inline\n```\n/provefab F4 waived\n```\n/provefab F5 maybe\n/provefab F6 accepted   \n";
        let got = parse_commands(body);
        assert_eq!(got.len(), 4);
        assert_eq!(got[0], Ok(Command { key: "F2".into(), disposition: Disposition::Rejected, reason: Some("the value is bounded above".into()) }));
        assert_eq!(got[1], Ok(Command { key: "F3".into(), disposition: Disposition::Fixed, reason: None }));
        assert_eq!(got[2], Err("/provefab F5 maybe".into()));
        assert_eq!(got[3], Ok(Command { key: "F6".into(), disposition: Disposition::Accepted, reason: None }));
    }
```

- [ ] **Step 2: Integration tests (tests/record.rs)**

```rust
fn comment(author: &str, association: &str, body: &str, at: &str) -> provefab::forge::Comment {
    provefab::forge::Comment { author: author.into(), association: association.into(), body: body.into(), created_at: at.into() }
}

#[tokio::test]
async fn authorized_commands_record_dispositions_once_and_the_latest_wins() {
    let (_f, p, id) = open_task_with_findings().await; // PrOpen, findings F1 and F2
    let task = p.store.task(id).await.unwrap().unwrap();
    let cs = vec![
        comment("alice", "NONE", "/provefab F1 rejected: false positive", "2026-09-30T10:00:00Z"), // issue author
        comment("mallory", "NONE", "/provefab F2 accepted", "2026-09-30T10:01:00Z"),             // not authorized
        comment("bob", "MEMBER", "/provefab F9 fixed\n/provefab F2 fixed", "2026-09-30T10:02:00Z"),
        comment("bob", "MEMBER", "/provefab F1 accepted", "2026-09-30T10:03:00Z"),
    ];
    p.apply_finding_commands(&task, &cs).await.unwrap();
    p.apply_finding_commands(&task, &cs).await.unwrap(); // replay: no duplicates
    let ev = p.store.events(id).await.unwrap();
    assert_eq!(ev.iter().filter(|e| e.kind == "finding_disposition").count(), 3);
    let ignored: Vec<_> = ev.iter().filter(|e| e.kind == "command_ignored").map(|e| e.payload["why"].as_str().unwrap().to_string()).collect();
    assert_eq!(ignored, ["not authorized", "unknown finding"]);
    let current = p.store.current_dispositions(id).await.unwrap();
    assert_eq!(current["F1"], provefab::record::Disposition::Accepted);
    assert_eq!(current["F2"], provefab::record::Disposition::Fixed);
}

#[tokio::test]
async fn open_prs_are_read_every_tick() {
    let (_f, p, id) = open_task_with_findings().await;
    let mut status = p.hub.pr_status.lock().unwrap().clone();
    status.comments = vec![comment("alice", "NONE", "/provefab F1 waived", "2026-09-30T10:00:00Z")];
    *p.hub.pr_status.lock().unwrap() = status;
    p.watch_pr(id).await.unwrap();
    assert_eq!(p.store.current_dispositions(id).await.unwrap()["F1"], provefab::record::Disposition::Waived);
}

#[tokio::test]
async fn merged_prs_are_still_read_after_the_merge() {
    let (_f, p, id) = merged_task(&[]).await;
    p.hub.pr_status.lock().unwrap().comments =
        vec![comment("alice", "NONE", "/provefab F2 accepted", "2026-09-30T11:00:00Z")];
    // pr_state is "merged": watch_pr goes through watch_merged, which reads the PR once.
    p.watch_pr(id).await.unwrap();
    assert_eq!(p.store.current_dispositions(id).await.unwrap()["F2"], provefab::record::Disposition::Accepted);
}

#[tokio::test]
async fn a_command_on_a_closed_pr_is_not_a_change_request() {
    let (_f, p, id) = open_task_with_findings().await;
    let mut status = p.hub.pr_status.lock().unwrap().clone();
    status.state = provefab::forge::PrState::Closed;
    status.comments = vec![comment("alice", "NONE", "/provefab F1 rejected", "2026-09-30T10:00:00Z")];
    *p.hub.pr_status.lock().unwrap() = status;
    let state = p.watch_pr(id).await.unwrap();
    // Only a command, no human finding: the closed PR is given up, not re-implemented.
    assert_eq!(state, Failed);
    assert!(p.store.current_dispositions(id).await.unwrap().contains_key("F1"));
}
```

Run: `cargo nextest run --all-features --test record` and `-p provefab record::tests` → FAIL.

- [ ] **Step 3: Implement**

`record.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub key: String,
    pub disposition: Disposition,
    pub reason: Option<String>,
}

pub fn parse_commands(body: &str) -> Vec<Result<Command, String>> {
    let mut out = Vec::new();
    let mut fenced = false;
    for raw in body.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced || !line.to_ascii_lowercase().starts_with("/provefab ") {
            continue;
        }
        let rest = line["/provefab ".len()..].trim();
        let mut parts = rest.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or_default().to_ascii_uppercase();
        let tail = parts.next().unwrap_or_default().trim();
        let (word, reason) = match tail.find(|c: char| c == ':' || c.is_whitespace()) {
            Some(i) => (&tail[..i], tail[i + 1..].trim()),
            None => (tail, ""),
        };
        let valid_key = key.len() > 1 && key.starts_with('F') && key[1..].chars().all(|c| c.is_ascii_digit());
        match (valid_key, Disposition::parse(word)) {
            (true, Some(d)) => out.push(Ok(Command {
                key,
                disposition: d,
                reason: (!reason.is_empty()).then(|| reason.to_string()),
            })),
            _ => out.push(Err(line.to_string())),
        }
    }
    out
}
```

`impl Pipeline` block in `record.rs` (same bounds as the `impl` in `pipeline.rs`; make `task`/`repo` helpers `pub(crate)` if needed):

```rust
    pub(crate) async fn apply_finding_commands(&self, task: &TaskRow, comments: &[Comment]) -> Result<(), PipelineError> {
        let keys = self.store.finding_keys(task.id).await?;
        for c in comments.iter().filter(|c| !is_bot_comment(&c.body)) {
            let comment = format!("{}@{}", c.author, c.created_at);
            let authorized = c.author == task.author || matches!(c.association.as_str(), "OWNER" | "MEMBER" | "COLLABORATOR");
            for parsed in parse_commands(&c.body) {
                let event = match (&parsed, authorized) {
                    (_, false) => Event::CommandIgnored { comment: comment.clone(), login: c.author.clone(), line: line_of(&parsed), why: "not authorized".into() },
                    (Err(line), true) => Event::CommandIgnored { comment: comment.clone(), login: c.author.clone(), line: line.clone(), why: "not a command".into() },
                    (Ok(cmd), true) if !keys.contains(&cmd.key) => Event::CommandIgnored { comment: comment.clone(), login: c.author.clone(), line: line_of(&parsed), why: "unknown finding".into() },
                    (Ok(cmd), true) => Event::FindingDisposition {
                        finding: cmd.key.clone(),
                        disposition: cmd.disposition,
                        reason: cmd.reason.clone(),
                        login: c.author.clone(),
                        association: c.association.clone(),
                        comment: comment.clone(),
                    },
                };
                self.store.record_human(task.id, &event).await?;
            }
        }
        Ok(())
    }
```

with `fn line_of(p: &Result<Command, String>) -> String` returning the original text (`Err(l)` → `l`, `Ok(c)` → `format!("/provefab {} {}", c.key, c.disposition.as_str())`).

`store.rs`:
- `record_human`: in one immediate transaction, check `SELECT 1 FROM change_events WHERE task_id = ? AND kind = ? AND json_extract(payload,'$.comment') = ? AND COALESCE(json_extract(payload,'$.finding'), json_extract(payload,'$.line')) = ?` (finding for dispositions, line for ignored); insert via `append_event` when absent; return whether inserted.
- `finding_keys`: `SELECT key FROM findings WHERE task_id = ?` into a `HashSet`.
- `current_dispositions`: `SELECT json_extract(payload,'$.finding'), json_extract(payload,'$.disposition') FROM change_events WHERE task_id = ? AND kind = 'finding_disposition' ORDER BY seq`, last write wins per finding.

`pipeline.rs`:
- `watch_pr` Open branch: `PrState::Open => { self.apply_finding_commands(&task, &status.comments).await?; Ok(task.state) }`.
- Closed branch: add `.filter(|c| !c.body.trim_start().to_ascii_lowercase().starts_with("/provefab "))` before building human findings, and call `apply_finding_commands` on the full comment list first.
- Merged branch and `watch_merged` (hourly after merge until archived): call `self.hub.pr_status(&repo.slug, url)` once per `watch_merged` run and `apply_finding_commands` on its comments; a `pr_status` error is logged and ignored (the next hourly run retries).

- [ ] **Step 4: Run** `cargo nextest run --all-features` → PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/provefab/src/record.rs crates/provefab/src/store.rs crates/provefab/src/pipeline.rs crates/provefab/tests/record.rs
git commit -m "record: /provefab finding commands from PR comments, authorized, deduplicated

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: `log`, `export`, `prune`

**Files:**
- Modify: `crates/provefab/src/record.rs`, `crates/provefab/src/store.rs`, `crates/provefab/src/commands.rs`, `crates/provefab/src/app.rs`
- Test: `crates/provefab/tests/record.rs`

**Interfaces:**
- Produces (`record`): `redact_text(s: &str) -> Value`; `redact_event(e: &StoredEvent) -> Value` (payload with free-text fields replaced); `parse_date(s: &str) -> Option<i64>` (`YYYY-MM-DD` → unix seconds at 00:00 UTC).
- Produces (`store`): `Store::export_rows(&self, repo: Option<&str>, since: Option<i64>) -> Result<(Vec<(TaskRow, StoredEvent)>, Vec<(TaskRow, FindingRow, Option<Disposition>)>), StoreError>`; `Store::prunable_tasks(&self, before: i64) -> Result<Vec<TaskRow>, StoreError>`; `Store::prune_record(&self, task_ids: &[i64]) -> Result<(u64, u64), StoreError>` (events, findings deleted).
- Produces (`commands`): `pub async fn export(store: &Store, repo: Option<&str>, since: Option<&str>, with_text: bool) -> Result<String, CommandError>`; `pub async fn prune(store: &Store, before: &str, yes: bool) -> Result<String, CommandError>`; `log` gains a "record" section.

Free-text fields by kind (redacted unless `--with-text`): `plan`: `summary`, `steps`, `risks`; `finding_disposition`: `reason`; `command_ignored`: `line`; finding rows: `text`.

- [ ] **Step 1: Failing tests**

```rust
#[tokio::test]
async fn export_redacts_free_text_unless_asked_and_every_line_is_json() {
    let (_f, p, id) = open_task_with_findings().await; // a finding text containing "SENTINEL_SECRET_42"
    let out = provefab::commands::export(&p.store, None, None, false).await.unwrap();
    assert!(!out.contains("SENTINEL_SECRET_42"), "{out}");
    for line in out.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v["type"] == "event" || v["type"] == "finding");
    }
    assert!(out.contains("\"redacted\":true"));
    let full = provefab::commands::export(&p.store, None, None, true).await.unwrap();
    assert!(full.contains("SENTINEL_SECRET_42"));
    let _ = id;
}

#[tokio::test]
async fn export_of_an_empty_record_prints_nothing() {
    let f = fixture(&["true"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    assert_eq!(provefab::commands::export(&p.store, None, None, false).await.unwrap(), "");
}

#[tokio::test]
async fn prune_deletes_only_with_yes_and_only_finished_tasks() {
    let (_f, p, id) = merged_task(&[]).await;
    p.store.set_pr_state(id, "archived").await.unwrap();
    let dry = provefab::commands::prune(&p.store, "2999-01-01", false).await.unwrap();
    assert!(dry.contains(&format!("task {id}")), "{dry}");
    assert!(!p.store.events(id).await.unwrap().is_empty());
    provefab::commands::prune(&p.store, "2999-01-01", true).await.unwrap();
    assert!(p.store.events(id).await.unwrap().is_empty());
    assert!(p.store.findings(id).await.unwrap().is_empty());
    assert!(provefab::commands::prune(&p.store, "30/09/2026", true).await.is_err());
}

#[tokio::test]
async fn log_shows_the_record_with_sources_and_current_dispositions() {
    let (_f, p, id) = open_task_with_findings().await;
    let task = p.store.task(id).await.unwrap().unwrap();
    p.apply_finding_commands(&task, &[comment("alice", "NONE", "/provefab F1 rejected", "2026-09-30T10:00:00Z")]).await.unwrap();
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.contains("record:"), "{log}");
    assert!(log.contains("[fact] routed"), "{log}");
    assert!(log.contains("[claim] review"), "{log}");
    assert!(log.contains("[human] finding_disposition F1 rejected"), "{log}");
    assert!(log.contains("F1 · "), "{log}");
    assert!(!log.to_lowercase().contains("proof"));
}
```

Make `apply_finding_commands` `pub` if the test (an integration test) needs it (it does): change its visibility to `pub` in Task 5's code and note it.

Unit test in `record.rs`: `parse_date("2026-09-30") == Some(1790726400)` and `parse_date("2026-02-30") == None`, `parse_date("x") == None`.

Run → FAIL.

- [ ] **Step 2: Implement**

`record.rs`:

```rust
use sha2::{Digest, Sha256};

pub fn redact_text(s: &str) -> Value {
    let hash = Sha256::digest(s.as_bytes());
    serde_json::json!({"redacted": true, "len": s.chars().count(), "sha256": hash.iter().map(|b| format!("{b:02x}")).collect::<String>()})
}

pub fn redact_event(e: &StoredEvent) -> Value {
    let mut p = e.payload.clone();
    let fields: &[&str] = match e.kind.as_str() {
        "plan" => &["summary", "steps", "risks"],
        "finding_disposition" => &["reason"],
        "command_ignored" => &["line"],
        _ => &[],
    };
    for f in fields {
        if let Some(v) = p.get_mut(*f) && !v.is_null() {
            let text = match v { Value::String(s) => s.clone(), other => other.to_string() };
            *v = redact_text(&text);
        }
    }
    p
}

/// `YYYY-MM-DD` at 00:00 UTC, or `None` for anything else or an impossible date.
pub fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let (y, m, d) = (it.next()?.parse::<i64>().ok()?, it.next()?.parse::<i64>().ok()?, it.next()?.parse::<i64>().ok()?);
    if it.next().is_some() || s.len() != 10 || !(1..=12).contains(&m) {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(m - 1) as usize];
    if !(1..=dim).contains(&d) {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400)
}
```

`store.rs`:
- `export_rows`: tasks filtered by `repo` (`WHERE repo = ?` when given); events with `at >= since` when given; findings with their `current_dispositions`.
- `prunable_tasks(before)`: `SELECT * FROM tasks WHERE updated_at < ? AND (state = 'failed' OR (state = 'pr_open' AND pr_state IN ('done', 'archived'))) AND NOT EXISTS (SELECT 1 FROM post_merge_checks c WHERE c.task_id = tasks.id AND c.state NOT IN ('passed','superseded','revert_open','blocked'))`. Use the real stored spelling of `TaskState::Failed`/`PrOpen` (`TaskState::as_str`).
- `prune_record(ids)`: one immediate transaction deleting `findings` then `change_events` for those ids; return counts.

`commands.rs`:
- `export`: for each event row, `{"type":"event","task":id,"repo":..,"issue":..,"seq":..,"kind":..,"source":..,"schema_version":..,"at":..,"payload": if with_text { payload } else { redact_event(e) }}`; for each finding, `{"type":"finding","task":..,"key":..,"pass":..,"round":..,"reviewer_model":..,"severity":..,"file":..,"line":..,"text": if with_text { text } else { redact_text(text) },"disposition": current or null}`. One `serde_json::to_string` per line, newline-terminated. `since` parsed with `parse_date` (error message `"--since must be YYYY-MM-DD"`).
- `prune`: parse `before` with `parse_date` (error `"--before must be YYYY-MM-DD"`, add a `CommandError` variant if none fits); list `task <id> <repo>#<issue>` lines; when `yes`, delete and append `deleted N events, M findings`; otherwise append `dry run: pass --yes to delete`.
- `log`: append after the post-merge section:

```text
record:
  1 [fact] routed
  2 [fact] stage_run plan claude-sonnet
  ...
  9 [human] finding_disposition F1 rejected (alice)
  10 [inferred] finding_unaddressed_at_merge F2 (rule v1)
findings:
  F1 · blocking · src/a.rs:3 · round 0 · rejected
  F2 · minor · src/b.rs · round 0 · open
```

(one summary line per event from its typed payload; unknown kinds print `<seq> [<source>] <kind> (unknown to this version)`).

`app.rs`: add

```rust
    /// The evidence record as JSON Lines (free text redacted unless --with-text).
    Export {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        with_text: bool,
    },
    /// Delete the record of finished tasks last updated before a date (dry run without --yes).
    Prune {
        #[arg(long)]
        before: String,
        #[arg(long)]
        yes: bool,
    },
```

and dispatch arms mirroring `Cmd::Log` (open the store, `print!` the result, `ExitCode::SUCCESS`; an error returns non-zero through the existing error path).

- [ ] **Step 3: Run** `cargo nextest run --all-features` → PASS. Also run `cargo run -q -p provefab -- export --help` and `-- prune --help` and check the help text.

- [ ] **Step 4: Commit**

```bash
git add crates/provefab/src/record.rs crates/provefab/src/store.rs crates/provefab/src/commands.rs crates/provefab/src/app.rs crates/provefab/tests/record.rs
git commit -m "record: log timeline, redacted JSON Lines export, dry-run prune

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Docs, spec amendment, final checks

**Files:**
- Modify: `docs/guide/usage.md`, `docs/guide/operations.md`, `README.md`, `docs/specs/2026-09-30-evidence-record-design.md`

- [ ] **Step 1: Docs**

- `usage.md`: a section "Recording decisions on review findings": the `F<n>` keys in Review notes, the `/provefab F<n> accepted|rejected|fixed|waived[: reason]` syntax (one per line, issue author or write access), that commands are read while the PR is open and hourly for two weeks after the merge, that nothing is replied; `provefab log` record section; `provefab export` (redacted by default, `--with-text`, `--repo`, `--since`); `provefab prune --before YYYY-MM-DD [--yes]`.
- `operations.md`: the record lives in `provefab.db` (`change_events`, `findings`), kept without limit; prune removes it; export never contains command output.
- `README.md`: one sentence: Provefab keeps a local record of what each change observed, claimed and decided, exportable as JSON Lines.
- Spec amendment (section 3.2 `pass` column; section 3.3 `command_ignored` kind; section 5 comment identity `author@created_at` instead of URL), each with "(amended 2026-09-30 in the plan)".
- `grep -rn $'—' docs/guide README.md` prints nothing for new text.

- [ ] **Step 2: Final checks**

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo nextest run --all-features
```

All pass. Paste the Summary line into the commit body.

- [ ] **Step 3: Real run (needs the owner's go; sandbox only)**

On `antoinehoriot/factory-sandbox` with an isolated `PROVEFAB_HOME` (as for post-merge v2): one issue whose review returns a minor finding, a `/provefab F1 rejected: test` comment on the PR, a squash merge; then `provefab log <id>` shows the disposition and the merge, and `provefab export` shows no free text. Record the evidence in `docs/handoff-2026-09-29-feature1-post-merge.md` under a new "Feature #2" section, or a new handoff file `docs/handoff-2026-09-30-evidence-record.md`.

- [ ] **Step 4: Commit**

```bash
git add docs README.md
git commit -m "docs: evidence record usage, operations and spec amendments

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
