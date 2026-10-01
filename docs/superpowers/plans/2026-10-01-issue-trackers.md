# Issue Trackers (Jira Cloud, Linear) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A repository can take its issues from a Jira Cloud project or a Linear team (label trigger, label and comment feedback, never a status change) while code, pull requests and `/provefab` finding commands stay on GitHub.

**Architecture:** The GitHub port `Hub` splits into `Tracker` (issues, comments, labels, intake) and `Forge` (pull requests, clone, visibility); `Hub` stays as `Tracker + Forge` with a blanket impl so the pipeline and Pro keep their bounds. Two adapters (`jira.rs`, `linear.rs`) implement `Tracker` over `reqwest`; `tracker::Routed` sends each call to the repository's tracker by slug and every `Forge` call to `Gh`. Issues keep their `(slug, number)` address; a nullable `tasks.issue_key` (`ENG-123`) drives every visible reference (branch, PR title and first line, prompts, commit, `status`/`log`/`export`/`prune`).

**Tech Stack:** Rust 2024, tokio, reqwest 0.13 (`json` feature only), serde_json, sqlx/SQLite, clap, wiremock (existing dev-dependency), `cargo nextest`.

**Spec:** `docs/specs/2026-10-01-issue-trackers-design.md`.

## Global Constraints

- Core repo `/Users/antoinehoriot/Projects/provefab/provefab`, branch `feature/issue-trackers` (checked out). Pro repo `/Users/antoinehoriot/Projects/provefab/provefab-pro`, branch `feature/issue-trackers` created from `main` in Task 8. Local commits only; never push, tag or deploy. Landing copy is out of this plan (done at release).
- Core: exactly THREE new modules `crates/provefab/src/tracker.rs`, `jira.rs`, `linear.rs`; exactly ONE migration `crates/provefab/migrations/0006_issue_key.sql` (`ALTER TABLE tasks ADD COLUMN issue_key TEXT`); NO new dependency and no new feature of an existing dependency (reqwest stays `features = ["json"]`: build query strings with `reqwest::Url::query_pairs_mut`, never `RequestBuilder::query`, which needs reqwest's `query` feature). A fourth module, a second migration or a new dependency/feature is a STOP: ask the owner. New integration test files under `crates/provefab/tests/` are not modules of the crate and are allowed.
- Ports: `pub trait Hub: Tracker + Forge {}` with `impl<T: Tracker + Forge> Hub for T {}`. Issues stay addressed by `(slug, number)` in every port signature and in stored pending effects.
- `Tracker` methods: `open_issues(slug, label)`, `issue`, `comments`, `comment`, `edit_labels`, `ensure_label`, `issue_open`. `Forge` methods: `pr_comment`, `pr_create`, `repo_clone`, `pr_status`, `pr_merge`, `repo_is_public`. `intake::IssueSource` and `GhLabelPoller` are removed.
- Display: `#123` for GitHub, the key (`ENG-123`) otherwise. Branch `provefab/ENG-123-<slug>` (key upper case), PR title `ENG-123: <title>`, PR body first line `Closes #123.` (GitHub), `Issue: [ENG-123](<url>).` (Jira), `Issue: [ENG-123](<url>). Fixes ENG-123` (Linear). Commit `Provefab task N, issue ENG-123.`. Prompts `Issue {{ref}}: {{title}}`. GitHub output stays byte-identical to today.
- Credentials never in `provefab.toml` (a `token` key in `[repos.tracker]` is refused by `deny_unknown_fields`), never in an error, a log line or a `Debug` output. Keychain: service `provefab-jira`, account `<site>` (the token as the password, the account e-mail as the item comment); service `provefab-linear`, account `provefab`. Environment overrides: `PROVEFAB_JIRA_EMAIL`, `PROVEFAB_JIRA_TOKEN`, `PROVEFAB_LINEAR_KEY`.
- HTTP status: 401, 403, 404 (and every other 4xx except 408 and 429) are permanent; 429 and 5xx and network failures are transient. New `ForgeError::Tracker { service, status, message }`.
- Provefab never changes a ticket's status and no text claims it does.
- The public core never contains the four symbols that `crates/provefab/tests/no_paid_code.rs` searches for. That test scans every file of the repository, docs and this plan included: never write those symbols anywhere.
- Wording: English; no em-dashes in any user-facing text (docs, CLI help, comments posted on tickets, error messages).
- Version `0.3.0` at the end (Task 7).
- Commit trailer: blank line then `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Checks after every core task, from the repository root: `cargo fmt -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --all-features`. Pro (Task 8): `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run`.

## Decisions taken in this plan (each with its why; spec amendments in Task 7)

1. **Order changed from the brief:** config and credentials (Task 2) come before identity (Task 3), because the PR body first line depends on the tracker kind. `tracker::Routed` lands in Task 6, not Task 1: before the adapters exist it would only wrap `Gh`. The pipeline-level Jira-like and Linear-like tests move to Task 3 (they need only `FakeHub` with a key). `is_bot_comment` without emphasis moves to Task 3 (its pipeline test is there and the Jira adapter test of Task 4 relies on it).
2. **`scheduler::run` loses its `source` argument** and polls `p.hub` (the hub is the tracker); `scheduler::dry_run` takes `&impl Tracker`. `tests/scheduler.rs` changes mechanically (its `FakeSource` disappears with `IssueSource`); the spec's "tests pass unchanged" cannot hold for a file that implements the removed trait. `FakeHub::open_issues` answers `vec![self.issue.clone()]`, exactly what every `FakeSource` in that file returned.
3. **Adapter tests use `wiremock`**, already a dev-dependency used by `prices.rs`, `jevq.rs`, `commands.rs` and `tests/pipeline.rs`, instead of a hand-written tokio `TcpListener` server: same "no new dependency" intent, far less test code, and tokio's `net` feature is not enabled for the core.
4. **Provefab's own comments are recognised by the bot line only**, as on GitHub, not by "written by the API account". Jira API tokens and Linear personal keys belong to a person: an operator who answers a question from the same account must be heard (on GitHub `gh` also runs with the person's token and only the bot line is checked). Comments without a human author (Jira `accountType = "app"`, Linear comments without `user`) are skipped. Cost if wrong: none for the operator; a re-open trigger is a pilot using a dedicated bot account who wants its manual comments ignored.
5. **Jira e-mail storage:** one Keychain item per site as the spec says, the token as its password (entered at the `security` prompt, so it never passes through Provefab or a command line) and the e-mail as the item comment (`-j`), read back from `security find-generic-password` attributes (`"icmt"<blob>="..."`). The real run before release confirms the attribute format on the owner's Mac.
6. **Linear workspace:** `[repos.tracker]` has no workspace for Linear (the spec refuses `site` for it). `provefab add` matches a Linear URL by team key and then refuses it when the ticket's URL from the API is in another workspace (so `linear.app/other/issue/ENG-5` never queues our `ENG-5`). `doctor` reads the workspace (`organization.urlKey`) from the API.
7. **Shared project:** when several repositories match a ticket URL, `provefab add` picks the one whose `label` is on the ticket; none or several is an error naming the candidates.
8. **`project` is refused for `kind = "github"`** (as `site` is refused outside Jira): an unused key is a typo.
9. **Retry-After:** a 429 whose `Retry-After` is at most 30 s is waited out once inside the adapter call; a longer one returns a transient error and the pipeline's own `retry_delays` (minutes) apply.
10. **Other 4xx are permanent** (400 malformed request, 410, ...): waiting cannot fix them. 408 and 429 stay transient.
11. **Unreadable timestamps:** a comment whose time cannot be normalised is skipped with a log line (never compared as a raw string).
12. **Prompts:** the first line of each template becomes "turns an issue into a pull request" (was "a GitHub issue"); the stage detection in `testkit::stage_of` keys on "You are the planning/implementation" and is unaffected.
13. **Pro version** stays as is in Task 8 (only the temporary path dependency and two test literals change); bumping it belongs to the release.

## Review Focus

1. **Timestamps from Jira (`2026-10-01T10:00:00.000+0200`) and Linear (`...08:00:00.123Z`) compared as strings with Provefab's `YYYY-MM-DDTHH:MM:SSZ`:** a reply posted after the question counts, one posted before never does, across offsets and midnight. Tests in Task 4 (`utc_seconds_normalises_offsets_fractions_and_dates`, `comments_are_adf_both_ways_with_members_and_utc_times`).
2. **A label with a quote or backslash in JQL** (`label = "pro\"fab"`): Provefab escapes it so the query stays one string literal and cannot add clauses. Test in Task 4 (`jql_quotes_and_escapes_every_value`).
3. **The post-merge marker `<!-- provefab-post-merge:N -->` survives the Jira ADF round trip**, so a crash between posting and recording never double-posts. Test in Task 4 (`adf_round_trip_keeps_what_provefab_compares`).
4. **A Linear URL from another workspace with the same team key** is refused by `provefab add` instead of queuing a different ticket. Test in Task 6 (`add_refuses_a_linear_ticket_from_another_workspace`).
5. **Two repositories sharing one Jira project with different labels:** `add` picks the repository by the ticket's label; a ticket with both labels or neither is a clear error. Test in Task 6 (`add_picks_the_repository_by_label_when_a_project_is_shared`).

## File Structure

| File | Responsibility |
|---|---|
| `crates/provefab/src/ports.rs` | `Tracker`, `Forge`, `Hub` (blanket), `Gh` implementing both |
| `crates/provefab/src/tracker.rs` (new) | `[repos.tracker]` types and validation, credentials (env, Keychain), references (`issue_ref`, `repo_ref`, PR title and first line), ticket URL parsing, timestamp normalisation, shared HTTP rules (`send`, `http_error`, redaction), `Routed` |
| `crates/provefab/src/jira.rs` (new) | Jira Cloud REST v3 adapter, JQL, ADF converter (to and from) |
| `crates/provefab/src/linear.rs` (new) | Linear GraphQL adapter |
| `crates/provefab/migrations/0006_issue_key.sql` (new) | `tasks.issue_key` |
| `crates/provefab/src/forge.rs` | `Issue.key`, `branch_name(&str, ..)`, `ForgeError::Tracker`, `is_bot_comment` without emphasis, `ISSUE_LIMIT` shared |
| `crates/provefab/src/store.rs` | `NewIssue.issue_key`, `TaskRow.issue_key`, `TaskRow::reference`, `TaskRow::repo_reference` |
| `crates/provefab/src/intake.rs` | `poll(&impl Tracker, ..)`, `IssueSource` removed |
| `crates/provefab/src/scheduler.rs` | polling through `p.hub`, dry-run references |
| `crates/provefab/src/pipeline.rs` | branch, PR title and first line, prompts, commit message |
| `crates/provefab/src/config.rs` | `RepoConfig.tracker`, `tracker_kind()`, `ConfigError::Tracker` |
| `crates/provefab/src/commands.rs` | `add` ticket URLs, `status`/`log`/`export`/`prune` references, `tracker_checks` |
| `crates/provefab/src/app.rs` | `login jira`/`login linear`, `Routed` wiring, doctor tracker lines, CLI description |
| `crates/provefab/src/testkit.rs` | `FakeHub` implements `Tracker` and `Forge`, `queue_ticket` |
| `crates/provefab/prompts/*.md` | `Issue {{ref}}` |
| `crates/provefab/tests/trackers.rs` (new) | pipeline runs on Jira-like and Linear-like tickets |
| `docs/guide/trackers.md` (new) and the other docs | user documentation |

---

### Task 1: Port split: `Tracker`, `Forge`, `Hub` blanket (pure refactor)

**Files:**
- Modify: `crates/provefab/src/ports.rs:118-262` (trait `Hub` and `impl Hub for Gh`)
- Modify: `crates/provefab/src/intake.rs` (remove `IssueSource`, `GhLabelPoller`; `poll` takes `&impl Tracker`; test fake)
- Modify: `crates/provefab/src/scheduler.rs:89-110` (`run` signature and its `poll` call), `:342-347` (`dry_run`)
- Modify: `crates/provefab/src/app.rs:15`, `:244-249`, `:323`
- Modify: `crates/provefab/src/testkit.rs:18`, `:335-503` (`impl Hub for FakeHub`)
- Modify: `crates/provefab/tests/scheduler.rs` (mechanical)

**Interfaces:**
- Consumes: nothing new.
- Produces:

```rust
// ports.rs
pub trait Tracker {
    fn open_issues(&self, slug: &str, label: &str) -> impl Future<Output = Result<Vec<Issue>, ForgeError>> + Send;
    fn issue(&self, slug: &str, number: u64) -> impl Future<Output = Result<Issue, ForgeError>> + Send;
    fn comments(&self, slug: &str, number: u64) -> impl Future<Output = Result<Vec<Comment>, ForgeError>> + Send;
    fn comment(&self, slug: &str, number: u64, body: &str) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn edit_labels(&self, slug: &str, number: u64, add: &[&str], remove: &[&str]) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn ensure_label(&self, slug: &str, name: &str, color: &str, description: &str) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn issue_open(&self, slug: &str, number: u64) -> impl Future<Output = Result<bool, ForgeError>> + Send;
}
pub trait Forge {
    fn pr_comment(&self, slug: &str, url: &str, body: &str) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn pr_create(&self, slug: &str, head: &str, base: &str, title: &str, body: &str) -> impl Future<Output = Result<String, ForgeError>> + Send;
    fn repo_clone(&self, slug: &str, dest: &Path) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn pr_status(&self, slug: &str, url: &str) -> impl Future<Output = Result<PrStatus, ForgeError>> + Send;
    fn pr_merge(&self, slug: &str, url: &str, head: &str) -> impl Future<Output = Result<(), ForgeError>> + Send;
    fn repo_is_public(&self, slug: &str) -> impl Future<Output = Result<bool, ForgeError>> + Send;
}
pub trait Hub: Tracker + Forge {}
impl<T: Tracker + Forge> Hub for T {}
// intake.rs
pub async fn poll(source: &impl Tracker, repo: &RepoConfig, store: &Store) -> Result<Vec<i64>, IntakeError>;
// scheduler.rs
pub async fn run<R, O, H>(p: Arc<Pipeline<R, O, H>>, opts: RunOptions, stop: impl Future<Output = ()>) -> Result<(), PipelineError>;
pub async fn dry_run<O: Oracle, T: Tracker>(config: &Config, tracker: &T, oracle: &O) -> Result<Vec<String>, ForgeError>;
// testkit.rs: FakeHub implements Tracker (open_issues -> vec![self.issue.clone()]) and Forge;
// `pub use crate::ports::{Forge, Hub, Oracle, Tracker};`
```

- [ ] **Step 1: Write the failing tests**

In `ports.rs` `mod tests`, add:

```rust
    /// The port split (issue trackers spec §3): `Gh` is both ports, so a hub.
    #[test]
    fn gh_is_a_tracker_and_a_forge_so_a_hub() {
        fn tracker<T: Tracker>() {}
        fn forge<F: Forge>() {}
        fn hub<H: Hub>() {}
        tracker::<Gh>();
        forge::<Gh>();
        hub::<Gh>();
    }
```

In `intake.rs` `mod tests`, replace `struct FakeSource(Vec<Issue>);` and its `impl IssueSource for FakeSource` with:

```rust
    #[derive(Default)]
    struct FakeSource {
        issues: Vec<Issue>,
        asked: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl Tracker for FakeSource {
        async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
            self.asked.lock().unwrap().push((slug.into(), label.into()));
            Ok(self.issues.clone())
        }
        async fn issue(&self, _: &str, n: u64) -> Result<Issue, ForgeError> {
            Ok(issue(n))
        }
        async fn comments(&self, _: &str, _: u64) -> Result<Vec<Comment>, ForgeError> {
            Ok(Vec::new())
        }
        async fn comment(&self, _: &str, _: u64, _: &str) -> Result<(), ForgeError> {
            Ok(())
        }
        async fn edit_labels(&self, _: &str, _: u64, _: &[&str], _: &[&str]) -> Result<(), ForgeError> {
            Ok(())
        }
        async fn ensure_label(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), ForgeError> {
            Ok(())
        }
        async fn issue_open(&self, _: &str, _: u64) -> Result<bool, ForgeError> {
            Ok(true)
        }
    }
```

change `poll_queues_each_issue_once` to build `let source = FakeSource { issues: vec![issue(1), issue(2)], ..Default::default() };`, and add:

```rust
    #[tokio::test]
    async fn poll_asks_the_tracker_for_the_repo_and_its_label() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("f.db")).await.unwrap();
        let source = FakeSource::default();
        poll(&source, &repo(), &store).await.unwrap();
        assert_eq!(
            *source.asked.lock().unwrap(),
            vec![("o/r".to_string(), "provefab".to_string())]
        );
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab gh_is_a_tracker poll_asks_the_tracker`
Expected: compile error, `cannot find trait Tracker in this scope`.

- [ ] **Step 3: Implement the split**

`ports.rs`: replace the doc comment and trait `Hub` (lines 118-200) with the `Tracker` and `Forge` traits from Interfaces, each method keeping its existing doc comment (`repo_clone`: "Clones the repo into `dest` (a provefab-managed checkout, D48)."; `pr_merge`: "Squash-merges the PR and deletes its branch, only at `head` (auto-merge, D49)."; `repo_is_public`: "Whether anyone can write the repository's issues (a public repository)."), with these trait docs and the blanket:

```rust
/// Where a repository's issues live and Provefab reports progress on them:
/// GitHub, Jira or Linear (issue trackers spec §3). Addressed by repository
/// slug and issue number; an adapter rebuilds a ticket key from its config.
pub trait Tracker { /* the seven methods */ }

/// Where the code lives: GitHub, always (pull requests, clone, visibility).
pub trait Forge { /* the six methods */ }

/// Everything the pipeline needs: one tracker and the forge. A blanket
/// implementation, so the pipeline's bounds and Provefab Pro stay unchanged.
pub trait Hub: Tracker + Forge {}

impl<T: Tracker + Forge> Hub for T {}
```

Split `impl Hub for Gh` into `impl Tracker for Gh` (`issue`, `comments`, `comment`, `edit_labels`, `ensure_label`, `issue_open`, plus the new method below) and `impl Forge for Gh` (`pr_comment`, `pr_create`, `repo_clone`, `pr_status`, `pr_merge`, `repo_is_public`), bodies unchanged:

```rust
    async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        Gh::labeled_issues(self, slug, label).await
    }
```

`intake.rs`: delete `IssueSource`, `GhLabelPoller` and their imports (`std::future::Future`, `Gh`); import `crate::ports::Tracker`; new module doc line 1-2 stays. `poll` becomes:

```rust
/// Queues every labelled open issue the store does not know yet; returns the new task ids.
pub async fn poll(
    source: &impl Tracker,
    repo: &RepoConfig,
    store: &Store,
) -> Result<Vec<i64>, IntakeError> {
    let mut added = Vec::new();
    for issue in source.open_issues(&repo.slug, &repo.label).await? {
```

(the rest of the body is unchanged).

`scheduler.rs`: `use crate::intake::poll;` and `use crate::ports::{Hub, Oracle, Tracker};`. In `run`, delete the `source: &S,` parameter and the `S: IssueSource,` bound, and change the poll call to `match poll(&p.hub, repo, &p.store).await {`. `dry_run` becomes `pub async fn dry_run<O: Oracle, T: Tracker>(config: &Config, tracker: &T, oracle: &O)` and its loop `for issue in tracker.open_issues(&repo.slug, &repo.label).await? {`.

`app.rs`: delete `use crate::intake::GhLabelPoller;`; in `Cmd::Run`, replace `let source = GhLabelPoller { gh: gh() };` with nothing, call `scheduler::dry_run(&config, &gh(), &oracle)` and `scheduler::run(pipeline, RunOptions { workers, once }, stop)`.

`testkit.rs`: `pub use crate::ports::{Forge, Hub, Oracle, Tracker};`. Split `impl Hub for FakeHub` the same way as `Gh` (bodies unchanged) and add to `impl Tracker for FakeHub`:

```rust
    /// The fake's one issue, labelled or not (what every scheduler test polled).
    async fn open_issues(&self, _: &str, _: &str) -> Result<Vec<Issue>, ForgeError> {
        Ok(vec![self.issue.clone()])
    }
```

`tests/scheduler.rs` (mechanical, no behaviour change): delete lines 5-21 (`pub struct FakeSource` and its `impl provefab::intake::IssueSource`), delete every `let source = FakeSource(...);` line, then:

```bash
sed -i '' -e 's/run(p\.clone(), &source, /run(p.clone(), /' -e 's/dry_run(&f\.config, &source, /dry_run(\&f.config, \&hub, /' crates/provefab/tests/scheduler.rs
grep -n "source" crates/provefab/tests/scheduler.rs
```

Expected: the grep prints nothing.

- [ ] **Step 4: Run all checks**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: all green, the two new tests included; no other test changed.

- [ ] **Step 5: Commit**

```bash
git add -A crates/provefab
git commit -m "ports: split Hub into Tracker and Forge (Hub is their blanket combination)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: `[repos.tracker]`, validation, credentials, `provefab login jira|linear`

**Files:**
- Create: `crates/provefab/src/tracker.rs`
- Modify: `crates/provefab/src/lib.rs` (`pub mod tracker;` after `pub mod task;`)
- Modify: `crates/provefab/src/config.rs:119-178` (field, `tracker_kind()`), `:301-328` (`ConfigError::Tracker`), `:337-392` (validation)
- Modify: `crates/provefab/src/intake.rs` test `repo()` literal (add `tracker: None,`)
- Modify: `crates/provefab/src/app.rs` (`LoginWorker::Jira | Linear`, `--site`, `tracker_login`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces:

```rust
// tracker.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind { #[default] Github, Jira, Linear }
impl TrackerKind { pub fn as_str(self) -> &'static str; }
#[derive(Debug, Clone, PartialEq, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackerConfig { #[serde(default)] pub kind: TrackerKind, #[serde(default)] pub site: Option<String>, #[serde(default)] pub project: Option<String> }
pub fn validate(cfg: &TrackerConfig, label: &str) -> Result<(), String>;
pub fn is_host(s: &str) -> bool;
pub fn is_project_key(s: &str) -> bool;
pub const JIRA_KEYCHAIN_SERVICE: &str = "provefab-jira";
pub const LINEAR_KEYCHAIN_SERVICE: &str = "provefab-linear";
pub type Env<'a> = &'a (dyn Fn(&str) -> Option<String> + Sync);
pub fn process_env(name: &str) -> Option<String>;
#[derive(Clone)] pub struct JiraAuth { pub email: String, pub token: String } // Debug redacts the token
pub async fn jira_auth(security: &Path, site: &str, env: Env<'_>) -> Result<JiraAuth, String>;
pub async fn linear_key(security: &Path, env: Env<'_>) -> Result<String, String>;
// config.rs
pub struct RepoConfig { /* ... */ #[serde(default)] pub tracker: Option<crate::tracker::TrackerConfig> }
impl RepoConfig { pub fn tracker_kind(&self) -> crate::tracker::TrackerKind; }
ConfigError::Tracker(String /*slug*/, String /*reason*/) // "{slug}: [repos.tracker]: {reason}"
```

- [ ] **Step 1: Write the failing tests**

`config.rs` `mod tests` (uses the existing `BASE` constant):

```rust
    #[test]
    fn tracker_table_is_optional_and_validated() {
        use crate::tracker::TrackerKind;
        let base = format!("{BASE}[[repos]]\nslug = \"o/r\"\ngates = [\"make\"]\n");
        let c = Config::from_toml_str(&base).unwrap();
        assert_eq!(c.repos[0].tracker, None);
        assert_eq!(c.repos[0].tracker_kind(), TrackerKind::Github);
        let with = |t: &str| Config::from_toml_str(&format!("{base}[repos.tracker]\n{t}"));
        let jira = with("kind = \"jira\"\nsite = \"acme.atlassian.net\"\nproject = \"ENG\"\n").unwrap();
        assert_eq!(jira.repos[0].tracker_kind(), TrackerKind::Jira);
        let linear = with("kind = \"linear\"\nproject = \"ENG_2\"\n").unwrap();
        assert_eq!(linear.repos[0].tracker_kind(), TrackerKind::Linear);
        for bad in [
            "kind = \"jira\"\nproject = \"ENG\"\n",
            "kind = \"jira\"\nsite = \"https://acme.atlassian.net\"\nproject = \"ENG\"\n",
            "kind = \"jira\"\nsite = \"acme.atlassian.net/jira\"\nproject = \"ENG\"\n",
            "kind = \"jira\"\nsite = \"acme.atlassian.net\"\n",
            "kind = \"jira\"\nsite = \"acme.atlassian.net\"\nproject = \"eng\"\n",
            "kind = \"jira\"\nsite = \"acme.atlassian.net\"\nproject = \"1ENG\"\n",
            "kind = \"linear\"\nsite = \"linear.app\"\nproject = \"ENG\"\n",
            "kind = \"linear\"\nproject = \"EN G\"\n",
            "kind = \"github\"\nproject = \"ENG\"\n",
        ] {
            assert!(matches!(with(bad), Err(ConfigError::Tracker(_, _))), "{bad}");
        }
        // Unknown kinds and unknown keys (a token, say) never load.
        for bad in [
            "kind = \"gitlab\"\n",
            "kind = \"linear\"\nproject = \"ENG\"\ntoken = \"lin_api_x\"\n",
        ] {
            assert!(matches!(with(bad), Err(ConfigError::Parse(_))), "{bad}");
        }
        // Jira labels cannot hold whitespace, and the derived ones extend the label.
        let spaced = format!(
            "{BASE}[[repos]]\nslug = \"o/r\"\ngates = [\"make\"]\nlabel = \"pro fab\"\n[repos.tracker]\nkind = \"jira\"\nsite = \"acme.atlassian.net\"\nproject = \"ENG\"\n"
        );
        assert!(matches!(
            Config::from_toml_str(&spaced),
            Err(ConfigError::Tracker(_, _))
        ));
    }
```

`tracker.rs` `#[cfg(test)] mod tests` (the module file is created in Step 3 with these tests at its end):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_security(dir: &Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> + Sync {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    /// `security` as macOS answers: the password alone with `-w`, the
    /// attributes (the comment holds the e-mail) without it.
    const KEYCHAIN: &str = r#"case "$*" in
  *"-s provefab-jira -a acme.atlassian.net -w") echo "kc-token" ;;
  *"-s provefab-jira -a acme.atlassian.net") echo 'keychain: "/Users/x/Library/Keychains/login.keychain-db"'; echo 'attributes:'; echo '    "acct"<blob>="acme.atlassian.net"'; echo '    "icmt"<blob>="bot@acme.test"' ;;
  *"-s provefab-linear -a provefab -w") echo "lin_api_kc" ;;
  *) exit 44 ;;
esac"#;

    #[test]
    fn hosts_and_project_keys() {
        for ok in ["acme.atlassian.net", "jira.acme-corp.io"] {
            assert!(is_host(ok), "{ok}");
        }
        for bad in ["", "acme", "https://acme.atlassian.net", "acme.atlassian.net/x", "-acme.net", "acme.net.", "ac me.net"] {
            assert!(!is_host(bad), "{bad}");
        }
        for ok in ["ENG", "E", "ENG_2", "A1"] {
            assert!(is_project_key(ok), "{ok}");
        }
        for bad in ["", "eng", "1ENG", "EN-G", "EN G"] {
            assert!(!is_project_key(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn jira_credentials_come_from_the_environment_then_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let security = fake_security(dir.path(), "security", KEYCHAIN);
        let env = env_of(&[("PROVEFAB_JIRA_EMAIL", "env@acme.test"), ("PROVEFAB_JIRA_TOKEN", "env-token")]);
        let a = jira_auth(&security, "acme.atlassian.net", &env).await.unwrap();
        assert_eq!((a.email.as_str(), a.token.as_str()), ("env@acme.test", "env-token"));
        let none = env_of(&[]);
        let a = jira_auth(&security, "acme.atlassian.net", &none).await.unwrap();
        assert_eq!((a.email.as_str(), a.token.as_str()), ("bot@acme.test", "kc-token"));
        assert!(!format!("{a:?}").contains("kc-token"), "{a:?}");
        let err = jira_auth(&security, "other.atlassian.net", &none).await.unwrap_err();
        assert!(err.contains("provefab login jira --site other.atlassian.net"), "{err}");
    }

    #[tokio::test]
    async fn the_linear_key_comes_from_the_environment_then_the_keychain() {
        let dir = tempfile::tempdir().unwrap();
        let security = fake_security(dir.path(), "security", KEYCHAIN);
        let env = env_of(&[("PROVEFAB_LINEAR_KEY", "lin_api_env")]);
        assert_eq!(linear_key(&security, &env).await.unwrap(), "lin_api_env");
        assert_eq!(linear_key(&security, &env_of(&[])).await.unwrap(), "lin_api_kc");
        let missing = fake_security(dir.path(), "missing", "exit 44");
        let err = linear_key(&missing, &env_of(&[])).await.unwrap_err();
        assert!(err.contains("provefab login linear"), "{err}");
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab tracker_table hosts_and_project jira_credentials the_linear_key`
Expected: compile errors (`tracker` module, `ConfigError::Tracker`, `tracker_kind` missing).

- [ ] **Step 3: Implement**

`crates/provefab/src/tracker.rs` (this task's part; later tasks append to it):

```rust
//! Issue trackers besides GitHub (docs/specs/2026-10-01-issue-trackers-design.md):
//! the `[repos.tracker]` table, credentials, and what Jira and Linear share.
//! Code and pull requests stay on GitHub; Provefab never changes a ticket's status.

use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use tokio::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackerKind {
    #[default]
    Github,
    Jira,
    Linear,
}

impl TrackerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackerKind::Github => "github",
            TrackerKind::Jira => "jira",
            TrackerKind::Linear => "linear",
        }
    }
}

/// `[repos.tracker]`: where the repository's issues live (spec §5). Absent
/// means GitHub. Unknown keys are refused, so a credential never loads from
/// `provefab.toml`.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackerConfig {
    #[serde(default)]
    pub kind: TrackerKind,
    /// Jira only: the site's host name, `acme.atlassian.net`.
    #[serde(default)]
    pub site: Option<String>,
    /// The Jira project key or the Linear team key: `ENG` in `ENG-123`.
    #[serde(default)]
    pub project: Option<String>,
}

/// A bare host name: letters, digits, dots and hyphens, at least one dot, no
/// scheme, port or path.
pub fn is_host(s: &str) -> bool {
    !s.is_empty()
        && s.contains('.')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !s.starts_with(['.', '-'])
        && !s.ends_with(['.', '-'])
}

/// `[A-Z][A-Z0-9_]*`: the key part of `ENG-123`.
pub fn is_project_key(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Load-time rules of `[repos.tracker]` (spec §5).
pub fn validate(cfg: &TrackerConfig, label: &str) -> Result<(), String> {
    let kind = cfg.kind.as_str();
    match (cfg.kind, cfg.site.as_deref()) {
        (TrackerKind::Jira, None) => return Err("site is required for jira".into()),
        (TrackerKind::Jira, Some(site)) if !is_host(site) => {
            return Err(format!(
                "site `{site}` must be a host name such as acme.atlassian.net, without https:// or a path"
            ));
        }
        (TrackerKind::Github | TrackerKind::Linear, Some(_)) => {
            return Err(format!("site is for jira only, not {kind}"));
        }
        _ => {}
    }
    match (cfg.kind, cfg.project.as_deref()) {
        (TrackerKind::Github, Some(_)) => return Err("project is for jira and linear only".into()),
        (TrackerKind::Github, None) => {}
        (_, None) => return Err(format!("project is required for {kind}")),
        (_, Some(p)) if !is_project_key(p) => {
            return Err(format!("project `{p}` must look like ENG: [A-Z][A-Z0-9_]*"));
        }
        _ => {}
    }
    if cfg.kind == TrackerKind::Jira && label.chars().any(char::is_whitespace) {
        return Err(format!(
            "label `{label}` contains whitespace, which Jira labels cannot hold"
        ));
    }
    Ok(())
}

pub const JIRA_KEYCHAIN_SERVICE: &str = "provefab-jira";
pub const LINEAR_KEYCHAIN_SERVICE: &str = "provefab-linear";

/// Reads one environment variable; tests pass their own.
pub type Env<'a> = &'a (dyn Fn(&str) -> Option<String> + Sync);

/// The process environment, trimmed; an empty value counts as unset.
pub fn process_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Jira Cloud Basic authentication: account e-mail and API token.
#[derive(Clone)]
pub struct JiraAuth {
    pub email: String,
    pub token: String,
}

impl std::fmt::Debug for JiraAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "JiraAuth {{ email: {:?}, token: <redacted> }}", self.email)
    }
}

/// `security` with `args`: its stdout (and stderr, for attribute listings) on
/// success. Never prompts.
async fn security_out(security: &Path, args: &[&str], with_stderr: bool) -> Option<String> {
    let out = Command::new(security)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let mut text = String::from_utf8_lossy(&out.stdout).to_string();
    if with_stderr {
        text.push_str(&String::from_utf8_lossy(&out.stderr));
    }
    Some(text)
}

/// The item comment in a `security find-generic-password` listing:
/// `    "icmt"<blob>="bot@acme.test"`.
fn keychain_comment(listing: &str) -> Option<String> {
    listing.lines().find_map(|l| {
        let v = l.trim().strip_prefix("\"icmt\"<blob>=")?;
        let v = v.strip_prefix('"')?.strip_suffix('"')?;
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// Jira credentials for `site`: `PROVEFAB_JIRA_EMAIL` and `PROVEFAB_JIRA_TOKEN`,
/// each overriding its part of the Keychain item `provefab-jira` / `<site>`
/// (spec §5). The error names the fix, never a secret.
pub async fn jira_auth(security: &Path, site: &str, env: Env<'_>) -> Result<JiraAuth, String> {
    let fix = format!(
        "run `provefab login jira --site {site}` or set PROVEFAB_JIRA_EMAIL and PROVEFAB_JIRA_TOKEN"
    );
    let token = match env("PROVEFAB_JIRA_TOKEN") {
        Some(t) => t,
        None => security_out(
            security,
            &["find-generic-password", "-s", JIRA_KEYCHAIN_SERVICE, "-a", site, "-w"],
            false,
        )
        .await
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("no Jira API token for {site}: {fix}"))?,
    };
    let email = match env("PROVEFAB_JIRA_EMAIL") {
        Some(e) => e,
        None => security_out(
            security,
            &["find-generic-password", "-s", JIRA_KEYCHAIN_SERVICE, "-a", site],
            true,
        )
        .await
        .as_deref()
        .and_then(keychain_comment)
        .ok_or_else(|| format!("no Jira account e-mail for {site}: {fix}"))?,
    };
    Ok(JiraAuth { email, token })
}

/// The Linear personal API key: `PROVEFAB_LINEAR_KEY`, else the Keychain item
/// `provefab-linear` / `provefab` (spec §5).
pub async fn linear_key(security: &Path, env: Env<'_>) -> Result<String, String> {
    if let Some(k) = env("PROVEFAB_LINEAR_KEY") {
        return Ok(k);
    }
    security_out(
        security,
        &["find-generic-password", "-s", LINEAR_KEYCHAIN_SERVICE, "-a", "provefab", "-w"],
        false,
    )
    .await
    .map(|k| k.trim().to_string())
    .filter(|k| !k.is_empty())
    .ok_or_else(|| {
        "no Linear API key: run `provefab login linear` or set PROVEFAB_LINEAR_KEY".to_string()
    })
}
```

`config.rs`: after the `risk` field of `RepoConfig`:

```rust
    /// `[repos.tracker]`: where issues come from. Absent: GitHub (issue trackers spec §5).
    #[serde(default)]
    pub tracker: Option<crate::tracker::TrackerConfig>,
```

in `impl RepoConfig`:

```rust
    /// The repository's issue tracker, GitHub unless `[repos.tracker]` says otherwise.
    pub fn tracker_kind(&self) -> crate::tracker::TrackerKind {
        self.tracker.as_ref().map(|t| t.kind).unwrap_or_default()
    }
```

in `ConfigError`, after `Risk`:

```rust
    #[error("{0}: [repos.tracker]: {1}")]
    Tracker(String, String),
```

in `validate`, right after the label emptiness check (so the label rule sees a non-empty label):

```rust
            if let Some(t) = &r.tracker {
                crate::tracker::validate(t, &r.label)
                    .map_err(|e| ConfigError::Tracker(r.slug.clone(), e))?;
            }
```

`intake.rs` test `repo()`: add `tracker: None,` after `risk: None,`.

`app.rs` login. `LoginWorker` gains two variants and `Cmd::Login` a field:

```rust
#[derive(Clone, Copy, ValueEnum)]
enum LoginWorker {
    Claude,
    Codex,
    /// Jira Cloud: account e-mail and API token, per site.
    Jira,
    /// Linear: a personal API key.
    Linear,
}
```

```rust
    /// One-time sign-in for a worker, in Provefab's own config directory, or the
    /// credentials of a Jira site or a Linear workspace (stored in the Keychain).
    Login {
        #[arg(value_enum)]
        worker: LoginWorker,
        /// Sign this worker in with your own API key instead of your subscription
        /// (used by catalog models with `auth = "api_key"`).
        #[arg(long)]
        api_key: bool,
        /// Jira only: the site host name, such as acme.atlassian.net.
        #[arg(long)]
        site: Option<String>,
    },
```

In `dispatch`, before the existing `Cmd::Login { worker, api_key: true }` arm:

```rust
        Cmd::Login {
            worker: worker @ (LoginWorker::Jira | LoginWorker::Linear),
            api_key,
            site,
        } => {
            if api_key {
                bail!("--api-key is for claude and codex; tracker logins always store an API token");
            }
            tracker_login(worker, site)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Login { site: Some(_), .. } => bail!("--site is for `provefab login jira` only"),
```

The two existing arms get `..` (`Cmd::Login { worker, api_key: true, .. }` and `Cmd::Login { worker, .. }`), and each of their three `match worker` expressions gets the arm `LoginWorker::Jira | LoginWorker::Linear => unreachable!("tracker logins are handled above"),`. Add the function after `gh()`:

```rust
/// `provefab login jira --site <site>` and `provefab login linear` (spec §5):
/// the token is typed at the Keychain's own prompt, so it never passes through
/// Provefab or a command line. The Jira e-mail is kept as the item's comment.
fn tracker_login(worker: LoginWorker, site: Option<String>) -> anyhow::Result<()> {
    use crate::tracker::{JIRA_KEYCHAIN_SERVICE, LINEAR_KEYCHAIN_SERVICE, is_host};
    let mut args: Vec<String> = vec!["add-generic-password".into(), "-U".into(), "-s".into()];
    let done = match worker {
        LoginWorker::Jira => {
            let Some(site) = site.map(|s| s.trim().to_lowercase()).filter(|s| is_host(s)) else {
                bail!("give the Jira site as a host name: provefab login jira --site acme.atlassian.net");
            };
            print!("Atlassian account e-mail for {site}: ");
            std::io::stdout().flush()?;
            let mut email = String::new();
            std::io::stdin().read_line(&mut email)?;
            let email = email.trim().to_string();
            if !email.contains('@') {
                bail!("an account e-mail address is required");
            }
            args.extend([JIRA_KEYCHAIN_SERVICE.into(), "-a".into(), site.clone(), "-j".into(), email]);
            println!(
                "Enter your Jira API token at the Keychain prompt (create one at https://id.atlassian.com/manage-profile/security/api-tokens)."
            );
            format!("stored; repositories with kind = \"jira\" and site = \"{site}\" use it")
        }
        LoginWorker::Linear => {
            if site.is_some() {
                bail!("--site is for `provefab login jira` only");
            }
            args.extend([LINEAR_KEYCHAIN_SERVICE.into(), "-a".into(), "provefab".into()]);
            println!("Enter a Linear personal API key (from Linear's settings) at the Keychain prompt.");
            "stored; repositories with kind = \"linear\" use it".to_string()
        }
        LoginWorker::Claude | LoginWorker::Codex => unreachable!("worker logins are handled in dispatch"),
    };
    args.push("-w".into());
    let status = std::process::Command::new("security")
        .args(&args)
        .status()
        .context("running security")?;
    if !status.success() {
        bail!("could not store the credential in the Keychain");
    }
    println!("{done}");
    Ok(())
}
```

- [ ] **Step 4: Run all checks, and one real execution of the login refusals**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

Run (no Keychain is touched: both commands stop before `security`):

```bash
cargo run -q -p provefab -- login jira 2>&1 | grep -c "provefab login jira --site acme.atlassian.net"
cargo run -q -p provefab -- login claude --site x 2>&1 | grep -c "is for \`provefab login jira\` only"
```

Expected: `1` twice.

- [ ] **Step 5: Commit**

```bash
git add -A crates/provefab
git commit -m "tracker: [repos.tracker] with validation, Jira and Linear credentials, provefab login jira|linear

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Ticket identity: `issue_key`, references, branch, PR title and first line, prompts, commit, `status`/`log`/`export`/`prune`

**Files:**
- Create: `crates/provefab/migrations/0006_issue_key.sql`
- Create: `crates/provefab/tests/trackers.rs`
- Modify: `crates/provefab/src/tracker.rs` (references)
- Modify: `crates/provefab/src/forge.rs:22-24` (`is_bot_comment`), `:158-171` (`branch_name`), `:706-714` (`Issue.key`), `:825-845` and `:872-889` (`key: None` in `Gh`), tests `:1222-1246` and `:1561`
- Modify: `crates/provefab/src/store.rs:43-50` (`NewIssue`), `:53-71` (`TaskRow`), `:224-250` (`add_issue`), `:1284-1306` (`task_row`), test helper `:1569`
- Modify: `crates/provefab/src/intake.rs:55-61` (`poll`), test `issue(n)` literal
- Modify: `crates/provefab/src/pipeline.rs:487-495` (`task_branch`), `:2133-2141`, `:2412-2422`, `:2872-2884` (prompts), `:2584-2587` (commit), `:3119` (`pr_body`), `:3234` (`pr_create` title), `:1510-1513` (log line)
- Modify: `crates/provefab/prompts/plan.md`, `implement.md`, `review.md` (first and fifth lines)
- Modify: `crates/provefab/src/prompts.rs:96` (test)
- Modify: `crates/provefab/src/scheduler.rs` (`dry_run` head line)
- Modify: `crates/provefab/src/commands.rs:69-75` (`add`), `:129-140` (`status`), `:283-295` (`log`), `:528-532` (`export`), `:555` (`prune`)
- Modify: `crates/provefab/src/testkit.rs` (`FakeHub::new` issue literal, `queue_n`, new `queue_ticket`)

**Interfaces:**
- Consumes: `TrackerKind`, `TrackerConfig`, `RepoConfig::tracker_kind()` (Task 2).
- Produces:

```rust
// tracker.rs
pub fn issue_ref(number: u64, key: Option<&str>) -> String;          // "#7" | "ENG-7"
pub fn repo_ref(slug: &str, number: u64, key: Option<&str>) -> String; // "o/r#7" | "o/r ENG-7"
pub fn pr_title(title: &str, key: Option<&str>) -> String;            // title | "ENG-7: title"
pub fn pr_first_line(kind: TrackerKind, number: u64, key: Option<&str>, url: &str) -> String;
// forge.rs
pub struct Issue { /* ... */ #[serde(default)] pub key: Option<String> }
pub fn branch_name(issue: &str, title: &str) -> String; // "7" or "ENG-7"
pub fn is_bot_comment(body: &str) -> bool;               // also without Markdown emphasis
// store.rs
pub struct NewIssue { /* ... */ pub issue_key: Option<String> }
pub struct TaskRow { /* ... */ pub issue_key: Option<String> }
impl TaskRow { pub fn reference(&self) -> String; pub fn repo_reference(&self) -> String; }
// testkit.rs
pub async fn queue_ticket<R: StageRunner + Sync, O: Oracle + Sync, H: Hub + Sync>(p: &Pipeline<R, O, H>, number: u64, key: &str, url: &str) -> i64;
```

- [ ] **Step 1: Write the failing tests**

`tracker.rs` tests:

```rust
    #[test]
    fn references_titles_and_first_lines() {
        assert_eq!(issue_ref(7, None), "#7");
        assert_eq!(issue_ref(7, Some("ENG-7")), "ENG-7");
        assert_eq!(repo_ref("o/r", 7, None), "o/r#7");
        assert_eq!(repo_ref("o/r", 7, Some("ENG-7")), "o/r ENG-7");
        assert_eq!(pr_title("Fix it", None), "Fix it");
        assert_eq!(pr_title("Fix it", Some("ENG-7")), "ENG-7: Fix it");
        let gh = "https://github.com/o/r/issues/7";
        assert_eq!(pr_first_line(TrackerKind::Github, 7, None, gh), "Closes #7.");
        let jira = "https://acme.atlassian.net/browse/ENG-7";
        assert_eq!(
            pr_first_line(TrackerKind::Jira, 7, Some("ENG-7"), jira),
            "Issue: [ENG-7](https://acme.atlassian.net/browse/ENG-7)."
        );
        let linear = "https://linear.app/acme/issue/ENG-7/fix-it";
        assert_eq!(
            pr_first_line(TrackerKind::Linear, 7, Some("ENG-7"), linear),
            "Issue: [ENG-7](https://linear.app/acme/issue/ENG-7/fix-it). Fixes ENG-7"
        );
        // A task queued from GitHub before the repository moved keeps its closing line.
        assert_eq!(pr_first_line(TrackerKind::Jira, 7, None, gh), "Closes #7.");
    }
```

`forge.rs` tests: change `branch_names_are_short_and_safe` calls to `branch_name("42", ...)`, `branch_name("7", ...)`, `branch_name("3", ...)` and add:

```rust
    #[test]
    fn ticket_branches_keep_the_key_in_upper_case() {
        assert_eq!(
            branch_name("ENG-123", "Fix: crash"),
            "provefab/ENG-123-fix-crash"
        );
    }

    /// A Jira ADF round trip drops the asterisks of the bot line, and Linear
    /// may rewrite `*x*` as `_x_` (issue trackers spec §8).
    #[test]
    fn bot_comments_are_recognised_without_their_emphasis() {
        assert!(is_bot_comment(
            "Posted by Provefab (automated), not typed by a person.\n\nWhich version?"
        ));
        assert!(is_bot_comment(
            "_Posted by Provefab (automated), not typed by a person._\n\nhi"
        ));
        assert!(is_bot_comment(&format!("  {BOT_PREFIX}\n\nhi")));
        assert!(!is_bot_comment("Posted by Provefab, I think"));
        assert!(!is_bot_comment("It is version 2."));
    }
```

`store.rs` tests:

```rust
    #[tokio::test]
    async fn a_ticket_key_is_stored_and_shown() {
        let (_d, s) = store().await;
        let jira = s
            .add_issue(&NewIssue {
                issue_key: Some("ENG-7".into()),
                url: "https://acme.atlassian.net/browse/ENG-7".into(),
                ..issue(7)
            })
            .await
            .unwrap()
            .unwrap();
        let t = s.task(jira).await.unwrap().unwrap();
        assert_eq!(t.issue_key.as_deref(), Some("ENG-7"));
        assert_eq!((t.reference(), t.repo_reference()), ("ENG-7".into(), "o/r ENG-7".into()));
        let gh = s.add_issue(&issue(8)).await.unwrap().unwrap();
        let t = s.task(gh).await.unwrap().unwrap();
        assert_eq!(
            (t.issue_key.clone(), t.reference(), t.repo_reference()),
            (None, "#8".into(), "o/r#8".into())
        );
    }
```

(`store()` and `issue(n)` are the store test module's existing helpers, `store.rs:1569-1583`.)

`crates/provefab/tests/trackers.rs`:

```rust
//! Pipeline runs on a Jira-like and a Linear-like ticket (issue trackers spec
//! §10): `FakeHub` with a ticket key, the repository configured for that tracker.

use provefab::store::{now, rfc3339};
use provefab::testkit::*;
use provefab::tracker::{TrackerConfig, TrackerKind};

const JIRA_URL: &str = "https://acme.atlassian.net/browse/ENG-7";
const LINEAR_URL: &str = "https://linear.app/acme/issue/ENG-7/add-a-feature-file";

fn on(f: &mut Fixture, kind: TrackerKind) {
    f.config.repos[0].tracker = Some(TrackerConfig {
        kind,
        site: (kind == TrackerKind::Jira).then(|| "acme.atlassian.net".to_string()),
        project: Some("ENG".into()),
    });
}

fn url_of(kind: TrackerKind) -> &'static str {
    if kind == TrackerKind::Linear { LINEAR_URL } else { JIRA_URL }
}

fn ticket_hub(kind: TrackerKind, body: &str) -> FakeHub {
    let mut hub = FakeHub::new(body);
    hub.issue.key = Some("ENG-7".into());
    hub.issue.url = url_of(kind).into();
    hub
}

async fn opened(kind: TrackerKind) -> (Fixture, Pipeline<FakeRunner, FakeOracle, FakeHub>, i64) {
    let mut f = fixture(&["test -f feature.txt"]);
    on(&mut f, kind);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), ticket_hub(kind, "please")).await;
    let id = queue_ticket(&p, 7, "ENG-7", url_of(kind)).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    (f, p, id)
}

#[tokio::test]
async fn a_jira_ticket_becomes_a_pr_named_after_its_key() {
    let (f, p, id) = opened(TrackerKind::Jira).await;
    let (head, base, title, body) = p.hub.prs.lock().unwrap()[0].clone();
    assert_eq!((head.as_str(), base.as_str()), ("provefab/ENG-7-add-a-feature-file", "main"));
    assert_eq!(title, "ENG-7: Add a feature file");
    assert_eq!(
        body.lines().next().unwrap(),
        "Issue: [ENG-7](https://acme.atlassian.net/browse/ENG-7)."
    );
    assert!(!body.contains("Closes #"), "{body}");
    let message = git(&f.origin, &["log", "-1", "--format=%B", "provefab/ENG-7-add-a-feature-file"]);
    assert!(message.contains(&format!("Provefab task {id}, issue ENG-7.")), "{message}");
    let plan_prompt = p.runner.calls()[0].2.clone();
    assert!(plan_prompt.contains("Issue ENG-7: Add a feature file"), "{plan_prompt}");
    assert_eq!(
        p.hub.last_labels(),
        (vec!["provefab:in-pr".into()], vec!["provefab".into()])
    );
}

#[tokio::test]
async fn a_linear_ticket_pr_lets_linear_link_it() {
    let (_f, p, _id) = opened(TrackerKind::Linear).await;
    let (head, _, title, body) = p.hub.prs.lock().unwrap()[0].clone();
    assert_eq!(head, "provefab/ENG-7-add-a-feature-file");
    assert_eq!(title, "ENG-7: Add a feature file");
    assert_eq!(
        body.lines().next().unwrap(),
        "Issue: [ENG-7](https://linear.app/acme/issue/ENG-7/add-a-feature-file). Fixes ENG-7"
    );
}

#[tokio::test]
async fn status_log_and_export_show_the_key() {
    let (_f, p, id) = opened(TrackerKind::Jira).await;
    let status = provefab::commands::status(&p.store).await.unwrap();
    assert!(status.contains("o/r ENG-7"), "{status}");
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.starts_with(&format!("task {id} o/r ENG-7 \"Add a feature file\"")), "{log}");
    let export = provefab::commands::export(&p.store, None, None, false).await.unwrap();
    let first: Value = serde_json::from_str(export.lines().next().unwrap()).unwrap();
    assert_eq!((first["issue"].as_u64(), first["issue_key"].as_str()), (Some(7), Some("ENG-7")));
}

#[tokio::test]
async fn a_member_answers_a_question_and_the_bot_line_without_emphasis_does_not() {
    let mut f = fixture(&["true"]);
    on(&mut f, TrackerKind::Jira);
    let oracle = FakeOracle {
        verdict: Some(verdict(TaskKind::Feature, 0.9)),
        reply: Some(0.9),
        ..Default::default()
    };
    let p = pipeline(&f, Box::new(happy), oracle, ticket_hub(TrackerKind::Jira, "do it")).await;
    let id = queue_ticket(&p, 7, "ENG-7", JIRA_URL).await;
    assert_eq!(p.drive(id).await.unwrap(), NeedsInfo);
    let member = |author: &str, body: &str, later: i64| Comment {
        author: author.into(),
        association: "MEMBER".into(),
        body: body.into(),
        created_at: rfc3339(now() + later),
    };
    // Provefab's own question as Jira reads it back: no asterisks.
    p.hub.comments.lock().unwrap().push(member(
        "acc-operator",
        "Posted by Provefab (automated), not typed by a person.\n\nWhich version?",
        5,
    ));
    assert_eq!(p.step(id).await.unwrap(), NeedsInfo);
    // The same account, typing as a person, is heard (plan decision 4).
    p.hub.comments.lock().unwrap().push(member("acc-operator", "It should create feature.txt", 6));
    assert_eq!(p.step(id).await.unwrap(), Queued);
    assert_eq!(
        p.hub.last_labels(),
        (vec!["provefab".into()], vec!["provefab:needs-info".into()])
    );
}

#[tokio::test]
async fn a_ticket_reopened_after_merge_starts_a_pass_on_a_keyed_branch() {
    let (_f, p, id) = opened(TrackerKind::Linear).await;
    *p.hub.pr_status.lock().unwrap() = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Merged,
        comments: vec![],
        head_sha: None,
        merge_sha: None,
        base_ref: None,
        commit_count: None,
    };
    assert_eq!(p.watch_pr(id).await.unwrap(), PrOpen);
    // Merged and the ticket done: nothing to do (as `tests/autonomy.rs` does it).
    assert_eq!(p.watch_pr(id).await.unwrap(), PrOpen);
    p.hub.issue_is_open.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(p.watch_pr(id).await.unwrap(), Queued);
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let prs = p.hub.prs.lock().unwrap().clone();
    assert_eq!(prs.last().unwrap().0, "provefab/ENG-7-add-a-feature-file-r2");
}
```

`prompts.rs` test `renders_placeholders_and_keeps_the_rules`: replace `("number", "7")` with `("ref", "#7")` (the assertion `p.contains("Issue #7: Crash")` stays).

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab references_titles ticket_branches bot_comments_are_recognised a_ticket_key_is_stored --test trackers`
Expected: compile errors (`issue_key`, `queue_ticket`, `issue_ref`, `branch_name` takes `u64`).

- [ ] **Step 3: Implement**

`migrations/0006_issue_key.sql`:

```sql
-- Issue trackers (docs/specs/2026-10-01-issue-trackers-design.md section 4): the
-- ticket key (ENG-123) of a Jira or Linear issue; NULL for a GitHub issue.
-- issue_number and UNIQUE (repo, issue_number) are unchanged: one project per repo.
ALTER TABLE tasks ADD COLUMN issue_key TEXT;
```

`tracker.rs` (append after `linear_key`):

```rust
/// How Provefab names an issue: `#123` on GitHub, its key (`ENG-123`) on a tracker.
pub fn issue_ref(number: u64, key: Option<&str>) -> String {
    match key {
        Some(k) => k.to_string(),
        None => format!("#{number}"),
    }
}

/// An issue with its repository: `o/r#123`, or `o/r ENG-123`.
pub fn repo_ref(slug: &str, number: u64, key: Option<&str>) -> String {
    match key {
        Some(k) => format!("{slug} {k}"),
        None => format!("{slug}#{number}"),
    }
}

/// The pull request title: the issue title, prefixed with a ticket's key so
/// the Jira and Linear GitHub integrations link it (spec §4).
pub fn pr_title(title: &str, key: Option<&str>) -> String {
    match key {
        Some(k) => format!("{k}: {title}"),
        None => title.to_string(),
    }
}

/// The pull request body's first line (spec §4). `Fixes ENG-123` lets Linear's
/// own GitHub integration close the ticket on merge if the team enabled it;
/// Provefab never changes a status itself.
pub fn pr_first_line(kind: TrackerKind, number: u64, key: Option<&str>, url: &str) -> String {
    match (key, kind) {
        (None, _) => format!("Closes #{number}."),
        (Some(k), TrackerKind::Linear) => format!("Issue: [{k}]({url}). Fixes {k}"),
        (Some(k), _) => format!("Issue: [{k}]({url})."),
    }
}
```

`forge.rs`:

```rust
/// Whether a comment was posted by Provefab, under its current or former name.
/// Compared without Markdown emphasis: a Jira ADF round trip drops the
/// asterisks of the bot line (issue trackers spec §8).
pub fn is_bot_comment(body: &str) -> bool {
    let plain = |s: &str| s.replace(['*', '_'], "");
    let start: String = body.trim_start().chars().take(200).collect();
    let start = plain(&start);
    [BOT_PREFIX, LEGACY_BOT_PREFIX]
        .iter()
        .any(|p| start.starts_with(&plain(p)))
}
```

```rust
/// `provefab/<issue>-<slug of the title>`, at most 60 characters. `issue` is
/// the number on GitHub, the key (`ENG-123`, upper case) on a tracker.
pub fn branch_name(issue: &str, title: &str) -> String {
```

(only the signature and doc change; the body is unchanged.) `Issue` gains, after `labels`:

```rust
    /// The ticket key on Jira or Linear (`ENG-123`); `None` on GitHub.
    #[serde(default)]
    pub key: Option<String>,
```

and both `Issue { ... }` literals in `Gh::labeled_issues` and `Gh::issue`, and the test literal near line 1561, get `key: None,`.

`store.rs`: `NewIssue` gains `pub issue_key: Option<String>,` (doc: `/// The ticket key on Jira or Linear; None on GitHub.`); `TaskRow` gains `pub issue_key: Option<String>,` after `issue_number`. `add_issue`:

```rust
            "INSERT INTO tasks (repo, issue_number, issue_key, issue_url, title, author, state, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (repo, issue_number) DO NOTHING",
        )
        // GitHub slugs are case-insensitive; (repo, number) is the issue's identity.
        .bind(issue.repo.to_lowercase())
        .bind(issue.number as i64)
        .bind(&issue.issue_key)
        .bind(&issue.url)
```

`task_row`: `issue_key: r.get("issue_key"),` after `issue_number`. Then:

```rust
impl TaskRow {
    /// `#123` or `ENG-123` (issue trackers spec §4).
    pub fn reference(&self) -> String {
        crate::tracker::issue_ref(self.issue_number, self.issue_key.as_deref())
    }

    /// `o/r#123` or `o/r ENG-123`.
    pub fn repo_reference(&self) -> String {
        crate::tracker::repo_ref(&self.repo, self.issue_number, self.issue_key.as_deref())
    }
}
```

The store test helper `issue(n)` gets `issue_key: None,`.

`intake.rs` `poll`: `issue_key: issue.key,` in the `NewIssue` literal; test `issue(n)` gets `key: None,`.

The migration checksum test `store.rs` `migrations_are_frozen` pins one SHA-384 per migration. After writing `0006_issue_key.sql` (and never editing it again once committed: installed databases would refuse to open), append its checksum to the test's list:

```bash
shasum -a 384 crates/provefab/migrations/0006_issue_key.sql | cut -d' ' -f1
```

and add that hex string as the sixth element of the `sums` array.

`pipeline.rs`:

```rust
fn task_branch(task: &TaskRow) -> String {
    let id = task
        .issue_key
        .clone()
        .unwrap_or_else(|| task.issue_number.to_string());
    let base = branch_name(&id, &task.title);
```

In each of the three prompt renders (plan, implement, review), replace `let number = task.issue_number.to_string();` with `let reference = task.reference();` and `("number", &number),` with `("ref", &reference),`. Commit message:

```rust
        let message = format!(
            "{}\n\nProvefab task {}, issue {}.",
            task.title,
            task.id,
            task.reference()
        );
```

`pr_body` first line:

```rust
        let mut b = format!(
            "{}\n\n",
            crate::tracker::pr_first_line(
                repo.tracker_kind(),
                task.issue_number,
                task.issue_key.as_deref(),
                &task.issue_url,
            )
        );
```

`open_pr`:

```rust
        let title = crate::tracker::pr_title(&task.title, task.issue_key.as_deref());
        let url = match self
            .hub
            .pr_create(&repo.slug, &branch, &repo.base, &title, &body)
            .await
```

and the `could not read {}#{}` log line in `watch_merged` becomes `eprintln!("provefab: could not read {}: {e}", task.repo_reference());`.

Prompts: in each of `plan.md`, `implement.md`, `review.md`, line 1 `... turns a GitHub issue into a pull request.` becomes `... turns an issue into a pull request.` and line 5 `Issue #{{number}}: {{title}}` becomes `Issue {{ref}}: {{title}}`.

`scheduler.rs` `dry_run`: `let head = format!("{} {}", crate::tracker::repo_ref(&repo.slug, issue.number, issue.key.as_deref()), issue.title);`

`commands.rs`: in `add`, the `NewIssue` literal gets `issue_key: issue.key.clone(),`. `status`:

```rust
        let _ = writeln!(
            out,
            "{:>4}  {}  {:<12} {}{}",
            t.id,
            t.repo_reference(),
            t.state.as_str(),
```

`log` header: `"task {} {} \"{}\" ({})\nstate ..."` with `t.repo_reference()` in place of `t.repo, t.issue_number`. `export` event line: `"issue": t.issue_number, "issue_key": t.issue_key,`. `prune`: `let _ = writeln!(out, "task {} {}", t.id, t.repo_reference());`.

`testkit.rs`: `FakeHub::new`'s `Issue` literal gets `key: None,`; `queue_n`'s `NewIssue` gets `issue_key: None,`; add after `queue_n`:

```rust
/// Queues a Jira or Linear ticket: `number` with its `key` (`ENG-7`).
pub async fn queue_ticket<R: StageRunner + Sync, O: Oracle + Sync, H: Hub + Sync>(
    p: &Pipeline<R, O, H>,
    number: u64,
    key: &str,
    url: &str,
) -> i64 {
    p.store
        .add_issue(&NewIssue {
            repo: "o/r".into(),
            number,
            issue_key: Some(key.into()),
            url: url.into(),
            title: "Add a feature file".into(),
            author: "alice".into(),
        })
        .await
        .unwrap()
        .unwrap()
}
```

- [ ] **Step 4: Run all checks**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green; `tests/pipeline.rs` (`Closes #7`, `provefab/7-...`), `tests/commands.rs` (`o/r#7`) and every other existing test unchanged and passing (GitHub output is byte-identical).

- [ ] **Step 5: Commit**

```bash
git add -A crates/provefab
git commit -m "trackers: ticket keys in the store and in every reference (branch, PR, prompts, commit, status, log, export)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: Jira Cloud adapter, ADF converter, HTTP error rules

**Files:**
- Create: `crates/provefab/src/jira.rs`
- Modify: `crates/provefab/src/lib.rs` (`pub mod jira;` after `pub mod jevq;`)
- Modify: `crates/provefab/src/tracker.rs` (timestamps, `number_of`, HTTP helpers)
- Modify: `crates/provefab/src/forge.rs:27-75` (`ForgeError::Tracker`, `is_permanent`), `:771-784` (`ISSUE_LIMIT`, `issue_limit_warning` become `pub(crate)`)

**Interfaces:**
- Consumes: `Tracker` (Task 1), `JiraAuth` (Task 2), `Issue.key`, `is_bot_comment` (Task 3), `forge::with_prefix`, `store::rfc3339`.
- Produces:

```rust
// forge.rs
ForgeError::Tracker { service: &'static str, status: Option<u16>, message: String } // "{service}: HTTP {status}: {message}" or "{service}: network: {message}"
pub(crate) const ISSUE_LIMIT: usize; pub(crate) fn issue_limit_warning(slug: &str, label: &str, count: usize) -> Option<String>;
// tracker.rs
pub fn utc_seconds(s: &str) -> Option<String>;                 // -> "YYYY-MM-DDTHH:MM:SSZ"
pub fn number_of(key: &str, project: &str) -> Option<u64>;     // ("ENG-7", "ENG") -> 7
pub const SHORT_RETRY: u64 = 30;
pub fn http_client() -> reqwest::Client;
pub async fn send(service: &'static str, secrets: &[&str], make: impl Fn() -> reqwest::RequestBuilder) -> Result<(u16, Option<u64>, String), ForgeError>;
pub fn http_error(service: &'static str, status: u16, retry_after: Option<u64>, body: &str, secrets: &[&str]) -> ForgeError;
pub fn redact(text: &str, secrets: &[&str]) -> String;
// jira.rs
pub struct Jira { pub api: String, pub site: String, pub project: String, /* auth, http: private */ }
impl Jira { pub fn new(api: &str, site: &str, project: &str, auth: JiraAuth) -> Self; pub async fn check(&self) -> Result<String, ForgeError>; }
impl Tracker for Jira { /* all seven */ }
pub fn jql(project: &str, label: &str) -> String;
pub fn to_adf(markdown: &str) -> serde_json::Value;
pub fn from_adf(doc: &serde_json::Value) -> String;
```

- [ ] **Step 1: Write the failing tests**

`tracker.rs` tests:

```rust
    #[test]
    fn utc_seconds_normalises_offsets_fractions_and_dates() {
        for (raw, utc) in [
            ("2026-10-01T10:00:00.000+0200", "2026-10-01T08:00:00Z"),
            ("2026-10-01T01:30:00.123+02:00", "2026-09-30T23:30:00Z"),
            ("2026-10-01T08:00:00.123Z", "2026-10-01T08:00:00Z"),
            ("2026-10-01T08:00:00Z", "2026-10-01T08:00:00Z"),
            ("2024-02-29T23:00:00-0130", "2024-03-01T00:30:00Z"),
            ("2026-12-31T23:59:59.999-0100", "2027-01-01T00:59:59Z"),
        ] {
            assert_eq!(utc_seconds(raw).as_deref(), Some(utc), "{raw}");
        }
        for bad in ["", "2026-10-01", "garbage", "2026-13-01T00:00:00Z", "2026-10-01T08:00:00", "2026-10-01T08:00:00+2"] {
            assert_eq!(utc_seconds(bad), None, "{bad}");
        }
        // Provefab's own timestamps are a fixed point, so string order is time order.
        for secs in [0, 951_782_400, 1_790_000_000] {
            let s = crate::store::rfc3339(secs);
            assert_eq!(utc_seconds(&s), Some(s.clone()));
        }
    }

    #[test]
    fn numbers_come_from_keys_of_the_project() {
        assert_eq!(number_of("ENG-123", "ENG"), Some(123));
        assert_eq!(number_of("OPS-123", "ENG"), None);
        assert_eq!(number_of("ENG-x", "ENG"), None);
        assert_eq!(number_of("ENGX-1", "ENG"), None);
    }

    #[test]
    fn http_errors_are_classified_and_redacted() {
        let secrets = ["tok-SECRET-123", "bot@acme.test"];
        let body = r#"{"errorMessages":["no access for bot@acme.test with tok-SECRET-123"]}"#;
        for (status, permanent) in [(400, true), (401, true), (403, true), (404, true), (408, false), (429, false), (500, false), (503, false)] {
            let e = http_error("jira", status, None, body, &secrets);
            assert_eq!(e.is_permanent(), permanent, "{status}");
            let shown = format!("{e} {e:?}");
            assert!(!shown.contains("tok-SECRET-123") && !shown.contains("bot@acme.test"), "{shown}");
            assert!(shown.contains(&format!("HTTP {status}")), "{shown}");
        }
        let e = http_error("jira", 429, Some(120), "", &secrets);
        assert!(e.to_string().contains("retry after 120 s"), "{e}");
    }
```

`jira.rs` `#[cfg(test)] mod tests` (at the end of the file created in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::is_bot_comment;
    use crate::intake::new_replies;
    use wiremock::matchers::{body_json, header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SECRET: &str = "tok-SECRET-123";
    const EMAIL: &str = "bot@acme.test";

    fn jira(server: &MockServer) -> Jira {
        Jira::new(
            &server.uri(),
            "acme.atlassian.net",
            "ENG",
            JiraAuth { email: EMAIL.into(), token: SECRET.into() },
        )
    }

    fn adf(text: &str) -> Value {
        json!({"version": 1, "type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": text}]}]})
    }

    fn ticket(n: u64) -> Value {
        json!({"key": format!("ENG-{n}"), "fields": {
            "summary": format!("Ticket {n}"),
            "description": adf("Crash on start"),
            "reporter": {"accountId": "acc-alice"},
            "labels": ["provefab"]
        }})
    }

    #[test]
    fn jql_quotes_and_escapes_every_value() {
        assert_eq!(
            jql("ENG", "provefab"),
            r#"project = "ENG" AND labels = "provefab" AND statusCategory != Done ORDER BY created ASC"#
        );
        assert_eq!(
            jql("ENG", r#"pro"fab\x OR project = OPS"#),
            r#"project = "ENG" AND labels = "pro\"fab\\x OR project = OPS" AND statusCategory != Done ORDER BY created ASC"#
        );
    }

    #[test]
    fn adf_round_trip_keeps_what_provefab_compares() {
        let md = "*Posted by Provefab (automated), not typed by a person.*\n\nProvefab needs a person to continue.\n\nReason: the `gates` failed, see [the log](https://example.com/log).\n\n- one\n- `two`\n\n```sh\ncargo test\n```\n\n<!-- provefab-post-merge:3 -->";
        let text = from_adf(&to_adf(md));
        assert_eq!(
            text,
            "Posted by Provefab (automated), not typed by a person.\n\nProvefab needs a person to continue.\n\nReason: the `gates` failed, see [the log](https://example.com/log).\n\n- one\n- `two`\n\n```sh\ncargo test\n```\n\n<!-- provefab-post-merge:3 -->"
        );
        assert!(is_bot_comment(&text));
        assert!(text.contains(&crate::post_merge::marker(3)));
    }

    #[test]
    fn adf_marks_breaks_and_plain_stars() {
        let doc = to_adf("**bold** and *it*\nnext line");
        let para = &doc["content"][0]["content"];
        assert_eq!(para[0]["marks"][0]["type"], "strong");
        assert_eq!(para[2]["marks"][0]["type"], "em");
        assert_eq!(para[3]["type"], "hardBreak");
        let plain = to_adf("2 * 3 * 4");
        assert_eq!(plain["content"][0]["content"], json!([{"type": "text", "text": "2 * 3 * 4"}]));
        assert_eq!(to_adf("")["content"], json!([]));
        // Nodes Provefab never writes still read as text.
        let rich = json!({"type": "doc", "content": [
            {"type": "heading", "attrs": {"level": 2}, "content": [{"type": "text", "text": "Steps"}]},
            {"type": "orderedList", "content": [
                {"type": "listItem", "content": [{"type": "paragraph", "content": [
                    {"type": "mention", "attrs": {"text": "@Bob"}}, {"type": "text", "text": " runs it"}]}]}]},
            {"type": "panel", "content": [{"type": "paragraph", "content": [{"type": "inlineCard", "attrs": {"url": "https://x.test"}}]}]}
        ]});
        assert_eq!(from_adf(&rich), "## Steps\n\n1. @Bob runs it\n\nhttps://x.test");
    }

    #[tokio::test]
    async fn open_issues_pages_with_the_token_and_builds_the_jql() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/search/jql"))
            .and(header("authorization", "Basic Ym90QGFjbWUudGVzdDp0b2stU0VDUkVULTEyMw=="))
            .and(query_param("jql", jql("ENG", "provefab")))
            .and(query_param("fields", "summary,description,reporter,labels"))
            .and(query_param_is_missing("nextPageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"issues": [ticket(1)], "nextPageToken": "p2", "isLast": false}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/search/jql"))
            .and(query_param("nextPageToken", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"issues": [ticket(2)], "isLast": true})))
            .expect(1)
            .mount(&server)
            .await;
        let issues = jira(&server).open_issues("acme/api", "provefab").await.unwrap();
        assert_eq!(issues.iter().map(|i| i.number).collect::<Vec<_>>(), [1, 2]);
        let i = &issues[0];
        assert_eq!(i.key.as_deref(), Some("ENG-1"));
        assert_eq!(i.url, "https://acme.atlassian.net/browse/ENG-1");
        assert_eq!((i.title.as_str(), i.body.as_str(), i.author.as_str()), ("Ticket 1", "Crash on start", "acc-alice"));
        assert_eq!(i.labels, ["provefab"]);
    }

    #[tokio::test]
    async fn open_issues_stop_at_the_cap() {
        let server = MockServer::start().await;
        let page: Vec<Value> = (1..=100).map(ticket).collect();
        Mock::given(method("GET"))
            .and(path("/rest/api/3/search/jql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"issues": page, "nextPageToken": "more", "isLast": false})))
            .expect(10)
            .mount(&server)
            .await;
        let issues = jira(&server).open_issues("acme/api", "provefab").await.unwrap();
        assert_eq!(issues.len(), crate::forge::ISSUE_LIMIT);
    }

    #[tokio::test]
    async fn labels_are_added_and_removed_in_one_update_and_never_created() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .and(body_json(json!({"update": {"labels": [{"add": "provefab:in-pr"}, {"remove": "provefab"}]}})))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let j = jira(&server);
        j.edit_labels("acme/api", 7, &["provefab:in-pr"], &["provefab"]).await.unwrap();
        j.ensure_label("acme/api", "provefab:failed", "d93f0b", "x").await.unwrap();
        j.edit_labels("acme/api", 7, &[], &[]).await.unwrap();
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn comments_are_adf_both_ways_with_members_and_utc_times() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/rest/api/3/issue/ENG-7/comment"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "1"})))
            .expect(1)
            .mount(&server)
            .await;
        let ours = to_adf(&crate::forge::with_prefix("Which version?"));
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-7/comment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "startAt": 0, "maxResults": 100, "total": 4,
                "comments": [
                    {"author": {"accountId": "acc-op", "accountType": "atlassian"}, "body": ours, "created": "2026-10-01T09:30:00.000+0200"},
                    {"author": {"accountId": "acc-bob", "accountType": "atlassian"}, "body": adf("It is v2"), "created": "2026-10-01T10:00:00.000+0200"},
                    {"author": {"accountId": "acc-app", "accountType": "app"}, "body": adf("Build passed"), "created": "2026-10-01T10:01:00.000+0200"},
                    {"author": {"accountId": "acc-bob", "accountType": "atlassian"}, "body": adf("earlier"), "created": "2026-10-01T09:00:00.000+0200"}
                ]
            })))
            .mount(&server)
            .await;
        let j = jira(&server);
        j.comment("acme/api", 7, "Which version?").await.unwrap();
        let sent: Value = server.received_requests().await.unwrap()[0].body_json().unwrap();
        assert_eq!(sent["body"]["type"], "doc");
        assert_eq!(sent["body"]["content"][0]["content"][0]["marks"][0]["type"], "em");
        let comments = j.comments("acme/api", 7).await.unwrap();
        assert_eq!(comments.len(), 3, "the app's comment is not a person's");
        assert!(comments.iter().all(|c| c.association == "MEMBER"));
        assert_eq!(comments[1].created_at, "2026-10-01T08:00:00Z");
        // The question was asked at 07:30 UTC: only Bob's later reply counts.
        let replies = new_replies(&comments, "acc-alice", Some("2026-10-01T07:30:00Z"));
        assert_eq!(replies.iter().map(|c| c.body.as_str()).collect::<Vec<_>>(), ["It is v2"]);
    }

    #[tokio::test]
    async fn issue_and_issue_open_read_one_ticket() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .and(query_param("fields", "summary,description,reporter,labels"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ticket(7)))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .and(query_param("fields", "status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": "ENG-7", "fields": {"status": {"statusCategory": {"key": "done"}}}})))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-8"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": "ENG-8", "fields": {"status": {"statusCategory": {"key": "indeterminate"}}}})))
            .mount(&server)
            .await;
        let j = jira(&server);
        assert_eq!(j.issue("acme/api", 7).await.unwrap().key.as_deref(), Some("ENG-7"));
        assert!(!j.issue_open("acme/api", 7).await.unwrap());
        assert!(j.issue_open("acme/api", 8).await.unwrap());
    }

    #[tokio::test]
    async fn errors_are_permanent_or_transient_and_never_hold_credentials() {
        let echo = json!({"errorMessages": [format!("nothing for {EMAIL} {SECRET}")]});
        for (n, status, retry, permanent) in [(1u64, 401u16, None, true), (2, 403, None, true), (3, 404, None, true), (4, 500, None, false), (5, 429, Some("120"), false)] {
            let server = MockServer::start().await;
            let mut answer = ResponseTemplate::new(status).set_body_json(echo.clone());
            if let Some(r) = retry {
                answer = answer.insert_header("Retry-After", r);
            }
            Mock::given(method("GET")).respond_with(answer).mount(&server).await;
            let err = jira(&server).issue("acme/api", n).await.unwrap_err();
            assert_eq!(err.is_permanent(), permanent, "{status}: {err}");
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(SECRET) && !shown.contains(EMAIL), "{shown}");
        }
    }

    #[tokio::test]
    async fn a_short_retry_after_is_waited_out_once() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"fields": {"status": {"statusCategory": {"key": "new"}}}})))
            .mount(&server)
            .await;
        assert!(jira(&server).issue_open("acme/api", 7).await.unwrap());
    }

    #[tokio::test]
    async fn check_reads_the_account_and_the_project() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/rest/api/3/myself"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"accountId": "acc-op"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(path("/rest/api/3/project/ENG"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": "ENG"})))
            .mount(&server).await;
        assert_eq!(jira(&server).check().await.unwrap(), "credentials accepted; project ENG readable");
        let other = Jira::new(&server.uri(), "acme.atlassian.net", "OPS", JiraAuth { email: EMAIL.into(), token: SECRET.into() });
        assert!(other.check().await.unwrap_err().is_permanent());
    }
}
```

(The `OPS` project has no mock: wiremock answers 404, a permanent error.)

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab utc_seconds numbers_come_from http_errors jira::`
Expected: compile errors (`jira` module, `utc_seconds`, `ForgeError::Tracker` missing).

- [ ] **Step 3: Implement**

`forge.rs`: make `ISSUE_LIMIT` and `issue_limit_warning` `pub(crate)`. Add the variant after `WrongWorktree`:

```rust
    /// A Jira or Linear call that failed (issue trackers spec §8). `message`
    /// never holds a credential: the adapters redact before building it.
    #[error("{service}: {}: {message}", status_text(.status))]
    Tracker {
        service: &'static str,
        /// `None` when the service did not answer (network, timeout).
        status: Option<u16>,
        message: String,
    },
```

```rust
fn status_text(status: &Option<u16>) -> String {
    status.map_or_else(|| "network".to_string(), |s| format!("HTTP {s}"))
}
```

and in `is_permanent`:

```rust
            // A refused credential, no access, nothing at that address or any
            // other client error cannot fix itself; a timeout, a rate limit or
            // a server error may (spec §8, plan decision 10).
            ForgeError::Tracker { status, .. } => {
                status.is_some_and(|s| (400..500).contains(&s) && s != 408 && s != 429)
            }
```

`tracker.rs` (append; add `use std::time::Duration;`, `use serde_json::Value;` and `use crate::forge::ForgeError;` to the imports):

```rust
/// A timestamp as Provefab stores and compares them: UTC, whole seconds,
/// `YYYY-MM-DDTHH:MM:SSZ` (spec §6, §7). Accepts `Z`, `+HH:MM` and `+HHMM`
/// offsets and fractional seconds; `None` for anything else.
pub fn utc_seconds(s: &str) -> Option<String> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes().all(|c| c.is_ascii_digit()).then(|| part.parse().ok())?
    };
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, se) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &frac[digits..];
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let digits: String = rest[1..].chars().filter(|c| *c != ':').collect();
            if digits.len() != 4 || !digits.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            sign * (digits[..2].parse::<i64>().ok()? * 3600 + digits[2..].parse::<i64>().ok()? * 60)
        }
    };
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se - offset;
    Some(crate::store::rfc3339(secs))
}

/// Howard Hinnant's days-from-civil: the inverse of `store::rfc3339`'s.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The number of `key` when it belongs to `project`: `("ENG-123", "ENG")` gives 123.
pub fn number_of(key: &str, project: &str) -> Option<u64> {
    key.strip_prefix(project)?.strip_prefix('-')?.parse().ok()
}

/// Budget of one Jira or Linear call.
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// A 429 whose `Retry-After` is at most this many seconds is waited out once
/// inside the call (plan decision 9); a longer one is a transient error.
pub const SHORT_RETRY: u64 = 30;

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// `text` with every credential replaced: an API may echo what it was sent.
pub fn redact(text: &str, secrets: &[&str]) -> String {
    secrets
        .iter()
        .filter(|s| s.len() >= 4)
        .fold(text.to_string(), |t, s| t.replace(s, "<redacted>"))
}

/// Sends the request `make` builds (built again for the one retry) and returns
/// status, `Retry-After` seconds and body. Only a failure to get an answer is
/// an error here; the caller decides what a status means.
pub async fn send(
    service: &'static str,
    secrets: &[&str],
    make: impl Fn() -> reqwest::RequestBuilder,
) -> Result<(u16, Option<u64>, String), ForgeError> {
    let mut waited = false;
    loop {
        let resp = make()
            .send()
            .await
            .map_err(|e| network_error(service, &e, secrets))?;
        let status = resp.status().as_u16();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok());
        let body = resp
            .text()
            .await
            .map_err(|e| network_error(service, &e, secrets))?;
        match retry_after {
            Some(secs) if status == 429 && !waited && secs <= SHORT_RETRY => {
                waited = true;
                tokio::time::sleep(Duration::from_secs(secs)).await;
            }
            _ => return Ok((status, retry_after, body)),
        }
    }
}

fn network_error(service: &'static str, e: &reqwest::Error, secrets: &[&str]) -> ForgeError {
    ForgeError::Tracker {
        service,
        status: None,
        message: redact(&e.to_string(), secrets),
    }
}

/// A status that is not a success, with what the service said about it (Jira
/// `errorMessages` and `errors`, GraphQL `errors[].message`), redacted.
pub fn http_error(
    service: &'static str,
    status: u16,
    retry_after: Option<u64>,
    body: &str,
    secrets: &[&str],
) -> ForgeError {
    let what = match status {
        401 => "the credentials were refused",
        403 => "no access to this project or ticket",
        404 => "project or ticket not found",
        429 => "rate limited",
        500..=599 => "server error",
        _ => "request refused",
    };
    let mut message = what.to_string();
    let said = server_messages(body);
    if !said.is_empty() {
        message.push_str(": ");
        message.push_str(&said);
    }
    if let Some(secs) = retry_after {
        message.push_str(&format!(" (retry after {secs} s)"));
    }
    ForgeError::Tracker {
        service,
        status: Some(status),
        message: redact(&message, secrets),
    }
}

fn server_messages(body: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return String::new();
    };
    let mut out: Vec<String> = Vec::new();
    out.extend(v["errorMessages"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_string)));
    if let Some(fields) = v["errors"].as_object() {
        out.extend(fields.iter().filter_map(|(k, m)| m.as_str().map(|m| format!("{k}: {m}"))));
    }
    out.extend(v["errors"].as_array().into_iter().flatten().filter_map(|e| e["message"].as_str().map(str::to_string)));
    out.join("; ").chars().take(300).collect()
}
```

`crates/provefab/src/jira.rs`:

```rust
//! Jira Cloud as an issue tracker (issue trackers spec §6): REST API v3 with
//! an account e-mail and API token. Labels and comments only: Provefab never
//! changes a ticket's status.

use reqwest::Method;
use serde_json::{Value, json};

use crate::forge::{Comment, ForgeError, ISSUE_LIMIT, Issue, issue_limit_warning, with_prefix};
use crate::ports::Tracker;
use crate::tracker::{JiraAuth, http_client, http_error, number_of, send, utc_seconds};

const SERVICE: &str = "jira";
const FIELDS: &str = "summary,description,reporter,labels";

pub struct Jira {
    /// `https://<site>`; a test server's address in tests.
    pub api: String,
    /// The host name in ticket links, `acme.atlassian.net`.
    pub site: String,
    /// The project key: `ENG` in `ENG-123`.
    pub project: String,
    auth: JiraAuth,
    http: reqwest::Client,
}

/// The JQL Provefab builds (never the user): every value quoted and escaped,
/// so a label cannot add clauses (spec §6).
pub fn jql(project: &str, label: &str) -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    format!(
        "project = {} AND labels = {} AND statusCategory != Done ORDER BY created ASC",
        quote(project),
        quote(label)
    )
}

impl Jira {
    pub fn new(api: &str, site: &str, project: &str, auth: JiraAuth) -> Self {
        Self {
            api: api.trim_end_matches('/').to_string(),
            site: site.to_string(),
            project: project.to_string(),
            auth,
            http: http_client(),
        }
    }

    fn key(&self, number: u64) -> String {
        format!("{}-{number}", self.project)
    }

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<reqwest::Url, ForgeError> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.api))
            .map_err(|e| ForgeError::Parse("jira url".into(), e.to_string()))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    async fn call(&self, method: Method, url: reqwest::Url, body: Option<&Value>) -> Result<String, ForgeError> {
        let secrets = [self.auth.token.as_str(), self.auth.email.as_str()];
        let (status, retry_after, text) = send(SERVICE, &secrets, || {
            let r = self
                .http
                .request(method.clone(), url.clone())
                .basic_auth(&self.auth.email, Some(&self.auth.token))
                .header("Accept", "application/json");
            match body {
                Some(b) => r.json(b),
                None => r,
            }
        })
        .await?;
        if (200..300).contains(&status) {
            Ok(text)
        } else {
            Err(http_error(SERVICE, status, retry_after, &text, &secrets))
        }
    }

    async fn get_json(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, ForgeError> {
        let text = self.call(Method::GET, self.url(path, query)?, None).await?;
        serde_json::from_str(&text).map_err(|e| ForgeError::Parse("jira".into(), e.to_string()))
    }

    fn issue_of(&self, v: &Value) -> Option<Issue> {
        let key = v["key"].as_str()?;
        let f = &v["fields"];
        Some(Issue {
            number: number_of(key, &self.project)?,
            key: Some(key.to_string()),
            title: f["summary"].as_str()?.to_string(),
            body: if f["description"].is_object() {
                from_adf(&f["description"])
            } else {
                String::new()
            },
            url: format!("https://{}/browse/{key}", self.site),
            // No reporter (an import, a deleted account): nobody answers as the author.
            author: f["reporter"]["accountId"].as_str().unwrap_or_default().to_string(),
            labels: f["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|l| l.as_str().map(str::to_string))
                .collect(),
        })
    }

    /// For `provefab doctor`: the credentials authenticate and the project is readable.
    pub async fn check(&self) -> Result<String, ForgeError> {
        self.get_json("/rest/api/3/myself", &[]).await?;
        self.get_json(&format!("/rest/api/3/project/{}", self.project), &[]).await?;
        Ok(format!("credentials accepted; project {} readable", self.project))
    }
}

/// A person's comment; an app's (`accountType = "app"`) is skipped (plan decision 4).
fn comment_of(c: &Value) -> Option<Comment> {
    if c["author"]["accountType"].as_str() == Some("app") {
        return None;
    }
    let created = c["created"].as_str()?;
    let Some(created_at) = utc_seconds(created) else {
        eprintln!("provefab: a jira comment with an unreadable time {created:?} is skipped");
        return None;
    };
    Some(Comment {
        author: c["author"]["accountId"].as_str()?.to_string(),
        // Only workspace members can comment on Jira (spec §8, decision 9).
        association: "MEMBER".into(),
        body: if c["body"].is_object() {
            from_adf(&c["body"])
        } else {
            c["body"].as_str().unwrap_or_default().to_string()
        },
        created_at,
    })
}

impl Tracker for Jira {
    async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        let jql = jql(&self.project, label);
        let mut out = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query = vec![("jql", jql.as_str()), ("fields", FIELDS), ("maxResults", "100")];
            if let Some(t) = &token {
                query.push(("nextPageToken", t.as_str()));
            }
            let v = self.get_json("/rest/api/3/search/jql", &query).await?;
            out.extend(v["issues"].as_array().into_iter().flatten().filter_map(|i| self.issue_of(i)));
            let next = v["nextPageToken"].as_str().map(str::to_string);
            if out.len() >= ISSUE_LIMIT || next.is_none() || v["isLast"].as_bool() == Some(true) {
                break;
            }
            token = next;
        }
        out.truncate(ISSUE_LIMIT);
        if let Some(warning) = issue_limit_warning(slug, label, out.len()) {
            eprintln!("{warning}");
        }
        Ok(out)
    }

    async fn issue(&self, _slug: &str, number: u64) -> Result<Issue, ForgeError> {
        let v = self
            .get_json(&format!("/rest/api/3/issue/{}", self.key(number)), &[("fields", FIELDS)])
            .await?;
        self.issue_of(&v).ok_or_else(|| {
            ForgeError::Parse("jira issue".into(), format!("no key or summary for {}", self.key(number)))
        })
    }

    async fn comments(&self, _slug: &str, number: u64) -> Result<Vec<Comment>, ForgeError> {
        let path = format!("/rest/api/3/issue/{}/comment", self.key(number));
        let mut out = Vec::new();
        let mut start = 0usize;
        loop {
            let at = start.to_string();
            let v = self
                .get_json(&path, &[("startAt", at.as_str()), ("maxResults", "100"), ("orderBy", "created")])
                .await?;
            let page = v["comments"].as_array().cloned().unwrap_or_default();
            out.extend(page.iter().filter_map(comment_of));
            start += page.len();
            if page.is_empty() || start >= v["total"].as_u64().unwrap_or(0) as usize {
                break;
            }
        }
        Ok(out)
    }

    async fn comment(&self, _slug: &str, number: u64, body: &str) -> Result<(), ForgeError> {
        let url = self.url(&format!("/rest/api/3/issue/{}/comment", self.key(number)), &[])?;
        let doc = json!({"body": to_adf(&with_prefix(body))});
        self.call(Method::POST, url, Some(&doc)).await.map(|_| ())
    }

    async fn edit_labels(&self, _slug: &str, number: u64, add: &[&str], remove: &[&str]) -> Result<(), ForgeError> {
        if add.is_empty() && remove.is_empty() {
            return Ok(());
        }
        let ops: Vec<Value> = add
            .iter()
            .map(|l| json!({"add": l}))
            .chain(remove.iter().map(|l| json!({"remove": l})))
            .collect();
        let url = self.url(&format!("/rest/api/3/issue/{}", self.key(number)), &[])?;
        self.call(Method::PUT, url, Some(&json!({"update": {"labels": ops}})))
            .await
            .map(|_| ())
    }

    /// Jira labels are free text: there is nothing to create (spec §6).
    async fn ensure_label(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), ForgeError> {
        Ok(())
    }

    async fn issue_open(&self, _slug: &str, number: u64) -> Result<bool, ForgeError> {
        let v = self
            .get_json(&format!("/rest/api/3/issue/{}", self.key(number)), &[("fields", "status")])
            .await?;
        match v.pointer("/fields/status/statusCategory/key").and_then(Value::as_str) {
            Some(category) => Ok(category != "done"),
            None => Err(ForgeError::Parse("jira issue".into(), "no status category".into())),
        }
    }
}

// ---------- Atlassian Document Format ----------

/// Provefab's Markdown as an ADF document (spec §6, decision 10): paragraphs
/// (lines joined by hard breaks), `- ` bullet lists, fenced code blocks, inline
/// code, links, `*emphasis*` and `**strong**`. Anything else stays plain text.
pub fn to_adf(markdown: &str) -> Value {
    let lines: Vec<&str> = markdown.lines().collect();
    let is_bullet = |l: &str| l.trim_start().starts_with("- ");
    let is_fence = |l: &str| l.trim_start().starts_with("```");
    let mut content = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim().is_empty() {
            i += 1;
        } else if is_fence(line) {
            let t = line.trim_start();
            let ticks = t.chars().take_while(|c| *c == '`').count();
            let lang = t[ticks..].trim();
            let close = "`".repeat(ticks);
            let mut code = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim() != close {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            let text = code.join("\n");
            let mut node = json!({"type": "codeBlock", "content": []});
            if !text.is_empty() {
                node["content"] = json!([{"type": "text", "text": text}]);
            }
            if !lang.is_empty() {
                node["attrs"] = json!({"language": lang});
            }
            content.push(node);
        } else if is_bullet(line) {
            let mut items = Vec::new();
            while i < lines.len() && is_bullet(lines[i]) {
                let text = &lines[i].trim_start()[2..];
                items.push(json!({"type": "listItem", "content": [{"type": "paragraph", "content": inline(text)}]}));
                i += 1;
            }
            content.push(json!({"type": "bulletList", "content": items}));
        } else {
            let mut para: Vec<Value> = Vec::new();
            while i < lines.len() && !lines[i].trim().is_empty() && !is_bullet(lines[i]) && !is_fence(lines[i]) {
                if !para.is_empty() {
                    para.push(json!({"type": "hardBreak"}));
                }
                para.extend(inline(lines[i]));
                i += 1;
            }
            content.push(json!({"type": "paragraph", "content": para}));
        }
    }
    json!({"version": 1, "type": "doc", "content": content})
}

fn inline(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let parsed = match c {
            '`' => span(rest, "`", "`", false).map(|(t, n)| (marked(t, "code"), n)),
            '[' => link(rest),
            '*' if rest.starts_with("**") => span(rest, "**", "**", true).map(|(t, n)| (marked(t, "strong"), n)),
            '*' => span(rest, "*", "*", true).map(|(t, n)| (marked(t, "em"), n)),
            _ => None,
        };
        match parsed {
            Some((node, used)) => {
                if !plain.is_empty() {
                    out.push(json!({"type": "text", "text": std::mem::take(&mut plain)}));
                }
                out.push(node);
                rest = &rest[used..];
            }
            None => {
                plain.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    if !plain.is_empty() {
        out.push(json!({"type": "text", "text": plain}));
    }
    out
}

/// `open inner close` at the start of `s`: (inner, bytes used). `tight`: the
/// inner text neither starts nor ends with a space (so `2 * 3 * 4` stays plain).
fn span<'a>(s: &'a str, open: &str, close: &str, tight: bool) -> Option<(&'a str, usize)> {
    let body = s.strip_prefix(open)?;
    let end = body.find(close)?;
    let inner = &body[..end];
    let ok = !inner.trim().is_empty()
        && (!tight || (!inner.starts_with(char::is_whitespace) && !inner.ends_with(char::is_whitespace)));
    ok.then_some((inner, open.len() + end + close.len()))
}

fn link(s: &str) -> Option<(Value, usize)> {
    let (label, used) = span(s, "[", "](", false)?;
    let after = &s[used..];
    let end = after.find(')')?;
    let href = &after[..end];
    (href.starts_with("https://") || href.starts_with("http://")).then(|| {
        (
            json!({"type": "text", "text": label, "marks": [{"type": "link", "attrs": {"href": href}}]}),
            used + end + 1,
        )
    })
}

fn marked(text: &str, mark: &str) -> Value {
    json!({"type": "text", "text": text, "marks": [{"type": mark}]})
}

/// An ADF document as text: what agents read and what Provefab compares.
/// Code keeps its backticks and links their target; emphasis is dropped.
pub fn from_adf(doc: &Value) -> String {
    doc["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(block)
        .filter(|b| !b.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn children(node: &Value) -> impl Iterator<Item = &Value> {
    node["content"].as_array().into_iter().flatten()
}

fn block(node: &Value) -> String {
    match node["type"].as_str().unwrap_or_default() {
        "paragraph" => inline_text(node),
        "heading" => {
            let level = node["attrs"]["level"].as_u64().unwrap_or(1).clamp(1, 6) as usize;
            format!("{} {}", "#".repeat(level), inline_text(node))
        }
        "codeBlock" => format!(
            "```{}\n{}\n```",
            node["attrs"]["language"].as_str().unwrap_or_default(),
            inline_text(node)
        ),
        "bulletList" => children(node).map(|i| format!("- {}", item_text(i))).collect::<Vec<_>>().join("\n"),
        "orderedList" => children(node)
            .enumerate()
            .map(|(n, i)| format!("{}. {}", n + 1, item_text(i)))
            .collect::<Vec<_>>()
            .join("\n"),
        "rule" => "---".into(),
        _ => {
            let inner: Vec<String> = children(node).map(block).filter(|b| !b.is_empty()).collect();
            if inner.is_empty() { inline_text(node) } else { inner.join("\n\n") }
        }
    }
}

fn item_text(item: &Value) -> String {
    children(item).map(block).collect::<Vec<_>>().join("\n  ")
}

fn inline_text(node: &Value) -> String {
    let mut s = String::new();
    for n in children(node) {
        match n["type"].as_str().unwrap_or_default() {
            "text" => {
                let text = n["text"].as_str().unwrap_or_default();
                let marks: Vec<&Value> = n["marks"].as_array().into_iter().flatten().collect();
                let code = marks.iter().any(|m| m["type"] == "code");
                let href = marks
                    .iter()
                    .find(|m| m["type"] == "link")
                    .and_then(|m| m["attrs"]["href"].as_str());
                match (code, href) {
                    (true, _) => s.push_str(&format!("`{text}`")),
                    (false, Some(h)) => s.push_str(&format!("[{text}]({h})")),
                    _ => s.push_str(text),
                }
            }
            "hardBreak" => s.push('\n'),
            "mention" | "emoji" | "status" | "date" => {
                s.push_str(n["attrs"]["text"].as_str().unwrap_or_default())
            }
            "inlineCard" => s.push_str(n["attrs"]["url"].as_str().unwrap_or_default()),
            _ => s.push_str(&inline_text(n)),
        }
    }
    s
}
```

- [ ] **Step 4: Run all checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green. (`cargo fmt` first: the code above is written for reading, rustfmt owns the layout.)

- [ ] **Step 5: Commit**

```bash
git add -A crates/provefab
git commit -m "jira: Jira Cloud tracker adapter with ADF comments and HTTP error rules

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: Linear adapter

**Files:**
- Create: `crates/provefab/src/linear.rs`
- Modify: `crates/provefab/src/lib.rs` (`pub mod linear;` after `pub mod jira;`)

**Interfaces:**
- Consumes: `Tracker` (Task 1); `send`, `http_error`, `http_client`, `number_of`, `utc_seconds` (Task 4); `forge::{ISSUE_LIMIT, issue_limit_warning, with_prefix}`.
- Produces:

```rust
pub const API: &str = "https://api.linear.app/graphql";
pub struct Linear { pub api: String, pub team: String, /* key, http, team_id: private */ }
impl Linear { pub fn new(api: &str, team: &str, key: String) -> Self; pub async fn check(&self) -> Result<String, ForgeError>; }
impl Tracker for Linear { /* all seven */ }
```

- [ ] **Step 1: Write the failing tests** (in `linear.rs` `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, body_string_contains, header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const KEY: &str = "lin_api_SECRET_123";

    fn linear(server: &MockServer) -> Linear {
        Linear::new(&server.uri(), "ENG", KEY.into())
    }

    fn node(n: u64, state: &str) -> Value {
        json!({
            "id": format!("uuid-{n}"), "identifier": format!("ENG-{n}"), "number": n,
            "title": format!("Ticket {n}"), "description": "Crash on **start**",
            "url": format!("https://linear.app/acme/issue/ENG-{n}/ticket-{n}"),
            "creator": {"id": "user-alice"},
            "labels": {"nodes": [{"id": "l-trigger", "name": "provefab"}]},
            "state": {"type": state}
        })
    }

    fn data(v: Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({"data": v}))
    }

    async fn on(server: &MockServer, needle: &str, answer: ResponseTemplate) {
        Mock::given(method("POST"))
            .and(body_string_contains(needle))
            .respond_with(answer)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn open_issues_page_with_the_raw_key_and_the_filter() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("authorization", KEY))
            .and(body_string_contains("issues(first"))
            .and(body_partial_json(json!({"variables": {"team": "ENG", "label": "provefab", "after": "c1"}})))
            .respond_with(data(json!({"issues": {"nodes": [node(2, "started")], "pageInfo": {"hasNextPage": false, "endCursor": "c2"}}})))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issues(first"))
            .respond_with(data(json!({"issues": {"nodes": [node(1, "unstarted")], "pageInfo": {"hasNextPage": true, "endCursor": "c1"}}})))
            .expect(1)
            .mount(&server)
            .await;
        let issues = linear(&server).open_issues("acme/web", "provefab").await.unwrap();
        assert_eq!(issues.iter().map(|i| i.number).collect::<Vec<_>>(), [1, 2]);
        let i = &issues[0];
        assert_eq!(i.key.as_deref(), Some("ENG-1"));
        assert_eq!((i.title.as_str(), i.body.as_str(), i.author.as_str()), ("Ticket 1", "Crash on **start**", "user-alice"));
        assert_eq!(i.url, "https://linear.app/acme/issue/ENG-1/ticket-1");
        assert_eq!(i.labels, ["provefab"]);
        let sent = String::from_utf8(server.received_requests().await.unwrap()[0].body.clone()).unwrap();
        assert!(sent.contains("nin") && sent.contains("completed") && sent.contains("canceled"), "{sent}");
    }

    #[tokio::test]
    async fn open_issues_stop_at_the_cap() {
        let server = MockServer::start().await;
        let page: Vec<Value> = (1..=100).map(|n| node(n, "started")).collect();
        Mock::given(method("POST"))
            .respond_with(data(json!({"issues": {"nodes": page, "pageInfo": {"hasNextPage": true, "endCursor": "more"}}})))
            .expect(10)
            .mount(&server)
            .await;
        let issues = linear(&server).open_issues("acme/web", "provefab").await.unwrap();
        assert_eq!(issues.len(), crate::forge::ISSUE_LIMIT);
    }

    #[tokio::test]
    async fn ensure_label_creates_a_team_label_only_when_none_exists() {
        let server = MockServer::start().await;
        on(&server, "teams(", data(json!({"teams": {"nodes": [{"id": "team-1"}]}}))).await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueLabels("))
            .and(body_partial_json(json!({"variables": {"name": "provefab:failed"}})))
            .respond_with(data(json!({"issueLabels": {"nodes": [{"id": "l-failed", "name": "provefab:failed", "team": null}]}})))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueLabels("))
            .respond_with(data(json!({"issueLabels": {"nodes": [{"id": "l-other-team", "name": "provefab:in-pr", "team": {"id": "team-9"}}]}})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueLabelCreate"))
            .and(body_partial_json(json!({"variables": {"input": {"name": "provefab:in-pr", "color": "#0e8a16", "teamId": "team-1"}}})))
            .respond_with(data(json!({"issueLabelCreate": {"success": true, "issueLabel": {"id": "l-new"}}})))
            .expect(1)
            .mount(&server)
            .await;
        let l = linear(&server);
        l.ensure_label("acme/web", "provefab:in-pr", "0e8a16", "Provefab opened a pull request").await.unwrap();
        l.ensure_label("acme/web", "provefab:failed", "d93f0b", "Provefab stopped").await.unwrap();
    }

    #[tokio::test]
    async fn edit_labels_resolves_names_and_sets_the_label_ids() {
        let server = MockServer::start().await;
        on(&server, "issue(id: $id) { id identifier", data(json!({"issue": node(7, "started")}))).await;
        on(&server, "teams(", data(json!({"teams": {"nodes": [{"id": "team-1"}]}}))).await;
        on(&server, "issueLabels(", data(json!({"issueLabels": {"nodes": [{"id": "l-in-pr", "name": "provefab:in-pr", "team": {"id": "team-1"}}]}}))).await;
        Mock::given(method("POST"))
            .and(body_string_contains("issueUpdate"))
            .and(body_partial_json(json!({"variables": {"id": "uuid-7", "ids": ["l-in-pr"]}})))
            .respond_with(data(json!({"issueUpdate": {"success": true}})))
            .expect(1)
            .mount(&server)
            .await;
        linear(&server).edit_labels("acme/web", 7, &["provefab:in-pr"], &["provefab"]).await.unwrap();
    }

    #[tokio::test]
    async fn comments_are_markdown_from_people_with_utc_times() {
        let server = MockServer::start().await;
        on(&server, "issue(id: $id) { id identifier", data(json!({"issue": node(7, "started")}))).await;
        Mock::given(method("POST"))
            .and(body_string_contains("commentCreate"))
            .and(body_partial_json(json!({"variables": {"id": "uuid-7", "body": crate::forge::with_prefix("Which version?")}})))
            .respond_with(data(json!({"commentCreate": {"success": true}})))
            .expect(1)
            .mount(&server)
            .await;
        on(&server, "{ comments(first", data(json!({"issue": {"comments": {
            "nodes": [
                {"body": "_Posted by Provefab (automated), not typed by a person._\n\nWhich version?", "createdAt": "2026-10-01T07:30:00.000Z", "user": {"id": "user-op"}},
                {"body": "It is **v2**", "createdAt": "2026-10-01T08:00:00.123Z", "user": {"id": "user-bob"}},
                {"body": "Linked a PR", "createdAt": "2026-10-01T08:01:00.000Z", "user": null}
            ],
            "pageInfo": {"hasNextPage": false, "endCursor": null}
        }}}))).await;
        let l = linear(&server);
        l.comment("acme/web", 7, "Which version?").await.unwrap();
        let comments = l.comments("acme/web", 7).await.unwrap();
        assert_eq!(comments.len(), 2, "an integration's comment is not a person's");
        assert!(comments.iter().all(|c| c.association == "MEMBER"));
        assert_eq!(comments[1].created_at, "2026-10-01T08:00:00Z");
        let replies = crate::intake::new_replies(&comments, "user-alice", Some("2026-10-01T07:30:00Z"));
        assert_eq!(replies.iter().map(|c| c.body.as_str()).collect::<Vec<_>>(), ["It is **v2**"]);
    }

    #[tokio::test]
    async fn issue_open_is_false_once_completed_or_canceled() {
        for (state, open) in [("started", true), ("backlog", true), ("completed", false), ("canceled", false)] {
            let server = MockServer::start().await;
            on(&server, "issue(id:", data(json!({"issue": node(7, state)}))).await;
            assert_eq!(linear(&server).issue_open("acme/web", 7).await.unwrap(), open, "{state}");
        }
    }

    #[tokio::test]
    async fn errors_in_the_status_or_the_body_are_classified_without_the_key() {
        let echo = |code: &str, message: &str| json!({"errors": [{"message": format!("{message} ({KEY})"), "extensions": {"code": code}}]});
        for (status, body, permanent) in [
            (401, json!({}), true),
            (200, echo("AUTHENTICATION_ERROR", "Authentication required"), true),
            (200, echo("FORBIDDEN", "Forbidden"), true),
            (200, echo("INVALID_INPUT", "Entity not found: Issue"), true),
            (400, echo("RATELIMITED", "Rate limit exceeded"), false),
            (500, json!({}), false),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).respond_with(ResponseTemplate::new(status).set_body_json(body.clone())).mount(&server).await;
            let err = linear(&server).issue("acme/web", 7).await.unwrap_err();
            assert_eq!(err.is_permanent(), permanent, "{status} {body}: {err}");
            let shown = format!("{err} {err:?}");
            assert!(!shown.contains(KEY), "{shown}");
        }
    }

    #[tokio::test]
    async fn check_names_the_workspace_and_the_team() {
        let server = MockServer::start().await;
        on(&server, "viewer", data(json!({"viewer": {"id": "user-op", "organization": {"urlKey": "acme"}}, "teams": {"nodes": [{"id": "team-1"}]}}))).await;
        assert_eq!(linear(&server).check().await.unwrap(), "workspace acme; credentials accepted; team ENG readable");
        let empty = MockServer::start().await;
        on(&empty, "viewer", data(json!({"viewer": {"id": "user-op", "organization": {"urlKey": "acme"}}, "teams": {"nodes": []}}))).await;
        assert!(linear(&empty).check().await.unwrap_err().is_permanent());
    }
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab linear::`
Expected: compile error (`linear` module missing).

- [ ] **Step 3: Implement** `crates/provefab/src/linear.rs`

```rust
//! Linear as an issue tracker (issue trackers spec §7): the GraphQL API with a
//! personal API key. Labels and comments only: Provefab never changes an
//! issue's state.

use serde_json::{Value, json};
use tokio::sync::OnceCell;

use crate::forge::{Comment, ForgeError, ISSUE_LIMIT, Issue, issue_limit_warning, with_prefix};
use crate::ports::Tracker;
use crate::tracker::{http_client, http_error, number_of, send, utc_seconds};

pub const API: &str = "https://api.linear.app/graphql";
const SERVICE: &str = "linear";

/// The issue fields Provefab reads, shared by the queries below.
macro_rules! issue_fields {
    () => {
        "id identifier number title description url creator { id } labels { nodes { id name } } state { type }"
    };
}

const OPEN: &str = concat!(
    "query($team: String!, $label: String!, $after: String) { issues(first: 100, after: $after, filter: { team: { key: { eq: $team } }, labels: { name: { eq: $label } }, state: { type: { nin: [\"completed\", \"canceled\"] } } }) { nodes { ",
    issue_fields!(),
    " } pageInfo { hasNextPage endCursor } } }"
);
const ISSUE: &str = concat!("query($id: String!) { issue(id: $id) { ", issue_fields!(), " } }");
const COMMENTS: &str = "query($id: String!, $after: String) { issue(id: $id) { comments(first: 100, after: $after) { nodes { body createdAt user { id } } pageInfo { hasNextPage endCursor } } } }";
const COMMENT: &str = "mutation($id: String!, $body: String!) { commentCreate(input: { issueId: $id, body: $body }) { success } }";
const UPDATE_LABELS: &str = "mutation($id: String!, $ids: [String!]!) { issueUpdate(id: $id, input: { labelIds: $ids }) { success } }";
const FIND_LABEL: &str = "query($name: String!) { issueLabels(filter: { name: { eq: $name } }) { nodes { id name team { id } } } }";
const CREATE_LABEL: &str = "mutation($input: IssueLabelCreateInput!) { issueLabelCreate(input: $input) { success issueLabel { id } } }";
const TEAM: &str = "query($key: String!) { teams(filter: { key: { eq: $key } }) { nodes { id } } }";
const CHECK: &str = "query($key: String!) { viewer { id organization { urlKey } } teams(filter: { key: { eq: $key } }) { nodes { id } } }";

pub struct Linear {
    /// The GraphQL endpoint; a test server's address in tests.
    pub api: String,
    /// The team key: `ENG` in `ENG-123`.
    pub team: String,
    key: String,
    http: reqwest::Client,
    team_id: OnceCell<String>,
}

fn not_found(message: String) -> ForgeError {
    ForgeError::Tracker { service: SERVICE, status: Some(404), message }
}

/// The status a GraphQL `errors` answer stands for: Linear reports most
/// failures in the body, whatever the HTTP status (spec §8). `None` without errors.
fn graphql_status(v: &Value) -> Option<u16> {
    let first = v["errors"].as_array()?.first()?;
    let code = ["/extensions/code", "/extensions/type"]
        .iter()
        .filter_map(|p| first.pointer(p).and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_uppercase();
    let message = first["message"].as_str().unwrap_or_default().to_lowercase();
    Some(if code.contains("RATELIMIT") {
        429
    } else if code.contains("AUTHENTICATION") || message.contains("authentication") {
        401
    } else if code.contains("FORBIDDEN") || message.contains("forbidden") || message.contains("permission") {
        403
    } else if message.contains("not found") {
        404
    } else if code.contains("INTERNAL") {
        500
    } else {
        400
    })
}

fn succeeded(d: &Value, what: &str) -> Result<(), ForgeError> {
    if d[what]["success"].as_bool() == Some(true) {
        Ok(())
    } else {
        Err(ForgeError::Parse(format!("linear {what}"), "not successful".into()))
    }
}

/// A person's comment; one without a user is an integration's (plan decision 4).
fn comment_of(c: &Value) -> Option<Comment> {
    let author = c.pointer("/user/id")?.as_str()?.to_string();
    let created = c["createdAt"].as_str()?;
    let Some(created_at) = utc_seconds(created) else {
        eprintln!("provefab: a linear comment with an unreadable time {created:?} is skipped");
        return None;
    };
    Some(Comment {
        author,
        // Only workspace members can comment on Linear (spec §8, decision 9).
        association: "MEMBER".into(),
        body: c["body"].as_str().unwrap_or_default().to_string(),
        created_at,
    })
}

fn page_end(page: &Value) -> Option<String> {
    let more = page.pointer("/pageInfo/hasNextPage").and_then(Value::as_bool).unwrap_or(false);
    let cursor = page.pointer("/pageInfo/endCursor").and_then(Value::as_str);
    cursor.filter(|_| more).map(str::to_string)
}

impl Linear {
    pub fn new(api: &str, team: &str, key: String) -> Self {
        Self { api: api.to_string(), team: team.to_string(), key, http: http_client(), team_id: OnceCell::new() }
    }

    fn ident(&self, number: u64) -> String {
        format!("{}-{number}", self.team)
    }

    /// One GraphQL request: its `data`, or the error it stands for.
    async fn query(&self, query: &str, variables: Value) -> Result<Value, ForgeError> {
        let secrets = [self.key.as_str()];
        let body = json!({"query": query, "variables": variables});
        // Linear wants the personal key as is, without `Bearer` (spec §7).
        let (status, retry_after, text) = send(SERVICE, &secrets, || {
            self.http.post(&self.api).header("Authorization", &self.key).json(&body)
        })
        .await?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if let Some(code) = graphql_status(&v) {
            return Err(http_error(SERVICE, code, retry_after, &text, &secrets));
        }
        if !(200..300).contains(&status) {
            return Err(http_error(SERVICE, status, retry_after, &text, &secrets));
        }
        match v.get("data") {
            Some(d) if !d.is_null() => Ok(d.clone()),
            _ => Err(ForgeError::Parse("linear".into(), "no data in the answer".into())),
        }
    }

    async fn issue_value(&self, number: u64) -> Result<Value, ForgeError> {
        let ident = self.ident(number);
        let d = self.query(ISSUE, json!({"id": ident})).await?;
        match d.get("issue") {
            Some(i) if !i.is_null() => Ok(i.clone()),
            _ => Err(not_found(format!("{ident} not found"))),
        }
    }

    fn issue_of(&self, v: &Value) -> Option<Issue> {
        let ident = v["identifier"].as_str()?;
        Some(Issue {
            number: number_of(ident, &self.team)?,
            key: Some(ident.to_string()),
            title: v["title"].as_str()?.to_string(),
            body: v["description"].as_str().unwrap_or_default().to_string(),
            url: v["url"].as_str()?.to_string(),
            author: v["creator"]["id"].as_str().unwrap_or_default().to_string(),
            labels: v
                .pointer("/labels/nodes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|l| l["name"].as_str().map(str::to_string))
                .collect(),
        })
    }

    async fn team_id(&self) -> Result<&str, ForgeError> {
        self.team_id
            .get_or_try_init(|| async {
                let d = self.query(TEAM, json!({"key": self.team})).await?;
                d.pointer("/teams/nodes/0/id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .ok_or_else(|| not_found(format!("team {} not found", self.team)))
            })
            .await
            .map(String::as_str)
    }

    /// A label of that name usable on this team's issues: the team's own or a
    /// workspace label.
    async fn find_label(&self, name: &str) -> Result<Option<String>, ForgeError> {
        let team = self.team_id().await?.to_string();
        let d = self.query(FIND_LABEL, json!({"name": name})).await?;
        Ok(d.pointer("/issueLabels/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|l| {
                l["name"].as_str() == Some(name)
                    && (l["team"].is_null() || l.pointer("/team/id").and_then(Value::as_str) == Some(team.as_str()))
            })
            .and_then(|l| l["id"].as_str().map(str::to_string)))
    }

    async fn create_label(&self, name: &str, color: &str, description: &str) -> Result<String, ForgeError> {
        let team = self.team_id().await?.to_string();
        let input = json!({"name": name, "color": format!("#{color}"), "description": description, "teamId": team});
        let d = self.query(CREATE_LABEL, json!({"input": input})).await?;
        d.pointer("/issueLabelCreate/issueLabel/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| ForgeError::Parse("linear issueLabelCreate".into(), "no label id".into()))
    }

    /// For `provefab doctor`: the key authenticates and the team is readable.
    pub async fn check(&self) -> Result<String, ForgeError> {
        let d = self.query(CHECK, json!({"key": self.team})).await?;
        let workspace = d.pointer("/viewer/organization/urlKey").and_then(Value::as_str).unwrap_or("?").to_string();
        if d.pointer("/teams/nodes/0/id").is_none() {
            return Err(not_found(format!("team {} not found in workspace {workspace}", self.team)));
        }
        Ok(format!("workspace {workspace}; credentials accepted; team {} readable", self.team))
    }
}

impl Tracker for Linear {
    async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let d = self.query(OPEN, json!({"team": self.team, "label": label, "after": after})).await?;
            let page = &d["issues"];
            out.extend(page["nodes"].as_array().into_iter().flatten().filter_map(|i| self.issue_of(i)));
            after = page_end(page);
            if out.len() >= ISSUE_LIMIT || after.is_none() {
                break;
            }
        }
        out.truncate(ISSUE_LIMIT);
        if let Some(warning) = issue_limit_warning(slug, label, out.len()) {
            eprintln!("{warning}");
        }
        Ok(out)
    }

    async fn issue(&self, _slug: &str, number: u64) -> Result<Issue, ForgeError> {
        let v = self.issue_value(number).await?;
        self.issue_of(&v).ok_or_else(|| {
            ForgeError::Parse("linear issue".into(), format!("no title or URL for {}", self.ident(number)))
        })
    }

    async fn comments(&self, _slug: &str, number: u64) -> Result<Vec<Comment>, ForgeError> {
        let ident = self.ident(number);
        let mut out = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let d = self.query(COMMENTS, json!({"id": ident, "after": after})).await?;
            if d["issue"].is_null() {
                return Err(not_found(format!("{ident} not found")));
            }
            let page = &d["issue"]["comments"];
            out.extend(page["nodes"].as_array().into_iter().flatten().filter_map(comment_of));
            after = page_end(page);
            if after.is_none() {
                break;
            }
        }
        Ok(out)
    }

    async fn comment(&self, _slug: &str, number: u64, body: &str) -> Result<(), ForgeError> {
        let issue = self.issue_value(number).await?;
        let id = issue["id"].as_str().unwrap_or_default();
        let d = self.query(COMMENT, json!({"id": id, "body": with_prefix(body)})).await?;
        succeeded(&d, "commentCreate")
    }

    async fn edit_labels(&self, _slug: &str, number: u64, add: &[&str], remove: &[&str]) -> Result<(), ForgeError> {
        if add.is_empty() && remove.is_empty() {
            return Ok(());
        }
        let issue = self.issue_value(number).await?;
        let current: Vec<(String, String)> = issue
            .pointer("/labels/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|l| Some((l["id"].as_str()?.to_string(), l["name"].as_str()?.to_string())))
            .collect();
        let mut ids: Vec<String> = current
            .iter()
            .filter(|(_, name)| !remove.contains(&name.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        for name in add {
            if current.iter().any(|(_, n)| n == name) {
                continue;
            }
            let id = match self.find_label(name).await? {
                Some(id) => id,
                None => self.create_label(name, "ededed", "Provefab").await?,
            };
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        let d = self.query(UPDATE_LABELS, json!({"id": issue["id"], "ids": ids})).await?;
        succeeded(&d, "issueUpdate")
    }

    /// Creates a team label with this colour when none of that name exists (spec §7).
    async fn ensure_label(&self, _slug: &str, name: &str, color: &str, description: &str) -> Result<(), ForgeError> {
        if self.find_label(name).await?.is_none() {
            self.create_label(name, color, description).await?;
        }
        Ok(())
    }

    async fn issue_open(&self, _slug: &str, number: u64) -> Result<bool, ForgeError> {
        let v = self.issue_value(number).await?;
        match v.pointer("/state/type").and_then(Value::as_str) {
            Some(state) => Ok(!matches!(state, "completed" | "canceled")),
            None => Err(ForgeError::Parse("linear issue".into(), "no state".into())),
        }
    }
}
```

Note for the error test: an HTTP 401 with an empty JSON object has no `errors`, so it falls through to the HTTP status; an HTTP 400 carrying `RATELIMITED` maps to 429 (transient) because the body is read first.

- [ ] **Step 4: Run all checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

- [ ] **Step 5: Commit**

```bash
git add -A crates/provefab
git commit -m "linear: Linear tracker adapter over GraphQL

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: `Routed` hub, app wiring, `provefab add` ticket URLs, doctor tracker lines

**Files:**
- Modify: `crates/provefab/src/tracker.rs` (`TicketUrl`, `parse_ticket_url`, `split_key`, `Remote`, `Routed`)
- Modify: `crates/provefab/src/commands.rs:21-35` (errors), `:37-75` (`add`), new `tracker_checks`
- Modify: `crates/provefab/src/app.rs` (`Cmd::Run`, `Cmd::Add`, `Cmd::Doctor`)
- Test: `crates/provefab/tests/commands.rs`

**Interfaces:**
- Consumes: `Jira::new`, `Jira::check` (Task 4); `Linear::new`, `Linear::check`, `linear::API` (Task 5); `jira_auth`, `linear_key`, `Env`, `process_env`, `is_host`, `is_project_key` (Task 2); `RepoConfig::tracker_kind`, `Issue.key` (Tasks 2, 3).
- Produces:

```rust
// tracker.rs
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketUrl { Jira { site: String, key: String }, Linear { workspace: String, key: String } }
pub fn parse_ticket_url(url: &str) -> Option<TicketUrl>;   // key upper case, site lower case
pub fn split_key(key: &str) -> Option<(&str, u64)>;        // "ENG-12" -> ("ENG", 12), number > 0
pub enum Remote { Jira(crate::jira::Jira), Linear(crate::linear::Linear) }
pub struct Routed { pub gh: Gh, pub remotes: HashMap<String /* lower-case slug */, Remote> }
impl Routed {
    pub fn remote(&self, slug: &str) -> Option<&Remote>;
    pub async fn from_config(config: &Config, gh: Gh, security: &Path, env: Env<'_>) -> Result<Self, String>;
}
impl Tracker for Routed {} // by slug: Jira, Linear, else Gh
impl Forge for Routed {}   // always Gh
// commands.rs
CommandError::WrongTracker(String /*slug*/, &'static str /*kind*/)
CommandError::Ambiguous(String /*key*/, String /*slugs*/)
CommandError::OtherWorkspace(String /*key*/)
pub async fn tracker_checks(tools: &Tools, config: &Config, env: crate::tracker::Env<'_>) -> Vec<Check>;
```

- [ ] **Step 1: Write the failing tests**

`tracker.rs` tests (add `use crate::config::Config; use crate::forge::Gh; use crate::jira::Jira; use crate::ports::{Forge, Tracker}; use std::collections::HashMap; use serde_json::json; use wiremock::matchers::{method, path}; use wiremock::{Mock, MockServer, ResponseTemplate};` to the test module):

```rust
    #[test]
    fn ticket_urls() {
        let jira = |site: &str, key: &str| Some(TicketUrl::Jira { site: site.into(), key: key.into() });
        let linear = |ws: &str, key: &str| Some(TicketUrl::Linear { workspace: ws.into(), key: key.into() });
        assert_eq!(parse_ticket_url("https://acme.atlassian.net/browse/ENG-123"), jira("acme.atlassian.net", "ENG-123"));
        assert_eq!(
            parse_ticket_url(" https://Acme.Atlassian.net/browse/eng-123?focusedCommentId=9 "),
            jira("acme.atlassian.net", "ENG-123")
        );
        assert_eq!(parse_ticket_url("https://linear.app/acme/issue/ENG-123/fix-the-crash"), linear("acme", "ENG-123"));
        assert_eq!(parse_ticket_url("https://linear.app/acme/issue/ENG-123/"), linear("acme", "ENG-123"));
        for bad in [
            "https://acme.atlassian.net/browse/ENG",
            "https://acme.atlassian.net/browse/ENG-0",
            "https://acme.atlassian.net/jira/ENG-1",
            "https://linear.app/acme/project/ENG-1",
            "https://linear.app//issue/ENG-1",
            "ftp://acme.atlassian.net/browse/ENG-1",
            "https://github.com/o/r/issues/1",
        ] {
            assert_eq!(parse_ticket_url(bad), None, "{bad}");
        }
        assert_eq!(split_key("ENG_2-12"), Some(("ENG_2", 12)));
        assert_eq!(split_key("eng-12"), None);
    }

    #[tokio::test]
    async fn routed_sends_tickets_to_their_tracker_and_the_rest_to_github() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"key": "ENG-7", "fields": {"summary": "From Jira", "labels": []}})))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/rest/api/3/issue/ENG-7"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let auth = JiraAuth { email: "bot@acme.test".into(), token: "tok-x".into() };
        let routed = Routed {
            gh: Gh { program: "/nonexistent/gh".into() },
            remotes: HashMap::from([(
                "acme/api".to_string(),
                Remote::Jira(Jira::new(&server.uri(), "acme.atlassian.net", "ENG", auth)),
            )]),
        };
        // Slugs are case-insensitive, as everywhere else in Provefab.
        assert_eq!(routed.issue("Acme/API", 7).await.unwrap().title, "From Jira");
        // A stored pending label effect (slug, number) replays to the same tracker.
        routed.edit_labels("acme/api", 7, &["provefab:in-pr"], &["provefab"]).await.unwrap();
        // Another repository's issues and every forge call go to gh.
        assert!(matches!(routed.issue("o/r", 7).await, Err(ForgeError::Spawn { .. })));
        assert!(matches!(
            routed.pr_status("acme/api", "https://github.com/acme/api/pull/1").await,
            Err(ForgeError::Spawn { .. })
        ));
    }

    const CONFIG: &str = "[jev]\nmodel = \"jev-1.13\"\n\n[[models]]\nid = \"m\"\nworker = \"claude-code\"\nmodel = \"sonnet\"\ntier = \"standard\"\n\n[[repos]]\nslug = \"acme/api\"\ngates = [\"make\"]\n[repos.tracker]\nkind = \"jira\"\nsite = \"acme.atlassian.net\"\nproject = \"ENG\"\n\n[[repos]]\nslug = \"acme/web\"\ngates = [\"make\"]\n[repos.tracker]\nkind = \"linear\"\nproject = \"WEB\"\n\n[[repos]]\nslug = \"o/r\"\ngates = [\"make\"]\n";

    #[tokio::test]
    async fn routed_is_built_from_the_config_and_names_missing_credentials() {
        let config = Config::from_toml_str(CONFIG).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let nothing = fake_security(dir.path(), "security", "exit 44");
        let env = env_of(&[("PROVEFAB_JIRA_EMAIL", "e@acme.test"), ("PROVEFAB_JIRA_TOKEN", "tok"), ("PROVEFAB_LINEAR_KEY", "lin")]);
        let routed = Routed::from_config(&config, Gh { program: "gh".into() }, &nothing, &env).await.unwrap();
        assert!(matches!(routed.remote("ACME/api"), Some(Remote::Jira(j)) if j.api == "https://acme.atlassian.net" && j.project == "ENG"));
        assert!(matches!(routed.remote("acme/web"), Some(Remote::Linear(l)) if l.api == crate::linear::API && l.team == "WEB"));
        assert!(routed.remote("o/r").is_none());
        let err = Routed::from_config(&config, Gh { program: "gh".into() }, &nothing, &env_of(&[])).await.err().unwrap();
        assert!(err.contains("provefab login jira --site acme.atlassian.net"), "{err}");
    }
```

`commands.rs` `mod tests` (uses its existing `fake` helper):

```rust
    #[tokio::test]
    async fn tracker_checks_name_missing_credentials_per_repository() {
        let dir = tempfile::tempdir().unwrap();
        let tools = Tools { security: fake(dir.path(), "security", "exit 44"), ..Tools::default() };
        let config = Config::from_toml_str(
            "[jev]\nmodel = \"jev-1.13\"\n\n[[models]]\nid = \"m\"\nworker = \"claude-code\"\nmodel = \"sonnet\"\ntier = \"standard\"\n\n[[repos]]\nslug = \"acme/api\"\ngates = [\"make\"]\n[repos.tracker]\nkind = \"jira\"\nsite = \"acme.atlassian.net\"\nproject = \"ENG\"\n\n[[repos]]\nslug = \"o/r\"\ngates = [\"make\"]\n",
        )
        .unwrap();
        let checks = tracker_checks(&tools, &config, &|_: &str| None::<String>).await;
        assert_eq!(checks.len(), 1, "{checks:?}");
        assert_eq!((checks[0].name.as_str(), checks[0].ok), ("tracker acme/api", false));
        assert!(
            checks[0].detail.starts_with("jira acme.atlassian.net ENG: no Jira API token"),
            "{}",
            checks[0].detail
        );
    }
```

and in its `issue_urls` test nothing changes (GitHub parsing is untouched).

`tests/commands.rs` (append):

```rust
use provefab::tracker::{TrackerConfig, TrackerKind};

const JIRA: &str = "https://acme.atlassian.net/browse/ENG-7";

fn tracked(f: &mut Fixture, kind: TrackerKind) {
    f.config.repos[0].tracker = Some(TrackerConfig {
        kind,
        site: (kind == TrackerKind::Jira).then(|| "acme.atlassian.net".to_string()),
        project: Some("ENG".into()),
    });
}

fn keyed_hub(url: &str) -> FakeHub {
    let mut hub = FakeHub::new("x");
    hub.issue.key = Some("ENG-7".into());
    hub.issue.url = url.into();
    hub
}

#[tokio::test]
async fn add_queues_a_jira_ticket_by_its_url() {
    use provefab::commands::{CommandError, add};
    let mut f = fixture(&["true"]);
    tracked(&mut f, TrackerKind::Jira);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), keyed_hub(JIRA)).await;
    let out = add(&p.store, &f.config, &p.hub, &p.git, &p.paths, "https://acme.atlassian.net/browse/ENG-7?focusedCommentId=1")
        .await
        .unwrap();
    assert!(out.starts_with("queued as task"), "{out}");
    let t = p.store.task_by_url(JIRA).await.unwrap().unwrap();
    assert_eq!((t.issue_number, t.issue_key.as_deref()), (7, Some("ENG-7")));
    for (url, what) in [
        ("https://github.com/o/r/issues/7", "wrong tracker"),
        ("https://acme.atlassian.net/browse/OPS-7", "unknown project"),
        ("https://other.atlassian.net/browse/ENG-7", "unknown site"),
        ("https://acme.atlassian.net/projects/ENG", "not a ticket"),
    ] {
        let r = add(&p.store, &f.config, &p.hub, &p.git, &p.paths, url).await;
        let ok = match what {
            "wrong tracker" => matches!(r, Err(CommandError::WrongTracker(_, _))),
            "not a ticket" => matches!(r, Err(CommandError::BadUrl(_))),
            _ => matches!(r, Err(CommandError::UnknownRepo(_))),
        };
        assert!(ok, "{url}: {what}");
    }
}

#[tokio::test]
async fn add_refuses_a_linear_ticket_from_another_workspace() {
    use provefab::commands::{CommandError, add};
    let mut f = fixture(&["true"]);
    tracked(&mut f, TrackerKind::Linear);
    let p = pipeline(
        &f,
        Box::new(happy),
        FakeOracle::default(),
        keyed_hub("https://linear.app/acme/issue/ENG-7/add-a-feature-file"),
    )
    .await;
    let r = add(&p.store, &f.config, &p.hub, &p.git, &p.paths, "https://linear.app/other/issue/ENG-7").await;
    assert!(matches!(r, Err(CommandError::OtherWorkspace(_))));
    assert!(p.store.tasks_in(&TaskState::ALL).await.unwrap().is_empty());
    let out = add(&p.store, &f.config, &p.hub, &p.git, &p.paths, "https://linear.app/ACME/issue/eng-7/add")
        .await
        .unwrap();
    assert!(out.starts_with("queued as task"), "{out}");
}

#[tokio::test]
async fn add_picks_the_repository_by_label_when_a_project_is_shared() {
    use provefab::commands::{CommandError, add};
    let mut f = fixture(&["true"]);
    tracked(&mut f, TrackerKind::Jira);
    f.config.repos[0].label = "api".into();
    let mut web = f.config.repos[0].clone();
    web.slug = "o/s".into();
    web.label = "web".into();
    f.config.repos.push(web);
    let mut p = pipeline(&f, Box::new(happy), FakeOracle::default(), keyed_hub(JIRA)).await;
    for labels in [vec![], vec!["api".to_string(), "web".to_string()]] {
        p.hub.issue.labels = labels;
        let r = add(&p.store, &f.config, &p.hub, &p.git, &p.paths, JIRA).await;
        assert!(matches!(&r, Err(CommandError::Ambiguous(key, slugs)) if key == "ENG-7" && slugs == "o/r, o/s"));
    }
    p.hub.issue.labels = vec!["web".into()];
    add(&p.store, &f.config, &p.hub, &p.git, &p.paths, JIRA).await.unwrap();
    let t = p.store.task_by_url(JIRA).await.unwrap().unwrap();
    assert_eq!(t.repo, "o/s");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab ticket_urls routed_ tracker_checks_name add_queues_a_jira add_refuses_a_linear add_picks_the_repository`
Expected: compile errors (`TicketUrl`, `Routed`, `tracker_checks`, new `CommandError` variants missing).

- [ ] **Step 3: Implement**

`tracker.rs` (append; imports `std::collections::HashMap`, `crate::config::Config`, `crate::forge::{Comment, Gh, Issue, PrStatus}`, `crate::ports::{Forge, Tracker}`, `crate::jira::Jira`, `crate::linear::Linear`):

```rust
/// A ticket link `provefab add` accepts (spec §4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketUrl {
    /// `https://<site>/browse/ENG-123`
    Jira { site: String, key: String },
    /// `https://linear.app/<workspace>/issue/ENG-123[/...]`
    Linear { workspace: String, key: String },
}

/// `ENG-123` as (`ENG`, 123); the number is never 0.
pub fn split_key(key: &str) -> Option<(&str, u64)> {
    let (project, n) = key.rsplit_once('-')?;
    let n: u64 = n.parse().ok()?;
    (is_project_key(project) && n > 0).then_some((project, n))
}

pub fn parse_ticket_url(url: &str) -> Option<TicketUrl> {
    let url = url.trim();
    let url = url.split(['?', '#']).next()?.trim_end_matches('/');
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let parts: Vec<&str> = rest.split('/').collect();
    let key = |k: &str| {
        let k = k.to_uppercase();
        split_key(&k).map(|_| k.clone())
    };
    match parts.as_slice() {
        ["linear.app", workspace, "issue", k, ..] if !workspace.is_empty() => Some(TicketUrl::Linear {
            workspace: workspace.to_string(),
            key: key(k)?,
        }),
        [site, "browse", k] if is_host(site) => Some(TicketUrl::Jira {
            site: site.to_lowercase(),
            key: key(k)?,
        }),
        _ => None,
    }
}

/// A repository's tracker when it is not GitHub.
pub enum Remote {
    Jira(Jira),
    Linear(Linear),
}

/// The production hub (spec §3): each repository's issues go to its tracker,
/// found by slug (so stored pending effects replay to the right one), and
/// every forge call goes to GitHub.
pub struct Routed {
    pub gh: Gh,
    /// By lower-case slug; repositories without an entry use GitHub issues.
    pub remotes: HashMap<String, Remote>,
}

impl Routed {
    pub fn remote(&self, slug: &str) -> Option<&Remote> {
        self.remotes.get(&slug.to_lowercase())
    }

    /// One adapter per Jira or Linear repository, with its credentials. A
    /// missing credential is an error naming the `provefab login` to run.
    pub async fn from_config(config: &Config, gh: Gh, security: &Path, env: Env<'_>) -> Result<Self, String> {
        let mut remotes = HashMap::new();
        for repo in &config.repos {
            let Some(t) = &repo.tracker else { continue };
            let project = t.project.clone().unwrap_or_default();
            let remote = match t.kind {
                TrackerKind::Github => continue,
                TrackerKind::Jira => {
                    let site = t.site.clone().unwrap_or_default();
                    let auth = jira_auth(security, &site, env).await?;
                    Remote::Jira(Jira::new(&format!("https://{site}"), &site, &project, auth))
                }
                TrackerKind::Linear => {
                    Remote::Linear(Linear::new(crate::linear::API, &project, linear_key(security, env).await?))
                }
            };
            remotes.insert(repo.slug.to_lowercase(), remote);
        }
        Ok(Self { gh, remotes })
    }
}

/// Calls `method` on the slug's tracker, or on `Gh` for a GitHub repository.
macro_rules! route {
    ($self:ident, $slug:ident, $method:ident($($arg:expr),*)) => {
        match $self.remote($slug) {
            Some(Remote::Jira(j)) => j.$method($slug, $($arg),*).await,
            Some(Remote::Linear(l)) => l.$method($slug, $($arg),*).await,
            None => Tracker::$method(&$self.gh, $slug, $($arg),*).await,
        }
    };
}

impl Tracker for Routed {
    async fn open_issues(&self, slug: &str, label: &str) -> Result<Vec<Issue>, ForgeError> {
        route!(self, slug, open_issues(label))
    }
    async fn issue(&self, slug: &str, number: u64) -> Result<Issue, ForgeError> {
        route!(self, slug, issue(number))
    }
    async fn comments(&self, slug: &str, number: u64) -> Result<Vec<Comment>, ForgeError> {
        route!(self, slug, comments(number))
    }
    async fn comment(&self, slug: &str, number: u64, body: &str) -> Result<(), ForgeError> {
        route!(self, slug, comment(number, body))
    }
    async fn edit_labels(&self, slug: &str, number: u64, add: &[&str], remove: &[&str]) -> Result<(), ForgeError> {
        route!(self, slug, edit_labels(number, add, remove))
    }
    async fn ensure_label(&self, slug: &str, name: &str, color: &str, description: &str) -> Result<(), ForgeError> {
        route!(self, slug, ensure_label(name, color, description))
    }
    async fn issue_open(&self, slug: &str, number: u64) -> Result<bool, ForgeError> {
        route!(self, slug, issue_open(number))
    }
}

impl Forge for Routed {
    async fn pr_comment(&self, slug: &str, url: &str, body: &str) -> Result<(), ForgeError> {
        Forge::pr_comment(&self.gh, slug, url, body).await
    }
    async fn pr_create(&self, slug: &str, head: &str, base: &str, title: &str, body: &str) -> Result<String, ForgeError> {
        Forge::pr_create(&self.gh, slug, head, base, title, body).await
    }
    async fn repo_clone(&self, slug: &str, dest: &Path) -> Result<(), ForgeError> {
        Forge::repo_clone(&self.gh, slug, dest).await
    }
    async fn pr_status(&self, slug: &str, url: &str) -> Result<PrStatus, ForgeError> {
        Forge::pr_status(&self.gh, slug, url).await
    }
    async fn pr_merge(&self, slug: &str, url: &str, head: &str) -> Result<(), ForgeError> {
        Forge::pr_merge(&self.gh, slug, url, head).await
    }
    async fn repo_is_public(&self, slug: &str) -> Result<bool, ForgeError> {
        Forge::repo_is_public(&self.gh, slug).await
    }
}
```

`commands.rs` errors:

```rust
    #[error("`{0}` is not a GitHub, Jira or Linear issue URL")]
    BadUrl(String),
    #[error("{0} takes its issues from {1}: give the ticket's URL")]
    WrongTracker(String, &'static str),
    #[error("{0} matches several repositories ({1}): put exactly one of their labels on the ticket")]
    Ambiguous(String, String),
    #[error("{0} is in another Linear workspace than the one Provefab's key reads")]
    OtherWorkspace(String),
```

`add` (imports `crate::config::RepoConfig` and `crate::tracker::{TicketUrl, TrackerKind, parse_ticket_url, split_key}`); everything from `let issue = ...` down keeps its current code except the `NewIssue` already carrying `issue_key` (Task 3):

```rust
/// `provefab add <url>`: queues the issue, or requeues it when the task is
/// parked (terminal or waiting for information). A task in progress is left
/// alone. GitHub, Jira (`https://<site>/browse/ENG-123`) and Linear
/// (`https://linear.app/<workspace>/issue/ENG-123`) links (spec §4).
pub async fn add<H: Hub>(
    store: &Store,
    config: &Config,
    hub: &H,
    git: &Git,
    paths: &Paths,
    url: &str,
) -> Result<String, CommandError> {
    let (repo, number, ticket) = match parse_issue_url(url) {
        Some((slug, number)) => {
            let repo = config
                .repos
                .iter()
                .find(|r| r.slug.eq_ignore_ascii_case(&slug))
                .ok_or(CommandError::UnknownRepo(slug))?;
            // Its tracker would read a different ticket with that number.
            if repo.tracker_kind() != TrackerKind::Github {
                return Err(CommandError::WrongTracker(repo.slug.clone(), repo.tracker_kind().as_str()));
            }
            (repo, number, None)
        }
        None => {
            let ticket = parse_ticket_url(url).ok_or_else(|| CommandError::BadUrl(url.into()))?;
            let (repo, number) = ticket_repo(config, hub, &ticket).await?;
            (repo, number, Some(ticket))
        }
    };
    let issue = hub.issue(&repo.slug, number).await?;
    // The key alone does not name the workspace (plan decision 6).
    if let Some(TicketUrl::Linear { workspace, key }) = &ticket
        && !issue
            .url
            .to_lowercase()
            .contains(&format!("linear.app/{}/", workspace.to_lowercase()))
    {
        return Err(CommandError::OtherWorkspace(key.clone()));
    }
```

and below `add`:

```rust
/// The repository a ticket belongs to: same tracker, site (Jira) and project.
/// When several share the project, the one whose label the ticket carries
/// (plan decision 7).
async fn ticket_repo<'a, H: Hub>(
    config: &'a Config,
    hub: &H,
    ticket: &TicketUrl,
) -> Result<(&'a RepoConfig, u64), CommandError> {
    let (kind, site, key) = match ticket {
        TicketUrl::Jira { site, key } => (TrackerKind::Jira, Some(site.as_str()), key.as_str()),
        TicketUrl::Linear { key, .. } => (TrackerKind::Linear, None, key.as_str()),
    };
    let (project, number) = split_key(key).ok_or_else(|| CommandError::BadUrl(key.into()))?;
    let matches: Vec<&RepoConfig> = config
        .repos
        .iter()
        .filter(|r| {
            r.tracker.as_ref().is_some_and(|t| {
                t.kind == kind
                    && t.project.as_deref() == Some(project)
                    && site.is_none_or(|s| t.site.as_deref().is_some_and(|own| own.eq_ignore_ascii_case(s)))
            })
        })
        .collect();
    let repo = match matches.as_slice() {
        [] => return Err(CommandError::UnknownRepo(format!("{} {key}", kind.as_str()))),
        [one] => *one,
        many => {
            let labels = hub.issue(&many[0].slug, number).await?.labels;
            let labelled: Vec<&RepoConfig> = many.iter().copied().filter(|r| labels.contains(&r.label)).collect();
            match labelled.as_slice() {
                [one] => *one,
                _ => {
                    let slugs: Vec<&str> = many.iter().map(|r| r.slug.as_str()).collect();
                    return Err(CommandError::Ambiguous(key.into(), slugs.join(", ")));
                }
            }
        }
    };
    Ok((repo, number))
}
```

`tracker_checks` (after `doctor`):

```rust
/// One `tracker <slug>` line per repository whose issues are on Jira or Linear
/// (spec §5): kind, site or workspace, project, and whether the credentials
/// authenticate and the project or team is readable.
pub async fn tracker_checks(tools: &Tools, config: &Config, env: crate::tracker::Env<'_>) -> Vec<Check> {
    use crate::tracker::{TrackerKind, jira_auth, linear_key};
    let mut checks = Vec::new();
    for repo in &config.repos {
        let Some(t) = repo.tracker.as_ref() else { continue };
        let project = t.project.clone().unwrap_or_default();
        let (what, answer) = match t.kind {
            TrackerKind::Github => continue,
            TrackerKind::Jira => {
                let site = t.site.clone().unwrap_or_default();
                let answer = match jira_auth(&tools.security, &site, env).await {
                    Ok(auth) => crate::jira::Jira::new(&format!("https://{site}"), &site, &project, auth)
                        .check()
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                (format!("jira {site} {project}"), answer)
            }
            TrackerKind::Linear => {
                let answer = match linear_key(&tools.security, env).await {
                    Ok(key) => crate::linear::Linear::new(crate::linear::API, &project, key)
                        .check()
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                (format!("linear {project}"), answer)
            }
        };
        checks.push(check(
            &format!("tracker {}", repo.slug),
            answer.map(|d| format!("{what}: {d}")).map_err(|e| format!("{what}: {e}")),
        ));
    }
    checks
}
```

`app.rs`: in `Cmd::Run`, right after `let oracle = oracle(&config).await?;`:

```rust
            let hub = crate::tracker::Routed::from_config(
                &config,
                gh(),
                std::path::Path::new("security"),
                &crate::tracker::process_env,
            )
            .await
            .map_err(anyhow::Error::msg)?;
```

then `scheduler::dry_run(&config, &hub, &oracle)` and `hub,` in the `Pipeline { ... }` literal (replacing `hub: gh(),`). In `Cmd::Add`, build the same `hub` after `load_config` and pass `&hub` to `commands::add`. In `Cmd::Doctor`, `let mut checks = commands::doctor(...).await;` followed by:

```rust
            checks.extend(
                commands::tracker_checks(&Tools::default(), &config, &crate::tracker::process_env).await,
            );
```

- [ ] **Step 4: Run all checks, and one real `doctor` execution**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

Run (a throwaway home; the Keychain is only read):

```bash
home=$(mktemp -d)
cat > "$home/provefab.toml" <<'EOF'
[jev]
model = "jev-1.13.0"

[[models]]
id = "m"
worker = "claude-code"
model = "sonnet"
tier = "standard"

[[repos]]
slug = "acme/api"
gates = ["true"]
[repos.tracker]
kind = "jira"
site = "acme.atlassian.net"
project = "ENG"
EOF
env -u PROVEFAB_JIRA_EMAIL -u PROVEFAB_JIRA_TOKEN PROVEFAB_HOME="$home" cargo run -q -p provefab -- doctor | grep "tracker acme/api"
```

Expected: one line starting `FAIL tracker acme/api` and containing `jira acme.atlassian.net ENG: no Jira API token for acme.atlassian.net: run \`provefab login jira --site acme.atlassian.net\``.

- [ ] **Step 5: Commit**

```bash
git add -A crates/provefab
git commit -m "trackers: route issues per repository, provefab add for Jira and Linear links, doctor tracker lines

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Documentation, example config, CLI description, spec amendments, version 0.3.0

**Files:**
- Create: `docs/guide/trackers.md`
- Modify: `docs/guide/configuration.md`, `docs/guide/usage.md`, `docs/guide/security.md`, `README.md`, `provefab.example.toml`, `crates/provefab/src/app.rs:30`, `crates/provefab/Cargo.toml:3`, `Cargo.lock`, `docs/specs/2026-10-01-issue-trackers-design.md`

**Interfaces:**
- Consumes: the behaviour as built in Tasks 1-6 (read the code before writing; where this text and the code disagree, the code wins and the text is fixed).
- Produces: nothing code-facing.

- [ ] **Step 1: Write `docs/guide/trackers.md`**

````markdown
# Jira and Linear

A repository can take its issues from a Jira Cloud project or a Linear team instead of GitHub issues. The code, the branches and the pull requests stay on GitHub, and so do the `/provefab` commands on review findings.

It works like GitHub issues: a label hands a ticket to Provefab, Provefab reports progress with labels and comments, and the pull request is opened on GitHub. **Provefab never changes a ticket's status.**

## Setup

One Jira project or one Linear team per repository. Several repositories may share a project, each with its own label.

### Jira Cloud

1. Create an API token for the Atlassian account Provefab will use: <https://id.atlassian.com/manage-profile/security/api-tokens>. The account needs to browse the project, comment and edit labels.
2. Store it: `provefab login jira --site acme.atlassian.net`. Provefab asks for the account e-mail; the token is typed at the Keychain's own prompt (service `provefab-jira`, account `acme.atlassian.net`). `PROVEFAB_JIRA_EMAIL` and `PROVEFAB_JIRA_TOKEN` override it.
3. In `provefab.toml`:

   ```toml
   [[repos]]
   slug = "acme/api"
   label = "provefab"
   gates = ["make test"]

   [repos.tracker]
   kind = "jira"
   site = "acme.atlassian.net"
   project = "ENG"
   ```

4. `provefab doctor` prints `tracker acme/api` with the result of a real call.

### Linear

1. Create a personal API key in Linear's settings, for an account that is a member of the team.
2. Store it: `provefab login linear` (service `provefab-linear`, account `provefab`). `PROVEFAB_LINEAR_KEY` overrides it.
3. In `provefab.toml`:

   ```toml
   [repos.tracker]
   kind = "linear"
   project = "ENG"   # the team key, as in ENG-123
   ```

4. `provefab doctor` prints the workspace and whether the team is readable.

## Labels

Put the repository's `label` (`provefab` by default) on a ticket. Provefab polls for open tickets of the project or team with that label (Jira: status category not Done; Linear: state neither completed nor canceled), up to 1000 per poll. Then it moves the ticket between the same labels as on GitHub: `provefab:needs-info`, `provefab:in-pr`, `provefab:failed`, `provefab:merged`, and the risk labels `provefab:risk-<category>`.

- Jira labels are free text: nothing is created. A Jira label cannot contain a space, so the repository's `label` cannot either.
- Linear labels are created on the team at startup, with their colour, when none of that name exists in the team or the workspace.

## What Provefab writes

- **Comments** on the ticket, each starting with "Posted by Provefab (automated), not typed by a person." On Jira they are converted to Atlassian Document Format (paragraphs, lists, code, links, emphasis); on Linear they stay Markdown.
- **The branch** `provefab/ENG-123-<title>` and **the pull request title** `ENG-123: <title>`, so the Jira and Linear GitHub integrations link them to the ticket.
- **The pull request body** starts with `Issue: [ENG-123](<link>).` On Linear it adds `Fixes ENG-123`: if your team enabled Linear's GitHub integration, Linear may close the ticket when the pull request merges. That is Linear's setting, not Provefab's.

## Who may answer

When Provefab asks for details, the ticket's reporter (Jira) or creator (Linear) and any member of the workspace may answer in a comment: only members can comment on Jira and Linear. Provefab ignores its own comments and comments posted by apps or integrations. A ticket's text is data for the agents, never instructions.

## Limits

- Jira Cloud only (not Jira Data Center), polling only (no webhooks).
- A ticket cannot choose its repository: the project or team is configured per repository.
- Renaming a Jira project key or a Linear team key needs `project` updated in `provefab.toml`.
- Switch a repository's tracker only when it has no task in progress (`provefab status`).
- A bad token, missing access or a ticket that no longer exists stops the task (`needs_you`, with the reason in `provefab log`); rate limits and server errors are retried like a failing `gh` call.
````

- [ ] **Step 2: Update the other docs**

`docs/guide/configuration.md`:
- Secrets table: two rows after the GitHub row:
  `| Jira Cloud | \`provefab login jira --site <site>\`: e-mail and API token in the Keychain (\`provefab-jira\`); or \`PROVEFAB_JIRA_EMAIL\` and \`PROVEFAB_JIRA_TOKEN\` |`
  `| Linear | \`provefab login linear\`: personal API key in the Keychain (\`provefab-linear\`); or \`PROVEFAB_LINEAR_KEY\` |`
- `[[repos]]` table: the `slug` row's role becomes "`owner/name` on GitHub, where the code and the pull requests are."
- New section after `[repos.risk]` and before `[limits]`:

````markdown
## `[repos.tracker]`: Jira or Linear issues

Absent, the repository's issues are its GitHub issues. Setup, labels and limits: [Jira and Linear](trackers.md).

| Field | Default | Role |
|---|---|---|
| `kind` | `github` | `github`, `jira` or `linear`. |
| `site` | none | Jira only, required: the site's host name, such as `acme.atlassian.net` (no `https://`, no path). |
| `project` | none | Jira and Linear, required: the project or team key, `[A-Z][A-Z0-9_]*`, as in `ENG-123`. |

Refused at load: an unknown `kind` or key (credentials never go in this file), `site` outside Jira, `project` with `github`, and, for Jira, a `label` containing whitespace.
````

`docs/guide/usage.md`: in "The life of an issue", item 1 gets a second sentence "On Jira or Linear, the label goes on the ticket (see [Jira and Linear](trackers.md))."; item 8 gets "For a Jira or Linear ticket, the branch is `provefab/ENG-123-<title>`, the title starts with `ENG-123:` and the body links the ticket." Add under "Answering Provefab": "On Jira and Linear, the reporter or creator and any workspace member may answer." The `provefab add <url>` lines accept "a GitHub issue, Jira ticket or Linear issue URL".

`docs/guide/security.md`, "Who can trigger Provefab": add "- **On Jira and Linear**, the label is the authorization too, and any workspace member may answer a question (only members can comment there). Provefab's Jira token or Linear key stays in the Keychain or your environment, never in `provefab.toml`, and never appears in an error, a log or a comment. Ticket text is untrusted data, like issue text."

`README.md`: line 3 becomes "Provefab turns labelled GitHub, Jira or Linear issues into pull requests that arrive green, reviewed by a second model, with their evidence."; the Documentation list gains "- [Jira and Linear](docs/guide/trackers.md): issues from a Jira Cloud project or a Linear team, pull requests on GitHub."; the install line's tag becomes `v0.3.0`.

`provefab.example.toml`, after the `[repos.risk]` comment block:

```toml
# Issues from Jira Cloud or Linear instead of GitHub issues (optional). Code and
# pull requests stay on GitHub. Credentials: `provefab login jira --site <site>`
# or `provefab login linear`, never in this file. See docs/guide/trackers.md.
# [repos.tracker]
# kind = "jira"                  # "github" (default), "jira" or "linear"
# site = "acme.atlassian.net"    # Jira only
# project = "ENG"                # Jira project key or Linear team key
```

`crates/provefab/src/app.rs:30`: `about = "Turns labelled GitHub, Jira or Linear issues into tested pull requests"`.

- [ ] **Step 3: Spec amendments** (each marked "(amended 2026-10-01 in the plan)") in `docs/specs/2026-10-01-issue-trackers-design.md`: §3 `scheduler::run` polls through the hub, `dry_run` takes a tracker, and `Routed` lands with the adapters; §5 the Jira e-mail is the Keychain item's comment, `project` is refused for GitHub; §7 non-human comments are skipped; §8 Provefab's own comments are recognised by the bot line only (decision 4 of the plan), other 4xx are permanent, a `Retry-After` up to 30 s is waited out inside the call; §4 `provefab add` matches Linear by team key and refuses another workspace, and picks a shared project's repository by label; §10 adapter tests use the existing `wiremock` dev-dependency.

- [ ] **Step 4: Version 0.3.0**

Set `version = "0.3.0"` in `crates/provefab/Cargo.toml`, then `cargo build -p provefab` (updates `Cargo.lock`).

- [ ] **Step 5: Checks and the copy rule**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green (`example_config` loads the new commented block; `no_paid_code` still clean).

Run: `grep -n "—" docs/guide/*.md README.md provefab.example.toml crates/provefab/src/app.rs crates/provefab/src/tracker.rs crates/provefab/src/jira.rs crates/provefab/src/linear.rs`
Expected: no output.

Run: `cargo run -q -p provefab -- --help | head -1`
Expected: `Turns labelled GitHub, Jira or Linear issues into tested pull requests`.

- [ ] **Step 6: Commit**

```bash
git add -A docs README.md provefab.example.toml crates/provefab Cargo.lock
git commit -m "docs: Jira and Linear trackers; provefab 0.3.0

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 7: Real run before release (owner, not the implementer)**

Spec §10: the owner runs `provefab login jira --site <test site>` and `provefab login linear` (Provefab never handles the secrets in the conversation), points two sandbox repositories at the test project and team, runs `provefab doctor`, labels one Jira ticket and one Linear issue, and runs `provefab run --once`. Evidence to keep: the two `doctor` tracker lines, the two pull requests (branch, title, first line), the ticket comments and labels, and `provefab status`. This also confirms plan decisions 5 (Keychain comment format) and the Linear filter and error shapes.

---

### Task 8: Provefab Pro builds and passes against the core branch

**Files (Pro repository `/Users/antoinehoriot/Projects/provefab/provefab-pro`):**
- Modify: `crates/provefab-pro/Cargo.toml` (both `provefab` lines)
- Modify: `crates/provefab-pro/src/costs.rs:121-127`, `crates/provefab-pro/tests/calibration.rs:14-20` (`NewIssue` literals)

**Interfaces:**
- Consumes: the core's public API after Tasks 1-7. Pro uses `provefab::app::{Extensions, Extra, run}`, `policy`, `pipeline::{PipelineError, numstat_lines}`, `store::{NewIssue, Store, ...}`, `risk`, `record`, `testkit` (`FakeHub` fields `merged`, `posted`, `prs`, `pr_create_failures`, `break_on_pr_create`, `last_labels`). It implements no port and never names `Hub`, so the split is invisible to it; `NewIssue` gained `issue_key` and needs it in Pro's two literals.
- Produces: nothing.

- [ ] **Step 1: Branch and temporary path dependency**

```bash
git -C /Users/antoinehoriot/Projects/provefab/provefab-pro checkout -b feature/issue-trackers
```

In `crates/provefab-pro/Cargo.toml`:

```toml
provefab = { path = "../../../provefab/crates/provefab" } # TEMPORARY until core v0.3.0 is tagged
```

and under `[dev-dependencies]`:

```toml
provefab = { path = "../../../provefab/crates/provefab", features = ["testkit"] } # TEMPORARY until core v0.3.0
```

- [ ] **Step 2: Build to see the breakage**

Run: `cd /Users/antoinehoriot/Projects/provefab/provefab-pro && cargo build --all-targets`
Expected: errors `missing field issue_key in initializer of NewIssue` in `src/costs.rs` and `tests/calibration.rs`, nothing else. Any other error is a STOP: report it, it means the core broke its public API.

- [ ] **Step 3: Fix the two literals**

Add `issue_key: None,` after `number: ...,` in both `NewIssue { ... }` literals.

- [ ] **Step 4: Pro checks**

Run: `cargo fmt -- --check && cargo clippy --all-targets -- -D warnings && cargo nextest run`
Expected: green, `guarded_merge` included (GitHub PR bodies still start with `Closes #7.`).

- [ ] **Step 5: Commit (Pro)**

```bash
git -C /Users/antoinehoriot/Projects/provefab/provefab-pro add -A crates/provefab-pro Cargo.lock
git -C /Users/antoinehoriot/Projects/provefab/provefab-pro commit -m "Build against the core's issue trackers (temporary path dependency, NewIssue.issue_key)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Self-review

**Spec coverage.** §2 decisions: label trigger (Tasks 1, 4, 5 `open_issues`), labels and comments only, no status change (adapters never write a status; docs Task 7), one project per repo (Task 2), free core (no Pro code), port split (Task 1). §3 ports and `Routed` (Tasks 1, 6), `FakeHub` with a key (Task 3), finding commands unchanged (no change to `record.rs`). §4 migration, `Issue.key`, display function, branch, title, first line, `add` URLs (Tasks 3, 6). §5 config, validation, credentials, login, env, doctor (Tasks 2, 6). §6 Jira (Task 4). §7 Linear (Task 5). §8 member mapping, bot line without emphasis, `repo_is_public` on the forge, error classes and the new variant, pending effects by slug (`routed_sends_...` replays a label effect), reopen through `Tracker::issue_open` (Task 3 test), post-merge comments to tracker and forge (unchanged code through `Routed`) (Tasks 3-6). §9 docs (Task 7; landing out of plan by the brief). §10 tests (Tasks 1-6; real run Task 7 Step 7). §11 budget: three modules, one migration, no dependency (Global Constraints), Pro adapts (Task 8), 0.3.0 (Task 7).

**Placeholder scan.** No "TBD", "TODO", "similar to", or unshown code; every code step carries its code, every run step its command and expected outcome.

**Type consistency.** `Tracker`/`Forge` method names and argument orders are the same in Tasks 1, 4, 5, 6; `open_issues(slug, label)` everywhere; `JiraAuth { email, token }` in Tasks 2, 4, 6; `Jira::new(api, site, project, auth)` and `Linear::new(api, team, key: String)` in Tasks 4, 5, 6; `ForgeError::Tracker { service, status: Option<u16>, message }` in Tasks 4, 5; `issue_ref`, `repo_ref`, `pr_title`, `pr_first_line(kind, number, key, url)` in Task 3; `TaskRow::reference`/`repo_reference` in Tasks 3, 6; `queue_ticket(p, number, key, url)` in Task 3; `Env<'_>` in Tasks 2, 6.

**Review Focus.** Each of the five lines has its test in the owning task (Tasks 4 and 6), named in the Review Focus section.

