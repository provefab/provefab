# Repository Rules Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A repository keeps its conventions in `.provefab/rules.md`; Provefab reads them from the task's base commit, gives them to the plan, implementation and review prompts (path-scoped), has the reviewer report violations as findings that cite the rule, and exposes a daily periodic extension point (`ReviewPolicy::periodic` with core `PeriodicTools`) that Provefab Pro uses to propose rules.

**Architecture:** One new module `crates/provefab/src/rules.rs` holds the pure part (parse, select, render) and the pipeline side (load once per pass from the pinned base commit, `rules_given`, the `PeriodicTools` implementation `Maintenance`). Migration `0007` adds `findings.rule` and `maintenance_runs`. The prompts gain a trailing `{{rules}}` placeholder that renders as nothing without rules, so a repository without the file gets byte-identical prompts. The scheduler calls the policy's `periodic` once per repository per local day on a separate `JoinSet`, never twice at once for a repository.

**Tech Stack:** Rust 2024, tokio, sqlx/SQLite, serde_json, schemars, sha2, libc (all existing dependencies), `cargo nextest`.

**Spec:** `docs/specs/2026-10-01-repo-rules-design.md`. Provefab Pro's part is planned in the Pro repository (`docs/superpowers/plans/2026-10-01-rule-proposals.md` there) and needs Tasks 1-8 of this plan.

## Global Constraints

- Core repo `/Users/antoinehoriot/Projects/provefab/provefab`, branch `feature/repo-rules` (checked out). Local commits only; never push, tag or deploy. Landing and terms are out of this plan (the controller updates them at release).
- Spec §11, verbatim: "Core: exactly one new module (`rules.rs`), one migration (`0007`), no new dependency. A second module, a second migration or a new dependency is a STOP." No new feature of an existing dependency either. New integration test files under `crates/provefab/tests/` are not modules of the crate and are allowed (this plan adds `tests/rules.rs`).
- Spec §11, verbatim: "Version 0.4.0." (Task 9).
- Spec §9, verbatim: "No em-dashes; no claim that rules guarantee correct code." No em-dashes in any user-facing text: docs, CLI help, PR bodies, prompts, error messages.
- Spec §3, verbatim: "Limits: at most 100 rules; a summary at most 120 characters; a rule text at most 2000 characters." The file is `.provefab/rules.md`; a rule heading is `## R<n>: <summary>`.
- Spec §5, verbatim: the block is "titled `Repository rules (approved by the maintainers of this repository)` placed outside the UNTRUSTED markers"; "Budget: 12 000 characters; rules beyond it are left out in number order and the prompt says how many were left out".
- Spec §5, verbatim: "Rules are guidance to the agents and stay under the guard: a rule cannot allow a tool call the guard refuses."
- Spec §8, verbatim: the doctor line for a public repository adds "`; rules are instructions to the agents: review pull requests that change .provefab/rules.md closely`".
- Spec §7: `propose_file` "never merges".
- The public core never contains the four symbols that `crates/provefab/tests/no_paid_code.rs` searches for. That test scans every file of the repository, docs and this plan included: never write those symbols anywhere.
- Commit trailer: blank line then `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Checks after every task, from the repository root: `cargo fmt` then `cargo fmt -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo nextest run --all-features`. A task is done only when all three are green.

## Decisions taken in this plan (each with its why; spec amendments in Task 9)

1. **Agents already cannot change the rules file; reading from the base commit is kept as a third layer.** The guard refuses agent writes to `.provefab/**` (`crates/provefab/src/guard/paths.rs:8`, `PROTECTED_DIRS`), and `Git::commit_all` unstages `.provefab/` before every task commit (`crates/provefab/src/forge.rs:434-460`). So a task pull request never changes `.provefab/rules.md`, and the spec's "an agent that edits the file in its own pull request" cannot happen; an edit in the worktree (a shell redirection the guard missed) still changes nothing because stages read the base commit. Consequence for spec §6: the `rules` risk category is kept as specified (sixth built-in, visible in `doctor`, disableable), but it can only fire on a change Provefab did not make. Re-open trigger: a task must be allowed to edit the rules file.
2. **One read per pass, kept in a `rules` stage output** `{"pass": n, "text": "<file>" | null}` written with the `rules_loaded`/`rules_invalid` event in one transaction (`Store::write_with_events`). `prepare` loads it right after `pin_base`; any stage of a pass without one (a pass that started before the upgrade) loads it from the pass's pinned base (`pass_rules`). Why: the spec's "reads them once" and a crash or upgrade mid-pass must not mix two versions.
3. **A missing file records no event**, only the stage output with `text: null`. Why: spec §3 "a missing file means no rules (no message)" and §10 "a repository without the file behaves exactly as before"; the record stays as it was.
4. **`rules_loaded.omitted` is the plan prompt's count.** The budget applies per prompt; the plan gets every rule, so its count is the largest any stage of the pass can leave out. The implementation and review prompts say their own count in the prompt. Why: one event per pass (spec §4) cannot carry three counts.
5. **`RulesError` never quotes the file** (line numbers, rule numbers, pattern positions only), so `rules_invalid.reason` goes to `provefab log`, `doctor` and `export` without redaction. Test `errors_never_quote_the_file`.
6. **Format details the spec leaves open:** blank lines between a heading and its `paths:`/`sources:` lines are allowed and the blank line before the text is optional; in the introduction, a `## ` heading is ignored unless it starts with `## R` and a digit (a malformed rule heading there is an error, not silently skipped); after the first rule, every `## ` line must be a rule heading; `R0` and leading zeros are malformed; `paths:` values are split on commas and trimmed. Windows line endings and a byte order mark read like plain text.
7. **`Rule` carries `span`**, the byte range of its section. Why: Provefab Pro must keep "the human's text of every rule the run does not change byte for byte" (Pro spec §6) and needs the core parser's own boundaries.
8. **Prompt placement:** each template ends with `{{rules}}` and `prompts::render` fills a missing `rules` with nothing, so every existing caller and a repository without rules get the exact previous bytes. The block starts with a newline and comes after every UNTRUSTED marker. Rendering "left out in number order" means: rules sorted by number, the first one that does not fit and every later one are left out.
9. **Review `rule` field:** `Finding.rule: Option<String>` with `#[serde(default)]` (stored answers from before the upgrade still read). The pipeline keeps a value only when it names a rule the review was *given* (selected and not left out by the budget), normalised to `R<n>`. The review prompt's instruction to use the field is appended only when rules were given.
10. **PR body `Rules:` line** comes from a `rules_given` stage output `{"pass", "round", "numbers"}` recorded when the review prompt carried rules, read for the PR's pass and round. It sits after the check lines and before the reproduction line.
11. **`PeriodicTools` is object-safe** (`BoxFuture`, like `MergeTools`) and every method returns `Result<_, String>`: the spec writes `signals(since) -> Vec<Signal>` and `last_run(kind) -> Option<MaintenanceRun>`, but both read the store, which can fail. `ReviewPolicy::periodic` returns `Result<(), String>` as specified.
12. **Two additions to the extension point, both needed by Pro's spec:** `highest_rule_number()` (Pro spec §5 "max existing number ever seen, including rules removed earlier according to the record": the core reads `rules_loaded` numbers and `findings.rule`), and a `detail` argument on `record_run` with a `detail TEXT` column in `maintenance_runs` (the policy's own JSON, never shown), so Pro knows what a pull request closed without merging proposed (Pro spec §4). Both stay inside migration `0007`.
13. **The scheduler records each periodic call as a `periodic` maintenance run** (`ok` or `error: <message>`). Why: spec §7 "at startup if the last call is older than 24 hours" needs the last call to survive a restart. `provefab status` hides a `periodic` run whose outcome is `ok`.
14. **Daily timing:** at startup, a repository whose last `periodic` call is 24 hours old or more (or absent) is called; afterwards, on the first tick whose local calendar day differs from the previous tick's. Local time comes from `libc::localtime_r` (`tm_gmtoff`), an existing dependency. A day change while the previous call still runs is skipped (never concurrently). Under `run --once` the run waits for the calls it started.
15. **Model calls of periodic work** go through the same claim as a stage (`claim_tier`: `[routing] prefer` order, cooldowns, `max_concurrency`, refused when the daily stage budget is spent) but are not `stage_runs` rows, so they do not count toward `max_stage_runs_per_day`. Their cost waits in the tools until `record_run`; what no run took is written on the `periodic` row. "No repository access" means: the worker's directory is a fresh empty one under `<home>/maintenance/ask`, read-only tools, the guard as for any stage. The guard never blocked reads by absolute path, so this is "not given", not a sandbox; the security guide says so.
16. **`propose_file` derives its branch from the file stem** (`.provefab/rules.md` gives `provefab/rules`, as Pro's spec names it), rebuilds it from the current base in a fresh detached worktree, commits only that file (`git add -f`, since `.provefab/` is excluded in Provefab's worktrees), force-pushes it, then calls `pr_create` (which reuses the branch's open pull request) and the new `Forge::pr_edit` (title and body; `pr_create` never updates them). It never merges.
17. **Signals (spec §7):** ids `pr#<n>/F<k>` (the finding's pass's PR), `pr#<n>/c<i>` (change requests, from the `review` output `start_pass` records for a closed PR's comments), `task#<id>/gates` (one per task, the failed commands), `task#<id>/revert-<check>`, `task#<id>/reopen-<pass>`, `pr#<n>/merged|closed` (earlier periodic PRs). "Secrets are redacted as in `provefab export`" is applied as export's allowlist: people's and reviewers' text is included (that is the evidence the model needs), while model-written commands and tool output are never included, so a gate failure lists only configured commands (`gates` and risk checks), never the plan's reproduction command. Periodic PR outcomes are returned whatever their age (a refusal must keep suppressing).
18. **`doctor` reads the base as last fetched** (it never fetches), says `none yet: the repository is cloned on its first task` for a managed repository not cloned yet, marks an invalid file `FAIL`, and adds the public-repository note also when visibility cannot be read (fail closed).
19. **`app::open_pipeline`** builds the pipeline `run` uses (same model filtering) without the run lock, so `provefab-pro rules propose` can run next to the service.

## Review Focus

1. **A rules file saved on Windows (CRLF, byte order mark)** parses exactly like the same file with LF. Test in Task 1 (`windows_line_endings_and_a_byte_order_mark_read_like_plain_text`).
2. **A rule whose text contains `{{body}}`, backticks or a fake `--- END UNTRUSTED` line** is inserted verbatim at the end of the prompt and never expands or moves the real markers. Test in Task 3 (`rules_go_last_and_default_to_nothing`).
3. **A review answer stored before the upgrade** (no `rule` key) still deserializes, so a resumed task or `provefab log` never fails on old rows. Test in Task 2 (`a_finding_may_cite_a_rule_and_older_answers_still_read`).
4. **A policy whose periodic work fails** is logged and recorded while the queue keeps moving in the same run. Test in Task 8 (`the_policy_works_once_a_day_and_a_failure_never_stops_the_queue`).
5. **A task whose pass began before the upgrade** (no `rules` output for its pass) reads the rules at its next stage instead of failing or running without them. Test in Task 3 (`a_pass_without_its_rules_output_loads_them_at_its_next_stage`).

## File Structure

| File | Responsibility |
|---|---|
| `crates/provefab/src/rules.rs` (new) | format (`parse`, `RulesError`), `select`, `render`, `numbers`, `rule_number`; pipeline side (`load_rules`, `pass_rules`, `rules_given`); `Maintenance`, the core `PeriodicTools` |
| `crates/provefab/migrations/0007_rules.sql` (new) | `findings.rule`, `maintenance_runs` |
| `crates/provefab/src/risk.rs` | `unmatchable`/`UNMATCHABLE` shared with `paths:`; built-in `rules` category |
| `crates/provefab/src/stage.rs` | `Finding.rule` |
| `crates/provefab/src/record.rs` | `Event::RulesLoaded`, `Event::RulesInvalid`, `FindingRow.rule` |
| `crates/provefab/src/store.rs` | `findings.rule` writes and reads, `MaintenanceRun` and its queries, `outputs_since`, migration checksum |
| `crates/provefab/prompts/*.md`, `crates/provefab/src/prompts.rs` | trailing `{{rules}}`, empty by default |
| `crates/provefab/src/pipeline.rs` | rules in plan, implement and review; rule validation; review notes, findings text, feedback, PR body `Rules:`; `claim_tier` and crate visibility for the tools |
| `crates/provefab/src/policy.rs` | `ReviewPolicy::periodic`, `PeriodicTools`, `Signal`, `SignalKind` |
| `crates/provefab/src/forge.rs`, `ports.rs`, `tracker.rs`, `testkit.rs` | `Git::show_file`, `commit_file`, `push_force`; `Forge::pr_edit` for `Gh`, `Routed`, `FakeHub` |
| `crates/provefab/src/scheduler.rs` | `PeriodicClock`, `local_day`, the daily call |
| `crates/provefab/src/commands.rs` | `rules_checks` (doctor), `status` maintenance lines, `log` and `export` rule fields and events |
| `crates/provefab/src/app.rs` | doctor wiring, `open_pipeline` |
| `crates/provefab/tests/rules.rs` (new) | end-to-end runs with a rules file, doctor, tools |
| `docs/guide/rules.md` (new) and the other docs | user documentation |

---

### Task 1: The rules format: `parse`, `select`, `render`; the `rules` risk category

**Files:**
- Create: `crates/provefab/src/rules.rs`
- Modify: `crates/provefab/src/lib.rs` (`pub mod rules;` after `pub mod risk;`)
- Modify: `crates/provefab/src/risk.rs:85-131` (`builtins`), `:152-160` (pattern check in `resolve`), tests `:357-366`
- Modify: `crates/provefab/src/commands.rs:1444` (test: `5 categories` becomes `6 categories`)
- Modify: `crates/provefab/tests/scheduler.rs:42` (expected labels)

**Interfaces:**
- Consumes: `risk::glob_match(pattern, path) -> bool`, `task::Stage`.
- Produces:

```rust
// risk.rs
pub const UNMATCHABLE: &str; // "must be relative, without \"./\", a trailing \"/\" or empty segments"
pub fn unmatchable(pattern: &str) -> bool;
// rules.rs
pub const PATH: &str = ".provefab/rules.md";
pub const MAX_RULES: usize = 100;
pub const MAX_SUMMARY: usize = 120;
pub const MAX_TEXT: usize = 2000;
pub const BUDGET: usize = 12_000;
pub const TITLE: &str = "Repository rules (approved by the maintainers of this repository)";
pub struct Rule { pub number: u32, pub summary: String, pub paths: Vec<String>, pub text: String, pub span: std::ops::Range<usize> }
pub enum RulesError { Heading(usize), Duplicate(u32), Misplaced { line: usize, key: &'static str }, Pattern { rule: u32, index: usize, why: &'static str }, TooMany, SummaryTooLong(u32), TextTooLong(u32) }
pub fn parse(text: &str) -> Result<Vec<Rule>, RulesError>;
pub fn select<'a>(rules: &'a [Rule], stage: Stage, files: &[String]) -> Vec<&'a Rule>;
pub fn render(selected: &[&Rule], budget: usize) -> (String, usize); // (block, rules left out)
pub fn numbers(selected: &[&Rule], omitted: usize) -> Vec<u32>;       // the rules render kept
pub fn rule_number(s: &str) -> Option<u32>;                            // "R3" -> 3
pub fn sha256_hex(text: &str) -> String;
```

- [ ] **Step 1: Write the failing tests**

Create `crates/provefab/src/rules.rs` with only this test module (the module does not compile yet, which is the failure):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "# Our rules\n\nA human introduction.\n\n## About\n\nIgnored too.\n\n## R3: Errors in the API layer use ApiError, never anyhow\npaths: src/api/**, src/web/*.rs\nsources: PR #41 F2 (rejected), PR #57 (closed with a change request)\n\nReturn `ApiError` from handlers; `anyhow` stays in the CLI.\n\n## R7: Keep pull requests small\n\nOne change per pull request.\n### Why\nReviews stay short.\n";

    #[test]
    fn a_valid_file_parses_with_its_introduction_ignored() {
        let rules = parse(FILE).unwrap();
        assert_eq!(rules.iter().map(|r| r.number).collect::<Vec<_>>(), [3, 7]);
        let r3 = &rules[0];
        assert_eq!(r3.summary, "Errors in the API layer use ApiError, never anyhow");
        assert_eq!(r3.paths, ["src/api/**", "src/web/*.rs"]);
        assert_eq!(
            r3.text,
            "Return `ApiError` from handlers; `anyhow` stays in the CLI."
        );
        let section = &FILE[r3.span.clone()];
        assert!(section.starts_with("## R3: ") && section.ends_with("CLI.\n\n"), "{section:?}");
        assert_eq!(
            rules[1].text,
            "One change per pull request.\n### Why\nReviews stay short."
        );
        assert!(rules[1].paths.is_empty());
        assert_eq!(rules[1].span.end, FILE.len());
    }

    #[test]
    fn a_missing_or_empty_file_means_no_rules() {
        assert_eq!(parse(""), Ok(vec![]));
        assert_eq!(parse("# Rules\n\nNone yet.\n"), Ok(vec![]));
    }

    #[test]
    fn every_validation_error_is_named() {
        for (text, want) in [
            ("## R1 Missing colon\n", RulesError::Heading(1)),
            ("## R0: Zero\n", RulesError::Heading(1)),
            ("## R01: Leading zero\n", RulesError::Heading(1)),
            ("## R1:   \n", RulesError::Heading(1)),
            ("## R1: One\n\ntext\n## Notes\n", RulesError::Heading(4)),
            ("## R1: One\n\n## R1: Again\n", RulesError::Duplicate(1)),
            (
                "## R1: One\npaths: a/**\npaths: b/**\n",
                RulesError::Misplaced { line: 3, key: "paths:" },
            ),
            (
                "## R1: One\n\ntext\nsources: PR #1\n",
                RulesError::Misplaced { line: 4, key: "sources:" },
            ),
            (
                "## R1: One\npaths: a/**, \n",
                RulesError::Pattern { rule: 1, index: 2, why: "is empty" },
            ),
            (
                "## R1: One\npaths: /src/**\n",
                RulesError::Pattern { rule: 1, index: 1, why: UNMATCHABLE },
            ),
            (
                "## R1: One\npaths: src/\n",
                RulesError::Pattern { rule: 1, index: 1, why: UNMATCHABLE },
            ),
        ] {
            assert_eq!(parse(text), Err(want), "{text:?}");
        }
    }

    #[test]
    fn limits_are_inclusive() {
        let many = |n: u32| {
            (1..=n)
                .map(|i| format!("## R{i}: Rule {i}\n\ntext\n\n"))
                .collect::<String>()
        };
        assert_eq!(parse(&many(100)).unwrap().len(), 100);
        assert_eq!(parse(&many(101)), Err(RulesError::TooMany));
        let summary = |n: usize| format!("## R1: {}\n", "s".repeat(n));
        assert!(parse(&summary(120)).is_ok());
        assert_eq!(parse(&summary(121)), Err(RulesError::SummaryTooLong(1)));
        // Characters, not bytes.
        let text = |n: usize| format!("## R1: One\n\n{}\n", "é".repeat(n));
        assert!(parse(&text(2000)).is_ok());
        assert_eq!(parse(&text(2001)), Err(RulesError::TextTooLong(1)));
    }

    /// Plan decision 5: the reason goes to the log, doctor and the export as is.
    #[test]
    fn errors_never_quote_the_file() {
        for text in [
            "## R1: SENTINEL_42\npaths: /SENTINEL_42/**\n",
            "## RSENTINEL_42\n## R1: x\n\n## R1: SENTINEL_42\n",
            "## R1: x\n\nSENTINEL_42\n## SENTINEL_42\n",
        ] {
            let e = parse(text).unwrap_err().to_string();
            assert!(!e.contains("SENTINEL_42"), "{e}");
        }
    }

    /// Review Focus 1.
    #[test]
    fn windows_line_endings_and_a_byte_order_mark_read_like_plain_text() {
        let plain = "## R1: One\npaths: src/**\n\nText.\n\n## R2: Two\n\nMore.\n";
        let windows = format!("\u{feff}{}", plain.replace('\n', "\r\n"));
        let strip = |rs: Vec<Rule>| {
            rs.into_iter()
                .map(|r| (r.number, r.summary, r.paths, r.text))
                .collect::<Vec<_>>()
        };
        assert_eq!(strip(parse(&windows).unwrap()), strip(parse(plain).unwrap()));
    }

    fn rule(number: u32, paths: &[&str], text: &str) -> Rule {
        Rule {
            number,
            summary: format!("Rule {number}"),
            paths: paths.iter().map(|p| p.to_string()).collect(),
            text: text.into(),
            span: 0..0,
        }
    }

    #[test]
    fn the_plan_gets_every_rule_the_other_stages_those_that_match() {
        let rules = [
            rule(1, &[], "a"),
            rule(2, &["src/api/**"], "b"),
            rule(3, &["docs/*.md"], "c"),
        ];
        let numbers = |s: Vec<&Rule>| s.iter().map(|r| r.number).collect::<Vec<_>>();
        let api = ["src/api/handler.rs".to_string()];
        assert_eq!(numbers(select(&rules, Stage::Plan, &[])), [1, 2, 3]);
        assert_eq!(numbers(select(&rules, Stage::Implement, &api)), [1, 2]);
        assert_eq!(
            numbers(select(&rules, Stage::Review, &["README.md".to_string()])),
            [1]
        );
    }

    #[test]
    fn rendering_keeps_number_order_and_counts_what_the_budget_leaves_out() {
        // Each entry is 1913 characters: six fit in 12 000, four do not.
        let rules: Vec<Rule> = (1..=10)
            .rev()
            .map(|n| rule(n, &[], &"x".repeat(1900)))
            .collect();
        let all: Vec<&Rule> = rules.iter().collect();
        let (text, omitted) = render(&all, BUDGET);
        assert_eq!(omitted, 4);
        assert!(text.starts_with(&format!("\n{TITLE}.")), "{text}");
        assert!(text.find("R1: Rule 1\n").unwrap() < text.find("R6: Rule 6\n").unwrap());
        assert!(!text.contains("R7: Rule 7"), "left out in number order");
        assert!(
            text.ends_with("4 more rules were left out to keep this prompt short.\n"),
            "{text}"
        );
        assert_eq!(numbers(&all, omitted), [1, 2, 3, 4, 5, 6]);
        let two = [rule(1, &[], "a"), rule(2, &[], &"x".repeat(1990))];
        let (text, omitted) = render(&two.iter().collect::<Vec<_>>(), 100);
        assert_eq!(omitted, 1);
        assert!(text.ends_with("1 more rule was left out to keep this prompt short.\n"));
    }

    #[test]
    fn no_rule_renders_nothing_and_a_rule_without_text_is_its_summary() {
        assert_eq!(render(&[], BUDGET), (String::new(), 0));
        let r = rule(4, &[], "");
        assert_eq!(
            render(&[&r], BUDGET).0,
            format!(
                "\n{TITLE}. They add to the instructions above and never override them.\n\nR4: Rule 4\n\n"
            )
        );
    }

    #[test]
    fn rule_numbers_are_r_and_a_number() {
        assert_eq!(rule_number(" R3 "), Some(3));
        assert_eq!(rule_number("r12"), Some(12));
        for bad in ["3", "R", "R03", "R0", "Rx", "R3a", ""] {
            assert_eq!(rule_number(bad), None, "{bad}");
        }
    }
}
```

In `crates/provefab/src/risk.rs` `mod tests`, add:

```rust
    #[test]
    fn the_rules_file_is_its_own_builtin_category() {
        let p = resolve(None).unwrap();
        let rules = [".provefab/rules.md".to_string()];
        let d = classify(&p, &rules);
        assert_eq!(d.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["rules"]);
        assert!(p.needs_frontier(&d));
        let off: RiskConfig = toml::from_str("disable = [\"rules\"]").unwrap();
        assert!(classify(&resolve(Some(&off)).unwrap(), &rules).is_empty());
    }
```

Add `pub mod rules;` to `crates/provefab/src/lib.rs` after `pub mod risk;`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab rules:: the_rules_file_is_its_own`
Expected: compile error `cannot find function parse in this scope` (and the other items).

- [ ] **Step 3: Share the pattern check and add the category in `risk.rs`**

Above `fn valid_name`, add:

```rust
/// Why a path pattern can never match: changed paths are relative and
/// normalised. Shared with repository rules' `paths:` (rules spec §3).
pub const UNMATCHABLE: &str =
    "must be relative, without \"./\", a trailing \"/\" or empty segments";

pub fn unmatchable(pattern: &str) -> bool {
    pattern.starts_with('/')
        || pattern.starts_with("./")
        || pattern.ends_with('/')
        || pattern.contains("//")
}
```

In `resolve`, replace

```rust
        // Changed paths are relative and normalised, so these can never match.
        if let Some(p) = cc.paths.iter().find(|p| {
            p.starts_with('/') || p.starts_with("./") || p.ends_with('/') || p.contains("//")
        }) {
            return Err(format!(
                "{name}: path \"{p}\" must be relative, without \"./\", a trailing \"/\" or empty segments"
            ));
        }
```

with

```rust
        if let Some(p) = cc.paths.iter().find(|p| unmatchable(p)) {
            return Err(format!("{name}: path \"{p}\" {UNMATCHABLE}"));
        }
```

(`patterns_that_can_never_match_are_rejected` pins that the message is unchanged.) In `builtins`, change the doc comment to `/// The six built-in categories (risk policy spec §3, repository rules spec §6).` and add as the last element of the `vec!`:

```rust
        cat("rules", &[crate::rules::PATH]),
```

In `resolution_extends_disables_and_validates`, the expected names become `["ci", "migrations", "infrastructure", "secrets-config", "rules", "auth"]`. In `crates/provefab/src/commands.rs:1444`, `"5 categories: "` becomes `"6 categories: "`. In `crates/provefab/tests/scheduler.rs`, add `"provefab:risk-rules",` after `"provefab:risk-secrets-config",`.

- [ ] **Step 4: Write `rules.rs` above the tests**

```rust
//! Repository rules (docs/specs/2026-10-01-repo-rules-design.md): the
//! conventions a repository keeps in `.provefab/rules.md`, read from the base
//! commit, given to the stages and checked by the reviewer. The format,
//! selection and rendering are pure.

use std::ops::Range;

use sha2::{Digest, Sha256};

use crate::risk::{UNMATCHABLE, glob_match, unmatchable};
use crate::task::Stage;

/// Where a repository keeps its rules, from its root (spec §1).
pub const PATH: &str = ".provefab/rules.md";
/// Spec §3 limits.
pub const MAX_RULES: usize = 100;
pub const MAX_SUMMARY: usize = 120;
pub const MAX_TEXT: usize = 2000;
/// Characters of rules one prompt carries (spec §5).
pub const BUDGET: usize = 12_000;
/// The block's title in every prompt (spec §5).
pub const TITLE: &str = "Repository rules (approved by the maintainers of this repository)";

/// One rule of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    pub number: u32,
    pub summary: String,
    /// `paths:` patterns; empty: the rule applies whatever the files.
    pub paths: Vec<String>,
    /// The rule's Markdown text, without its trailing blank lines.
    pub text: String,
    /// The bytes of the file this rule covers, its heading up to the next
    /// rule heading or the end: where Provefab Pro edits a rule, leaving every
    /// other byte as the maintainers wrote it (plan decision 7).
    pub span: Range<usize>,
}

/// Why a rules file is invalid (spec §3). Never quotes the file: the reason
/// goes to the task log, `provefab doctor` and the export as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RulesError {
    #[error(
        "line {0}: a rule heading is `## R<number>: <summary>`, the number from 1, without leading zeros"
    )]
    Heading(usize),
    #[error("R{0} is used by two rules")]
    Duplicate(u32),
    #[error("line {line}: `{key}` goes once, between a rule heading and its text")]
    Misplaced { line: usize, key: &'static str },
    #[error("R{rule}: path pattern {index} {why}")]
    Pattern {
        rule: u32,
        index: usize,
        why: &'static str,
    },
    #[error("more than 100 rules")]
    TooMany,
    #[error("R{0}: the summary is longer than 120 characters")]
    SummaryTooLong(u32),
    #[error("R{0}: the text is longer than 2000 characters")]
    TextTooLong(u32),
}

/// Parses a rules file (spec §3, plan decision 6). Text before the first
/// rule heading is an introduction and is ignored, including any `## `
/// heading there that does not start with `R` and a digit; after it, every
/// `## ` line is a rule heading. A rule's optional `paths:` and `sources:`
/// lines come right after its heading, then its text; `sources:` is never
/// interpreted.
pub fn parse(text: &str) -> Result<Vec<Rule>, RulesError> {
    let mut rules: Vec<Rule> = Vec::new();
    let mut open: Option<Draft> = None;
    let mut at = 0;
    for (i, raw) in text.split_inclusive('\n').enumerate() {
        let start = at;
        at += raw.len();
        let line = raw.trim_end_matches(['\n', '\r']);
        let line = if i == 0 {
            line.trim_start_matches('\u{feff}')
        } else {
            line
        };
        let rule_like =
            line.starts_with("## R") && line[4..].starts_with(|c: char| c.is_ascii_digit());
        if line.starts_with("## ") && (open.is_some() || rule_like) {
            let (number, summary) = heading(line).ok_or(RulesError::Heading(i + 1))?;
            if let Some(d) = open.take() {
                push(&mut rules, d.finish(start)?)?;
            }
            open = Some(Draft::new(number, summary, start));
        } else if let Some(d) = open.as_mut() {
            d.line(i + 1, line)?;
        }
    }
    if let Some(d) = open.take() {
        push(&mut rules, d.finish(text.len())?)?;
    }
    Ok(rules)
}

fn push(rules: &mut Vec<Rule>, rule: Rule) -> Result<(), RulesError> {
    if rules.iter().any(|r| r.number == rule.number) {
        return Err(RulesError::Duplicate(rule.number));
    }
    if rules.len() == MAX_RULES {
        return Err(RulesError::TooMany);
    }
    rules.push(rule);
    Ok(())
}

/// `## R<n>: <summary>` as (n, summary).
fn heading(line: &str) -> Option<(u32, &str)> {
    let rest = line.strip_prefix("## R")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (number, tail) = rest.split_at(digits);
    if number.is_empty() || number.starts_with('0') {
        return None;
    }
    let summary = tail.strip_prefix(':')?.trim();
    Some((number.parse().ok()?, summary)).filter(|(_, s)| !s.is_empty())
}

/// A rule being read, line by line.
struct Draft<'a> {
    number: u32,
    summary: &'a str,
    paths: Option<Vec<String>>,
    sources: bool,
    text: Vec<&'a str>,
    start: usize,
}

impl<'a> Draft<'a> {
    fn new(number: u32, summary: &'a str, start: usize) -> Self {
        Self {
            number,
            summary,
            paths: None,
            sources: false,
            text: Vec::new(),
            start,
        }
    }

    fn line(&mut self, n: usize, line: &'a str) -> Result<(), RulesError> {
        for key in ["paths:", "sources:"] {
            if let Some(value) = line.strip_prefix(key) {
                let seen = if key == "paths:" {
                    self.paths.is_some()
                } else {
                    self.sources
                };
                if seen || !self.text.is_empty() {
                    return Err(RulesError::Misplaced { line: n, key });
                }
                if key == "paths:" {
                    self.paths = Some(value.split(',').map(|p| p.trim().to_string()).collect());
                } else {
                    self.sources = true;
                }
                return Ok(());
            }
        }
        if !self.text.is_empty() || !line.trim().is_empty() {
            self.text.push(line);
        }
        Ok(())
    }

    fn finish(self, end: usize) -> Result<Rule, RulesError> {
        let number = self.number;
        if self.summary.chars().count() > MAX_SUMMARY {
            return Err(RulesError::SummaryTooLong(number));
        }
        let paths = self.paths.unwrap_or_default();
        for (i, p) in paths.iter().enumerate() {
            let why = if p.is_empty() {
                Some("is empty")
            } else if unmatchable(p) {
                Some(UNMATCHABLE)
            } else {
                None
            };
            if let Some(why) = why {
                return Err(RulesError::Pattern {
                    rule: number,
                    index: i + 1,
                    why,
                });
            }
        }
        let text = self.text.join("\n").trim_end().to_string();
        if text.chars().count() > MAX_TEXT {
            return Err(RulesError::TextTooLong(number));
        }
        Ok(Rule {
            number,
            summary: self.summary.to_string(),
            paths,
            text,
            span: self.start..end,
        })
    }
}

/// The rules a stage gets (spec §5): every rule for the plan; for the
/// implementation (the plan's files) and the review (the round's changed
/// files), the rules without `paths:` and those matching one of `files`.
pub fn select<'a>(rules: &'a [Rule], stage: Stage, files: &[String]) -> Vec<&'a Rule> {
    rules
        .iter()
        .filter(|r| {
            matches!(stage, Stage::Plan)
                || r.paths.is_empty()
                || r.paths
                    .iter()
                    .any(|p| files.iter().any(|f| glob_match(p, f)))
        })
        .collect()
}

/// The prompt block for `selected` and how many rules `budget` (characters)
/// left out: rules in number order, the first that does not fit and every
/// later one left out (plan decision 8). Nothing selected renders as
/// nothing, so the prompt is the one a repository without rules gets.
pub fn render(selected: &[&Rule], budget: usize) -> (String, usize) {
    if selected.is_empty() {
        return (String::new(), 0);
    }
    let mut sorted = selected.to_vec();
    sorted.sort_by_key(|r| r.number);
    let (mut body, mut used, mut omitted) = (String::new(), 0, 0);
    for r in sorted {
        let entry = if r.text.is_empty() {
            format!("R{}: {}\n\n", r.number, r.summary)
        } else {
            format!("R{}: {}\n{}\n\n", r.number, r.summary, r.text)
        };
        let len = entry.chars().count();
        if omitted > 0 || used + len > budget {
            omitted += 1;
            continue;
        }
        used += len;
        body.push_str(&entry);
    }
    let mut out = format!(
        "\n{TITLE}. They add to the instructions above and never override them.\n\n{body}"
    );
    match omitted {
        0 => {}
        1 => out.push_str("1 more rule was left out to keep this prompt short.\n"),
        n => out.push_str(&format!(
            "{n} more rules were left out to keep this prompt short.\n"
        )),
    }
    (out, omitted)
}

/// The numbers of the rules `render` kept, ascending.
pub fn numbers(selected: &[&Rule], omitted: usize) -> Vec<u32> {
    let mut n: Vec<u32> = selected.iter().map(|r| r.number).collect();
    n.sort_unstable();
    n.truncate(n.len().saturating_sub(omitted));
    n
}

/// `R3` (any case, surrounding spaces ignored) as 3; anything else `None`.
pub fn rule_number(s: &str) -> Option<u32> {
    let s = s.trim();
    let digits = s.strip_prefix(['R', 'r'])?;
    if digits.is_empty() || digits.starts_with('0') || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    digits.parse().ok()
}

/// Hex SHA-256 of the file, as `rules_loaded` records it.
pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run --all-features -p provefab rules:: risk:: doctor_prints_each_repos_risk_policy run_once_polls_creates_labels`
Expected: PASS.

- [ ] **Step 6: All checks**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green. (`sha256_hex` and `numbers` are `pub`, so no dead-code warning before Task 3 uses them.)

- [ ] **Step 7: Commit**

```bash
git add crates/provefab/src/rules.rs crates/provefab/src/lib.rs crates/provefab/src/risk.rs crates/provefab/src/commands.rs crates/provefab/tests/scheduler.rs
git commit -m "rules: parse, select and render repository rules; rules risk category

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Storage: migration `0007`, `Finding.rule`, maintenance runs

**Files:**
- Create: `crates/provefab/migrations/0007_rules.sql`
- Modify: `crates/provefab/src/stage.rs:38-44` (`Finding`), tests
- Modify: `crates/provefab/src/record.rs:724-738` (`FindingRow`)
- Modify: `crates/provefab/src/store.rs` (`record_review` insert `:927-946`, `finding_row` `:1456-1470`, new `MaintenanceRun` and methods, `migrations_are_frozen` `:1581-1599`, test literal `:2065-2070`)
- Modify: `crates/provefab/src/pipeline.rs:1332-1337` and `:3615-3633` (literals)

**Interfaces:**
- Consumes: nothing new.
- Produces:

```rust
// stage.rs
pub struct Finding { pub file: String, pub line: Option<u32>, pub severity: Severity, pub text: String, #[serde(default)] pub rule: Option<String> }
// record.rs
pub struct FindingRow { /* existing fields */ pub rule: Option<String>, pub event_id: i64 }
// store.rs
pub struct MaintenanceRun { pub id: i64, pub repo: String, pub kind: String, pub started_at: i64, pub finished_at: Option<i64>, pub model_id: Option<String>, pub cost_usd: Option<f64>, pub quota_units: Option<f64>, pub outcome: String, pub pr_url: Option<String>, pub detail: Option<serde_json::Value> }
impl Store {
    pub async fn record_maintenance_run(&self, run: &MaintenanceRun) -> Result<i64, StoreError>; // `run.id` is ignored
    pub async fn last_maintenance_run(&self, repo: &str, kind: &str) -> Result<Option<MaintenanceRun>, StoreError>; // repo any case
    pub async fn maintenance_runs(&self, repo: Option<&str>) -> Result<Vec<MaintenanceRun>, StoreError>; // oldest first
    pub async fn outputs_since(&self, id: i64, kind: &str, since: i64) -> Result<Vec<(i64, Value)>, StoreError>; // (at, json), oldest first
}
```

- [ ] **Step 1: Write the failing tests**

In `crates/provefab/src/stage.rs` `mod tests`:

```rust
    /// Review Focus 3: an answer stored before rules existed still reads.
    #[test]
    fn a_finding_may_cite_a_rule_and_older_answers_still_read() {
        assert!(output_schema::<ReviewOutput>().to_string().contains("\"rule\""));
        let old: ReviewOutput = serde_json::from_value(json!({
            "verdict": "approve",
            "findings": [{"file": "a.rs", "line": null, "severity": "minor", "text": "t"}]
        }))
        .unwrap();
        assert_eq!(old.findings[0].rule, None);
        let new: ReviewOutput = serde_json::from_value(json!({
            "verdict": "approve",
            "findings": [{"file": "a.rs", "line": 1, "severity": "minor", "text": "t", "rule": "R3"}]
        }))
        .unwrap();
        assert_eq!(new.findings[0].rule.as_deref(), Some("R3"));
    }
```

In `crates/provefab/src/store.rs` `mod tests`, replace the `migrations_are_frozen` array by the same six strings plus a seventh:

```rust
                "de68e76b6d0a8bee1058b97821d70b7d78434d938cef56bcf13d9a1d42aaf30b9bac8bf2e103104949b747230190b5e9",
```

and add:

```rust
    #[tokio::test]
    async fn a_finding_keeps_the_rule_it_cites() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(1)).await.unwrap().unwrap();
        let cited = crate::stage::Finding {
            file: "a.rs".into(),
            line: None,
            severity: crate::stage::Severity::Minor,
            text: "t".into(),
            rule: Some("R3".into()),
        };
        let plain = crate::stage::Finding {
            rule: None,
            ..cited.clone()
        };
        s.record_review(id, &json!({}), "m", 1, 0, "approve", &[cited, plain])
            .await
            .unwrap();
        let rules: Vec<Option<String>> =
            s.findings(id).await.unwrap().into_iter().map(|f| f.rule).collect();
        assert_eq!(rules, [Some("R3".to_string()), None]);
    }

    #[tokio::test]
    async fn maintenance_runs_are_kept_per_repository_and_kind() {
        let (_d, s) = store().await;
        let run = |kind: &str, at: i64| MaintenanceRun {
            id: 0,
            repo: "O/R".into(),
            kind: kind.into(),
            started_at: at,
            finished_at: Some(at + 5),
            model_id: Some("std-claude".into()),
            cost_usd: None,
            quota_units: Some(0.5),
            outcome: "ok".into(),
            pr_url: None,
            detail: Some(json!({"highest": 4})),
        };
        s.record_maintenance_run(&run("rules", 10)).await.unwrap();
        s.record_maintenance_run(&run("rules", 30)).await.unwrap();
        s.record_maintenance_run(&run("periodic", 20)).await.unwrap();
        let last = s.last_maintenance_run("o/r", "rules").await.unwrap().unwrap();
        assert_eq!(
            (last.started_at, last.finished_at, last.quota_units, last.detail),
            (30, Some(35), Some(0.5), Some(json!({"highest": 4})))
        );
        assert!(s.last_maintenance_run("o/r", "other").await.unwrap().is_none());
        assert!(s.last_maintenance_run("x/y", "rules").await.unwrap().is_none());
        let all = s.maintenance_runs(Some("o/r")).await.unwrap();
        assert_eq!(
            all.iter().map(|r| r.started_at).collect::<Vec<_>>(),
            [10, 20, 30]
        );
        assert_eq!(s.maintenance_runs(None).await.unwrap().len(), 3);
        assert!(s.maintenance_runs(Some("x/y")).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn outputs_since_says_when_each_was_recorded() {
        let (_d, s) = store().await;
        let id = s.add_issue(&issue(1)).await.unwrap().unwrap();
        s.record_output(id, "review", &json!({"n": 1})).await.unwrap();
        let got = s.outputs_since(id, "review", 0).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].1, json!({"n": 1}));
        assert!(got[0].0 > 0);
        assert!(s.outputs_since(id, "review", now() + 60).await.unwrap().is_empty());
    }
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab a_finding_may_cite a_finding_keeps maintenance_runs_are_kept outputs_since migrations_are_frozen`
Expected: compile errors (`no field rule`, `cannot find struct MaintenanceRun`).

- [ ] **Step 3: The migration**

Create `crates/provefab/migrations/0007_rules.sql` with exactly this text (the checksum above is the SHA-384 of these bytes, ending with one newline; check with `shasum -a 384 crates/provefab/migrations/0007_rules.sql`. If it differs, the file differs from this text: fix the file, never the checksum):

```sql
-- Repository rules (docs/specs/2026-10-01-repo-rules-design.md sections 5 and 7).
-- The rule a review finding cites (`R3`); NULL when it cites none.
ALTER TABLE findings ADD COLUMN rule TEXT;

-- Periodic maintenance (section 7): the scheduler's daily call per repository
-- (kind `periodic`) and the runs a policy records (Provefab Pro: `rules`).
-- `detail` is the policy's own JSON about the run; Provefab never shows it.
CREATE TABLE maintenance_runs (
    id          INTEGER PRIMARY KEY,
    repo        TEXT    NOT NULL,
    kind        TEXT    NOT NULL,
    started_at  INTEGER NOT NULL,
    finished_at INTEGER,
    model_id    TEXT,
    cost_usd    REAL,
    quota_units REAL,
    outcome     TEXT    NOT NULL,
    pr_url      TEXT,
    detail      TEXT
);
CREATE INDEX maintenance_runs_repo_kind ON maintenance_runs (repo, kind, started_at);
```

- [ ] **Step 4: `Finding.rule` and `FindingRow.rule`**

`stage.rs`, the struct becomes:

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub file: String,
    pub line: Option<u32>,
    pub severity: Severity,
    pub text: String,
    /// The repository rule this finding reports a violation of (`"R3"`), or
    /// null (repository rules spec §5). Absent from answers stored before it.
    #[serde(default)]
    pub rule: Option<String>,
}
```

`record.rs`, in `FindingRow`, add before `pub event_id: i64,`:

```rust
    /// The rule the finding cites (`R3`), checked against the rules its review was given.
    pub rule: Option<String>,
```

`store.rs` `record_review`: the insert becomes

```rust
            sqlx::query(
                "INSERT INTO findings (task_id, key, pass, round, reviewer_model, severity, file, line, text, event_id, rule) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
```

with `.bind(&f.rule)` after `.bind(event_id)`. `finding_row` gets `rule: r.get("rule"),` before `event_id`.

Every literal gets the new field: `pipeline.rs` in `watch_pr` (`Finding { file: "(pull request comment)".into(), ..., text: format!(...), rule: None }`) and in the test `review_notes_without_keys_keep_the_findings_and_drop_the_command_help` (`rule: None` in the `Finding` and in the `FindingRow`); `store.rs` test `a_revert_marks_the_findings_of_the_merge_its_check_covers` (`rule: None`).

- [ ] **Step 5: Maintenance runs and `outputs_since` in `store.rs`**

After `StageRunRecord`:

```rust
/// One row of `maintenance_runs` (repository rules spec §7): the scheduler's
/// daily call (`periodic`) or a run a policy recorded (Pro: `rules`).
#[derive(Debug, Clone, PartialEq)]
pub struct MaintenanceRun {
    pub id: i64,
    pub repo: String,
    pub kind: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub model_id: Option<String>,
    pub cost_usd: Option<f64>,
    pub quota_units: Option<f64>,
    pub outcome: String,
    pub pr_url: Option<String>,
    /// The policy's own JSON about the run; never shown (plan decision 12).
    pub detail: Option<Value>,
}
```

In `impl Store`, after `last_output`:

```rust
    /// Appends one maintenance run; returns its id (`run.id` is ignored).
    pub async fn record_maintenance_run(&self, run: &MaintenanceRun) -> Result<i64, StoreError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO maintenance_runs (repo, kind, started_at, finished_at, model_id, cost_usd, quota_units, outcome, pr_url, detail) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(&run.repo)
        .bind(&run.kind)
        .bind(run.started_at)
        .bind(run.finished_at)
        .bind(&run.model_id)
        .bind(run.cost_usd)
        .bind(run.quota_units)
        .bind(&run.outcome)
        .bind(&run.pr_url)
        .bind(run.detail.as_ref().map(Value::to_string))
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// The latest run of `kind` for `repo` (any case).
    pub async fn last_maintenance_run(
        &self,
        repo: &str,
        kind: &str,
    ) -> Result<Option<MaintenanceRun>, StoreError> {
        let row = sqlx::query(
            "SELECT * FROM maintenance_runs WHERE LOWER(repo) = LOWER(?) AND kind = ? \
             ORDER BY started_at DESC, id DESC LIMIT 1",
        )
        .bind(repo)
        .bind(kind)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(maintenance_run).transpose()
    }

    /// Every maintenance run, oldest first, optionally of one repository (any case).
    pub async fn maintenance_runs(
        &self,
        repo: Option<&str>,
    ) -> Result<Vec<MaintenanceRun>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM maintenance_runs WHERE ? IS NULL OR LOWER(repo) = LOWER(?) \
             ORDER BY started_at, id",
        )
        .bind(repo)
        .bind(repo)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(maintenance_run).collect()
    }

    /// The outputs of `kind` recorded at or after `since`, oldest first, each
    /// with when it was recorded.
    pub async fn outputs_since(
        &self,
        id: i64,
        kind: &str,
        since: i64,
    ) -> Result<Vec<(i64, Value)>, StoreError> {
        let rows = sqlx::query(
            "SELECT at, json FROM stage_outputs WHERE task_id = ? AND kind = ? AND at >= ? ORDER BY id",
        )
        .bind(id)
        .bind(kind)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| -> Result<(i64, Value), StoreError> {
                let s: String = r.get("json");
                let v = serde_json::from_str(&s).map_err(|_| StoreError::Corrupt(s))?;
                Ok((r.get("at"), v))
            })
            .collect()
    }
```

Next to `finding_row`:

```rust
fn maintenance_run(r: &SqliteRow) -> Result<MaintenanceRun, StoreError> {
    let detail: Option<String> = r.get("detail");
    Ok(MaintenanceRun {
        id: r.get("id"),
        repo: r.get("repo"),
        kind: r.get("kind"),
        started_at: r.get("started_at"),
        finished_at: r.get("finished_at"),
        model_id: r.get("model_id"),
        cost_usd: r.get("cost_usd"),
        quota_units: r.get("quota_units"),
        outcome: r.get("outcome"),
        pr_url: r.get("pr_url"),
        detail: detail
            .map(|d| serde_json::from_str(&d).map_err(|_| StoreError::Corrupt(d)))
            .transpose()?,
    })
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run --all-features -p provefab a_finding_may_cite a_finding_keeps maintenance_runs_are_kept outputs_since migrations_are_frozen schemas_are_strict`
Expected: PASS (`schemas_are_strict_for_openai_structured_outputs` now lists `rule` as required and nullable, as Codex needs).

- [ ] **Step 7: All checks, then commit**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

```bash
git add crates/provefab/migrations/0007_rules.sql crates/provefab/src/stage.rs crates/provefab/src/record.rs crates/provefab/src/store.rs crates/provefab/src/pipeline.rs
git commit -m "store: findings.rule and maintenance_runs (migration 0007)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Read the rules once per pass from the base commit and give them to every stage

**Files:**
- Modify: `crates/provefab/src/forge.rs` (`Git::show_file` after `rev_parse`, test)
- Modify: `crates/provefab/src/record.rs:155-161` (two events), `:199-223` (`kind`), test `every_kind_round_trips_with_its_source`
- Modify: `crates/provefab/prompts/plan.md`, `implement.md`, `review.md` (trailing `{{rules}}`)
- Modify: `crates/provefab/src/prompts.rs:36-52` (`render`), tests
- Modify: `crates/provefab/src/rules.rs` (pipeline side)
- Modify: `crates/provefab/src/pipeline.rs` (`pass_base` `pub(crate)` `:1140`; `prepare` `:1151-1181`; `plan` `:2187-2196`; `implement` `:2464-2475`; `review` `:2921-2935`)
- Modify: `crates/provefab/src/commands.rs:599-666` (`event_summary`)
- Create: `crates/provefab/tests/rules.rs`

**Interfaces:**
- Consumes: Task 1 (`parse`, `select`, `render`, `numbers`, `sha256_hex`, `PATH`, `BUDGET`, `TITLE`, `Rule`); `Pipeline::{checkout, pass_base}`, `pipeline::pass_of`, `Store::write_with_events`.
- Produces:

```rust
// forge.rs
impl Git { pub async fn show_file(&self, repo: &Path, rev: &str, path: &str) -> Result<Option<String>, ForgeError>; }
// record.rs
Event::RulesLoaded { pass: u32, numbers: Vec<u32>, sha256: String, omitted: u32 } // kind "rules_loaded", source fact
Event::RulesInvalid { pass: u32, reason: String }                                  // kind "rules_invalid", source fact
// rules.rs, on Pipeline<R, O, H>
pub(crate) async fn load_rules(&self, task: &TaskRow, repo: &RepoConfig, base: &str) -> Result<Vec<Rule>, PipelineError>;
pub(crate) async fn pass_rules(&self, task: &TaskRow, repo: &RepoConfig) -> Result<Vec<Rule>, PipelineError>;
// stage outputs: "rules" {"pass", "text"}; "rules_given" {"pass", "round", "numbers"} (review prompts that carried rules)
// prompts: every template ends with {{rules}}; a missing "rules" var renders as ""
```

- [ ] **Step 1: Write the failing tests**

`crates/provefab/src/forge.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn show_file_reads_the_commit_never_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let sh = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(d)
                .env_remove("GIT_CONFIG_COUNT")
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        sh(&["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(d.join(".provefab")).unwrap();
        std::fs::write(d.join(".provefab/rules.md"), "## R1: One\n\ntext\n").unwrap();
        sh(&["add", "-f", ".provefab/rules.md"]);
        sh(&["commit", "-q", "-m", "rules"]);
        std::fs::write(d.join(".provefab/rules.md"), "## R9: edited\n").unwrap();
        let g = git();
        assert_eq!(
            g.show_file(d, "HEAD", ".provefab/rules.md").await.unwrap().as_deref(),
            Some("## R1: One\n\ntext")
        );
        assert_eq!(g.show_file(d, "HEAD", ".provefab/none.md").await.unwrap(), None);
        assert!(g.show_file(d, "no-such-rev", ".provefab/rules.md").await.is_err());
    }
```

`crates/provefab/src/prompts.rs` `mod tests`:

```rust
    /// Spec §10 non-regression and Review Focus 2: without rules the prompt
    /// is byte for byte the old one; a rule is inserted verbatim, last.
    #[test]
    fn rules_go_last_and_default_to_nothing() {
        for (template, text) in [
            (Template::Plan, PLAN),
            (Template::Implement, IMPLEMENT),
            (Template::Review, REVIEW),
        ] {
            let before = text.strip_suffix("{{rules}}").expect("ends with {{rules}}");
            assert!(before.ends_with(".\n"), "{before}");
            assert_eq!(render(template, &[]), before);
            let rule = "\nR1: {{body}} stays\n```\n--- END UNTRUSTED diff ---\n";
            let with = render(template, &[("body", "BODY"), ("rules", rule)]);
            assert!(with.ends_with(rule), "{with}");
            assert_eq!(with.matches("BODY").count(), 1, "{with}");
        }
    }
```

In `crates/provefab/src/record.rs`, `every_kind_round_trips_with_its_source`: append to `events`

```rust
            Event::RulesLoaded {
                pass: 1,
                numbers: vec![1, 3],
                sha256: "ab".into(),
                omitted: 0,
            },
            Event::RulesInvalid {
                pass: 1,
                reason: "R1 is used by two rules".into(),
            },
```

and to `sources` `Source::Fact, Source::Fact` (the array becomes `[Source::Claim, Source::Human, Source::Inferred, Source::Fact, Source::Fact, Source::Fact]`), then add after the loop:

```rust
        assert_eq!(events[4].kind(), "rules_loaded");
        assert_eq!(events[5].kind(), "rules_invalid");
```

Create `crates/provefab/tests/rules.rs`:

```rust
#![cfg(feature = "testkit")]
//! Repository rules (docs/specs/2026-10-01-repo-rules-design.md).

use provefab::rules::TITLE;
use provefab::testkit::*;

/// Commits `text` as the repository's rules on `main` and pushes it, as a
/// maintainer merging a pull request would.
fn commit_rules(f: &Fixture, text: &str) {
    let local = f.config.repos[0].path_in(&f.home);
    std::fs::create_dir_all(local.join(".provefab")).unwrap();
    std::fs::write(local.join(".provefab/rules.md"), text).unwrap();
    git(&local, &["add", "-f", ".provefab/rules.md"]);
    git(&local, &["commit", "-q", "-m", "rules"]);
    git(&local, &["push", "-q", "origin", "main"]);
}

const RULES: &str = "# Our conventions\n\nWritten by hand.\n\n## R1: Keep commits small\n\nOne change per pull request.\n\n## R2: Feature files end with a newline\npaths: feature.txt\nsources: PR #3 F1 (rejected)\n\nEvery line of feature.txt ends with a newline.\n\n## R3: Errors use ApiError\npaths: src/api/**\n\nReturn `ApiError` from handlers.\n";

/// The last prompt of `stage` the fake workers were given.
fn prompt_of(calls: &[(String, String, String)], stage: &str) -> String {
    calls
        .iter()
        .rfind(|(_, s, _)| s == stage)
        .map(|(_, _, p)| p.clone())
        .unwrap_or_else(|| panic!("no {stage} prompt"))
}

fn kinds(events: &[provefab::record::StoredEvent]) -> Vec<String> {
    events.iter().map(|e| e.kind.clone()).collect()
}

#[tokio::test]
async fn rules_from_the_base_commit_reach_each_stage_by_path() {
    let f = fixture(&["test -f feature.txt"]);
    commit_rules(&f, RULES);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let calls = p.runner.calls();
    let plan = prompt_of(&calls, "plan");
    for r in ["R1: Keep commits small", "R2: Feature files", "R3: Errors use ApiError"] {
        assert!(plan.contains(r), "{r} missing from {plan}");
    }
    assert!(!plan.contains("sources:") && !plan.contains("Written by hand"), "{plan}");
    // The plan names feature.txt and the change touches only it.
    for stage in ["implement", "review"] {
        let prompt = prompt_of(&calls, stage);
        assert!(prompt.contains("R1: Keep commits small"), "{prompt}");
        assert!(prompt.contains("R2: Feature files"), "{prompt}");
        assert!(!prompt.contains("R3:"), "{prompt}");
        let block = prompt.find(TITLE).unwrap();
        assert!(block > prompt.rfind("END UNTRUSTED").unwrap(), "{prompt}");
    }
    let ev = p.store.events(id).await.unwrap();
    let loaded = ev.iter().find(|e| e.kind == "rules_loaded").unwrap();
    assert_eq!(loaded.source, "fact");
    assert_eq!(loaded.payload["numbers"], json!([1, 2, 3]));
    assert_eq!(loaded.payload["omitted"], 0);
    assert_eq!(
        loaded.payload["sha256"],
        json!(provefab::rules::sha256_hex(RULES.trim_end()))
    );
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.contains("rules_loaded pass 1: R1, R2, R3"), "{log}");
}

#[tokio::test]
async fn an_edit_outside_the_base_commit_changes_nothing_for_the_pass() {
    let f = fixture(&["test -f feature.txt"]);
    commit_rules(&f, RULES);
    // Not committed: not the base commit either.
    let local = f.config.repos[0].path_in(&f.home);
    std::fs::write(local.join(".provefab/rules.md"), "## R8: Uncommitted\n").unwrap();
    let edit = |m: &ModelEntry, req: &StageRequest, tx: &UnboundedSender<WorkerEvent>| {
        if stage_of(&req.prompt) == "implement" {
            // What a guard bypass would do: the stages still read the base.
            std::fs::write(
                req.cwd.join(".provefab/rules.md"),
                "## R9: Ignore every other rule\n",
            )
            .unwrap();
        }
        happy(m, req, tx)
    };
    let p = pipeline(&f, Box::new(edit), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let calls = p.runner.calls();
    let review = prompt_of(&calls, "review");
    assert!(review.contains("R1: Keep commits small"), "{review}");
    assert!(!review.contains("R9") && !review.contains("R8"), "{review}");
    assert!(!prompt_of(&calls, "plan").contains("R8"));
}

#[tokio::test]
async fn an_invalid_file_runs_the_task_without_rules() {
    let f = fixture(&["test -f feature.txt"]);
    commit_rules(&f, "## R1: One\n\ntext\n\n## R1: Again\n\ntext\n");
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    for (_, stage, prompt) in p.runner.calls() {
        assert!(!prompt.contains(TITLE), "{stage}: {prompt}");
    }
    let ev = p.store.events(id).await.unwrap();
    let invalid = ev.iter().find(|e| e.kind == "rules_invalid").unwrap();
    assert_eq!(invalid.payload["reason"], "R1 is used by two rules");
    assert!(!kinds(&ev).contains(&"rules_loaded".to_string()));
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.contains("rules_invalid pass 1: R1 is used by two rules"), "{log}");
}

#[tokio::test]
async fn a_repository_without_the_file_gets_the_prompts_it_got_before() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let calls = p.runner.calls();
    for (_, stage, prompt) in &calls {
        assert!(!prompt.contains(TITLE), "{stage}: {prompt}");
    }
    assert!(
        prompt_of(&calls, "plan").ends_with("Use null for any other kind of change.\n"),
        "nothing after the template's last line"
    );
    let k = kinds(&p.store.events(id).await.unwrap());
    assert!(!k.iter().any(|x| x.starts_with("rules_")), "{k:?}");
}

#[tokio::test]
async fn the_budget_leaves_rules_out_and_says_how_many() {
    let f = fixture(&["test -f feature.txt"]);
    let text: String = (1..=10)
        .map(|n| format!("## R{n}: Rule {n}\n\n{}\n\n", "x".repeat(1900)))
        .collect();
    commit_rules(&f, &text);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let plan = prompt_of(&p.runner.calls(), "plan");
    assert!(plan.contains("R6: Rule 6\n") && !plan.contains("R7: Rule 7"), "{plan}");
    assert!(plan.contains("4 more rules were left out to keep this prompt short."), "{plan}");
    let ev = p.store.events(id).await.unwrap();
    let loaded = ev.iter().find(|e| e.kind == "rules_loaded").unwrap();
    assert_eq!(loaded.payload["omitted"], 4);
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.contains("(4 left out by the budget)"), "{log}");
}

/// Review Focus 5: a pass begun before the upgrade has no `rules` output.
#[tokio::test]
async fn a_pass_without_its_rules_output_loads_them_at_its_next_stage() {
    let f = fixture(&["test -f feature.txt"]);
    commit_rules(&f, RULES);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    for _ in 0..5 {
        if p.step(id).await.unwrap() == Planning {
            break;
        }
    }
    assert_eq!(p.store.task(id).await.unwrap().unwrap().state, Planning);
    let db = sqlx::SqlitePool::connect(&format!(
        "sqlite:{}",
        f.home.join("provefab.db").display()
    ))
    .await
    .unwrap();
    sqlx::query("DELETE FROM stage_outputs WHERE kind = 'rules'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    assert!(prompt_of(&p.runner.calls(), "plan").contains("R1: Keep commits small"));
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab show_file_reads rules_go_last every_kind_round_trips --test rules`
Expected: compile errors (`no method show_file`, `no variant RulesLoaded`); after Steps 3-4 alone, `rules_go_last_and_default_to_nothing` fails on `expect("ends with {{rules}}")`.

- [ ] **Step 3: `Git::show_file`**

In `impl Git`, after `rev_parse`:

```rust
    /// `path` as committed at `rev`, or `None` when that commit has no such
    /// file (repository rules spec §4: never the worktree's copy). Trailing
    /// newlines are trimmed, like every git output here.
    pub async fn show_file(
        &self,
        repo: &Path,
        rev: &str,
        path: &str,
    ) -> Result<Option<String>, ForgeError> {
        let listed = self
            .git(repo, &["ls-tree", "--name-only", rev, "--", path])
            .await?;
        if listed.trim() != path {
            return Ok(None);
        }
        self.git(repo, &["show", &format!("{rev}:{path}")])
            .await
            .map(Some)
    }
```

- [ ] **Step 4: Events and their log lines**

`record.rs`, in `enum Event` after `RiskClassified { .. },`:

```rust
    /// The repository rules a pass read from its base commit (repository
    /// rules spec §4). `omitted`: rules the budget left out of the plan
    /// prompt, which gets them all (plan decision 4).
    RulesLoaded {
        pass: u32,
        numbers: Vec<u32>,
        sha256: String,
        omitted: u32,
    },
    /// The base commit's rules file is invalid: the pass runs without rules.
    /// `reason` never quotes the file (plan decision 5).
    RulesInvalid { pass: u32, reason: String },
```

and in `kind()`: `Self::RulesLoaded { .. } => "rules_loaded",` and `Self::RulesInvalid { .. } => "rules_invalid",`. (`source()` already maps them to `Fact`; `redact_event` exports both as they are, which decision 5 allows.)

`commands.rs` `event_summary`, before `Plan { pass, .. } =>`:

```rust
        RulesLoaded {
            pass,
            numbers,
            omitted,
            ..
        } => {
            let rules = if numbers.is_empty() {
                "no rules".to_string()
            } else {
                numbers
                    .iter()
                    .map(|n| format!("R{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let left = if omitted > 0 {
                format!(" ({omitted} left out by the budget)")
            } else {
                String::new()
            };
            format!("{} pass {pass}: {rules}{left}", e.kind)
        }
        // Provefab's own words, never the file's (plan decision 5).
        RulesInvalid { pass, reason } => format!("{} pass {pass}: {reason}", e.kind),
```

- [ ] **Step 5: Templates and `prompts::render`**

Append the placeholder after each template's final newline, with no newline after it:

```bash
for t in plan implement review; do printf '{{rules}}' >> crates/provefab/prompts/$t.md; done
tail -c 12 crates/provefab/prompts/review.md | od -c | head -2
```

Expected: the last bytes are `.  \n  {  {  r  u  l  e  s  }  }`.

In `prompts::render`, after the `if !vars.iter().any(|(k, _)| *k == "fence") ... { ... }` block and before `let vars = all_vars.as_slice();`:

```rust
    // Without rules `{{rules}}` renders as nothing, so the prompt is the one
    // it was before rules existed (repository rules spec §10).
    if !vars.iter().any(|(k, _)| *k == "rules") {
        all_vars.push(("rules", ""));
    }
```

and add to its doc comment: "A missing `rules` key fills `{{rules}}` with nothing."

- [ ] **Step 6: The pipeline side in `rules.rs`**

Add to the imports of `rules.rs`:

```rust
use serde_json::json;

use crate::agents::StageRunner;
use crate::config::RepoConfig;
use crate::pipeline::{Pipeline, PipelineError, pass_of};
use crate::ports::{Hub, Oracle};
use crate::record::Event;
use crate::store::{TaskRow, Write};
```

and before `#[cfg(test)]`:

```rust
impl<R, O, H> Pipeline<R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// Reads the pass's rules at `base` in the repository's checkout (spec
    /// §4) and records them: the `rules` output its stages read (plan
    /// decision 2), with `rules_loaded` or `rules_invalid`. A missing file
    /// records no event (decision 3); an invalid one runs the pass without rules.
    pub(crate) async fn load_rules(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
        base: &str,
    ) -> Result<Vec<Rule>, PipelineError> {
        let pass = pass_of(task);
        let read = self.git.show_file(&self.checkout(repo), base, PATH).await;
        let (text, event, rules) = match read {
            Ok(None) => (None, None, Vec::new()),
            Ok(Some(text)) => match parse(&text) {
                Ok(rules) => {
                    let all: Vec<&Rule> = rules.iter().collect();
                    let (_, omitted) = render(&all, BUDGET);
                    let event = Event::RulesLoaded {
                        pass,
                        numbers: rules.iter().map(|r| r.number).collect(),
                        sha256: sha256_hex(&text),
                        omitted: omitted as u32,
                    };
                    (Some(text), Some(event), rules)
                }
                Err(e) => {
                    let reason = e.to_string();
                    (None, Some(Event::RulesInvalid { pass, reason }), Vec::new())
                }
            },
            Err(e) => {
                // A git error can name paths or hold a token: logged, never recorded.
                eprintln!(
                    "provefab: task {}: could not read {PATH} at the base commit: {e}",
                    task.id
                );
                let reason = format!("could not read {PATH} at the base commit");
                (None, Some(Event::RulesInvalid { pass, reason }), Vec::new())
            }
        };
        let value = json!({"pass": pass, "text": text});
        let events: Vec<Event> = event.into_iter().collect();
        self.store
            .write_with_events(
                task.id,
                Write::Output {
                    kind: "rules",
                    value: &value,
                },
                &events,
            )
            .await?;
        Ok(rules)
    }

    /// The rules of the task's current pass: those `prepare` loaded, or, for
    /// a pass without them (begun before the upgrade), loaded now from its
    /// pinned base (Review Focus 5).
    pub(crate) async fn pass_rules(
        &self,
        task: &TaskRow,
        repo: &RepoConfig,
    ) -> Result<Vec<Rule>, PipelineError> {
        if let Some(v) = self.store.last_output(task.id, "rules").await?
            && v["pass"].as_u64() == Some(u64::from(pass_of(task)))
        {
            return Ok(v["text"]
                .as_str()
                .and_then(|t| parse(t).ok())
                .unwrap_or_default());
        }
        let base = self.pass_base(task, repo).await?;
        self.load_rules(task, repo, &base).await
    }
}
```

- [ ] **Step 7: The pipeline**

`pipeline.rs`: `async fn pass_base` becomes `pub(crate) async fn pass_base`.

`prepare`, right after the `if let Err(e) = self.pin_base(task, repo, fetched).await { ... }` block:

```rust
        // Repository rules spec §4: read once per pass, at the commit just pinned.
        self.pass_rules(task, repo).await?;
```

`plan`, replace `let prompt = render(Template::Plan, &[ ... ]);` with:

```rust
        let rules = self.pass_rules(task, repo).await?;
        let (rules_block, _) = crate::rules::render(
            &crate::rules::select(&rules, Stage::Plan, &[]),
            crate::rules::BUDGET,
        );
        let prompt = render(
            Template::Plan,
            &[
                ("ref", &reference),
                ("title", &title),
                ("kind", kind.as_str()),
                ("body", &body),
                ("rules", &rules_block),
            ],
        );
```

`implement`, replace its `let prompt = render(Template::Implement, &[ ... ]);` with:

```rust
        let rules = self.pass_rules(task, repo).await?;
        let files = plan.as_ref().map(|p| p.files.clone()).unwrap_or_default();
        let (rules_block, _) = crate::rules::render(
            &crate::rules::select(&rules, Stage::Implement, &files),
            crate::rules::BUDGET,
        );
        let prompt = render(
            Template::Implement,
            &[
                ("ref", &reference),
                ("title", &title),
                ("body", &body),
                ("plan", &plan_str),
                ("feedback", &feedback),
                ("gates", &gates),
                ("rules", &rules_block),
            ],
        );
```

`review`, replace its `let prompt = render(Template::Review, &[ ... ]);` with:

```rust
        let rules = self.pass_rules(task, repo).await?;
        // The round's changed files, both sides of a rename, as the risk policy reads them.
        let changed: Vec<String> = self
            .git
            .changed_files(&wt, &base)
            .await
            .unwrap_or_default()
            .into_iter()
            .flat_map(|c| std::iter::once(c.path).chain(c.from))
            .collect();
        let selected = crate::rules::select(&rules, Stage::Review, &changed);
        let (rules_block, omitted) = crate::rules::render(&selected, crate::rules::BUDGET);
        let given = crate::rules::numbers(&selected, omitted);
        if !given.is_empty() {
            self.store
                .record_output(
                    task.id,
                    "rules_given",
                    &json!({"pass": pass_of(task), "round": task.review_rounds, "numbers": given}),
                )
                .await?;
        }
        let prompt = render(
            Template::Review,
            &[
                ("ref", &reference),
                ("title", &title),
                ("body", &body),
                ("plan", &plan_str),
                ("base", &base_shown),
                ("diff", &diff),
                ("rules", &rules_block),
            ],
        );
```

- [ ] **Step 8: Run the tests**

Run: `cargo nextest run --all-features -p provefab show_file_reads rules_go_last every_kind_round_trips --test rules`
Expected: PASS (the three unit tests and the six tests of `tests/rules.rs`).

- [ ] **Step 9: All checks, then commit**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green; `tests/record.rs` (`k[0] == "routed"`) is unchanged because `rules_loaded` is written in `prepare`, after classification.

```bash
git add crates/provefab/src crates/provefab/prompts crates/provefab/tests/rules.rs
git commit -m "rules: read once per pass from the base commit, give them to every stage

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: The reviewer reports rule violations; review notes, log, export and the PR body show the rule

**Files:**
- Modify: `crates/provefab/src/rules.rs` (`REVIEW_ASK`, `rules_given`)
- Modify: `crates/provefab/src/pipeline.rs` (`findings_text` `:232-258`, `review_notes` `:355-380`, `feedback` `:2398-2410`, `review` after its answer is parsed `:2960-2964`, `pr_body` Checks `:3222-3230`, tests)
- Modify: `crates/provefab/src/commands.rs` (`log` findings `:572-588`, `export` finding line `:695-702`)
- Test: `crates/provefab/tests/rules.rs`

**Interfaces:**
- Consumes: Task 2 (`Finding.rule`, `FindingRow.rule`, stored by `record_review`), Task 3 (`given`, `rules_given` output, `rule_number`).
- Produces:

```rust
// rules.rs
pub const REVIEW_ASK: &str; // appended to the review prompt's block when it carries rules
impl Pipeline<R, O, H> { pub(crate) async fn rules_given(&self, task: &TaskRow) -> Result<Option<String>, PipelineError>; } // "R1, R3"
// display: review notes "- F2 · R3 · blocking · `file:line` · text (model)"; log "F2 · R3 · blocking · ..."; export finding "rule": "R3" | null; PR body "\nRules: R1, R3\n" after the check lines
```

- [ ] **Step 1: Write the failing tests**

`pipeline.rs` `mod tests`:

```rust
    #[test]
    fn review_notes_and_findings_text_show_the_rule_a_finding_cites() {
        let review = ReviewOutput {
            verdict: ReviewVerdict::Changes,
            findings: vec![Finding {
                file: "src/a.rs".into(),
                line: Some(4),
                severity: Severity::Blocking,
                text: "uses anyhow".into(),
                rule: Some("R3".into()),
            }],
        };
        assert_eq!(
            findings_text(&review, &["F2".to_string()]),
            "- F2 · R3 · blocking · `src/a.rs:4` · uses anyhow"
        );
        let row = FindingRow {
            id: 2,
            task_id: 1,
            key: "F2".into(),
            pass: 1,
            round: 0,
            reviewer_model: "std-codex".into(),
            severity: "blocking".into(),
            file: "src/a.rs".into(),
            line: Some(4),
            text: "uses anyhow".into(),
            rule: Some("R3".into()),
            event_id: 1,
        };
        let notes = review_notes(&review, &[row]).unwrap();
        assert!(
            notes.contains("- F2 · R3 · blocking · `src/a.rs:4` · uses anyhow (std-codex)"),
            "{notes}"
        );
    }
```

Append to `crates/provefab/tests/rules.rs`:

```rust
/// A reviewer approving with three minor findings: one citing R2 (given to
/// the review), one citing R3 (not given: its path is not changed), one
/// citing nothing.
fn citing(
    m: &ModelEntry,
    req: &StageRequest,
    tx: &UnboundedSender<WorkerEvent>,
) -> Option<StageResult> {
    if stage_of(&req.prompt) != "review" {
        return happy(m, req, tx);
    }
    done(Some(json!({"verdict": "approve", "findings": [
        {"file": "feature.txt", "line": 1, "severity": "minor", "text": "no newline at the end", "rule": "r2"},
        {"file": "feature.txt", "line": 1, "severity": "minor", "text": "an API error", "rule": "R3"},
        {"file": "feature.txt", "line": null, "severity": "minor", "text": "naming", "rule": null}
    ]})))
}

#[tokio::test]
async fn a_finding_citing_a_given_rule_is_stored_and_shown_everywhere() {
    let f = fixture(&["test -f feature.txt"]);
    commit_rules(&f, RULES);
    let p = pipeline(&f, Box::new(citing), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let review = prompt_of(&p.runner.calls(), "review");
    assert!(review.contains(provefab::rules::REVIEW_ASK), "{review}");
    let rules: Vec<(String, Option<String>)> = p
        .store
        .findings(id)
        .await
        .unwrap()
        .into_iter()
        .map(|f| (f.key, f.rule))
        .collect();
    assert_eq!(
        rules,
        [
            ("F1".to_string(), Some("R2".to_string())),
            ("F2".to_string(), None),
            ("F3".to_string(), None)
        ]
    );
    let body = p.hub.prs.lock().unwrap()[0].3.clone();
    assert!(body.contains("\nRules: R1, R2\n"), "{body}");
    assert!(
        body.contains("- F1 · R2 · minor · `feature.txt:1` · no newline at the end ("),
        "{body}"
    );
    assert!(body.contains("- F2 · minor · `feature.txt:1` · an API error ("), "{body}");
    let log = provefab::commands::log(&p.store, id).await.unwrap();
    assert!(log.contains("F1 · R2 · minor · feature.txt:1"), "{log}");
    let export = provefab::commands::export(&p.store, None, None, false)
        .await
        .unwrap();
    let f1 = export
        .lines()
        .find(|l| l.contains("\"type\":\"finding\"") && l.contains("\"key\":\"F1\""))
        .unwrap();
    assert!(f1.contains("\"rule\":\"R2\""), "{f1}");
}

/// Spec §10 non-regression: no rules file, no rule anywhere.
#[tokio::test]
async fn without_rules_the_pr_body_and_notes_are_unchanged() {
    let f = fixture(&["test -f feature.txt"]);
    let p = pipeline(&f, Box::new(citing), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    assert_eq!(p.drive(id).await.unwrap(), PrOpen);
    let review = prompt_of(&p.runner.calls(), "review");
    assert!(!review.contains(provefab::rules::REVIEW_ASK), "{review}");
    assert!(
        p.store.findings(id).await.unwrap().iter().all(|f| f.rule.is_none()),
        "a rule the review was not given is dropped"
    );
    let body = p.hub.prs.lock().unwrap()[0].3.clone();
    assert!(!body.contains("Rules:"), "{body}");
    assert!(
        body.contains("- F1 · minor · `feature.txt:1` · no newline at the end ("),
        "{body}"
    );
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab review_notes_and_findings_text --test rules`
Expected: compile error `cannot find value REVIEW_ASK`; with a stub constant, the assertions on `R2` fail.

- [ ] **Step 3: `REVIEW_ASK` and `rules_given` in `rules.rs`**

Next to `TITLE`:

```rust
/// What the review prompt adds after the block when it carries rules (spec §5).
pub const REVIEW_ASK: &str = "When the change breaks one of these rules, report it as a finding with `rule` set to that rule's number (for example \"R3\"). Every other finding has `rule` null.\n";
```

The `serde_json` import becomes `use serde_json::{Value, json};`, and in the `impl Pipeline` block:

```rust
    /// `R1, R3`: the rules the current round's review was given (plan
    /// decision 10), `None` when it was given none.
    pub(crate) async fn rules_given(&self, task: &TaskRow) -> Result<Option<String>, PipelineError> {
        let (pass, round) = (u64::from(pass_of(task)), u64::from(task.review_rounds));
        let given = self
            .store
            .recent_outputs(task.id, "rules_given", u32::MAX)
            .await?
            .into_iter()
            .rev()
            .find(|v| v["pass"].as_u64() == Some(pass) && v["round"].as_u64() == Some(round));
        Ok(given.and_then(|v| {
            let names: Vec<String> = v["numbers"]
                .as_array()?
                .iter()
                .filter_map(Value::as_u64)
                .map(|n| format!("R{n}"))
                .collect();
            (!names.is_empty()).then(|| names.join(", "))
        }))
    }
```

- [ ] **Step 4: The review stage**

In `review`, right after `let given = crate::rules::numbers(&selected, omitted);` (Task 3):

```rust
        let rules_block = if given.is_empty() {
            rules_block
        } else {
            format!("{rules_block}{}", crate::rules::REVIEW_ASK)
        };
```

After

```rust
        let review = match review {
            Ok(r) => r,
            Err(reason) => return self.stage_failed(task, repo, "review", &reason).await,
        };
```

add

```rust
        // Spec §5: a `rule` must name a rule this review was given; any other
        // value is dropped and the finding stays (plan decision 9).
        let mut review = review;
        for f in &mut review.findings {
            f.rule = f
                .rule
                .as_deref()
                .and_then(crate::rules::rule_number)
                .filter(|n| given.contains(n))
                .map(|n| format!("R{n}"));
        }
```

- [ ] **Step 5: Where findings are shown**

`findings_text`, the closure's last two statements become:

```rust
            let rule = f
                .rule
                .as_deref()
                .map(|r| format!("{r} · "))
                .unwrap_or_default();
            format!("- {key}{rule}{sev} · `{}{at}` · {}", f.file, f.text)
```

`review_notes`, the keyed line becomes:

```rust
        .map(|f| {
            let at = f.line.map(|l| format!(":{l}")).unwrap_or_default();
            let rule = f
                .rule
                .as_deref()
                .map(|r| format!("{r} · "))
                .unwrap_or_default();
            format!(
                "- {} · {rule}{} · `{}{at}` · {} ({})",
                f.key, f.severity, f.file, f.text, f.reviewer_model
            )
        })
```

`feedback`, the line becomes:

```rust
            .map(|f| {
                let at = f.line.map(|l| format!(":{l}")).unwrap_or_default();
                let rule = f.rule.map(|r| format!(" ({r})")).unwrap_or_default();
                format!("- {}{at}{rule} {}", f.file, f.text)
            })
```

`pr_body`, after the `if let Some(detected) = self.risk_of_round(task).await? ... { for (c, names) in risk_checks(...) { ... } }` block of the Checks section:

```rust
        // Spec §5: the rules the round's review was given; nothing without.
        if let Some(given) = self.rules_given(task).await? {
            b.push_str(&format!("\nRules: {given}\n"));
        }
```

`commands.rs` `log`, the findings line becomes:

```rust
            let rule = f
                .rule
                .as_deref()
                .map(|r| format!("{r} · "))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "  {} · {rule}{} · {place} · round {} · {}",
                f.key,
                f.severity,
                f.round,
                disp.get(&f.key).map(|d| d.as_str()).unwrap_or("open")
            );
```

`commands.rs` `export`, the finding object gains `"rule": f.rule,` after `"line": f.line,`.

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run --all-features -p provefab review_notes --test rules --test record --test commands`
Expected: PASS; the existing keyed and unkeyed notes tests still pass (no rule, no extra separator).

- [ ] **Step 7: All checks, then commit**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

```bash
git add crates/provefab/src crates/provefab/tests/rules.rs
git commit -m "rules: the reviewer cites rules; notes, log, export and the PR body show them

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `provefab doctor` prints one `rules <slug>` line

**Files:**
- Modify: `crates/provefab/src/commands.rs` (new `rules_checks` after `tracker_checks`; import `Forge`)
- Modify: `crates/provefab/src/app.rs:463-471` (`Cmd::Doctor`)
- Test: `crates/provefab/tests/rules.rs`

**Interfaces:**
- Consumes: `Git::{base_ref, show_file}`, `rules::{parse, PATH}`, `Forge::repo_is_public`.
- Produces: `pub async fn rules_checks(tools: &Tools, config: &Config, paths: &Paths, forge: &impl Forge) -> Vec<Check>`; one `Check { name: "rules <slug>", .. }` per repository.

- [ ] **Step 1: Write the failing test**

Append to `crates/provefab/tests/rules.rs`:

```rust
#[tokio::test]
async fn doctor_prints_the_rules_of_each_repository() {
    use provefab::commands::{Check, Tools, rules_checks};
    let f = fixture(&["true"]);
    let paths = Paths::new(&f.home);
    let tools = Tools::default();
    let hub = FakeHub::new("x");
    let line = |checks: Vec<Check>| checks.into_iter().find(|c| c.name == "rules o/r").unwrap();
    let none = line(rules_checks(&tools, &f.config, &paths, &hub).await);
    assert_eq!((none.ok, none.detail.as_str()), (true, "none"));
    commit_rules(&f, RULES);
    let three = line(rules_checks(&tools, &f.config, &paths, &hub).await);
    assert_eq!((three.ok, three.detail.as_str()), (true, "3 rules on main"));
    hub.public.store(true, std::sync::atomic::Ordering::SeqCst);
    let public = line(rules_checks(&tools, &f.config, &paths, &hub).await);
    assert_eq!(
        public.detail,
        "3 rules on main; rules are instructions to the agents: review pull requests that change .provefab/rules.md closely"
    );
    hub.public.store(false, std::sync::atomic::Ordering::SeqCst);
    commit_rules(&f, "## R0: Zero\n");
    let bad = line(rules_checks(&tools, &f.config, &paths, &hub).await);
    assert!(!bad.ok);
    assert_eq!(
        bad.detail,
        "invalid, tasks run without rules: line 1: a rule heading is `## R<number>: <summary>`, the number from 1, without leading zeros"
    );
    let mut managed = f.config.clone();
    managed.repos[0].local_path = None;
    let later = line(rules_checks(&tools, &managed, &paths, &hub).await);
    assert_eq!(
        (later.ok, later.detail.as_str()),
        (true, "none yet: the repository is cloned on its first task")
    );
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run --all-features -p provefab --test rules doctor_prints_the_rules`
Expected: compile error `cannot find function rules_checks`.

- [ ] **Step 3: Implement**

`commands.rs`: `use crate::ports::Hub;` becomes `use crate::ports::{Forge, Hub};`. After `tracker_checks`:

```rust
/// One `rules <slug>` line per repository (repository rules spec §8): the
/// rules on the base branch as last fetched (doctor never fetches), `none`,
/// or why the file is invalid. On a public repository, or one whose
/// visibility cannot be read, the line says the rules steer the agents.
pub async fn rules_checks(
    tools: &Tools,
    config: &Config,
    paths: &Paths,
    forge: &impl Forge,
) -> Vec<Check> {
    const PUBLIC_NOTE: &str = "; rules are instructions to the agents: review pull requests that change .provefab/rules.md closely";
    let git = Git {
        program: tools.git.clone(),
    };
    let mut checks = Vec::new();
    for repo in &config.repos {
        let checkout = repo.path_in(&paths.home);
        let (ok, mut detail) = if !checkout.join(".git").exists() {
            (
                true,
                "none yet: the repository is cloned on its first task".to_string(),
            )
        } else {
            let base = git.base_ref(&checkout, &repo.base).await;
            match git.show_file(&checkout, &base, crate::rules::PATH).await {
                Ok(None) => (true, "none".to_string()),
                Ok(Some(text)) => match crate::rules::parse(&text) {
                    Ok(rules) if rules.len() == 1 => (true, format!("1 rule on {}", repo.base)),
                    Ok(rules) => (true, format!("{} rules on {}", rules.len(), repo.base)),
                    Err(e) => (false, format!("invalid, tasks run without rules: {e}")),
                },
                Err(e) => (false, format!("could not read {}: {e}", crate::rules::PATH)),
            }
        };
        if forge.repo_is_public(&repo.slug).await.unwrap_or(true) {
            detail.push_str(PUBLIC_NOTE);
        }
        checks.push(Check {
            name: format!("rules {}", repo.slug),
            ok,
            detail,
        });
    }
    checks
}
```

`app.rs` `Cmd::Doctor`, after the `checks.extend(commands::tracker_checks(...).await);` statement:

```rust
            checks.extend(commands::rules_checks(&Tools::default(), &config, &paths, &gh()).await);
```

- [ ] **Step 4: Run the test**

Run: `cargo nextest run --all-features -p provefab --test rules doctor_prints_the_rules`
Expected: PASS.

- [ ] **Step 5: All checks, then commit**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

```bash
git add crates/provefab/src/commands.rs crates/provefab/src/app.rs crates/provefab/tests/rules.rs
git commit -m "doctor: one rules line per repository, with the public-repository note

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: The periodic extension point: `ReviewPolicy::periodic`, `PeriodicTools`, signals and runs

**Files:**
- Modify: `crates/provefab/src/policy.rs` (`Signal`, `SignalKind`, `PeriodicTools`, `ReviewPolicy::periodic`)
- Modify: `crates/provefab/src/rules.rs` (`Maintenance` and its `PeriodicTools` implementation, `Pipeline::maintenance`)
- Modify: `crates/provefab/src/pipeline.rs` (`PR_COMMENT_FILE` constant used by `watch_pr`; `refresh_checkout` and `base_ref` become `pub(crate)`)
- Test: `crates/provefab/tests/rules.rs`

**Interfaces:**
- Consumes: Task 2 store methods (`maintenance_runs`, `last_maintenance_run`, `record_maintenance_run`, `outputs_since`), `Store::{tasks_in, events, findings}`, `Forge::pr_status`, Task 3 `Git::show_file`.
- Produces:

```rust
// policy.rs
pub struct Signal { pub id: String, pub at: i64, pub url: String, pub kind: SignalKind }
pub enum SignalKind {
    Finding { key: String, file: String, text: String, rule: Option<u32>, disposition: Option<Disposition>, reason: Option<String> },
    ChangeRequest { text: String },
    GateFailure { commands: Vec<String> },
    Revert,
    Reopen,
    Proposal { kind: String, merged: bool, detail: Option<Value> },
}
pub trait PeriodicTools: Send + Sync {
    fn signals(&self, since: i64) -> BoxFuture<'_, Result<Vec<Signal>, String>>;
    fn highest_rule_number(&self) -> BoxFuture<'_, Result<u32, String>>;
    fn rules_at_base(&self) -> BoxFuture<'_, Result<Option<String>, String>>;
    fn last_run<'a>(&'a self, kind: &'a str) -> BoxFuture<'a, Result<Option<MaintenanceRun>, String>>;
    fn record_run<'a>(&'a self, kind: &'a str, outcome: &'a str, pr_url: Option<&'a str>, detail: Option<&'a Value>) -> BoxFuture<'a, Result<(), String>>;
    // ask_model and propose_file: Task 7
}
pub trait ReviewPolicy { /* existing */ fn periodic<'a>(&'a self, repo: &'a RepoConfig, tools: &'a dyn PeriodicTools) -> BoxFuture<'a, Result<(), String>>; /* default: Ok(()) */ }
// rules.rs
pub struct Maintenance<'a, R, O, H> { /* private */ }
impl Pipeline<R, O, H> { pub fn maintenance<'a>(&'a self, repo: &'a RepoConfig) -> Maintenance<'a, R, O, H>; }
// pipeline.rs
pub(crate) const PR_COMMENT_FILE: &str = "(pull request comment)";
```

- [ ] **Step 1: Write the failing tests**

Append to `crates/provefab/tests/rules.rs`:

```rust
use provefab::policy::{PeriodicTools, SignalKind};
use provefab::record::{Disposition, Event, GateEntry};
use provefab::stage::{Finding, Severity};
use provefab::store::{MaintenanceRun, Write};

fn run_of(kind: &str, pr_url: Option<&str>, detail: Option<Value>) -> MaintenanceRun {
    MaintenanceRun {
        id: 0,
        repo: "o/r".into(),
        kind: kind.into(),
        started_at: 1,
        finished_at: Some(2),
        model_id: None,
        cost_usd: None,
        quota_units: None,
        outcome: "proposed 1 change".into(),
        pr_url: pr_url.map(Into::into),
        detail,
    }
}

#[tokio::test]
async fn signals_hold_the_repository_record_since_a_time() {
    let f = fixture(&["make test"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    let s = &p.store;
    let pr = "https://github.com/o/r/pull/41";
    s.write_with_events(
        id,
        Write::Nothing,
        &[Event::PrOpened { url: pr.into(), head: None, base: "main".into(), pass: 1 }],
    )
    .await
    .unwrap();
    let finding = |text: &str, rule: Option<&str>| Finding {
        file: "src/a.rs".into(),
        line: Some(3),
        severity: Severity::Minor,
        text: text.into(),
        rule: rule.map(Into::into),
    };
    s.record_review(
        id,
        &json!({}),
        "std-codex",
        1,
        0,
        "approve",
        &[finding("uses anyhow", Some("R3")), finding("rename x", None), finding("typo", None)],
    )
    .await
    .unwrap();
    for (key, d, reason) in [
        ("F2", Disposition::Rejected, Some("x is the domain's word")),
        ("F3", Disposition::Accepted, None),
    ] {
        s.record_human(
            id,
            &Event::FindingDisposition {
                finding: key.into(),
                disposition: d,
                reason: reason.map(Into::into),
                login: "alice".into(),
                association: "OWNER".into(),
                comment: format!("alice@{key}"),
            },
        )
        .await
        .unwrap();
    }
    let gate = |command: &str| GateEntry {
        command: command.into(),
        exit: Some(1),
        timed_out: false,
        passed: false,
        output_ref: "/s".into(),
    };
    s.write_with_events(
        id,
        Write::Nothing,
        &[
            Event::GatesRun {
                stage: "gates".into(),
                round: 0,
                results: vec![gate("make test"), gate("grep SENTINEL_42 x")],
            },
            Event::IssueReopened { previous_pass: 1 },
            Event::PostMerge { check_id: 5, state: "revert_open".into(), failure_kind: None },
        ],
    )
    .await
    .unwrap();
    s.record_output(
        id,
        "review",
        &json!({"verdict": "changes", "findings": [{"file": "(pull request comment)", "line": null,
            "severity": "blocking", "text": "alice wrote: use the existing helper"}]}),
    )
    .await
    .unwrap();
    let refused = json!({"changes": [{"action": "add", "rule": 4, "summary": "s", "sources": []}]});
    s.record_maintenance_run(&run_of(
        "rules",
        Some("https://github.com/o/r/pull/90"),
        Some(refused.clone()),
    ))
    .await
    .unwrap();
    let closed = provefab::forge::PrStatus {
        state: provefab::forge::PrState::Closed,
        comments: vec![],
        head_sha: None,
        merge_sha: None,
        base_ref: None,
        commit_count: None,
    };
    p.hub
        .pr_statuses
        .lock()
        .unwrap()
        .insert("https://github.com/o/r/pull/90".into(), closed);
    let repo = f.config.repos[0].clone();
    let tools = p.maintenance(&repo);
    let signals = tools.signals(0).await.unwrap();
    let ids: Vec<&str> = signals.iter().map(|s| s.id.as_str()).collect();
    for want in [
        "pr#41/F1", "pr#41/F2", "pr#41/c1", "task#1/gates", "task#1/reopen-1", "task#1/revert-5",
        "pr#90/closed",
    ] {
        assert!(ids.contains(&want), "{want} missing from {ids:?}");
    }
    assert!(!ids.contains(&"pr#41/F3"), "accepted and citing no rule: {ids:?}");
    let get = |id: &str| signals.iter().find(|s| s.id == id).unwrap();
    assert_eq!(get("pr#41/F1").url, pr);
    assert!(matches!(&get("pr#41/F1").kind, SignalKind::Finding { rule: Some(3), disposition: None, .. }));
    assert!(matches!(&get("pr#41/F2").kind,
        SignalKind::Finding { disposition: Some(Disposition::Rejected), reason: Some(r), .. } if r == "x is the domain's word"));
    assert!(matches!(&get("pr#41/c1").kind,
        SignalKind::ChangeRequest { text } if text == "alice wrote: use the existing helper"));
    // Model-written commands never leave the machine; configured ones do.
    assert_eq!(
        get("task#1/gates").kind,
        SignalKind::GateFailure { commands: vec!["make test".into()] }
    );
    assert!(!format!("{signals:?}").contains("SENTINEL_42"));
    assert_eq!(
        get("pr#90/closed").kind,
        SignalKind::Proposal { kind: "rules".into(), merged: false, detail: Some(refused) }
    );
    // Later than everything: only the periodic pull request's outcome, whatever its age.
    let later = tools.signals(provefab::store::now() + 100).await.unwrap();
    assert_eq!(later.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), ["pr#90/closed"]);
}

#[tokio::test]
async fn the_highest_rule_number_counts_loaded_and_cited_rules() {
    let f = fixture(&["true"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let id = queue(&p).await;
    let repo = f.config.repos[0].clone();
    assert_eq!(p.maintenance(&repo).highest_rule_number().await.unwrap(), 0);
    p.store
        .write_with_events(
            id,
            Write::Nothing,
            &[Event::RulesLoaded { pass: 1, numbers: vec![1, 4], sha256: "s".into(), omitted: 0 }],
        )
        .await
        .unwrap();
    let cited = Finding {
        file: "a".into(),
        line: None,
        severity: Severity::Minor,
        text: "t".into(),
        rule: Some("R7".into()),
    };
    p.store
        .record_review(id, &json!({}), "m", 1, 0, "approve", &[cited])
        .await
        .unwrap();
    assert_eq!(p.maintenance(&repo).highest_rule_number().await.unwrap(), 7);
}

#[tokio::test]
async fn runs_are_recorded_and_read_back_per_kind() {
    let f = fixture(&["true"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let repo = f.config.repos[0].clone();
    let tools = p.maintenance(&repo);
    assert!(tools.last_run("rules").await.unwrap().is_none());
    let detail = json!({"highest": 3});
    tools
        .record_run("rules", "proposed 1 change", Some("https://github.com/o/r/pull/90"), Some(&detail))
        .await
        .unwrap();
    let run = tools.last_run("rules").await.unwrap().unwrap();
    assert_eq!(run.outcome, "proposed 1 change");
    assert_eq!(run.pr_url.as_deref(), Some("https://github.com/o/r/pull/90"));
    assert_eq!(run.detail, Some(detail));
    assert!(run.finished_at.is_some_and(|t| t >= run.started_at));
    assert_eq!((run.model_id, run.cost_usd, run.quota_units), (None, None, None));
    assert!(tools.last_run("other").await.unwrap().is_none());
}

#[tokio::test]
async fn rules_at_base_fetches_the_base_branch_first() {
    let f = fixture(&["true"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    let repo = f.config.repos[0].clone();
    let tools = p.maintenance(&repo);
    assert_eq!(tools.rules_at_base().await.unwrap(), None);
    // Merged elsewhere: only a fetch can see it.
    let other = f._dir.path().join("other");
    git(f._dir.path(), &["clone", "-q", f.origin.to_str().unwrap(), other.to_str().unwrap()]);
    std::fs::create_dir_all(other.join(".provefab")).unwrap();
    std::fs::write(other.join(".provefab/rules.md"), "## R1: Pushed elsewhere\n").unwrap();
    git(&other, &["add", "-f", ".provefab/rules.md"]);
    git(&other, &["commit", "-q", "-m", "rules"]);
    git(&other, &["push", "-q", "origin", "main"]);
    assert_eq!(
        tools.rules_at_base().await.unwrap().as_deref(),
        Some("## R1: Pushed elsewhere")
    );
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab --test rules signals_hold the_highest_rule runs_are_recorded rules_at_base`
Expected: compile errors (`unresolved import provefab::policy::PeriodicTools`, `no method maintenance`).

- [ ] **Step 3: The extension point in `policy.rs`**

Imports: add `use serde_json::Value;`, `use crate::record::Disposition;`, `use crate::store::MaintenanceRun;` (next to `use crate::store::TaskRow;`, as `use crate::store::{MaintenanceRun, TaskRow};`). After `PrOpened`:

```rust
/// One fact of a repository's record that periodic work reads (repository
/// rules spec §7). Its text is what people and reviewers wrote; command
/// output and model-written commands are never in it (plan decision 17).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Signal {
    /// Stable across calls: `pr#41/F2`, `pr#41/c1`, `task#12/gates`,
    /// `task#12/revert-3`, `task#12/reopen-1`, `pr#90/closed`.
    pub id: String,
    /// When it happened (unix seconds).
    pub at: i64,
    /// The pull request it is about, else the issue.
    pub url: String,
    pub kind: SignalKind,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalKind {
    /// A finding people rejected or waived, or any finding citing a rule.
    Finding {
        key: String,
        file: String,
        text: String,
        rule: Option<u32>,
        disposition: Option<Disposition>,
        reason: Option<String>,
    },
    /// What a person asked for on a pull request closed without merging.
    ChangeRequest { text: String },
    /// Configured checks (gates, risk checks) that failed in the task.
    GateFailure { commands: Vec<String> },
    /// A post-merge check opened a revert of the task's change.
    Revert,
    /// The issue was reopened after its pull request merged.
    Reopen,
    /// An earlier periodic pull request, merged or closed without merging.
    Proposal {
        kind: String,
        merged: bool,
        detail: Option<Value>,
    },
}

/// What the core lends a policy's periodic work for one repository
/// (repository rules spec §7). Errors are text fit for the log.
pub trait PeriodicTools: Send + Sync {
    /// The repository's signals since `since` (unix seconds), plus the
    /// outcome of every earlier periodic pull request, whatever its age.
    fn signals(&self, since: i64) -> BoxFuture<'_, Result<Vec<Signal>, String>>;
    /// The highest rule number the record saw: loaded by a pass or cited by a finding.
    fn highest_rule_number(&self) -> BoxFuture<'_, Result<u32, String>>;
    /// `.provefab/rules.md` on the base branch, fetched first; `None` when absent.
    fn rules_at_base(&self) -> BoxFuture<'_, Result<Option<String>, String>>;
    /// The latest run of `kind` for this repository.
    fn last_run<'a>(&'a self, kind: &'a str)
    -> BoxFuture<'a, Result<Option<MaintenanceRun>, String>>;
    /// Records a run of `kind` with the cost of the model calls made since
    /// the last record; `detail` is the policy's own JSON, never shown.
    fn record_run<'a>(
        &'a self,
        kind: &'a str,
        outcome: &'a str,
        pr_url: Option<&'a str>,
        detail: Option<&'a Value>,
    ) -> BoxFuture<'a, Result<(), String>>;
}
```

In `trait ReviewPolicy`, after `check`:

```rust
    /// Daily work for one repository (repository rules spec §7): called by
    /// the scheduler once a day, never twice at once for a repository. An
    /// `Err` is logged and recorded; the service goes on.
    fn periodic<'a>(
        &'a self,
        _repo: &'a RepoConfig,
        _tools: &'a dyn PeriodicTools,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
```

- [ ] **Step 4: `pipeline.rs` hooks**

After `const DIFF_LIMIT`:

```rust
/// The `file` of a finding made from a person's comment on a closed pull
/// request (D52): periodic work reads these as change requests.
pub(crate) const PR_COMMENT_FILE: &str = "(pull request comment)";
```

In `watch_pr`, `file: "(pull request comment)".into(),` becomes `file: PR_COMMENT_FILE.into(),`. `async fn base_ref` and `async fn refresh_checkout` become `pub(crate) async fn`.

- [ ] **Step 5: `Maintenance` in `rules.rs`**

Imports become:

```rust
use std::collections::HashSet;
use std::ops::Range;
use std::sync::Mutex;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::agents::StageRunner;
use crate::config::RepoConfig;
use crate::forge::PrState;
use crate::pipeline::{PR_COMMENT_FILE, Pipeline, PipelineError, pass_of};
use crate::policy::{BoxFuture, PeriodicTools, Signal, SignalKind};
use crate::ports::{Hub, Oracle};
use crate::post_merge::CheckState;
use crate::record::{Disposition, Event};
use crate::risk::{UNMATCHABLE, glob_match, unmatchable};
use crate::stage::ReviewOutput;
use crate::store::{MaintenanceRun, TaskRow, Write, now};
use crate::task::{Stage, TaskState};
```

In the `impl Pipeline` block:

```rust
    /// The core's periodic tools for `repo` (spec §7), for the scheduler and
    /// for Provefab Pro's commands. Its runs start now.
    pub fn maintenance<'a>(&'a self, repo: &'a RepoConfig) -> Maintenance<'a, R, O, H> {
        Maintenance {
            p: self,
            repo,
            started_at: now(),
            spent: Mutex::new(Spent::default()),
        }
    }
```

After the `impl Pipeline` block:

```rust
/// The core's `PeriodicTools` for one repository (spec §7). The cost of
/// its model calls waits here until a run is recorded (plan decision 15).
pub struct Maintenance<'a, R, O, H> {
    p: &'a Pipeline<R, O, H>,
    repo: &'a RepoConfig,
    started_at: i64,
    spent: Mutex<Spent>,
}

/// Model calls not yet written with a run.
#[derive(Debug, Default)]
struct Spent {
    model_id: Option<String>,
    cost_usd: Option<f64>,
    quota_units: Option<f64>,
}

/// `41` for `https://github.com/o/r/pull/41`.
fn pr_number(url: &str) -> Option<u64> {
    url.rsplit_once("/pull/")?
        .1
        .split(['/', '#', '?'])
        .next()?
        .parse()
        .ok()
}

/// `pr#<n>/<what>` for a pull request URL, else `task#<id>/<what>`.
fn signal_id(task: &TaskRow, url: &str, what: &str) -> String {
    match pr_number(url) {
        Some(n) => format!("pr#{n}/{what}"),
        None => format!("task#{}/{what}", task.id),
    }
}

impl<R, O, H> Maintenance<'_, R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    /// Plan decision 17.
    async fn collect(&self, since: i64) -> Result<Vec<Signal>, PipelineError> {
        let (p, slug) = (self.p, &self.repo.slug);
        let policy = crate::risk::resolve(self.repo.risk.as_ref()).unwrap_or_default();
        let configured: HashSet<&str> = self
            .repo
            .gates
            .iter()
            .chain(policy.categories.iter().flat_map(|c| &c.checks))
            .map(String::as_str)
            .collect();
        let mut out = Vec::new();
        for task in p.store.tasks_in(&TaskState::ALL).await? {
            if !task.repo.eq_ignore_ascii_case(slug) {
                continue;
            }
            let events = p.store.events(task.id).await?;
            let typed: Vec<(i64, Event)> =
                events.iter().filter_map(|e| Some((e.at, e.typed()?))).collect();
            let fallback = task.pr_url.clone().unwrap_or_else(|| task.issue_url.clone());
            let pr_of_pass = |pass: u32| {
                typed.iter().find_map(|(_, e)| match e {
                    Event::PrOpened { url, pass: n, .. } if *n == pass => Some(url.clone()),
                    _ => None,
                })
            };
            let pr_before = |at: i64| {
                typed.iter().rev().find_map(|(t, e)| match e {
                    Event::PrOpened { url, .. } if *t <= at => Some(url.clone()),
                    _ => None,
                })
            };
            for f in p.store.findings(task.id).await? {
                let reviewed = events.iter().find(|e| e.id == f.event_id).map_or(0, |e| e.at);
                let decided = typed.iter().rev().find_map(|(at, e)| match e {
                    Event::FindingDisposition {
                        finding,
                        disposition,
                        reason,
                        ..
                    } if *finding == f.key => Some((*at, *disposition, reason.clone())),
                    _ => None,
                });
                let rule = f.rule.as_deref().and_then(rule_number);
                let refused = decided
                    .as_ref()
                    .is_some_and(|(_, d, _)| matches!(d, Disposition::Rejected | Disposition::Waived));
                let at = decided.as_ref().map_or(reviewed, |(a, _, _)| (*a).max(reviewed));
                if !(refused || rule.is_some()) || at < since {
                    continue;
                }
                let url = pr_of_pass(f.pass).unwrap_or_else(|| fallback.clone());
                let (disposition, reason) = match decided {
                    Some((_, d, r)) => (Some(d), r),
                    None => (None, None),
                };
                out.push(Signal {
                    id: signal_id(&task, &url, &f.key),
                    at,
                    url,
                    kind: SignalKind::Finding {
                        key: f.key.clone(),
                        file: f.file.clone(),
                        text: f.text.clone(),
                        rule,
                        disposition,
                        reason,
                    },
                });
            }
            for (at, v) in p.store.outputs_since(task.id, "review", since).await? {
                let Ok(review) = serde_json::from_value::<ReviewOutput>(v) else {
                    continue;
                };
                let url = pr_before(at).unwrap_or_else(|| fallback.clone());
                let asked = review.findings.into_iter().filter(|f| f.file == PR_COMMENT_FILE);
                for (i, f) in asked.enumerate() {
                    out.push(Signal {
                        id: signal_id(&task, &url, &format!("c{}", i + 1)),
                        at,
                        url: url.clone(),
                        kind: SignalKind::ChangeRequest { text: f.text },
                    });
                }
            }
            let (mut failed, mut last): (Vec<String>, i64) = (Vec::new(), 0);
            for (at, e) in typed.iter().filter(|(at, _)| *at >= since) {
                match e {
                    Event::GatesRun { results, .. } => {
                        for r in results.iter().filter(|r| !r.passed) {
                            if configured.contains(r.command.as_str()) && !failed.contains(&r.command) {
                                failed.push(r.command.clone());
                                last = last.max(*at);
                            }
                        }
                    }
                    Event::PostMerge { check_id, state, .. }
                        if state == CheckState::RevertOpen.as_str() =>
                    {
                        out.push(Signal {
                            id: format!("task#{}/revert-{check_id}", task.id),
                            at: *at,
                            url: fallback.clone(),
                            kind: SignalKind::Revert,
                        });
                    }
                    Event::IssueReopened { previous_pass } => out.push(Signal {
                        id: format!("task#{}/reopen-{previous_pass}", task.id),
                        at: *at,
                        url: task.issue_url.clone(),
                        kind: SignalKind::Reopen,
                    }),
                    _ => {}
                }
            }
            if !failed.is_empty() {
                out.push(Signal {
                    id: format!("task#{}/gates", task.id),
                    at: last,
                    url: fallback.clone(),
                    kind: SignalKind::GateFailure { commands: failed },
                });
            }
        }
        // Every earlier periodic pull request that is merged or closed, whatever its age.
        let mut seen = HashSet::new();
        for run in p.store.maintenance_runs(Some(slug)).await?.into_iter().rev() {
            let Some(url) = run.pr_url.clone() else {
                continue;
            };
            if !seen.insert(url.clone()) {
                continue;
            }
            let merged = match p.hub.pr_status(slug, &url).await {
                Ok(s) => match s.state {
                    PrState::Merged => true,
                    PrState::Closed => false,
                    PrState::Open => continue,
                },
                Err(e) => {
                    eprintln!("provefab: could not read {url}: {e}");
                    continue;
                }
            };
            let what = if merged { "merged" } else { "closed" };
            out.push(Signal {
                id: format!("pr#{}/{what}", pr_number(&url).unwrap_or_default()),
                at: run.finished_at.unwrap_or(run.started_at),
                url,
                kind: SignalKind::Proposal {
                    kind: run.kind,
                    merged,
                    detail: run.detail,
                },
            });
        }
        out.sort_by(|a, b| (a.at, &a.id).cmp(&(b.at, &b.id)));
        Ok(out)
    }

    async fn highest(&self) -> Result<u32, PipelineError> {
        let mut high = 0;
        for task in self.p.store.tasks_in(&TaskState::ALL).await? {
            if !task.repo.eq_ignore_ascii_case(&self.repo.slug) {
                continue;
            }
            for e in self.p.store.events(task.id).await? {
                if e.kind == "rules_loaded"
                    && let Some(Event::RulesLoaded { numbers, .. }) = e.typed()
                {
                    high = numbers.into_iter().fold(high, u32::max);
                }
            }
            for f in self.p.store.findings(task.id).await? {
                if let Some(n) = f.rule.as_deref().and_then(rule_number) {
                    high = high.max(n);
                }
            }
        }
        Ok(high)
    }

    async fn base_rules(&self) -> Result<Option<String>, String> {
        let (p, repo) = (self.p, self.repo);
        let lock = p.repo_lock(repo);
        let _guard = lock.lock().await;
        if !p.refresh_checkout(repo).await.map_err(|e| e.to_string())? {
            return Err(format!("could not fetch {}", repo.slug));
        }
        let base = p.base_ref(repo).await;
        p.git
            .show_file(&p.checkout(repo), &base, PATH)
            .await
            .map_err(|e| e.to_string())
    }

    async fn record(
        &self,
        kind: &str,
        outcome: &str,
        pr_url: Option<&str>,
        detail: Option<&Value>,
    ) -> Result<(), String> {
        let spent = std::mem::take(
            &mut *self
                .spent
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let run = MaintenanceRun {
            id: 0,
            repo: self.repo.slug.clone(),
            kind: kind.into(),
            started_at: self.started_at,
            finished_at: Some(now()),
            model_id: spent.model_id,
            cost_usd: spent.cost_usd,
            quota_units: spent.quota_units,
            outcome: outcome.into(),
            pr_url: pr_url.map(Into::into),
            detail: detail.cloned(),
        };
        self.p
            .store
            .record_maintenance_run(&run)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

impl<R, O, H> PeriodicTools for Maintenance<'_, R, O, H>
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    fn signals(&self, since: i64) -> BoxFuture<'_, Result<Vec<Signal>, String>> {
        Box::pin(async move { self.collect(since).await.map_err(|e| e.to_string()) })
    }

    fn highest_rule_number(&self) -> BoxFuture<'_, Result<u32, String>> {
        Box::pin(async move { self.highest().await.map_err(|e| e.to_string()) })
    }

    fn rules_at_base(&self) -> BoxFuture<'_, Result<Option<String>, String>> {
        Box::pin(self.base_rules())
    }

    fn last_run<'a>(
        &'a self,
        kind: &'a str,
    ) -> BoxFuture<'a, Result<Option<MaintenanceRun>, String>> {
        Box::pin(async move {
            self.p
                .store
                .last_maintenance_run(&self.repo.slug, kind)
                .await
                .map_err(|e| e.to_string())
        })
    }

    fn record_run<'a>(
        &'a self,
        kind: &'a str,
        outcome: &'a str,
        pr_url: Option<&'a str>,
        detail: Option<&'a Value>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(self.record(kind, outcome, pr_url, detail))
    }
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run --all-features -p provefab --test rules signals_hold the_highest_rule runs_are_recorded rules_at_base`
Expected: PASS. If the compiler says a future is not `Send`, a `MutexGuard` is held across an `.await`: the only lock here is taken and released inside `record`'s first statement.

- [ ] **Step 7: All checks, then commit**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green (`Spent`'s fields are only read so far; `#[derive(Default)]` builds it, so no dead-code warning).

```bash
git add crates/provefab/src crates/provefab/tests/rules.rs
git commit -m "policy: periodic extension point with signals, rules at base and run records

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: `ask_model` and `propose_file`

**Files:**
- Modify: `crates/provefab/src/policy.rs` (two `PeriodicTools` methods)
- Modify: `crates/provefab/src/rules.rs` (`ask`, `scratch`, `propose`, `commit_and_push`, the two trait methods)
- Modify: `crates/provefab/src/pipeline.rs` (`claim` split into `claim_tier` `:1825-1855`; `pub(crate)` on `Claim` `:137`, `Slot` `:101`, `Outcome` `:146`, `exit_kind` `:187`, `watched` `:2018`, `request` `:2057`)
- Modify: `crates/provefab/src/forge.rs` (`Git::commit_file`, `Git::push_force` after `push`; `Gh::pr_edit` after `pr_create`; test)
- Modify: `crates/provefab/src/ports.rs:165-200` and `:245-268` (`Forge::pr_edit`, `impl Forge for Gh`)
- Modify: `crates/provefab/src/tracker.rs:719-745` (`impl Forge for Routed`)
- Modify: `crates/provefab/src/testkit.rs` (`FakeHub.edited`, `pr_edit`)
- Test: `crates/provefab/tests/rules.rs`

**Interfaces:**
- Consumes: Task 6 (`Maintenance`, `Spent`), `Pipeline::{repo_lock, refresh_checkout, base_ref, checkout, ordered_models}`, `Git::{rev_parse, worktree_fresh_detached, worktree_discard}`, `Forge::pr_create` (reuses the open PR of a head), `cost::stage_cost`.
- Produces:

```rust
// policy.rs, PeriodicTools
fn ask_model<'a>(&'a self, prompt: &'a str, schema: &'a Value) -> BoxFuture<'a, Result<Value, String>>;
fn propose_file<'a>(&'a self, path: &'a str, content: &'a str, title: &'a str, body: &'a str) -> BoxFuture<'a, Result<String, String>>; // the PR URL
// pipeline.rs
pub(crate) async fn claim_tier(&self, tier: Tier, avoid: &[String]) -> Result<Claim<'_>, PipelineError>;
// forge.rs
impl Git { pub async fn commit_file(&self, worktree: &Path, path: &str, message: &str) -> Result<Option<String>, ForgeError>; pub async fn push_force(&self, worktree: &Path, branch: &str) -> Result<(), ForgeError>; }
impl Gh { pub async fn pr_edit(&self, slug: &str, url: &str, title: &str, body: &str) -> Result<(), ForgeError>; }
// ports.rs, Forge
fn pr_edit(&self, slug: &str, url: &str, title: &str, body: &str) -> impl Future<Output = Result<(), ForgeError>> + Send;
// testkit.rs
pub edited: Mutex<Vec<(String, String, String)>> // FakeHub: (url, title, body) per pr_edit
```

- [ ] **Step 1: Write the failing tests**

`forge.rs` `mod tests`:

```rust
    #[tokio::test]
    async fn gh_edits_a_pull_requests_title_and_body() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), "");
        gh.pr_edit("o/r", "https://github.com/o/r/pull/9", "New title", "New body")
            .await
            .unwrap();
        let log = std::fs::read_to_string(dir.path().join("log.txt")).unwrap();
        assert_eq!(
            log,
            "ARG pr\nARG edit\nARG https://github.com/o/r/pull/9\nARG --repo\nARG o/r\nARG --title\nARG New title\nARG --body-file\nARG -\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("stdin.txt")).unwrap(),
            "New body"
        );
    }
```

Append to `crates/provefab/tests/rules.rs`:

```rust
#[tokio::test]
async fn ask_model_runs_a_standard_model_in_an_empty_directory_and_records_its_cost() {
    type Seen = std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, usize, bool, Option<Value>)>>>;
    let f = fixture(&["true"]);
    let seen: Seen = Default::default();
    let log = seen.clone();
    let script = move |_: &ModelEntry, req: &StageRequest, _: &UnboundedSender<WorkerEvent>| {
        let entries = std::fs::read_dir(&req.cwd).unwrap().count();
        let read_only = req.tools == agent_workers::ToolProfile::ReadOnly;
        log.lock()
            .unwrap()
            .push((req.cwd.clone(), entries, read_only, req.output_schema.clone()));
        let mut r = done(Some(json!({"changes": []})))?;
        r.usage = Usage {
            input_tokens: 1000,
            output_tokens: 100,
            ..Usage::default()
        };
        Some(r)
    };
    let p = pipeline(&f, Box::new(script), FakeOracle::default(), FakeHub::new("x")).await;
    let repo = f.config.repos[0].clone();
    let tools = p.maintenance(&repo);
    let schema = json!({"type": "object"});
    let answer = tools
        .ask_model("You are drafting repository rules.", &schema)
        .await
        .unwrap();
    assert_eq!(answer, json!({"changes": []}));
    let (cwd, entries, read_only, sent) = seen.lock().unwrap()[0].clone();
    assert_eq!((entries, read_only, sent), (0, true, Some(schema)));
    assert!(cwd.starts_with(f.home.join("maintenance")), "{}", cwd.display());
    assert!(!cwd.starts_with(repo.path_in(&f.home)), "no repository access");
    assert!(!cwd.exists(), "removed after the call");
    let model = p.runner.calls()[0].0.clone();
    let entry = f.config.models.iter().find(|m| m.id == model).unwrap();
    assert_eq!(entry.tier, provefab::task::Tier::Standard);
    tools
        .record_run("rules", "nothing to propose", None, None)
        .await
        .unwrap();
    let run = tools.last_run("rules").await.unwrap().unwrap();
    assert_eq!(run.model_id.as_deref(), Some(model.as_str()));
    assert!(run.quota_units.is_some_and(|q| q > 0.0), "{run:?}");
    // Written once: the next run has no model call of its own.
    tools.record_run("rules", "again", None, None).await.unwrap();
    assert_eq!(tools.last_run("rules").await.unwrap().unwrap().model_id, None);
}

#[tokio::test]
async fn propose_file_opens_then_updates_one_pull_request_and_never_merges() {
    let f = fixture(&["true"]);
    let hub = FakeHub::new("x");
    // Like GitHub: one open pull request per head branch, reused.
    *hub.revert_origin.lock().unwrap() = Some(f.origin.clone());
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), hub).await;
    let repo = f.config.repos[0].clone();
    let tools = p.maintenance(&repo);
    let main = git(&f.origin, &["rev-parse", "main"]);
    let first = tools
        .propose_file(".provefab/rules.md", "## R1: One\n\nText.\n", "Rules 1", "Body 1")
        .await
        .unwrap();
    assert_eq!(
        git(&f.origin, &["show", "provefab/rules:.provefab/rules.md"]),
        "## R1: One\n\nText."
    );
    assert_eq!(git(&f.origin, &["rev-parse", "provefab/rules^"]), main);
    let second = tools
        .propose_file(
            ".provefab/rules.md",
            "## R1: One\n\nText.\n\n## R2: Two\n\nMore.\n",
            "Rules 2",
            "Body 2",
        )
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(p.hub.prs.lock().unwrap().len(), 1);
    assert_eq!(
        p.hub.edited.lock().unwrap().last().cloned(),
        Some((second.clone(), "Rules 2".to_string(), "Body 2".to_string()))
    );
    assert!(git(&f.origin, &["show", "provefab/rules:.provefab/rules.md"]).ends_with("More."));
    assert_eq!(
        git(&f.origin, &["rev-parse", "provefab/rules^"]),
        main,
        "rebuilt from the base: one commit on top of it"
    );
    assert!(p.hub.merged.lock().unwrap().is_empty(), "never merges");
    assert!(tools.propose_file("../outside.md", "x", "t", "b").await.is_err());
    let worktrees = git(&repo.path_in(&f.home), &["worktree", "list"]);
    assert_eq!(worktrees.lines().count(), 1, "{worktrees}");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab gh_edits_a_pull --test rules ask_model propose_file`
Expected: compile errors (`no method pr_edit`, `no method ask_model`, `no field edited`).

- [ ] **Step 3: Git and GitHub**

`forge.rs`, in `impl Git` after `push`:

```rust
    /// Commits exactly `path`, forced past ignore rules (`.provefab/` is
    /// excluded in Provefab's worktrees). `None` when the file did not change.
    pub async fn commit_file(
        &self,
        worktree: &Path,
        path: &str,
        message: &str,
    ) -> Result<Option<String>, ForgeError> {
        self.git(worktree, &["add", "-f", "--", path]).await?;
        let staged = self
            .git(worktree, &["diff", "--cached", "--name-only"])
            .await?;
        if staged.trim().is_empty() {
            return Ok(None);
        }
        self.git(worktree, &["commit", "-q", "--no-verify", "-m", message])
            .await?;
        Ok(Some(self.git(worktree, &["rev-parse", "HEAD"]).await?))
    }

    /// Replaces `branch` on `origin` with the worktree's HEAD: a branch
    /// Provefab rebuilds from the base on every periodic run (rules spec §7).
    pub async fn push_force(&self, worktree: &Path, branch: &str) -> Result<(), ForgeError> {
        let refspec = format!("HEAD:refs/heads/{branch}");
        self.git(worktree, &["push", "--no-verify", "--force", "origin", &refspec])
            .await?;
        Ok(())
    }
```

In `impl Gh`, after `pr_create`:

```rust
    /// Replaces a pull request's title and body.
    pub async fn pr_edit(
        &self,
        slug: &str,
        url: &str,
        title: &str,
        body: &str,
    ) -> Result<(), ForgeError> {
        self.gh(
            &[
                "pr", "edit", url, "--repo", slug, "--title", title, "--body-file", "-",
            ],
            Some(body),
        )
        .await?;
        Ok(())
    }
```

`ports.rs`, in `trait Forge` after `pr_create`:

```rust
    /// Replaces a pull request's title and body (a periodic proposal updated in place).
    fn pr_edit(
        &self,
        slug: &str,
        url: &str,
        title: &str,
        body: &str,
    ) -> impl Future<Output = Result<(), ForgeError>> + Send;
```

and in `impl Forge for Gh`:

```rust
    async fn pr_edit(&self, slug: &str, url: &str, title: &str, body: &str) -> Result<(), ForgeError> {
        Gh::pr_edit(self, slug, url, title, body).await
    }
```

`tracker.rs`, in `impl Forge for Routed`:

```rust
    async fn pr_edit(&self, slug: &str, url: &str, title: &str, body: &str) -> Result<(), ForgeError> {
        Forge::pr_edit(&self.gh, slug, url, title, body).await
    }
```

`testkit.rs`: in `FakeHub` add, after `pr_comment_failures`:

```rust
    /// Every `pr_edit`: (url, title, body).
    pub edited: Mutex<Vec<(String, String, String)>>,
```

`edited: Mutex::new(Vec::new()),` in `FakeHub::new`, and in `impl Forge for FakeHub`:

```rust
    async fn pr_edit(&self, _: &str, url: &str, title: &str, body: &str) -> Result<(), ForgeError> {
        self.edited
            .lock()
            .unwrap()
            .push((url.into(), title.into(), body.into()));
        Ok(())
    }
```

- [ ] **Step 4: `claim_tier` and crate visibility in `pipeline.rs`**

`enum Claim`, `struct Slot`, `enum Outcome` become `pub(crate)`; `fn exit_kind`, `async fn watched` and `fn request` become `pub(crate)`. `claim` becomes:

```rust
    async fn claim(&self, task: &TaskRow, stage: Stage) -> Result<Claim<'_>, PipelineError> {
        let tier = self.tier_for(task, stage).await?;
        let avoid = match stage {
            Stage::Review => self.review_avoid(task).await?,
            _ => Vec::new(),
        };
        self.claim_tier(tier, &avoid).await
    }

    /// `claim` for a tier and the providers to avoid; also how periodic
    /// work's model calls get a model (repository rules plan decision 15).
    pub(crate) async fn claim_tier(
        &self,
        tier: Tier,
        avoid: &[String],
    ) -> Result<Claim<'_>, PipelineError> {
        let catalog = self.ordered_models();
        // Serialises this count+claim with `run_stage`'s record+mark, so the
        // count below can never be undercut by a run that is about to be
        // recorded on another worker (issue #12).
        let _gate = self.budget.lock().await;
        let done = self.store.worker_runs_since(now() - 86_400).await?;
        let mut cooldowns = self
            .cooldowns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if done + cooldowns.unrecorded() >= self.config.limits.max_stage_runs_per_day {
            return Ok(Claim::OverBudget);
        }
        let avail = cooldowns.availability();
        let Some(model) = select(tier, &catalog, &avail, SystemTime::now(), avoid).cloned() else {
            return Ok(Claim::Busy);
        };
        cooldowns.start(&model.id);
        let slot = Slot {
            cooldowns: &self.cooldowns,
            model_id: model.id.clone(),
            recorded: false,
        };
        Ok(Claim::Run(Box::new(model), slot))
    }
```

(the doc comment above `claim` stays where it is).

- [ ] **Step 5: The two tools in `rules.rs`**

Imports: add

```rust
use std::path::{Component, Path, PathBuf};
use std::sync::PoisonError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use agent_workers::{ExitReason, ToolProfile};

use crate::forge::ForgeError;
use crate::pipeline::{Claim, Outcome, exit_kind};
use crate::task::Tier;
```

(merge them with the existing `use` lines: `crate::forge::{ForgeError, PrState}`, `crate::pipeline::{Claim, Outcome, PR_COMMENT_FILE, Pipeline, PipelineError, exit_kind, pass_of}`, `crate::task::{Stage, TaskState, Tier}`). After `fn signal_id`:

```rust
/// Two optional costs added; `None` only when both are.
fn plus(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        _ => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}
```

In `impl Maintenance` (the inherent block):

```rust
    /// A fresh empty directory under `<home>/maintenance/ask`, outside every
    /// checkout, and the call's session directory (plan decision 15).
    fn scratch(&self) -> Result<(PathBuf, PathBuf), String> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "{}-{}-{}",
            self.repo.slug.replace('/', "-"),
            now(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let home = &self.p.paths.home;
        let dir = home.join("maintenance").join("ask").join(&name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok((dir, home.join("sessions").join("maintenance").join(name)))
    }

    async fn ask(&self, prompt: &str, schema: &Value) -> Result<Value, String> {
        let p = self.p;
        let claim = p
            .claim_tier(Tier::Standard, &[])
            .await
            .map_err(|e| e.to_string())?;
        let (model, _slot) = match claim {
            Claim::Run(m, s) => (*m, s),
            Claim::Busy => {
                return Err("no standard model is free (cooling down or at max_concurrency)".into());
            }
            Claim::OverBudget => return Err("the daily worker budget is spent".into()),
        };
        let (dir, session) = self.scratch()?;
        let req = p.request(
            &dir,
            prompt.to_string(),
            ToolProfile::ReadOnly,
            Some(schema.clone()),
            p.config.limits.max_turns.plan,
            session,
        );
        let started = SystemTime::now();
        let outcome = p.watched(&model, req, false).await;
        let _ = std::fs::remove_dir_all(&dir);
        let result = match outcome {
            Ok(Outcome::Finished(r)) => r,
            Ok(Outcome::Looping(_)) => return Err("the loop detector stopped the model".into()),
            Err(e) => {
                // Worker errors can quote the environment: logged, never recorded.
                eprintln!("provefab: periodic model call for {}: {e}", self.repo.slug);
                return Err("the worker failed to run".into());
            }
        };
        let cost = {
            let prices = p.prices.read().unwrap_or_else(PoisonError::into_inner);
            crate::cost::stage_cost(
                &model,
                &p.config.models,
                &result.usage,
                result.actual_model.as_deref(),
                &prices,
            )
        };
        {
            let mut spent = self.spent.lock().unwrap_or_else(PoisonError::into_inner);
            spent.model_id = Some(model.id.clone());
            spent.cost_usd = plus(spent.cost_usd, cost.usd);
            spent.quota_units = plus(spent.quota_units, cost.quota_units);
        }
        let limited = {
            let mut cooldowns = p.cooldowns.lock().unwrap_or_else(PoisonError::into_inner);
            match &result.exit {
                ExitReason::RateLimited(_) => {
                    cooldowns.strike(&model.cooldown_key(), SystemTime::now());
                    true
                }
                ExitReason::Completed => {
                    cooldowns.clear(&model.cooldown_key(), started);
                    false
                }
                _ => false,
            }
        };
        if limited {
            return Err("the model is rate limited; it cools down before the next call".into());
        }
        result.structured_output.ok_or_else(|| {
            format!("no structured answer (the model's run ended: {})", exit_kind(&result.exit))
        })
    }

    /// Plan decision 16. The shared checkout is written under the repo lock;
    /// the pull request calls run after it is released.
    async fn propose(
        &self,
        path: &str,
        content: &str,
        title: &str,
        body: &str,
    ) -> Result<String, String> {
        let (p, repo) = (self.p, self.repo);
        let rel = Path::new(path);
        let stem = rel
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|_| rel.components().all(|c| matches!(c, Component::Normal(_))))
            .ok_or_else(|| format!("{path} is not a file path inside the repository"))?;
        let branch = format!("provefab/{stem}");
        let checkout = p.checkout(repo);
        let wt = p
            .paths
            .home
            .join("maintenance")
            .join("worktrees")
            .join(format!("{}-{stem}", repo.slug.replace('/', "-")));
        let s = |e: ForgeError| e.to_string();
        {
            let lock = p.repo_lock(repo);
            let _guard = lock.lock().await;
            if !p.refresh_checkout(repo).await.map_err(s)? {
                return Err(format!("could not fetch {}", repo.slug));
            }
            let base = p
                .git
                .rev_parse(&checkout, &p.base_ref(repo).await)
                .await
                .map_err(s)?;
            p.git
                .worktree_fresh_detached(&checkout, &wt, &base)
                .await
                .map_err(s)?;
            let pushed = self
                .commit_and_push(&wt, rel, path, content, title, &branch)
                .await;
            if let Err(e) = p.git.worktree_discard(&checkout, &wt).await {
                eprintln!("provefab: could not remove {}: {e}", wt.display());
            }
            pushed?;
        }
        // `pr_create` reuses the branch's open pull request without touching
        // it: bring its title and body up to date.
        let url = p
            .hub
            .pr_create(&repo.slug, &branch, &repo.base, title, body)
            .await
            .map_err(s)?;
        p.hub
            .pr_edit(&repo.slug, &url, title, body)
            .await
            .map_err(s)?;
        Ok(url)
    }

    async fn commit_and_push(
        &self,
        wt: &Path,
        rel: &Path,
        path: &str,
        content: &str,
        message: &str,
        branch: &str,
    ) -> Result<(), String> {
        let file = wt.join(rel);
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&file, content).map_err(|e| e.to_string())?;
        let git = &self.p.git;
        if git
            .commit_file(wt, path, message)
            .await
            .map_err(|e| e.to_string())?
            .is_none()
        {
            return Err(format!("{path} on {} already reads like this", self.repo.base));
        }
        git.push_force(wt, branch).await.map_err(|e| e.to_string())
    }
```

`policy.rs`, in `trait PeriodicTools` after `rules_at_base`:

```rust
    /// One structured answer from a standard-tier model, routed like a stage
    /// (subscription first, then API keys by price), run under the guard in
    /// an empty directory with read-only tools. Its cost goes with the next
    /// recorded run.
    fn ask_model<'a>(&'a self, prompt: &'a str, schema: &'a Value)
    -> BoxFuture<'a, Result<Value, String>>;
    /// Commits `content` as `path` on the branch `provefab/<file stem>`,
    /// rebuilt from the current base, pushes it and opens its pull request or
    /// updates the open one. Never merges. Returns the pull request's URL.
    fn propose_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a str,
        title: &'a str,
        body: &'a str,
    ) -> BoxFuture<'a, Result<String, String>>;
```

`rules.rs`, in `impl PeriodicTools for Maintenance`:

```rust
    fn ask_model<'a>(
        &'a self,
        prompt: &'a str,
        schema: &'a Value,
    ) -> BoxFuture<'a, Result<Value, String>> {
        Box::pin(self.ask(prompt, schema))
    }

    fn propose_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a str,
        title: &'a str,
        body: &'a str,
    ) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(self.propose(path, content, title, body))
    }
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run --all-features -p provefab gh_edits_a_pull --test rules ask_model propose_file`
Expected: PASS. A "future cannot be sent between threads" error means a `MutexGuard` lives across an `.await`: every lock in `ask` is taken and released inside its own block.

- [ ] **Step 7: All checks, then commit**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green (the pipeline tests pin that `claim` behaves as before).

```bash
git add crates/provefab/src crates/provefab/tests/rules.rs
git commit -m "policy: periodic model calls and file proposals (pr_edit), never merging

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: The scheduler's daily call, `provefab status`, and `open_pipeline` for Pro

**Files:**
- Modify: `crates/provefab/src/scheduler.rs` (`PERIODIC`, `PeriodicClock`, `local_day`, `periodic_call`, the loop `:136-307`, tests)
- Modify: `crates/provefab/src/commands.rs:273-309` (`status`)
- Modify: `crates/provefab/src/app.rs:316-411` (`build_pipeline`, `open_pipeline`, `RunPipeline`)
- Test: `crates/provefab/tests/scheduler.rs`

**Interfaces:**
- Consumes: Task 6 (`Pipeline::maintenance`, `PeriodicTools::record_run`, `ReviewPolicy::periodic`), Task 2 (`last_maintenance_run`, `maintenance_runs`).
- Produces:

```rust
// scheduler.rs
pub const PERIODIC: &str = "periodic";
#[derive(Debug, Default)] pub struct PeriodicClock { /* private */ }
impl PeriodicClock {
    pub fn starting(&self) -> bool;
    pub fn due(&mut self, slug: &str, last_call: Option<i64>, now: i64, day: impl Fn(i64) -> i64) -> bool;
    pub fn ticked(&mut self, now: i64);
    pub fn finished(&mut self, slug: &str);
}
pub fn local_day(at: i64) -> i64;
// app.rs
pub type RunPipeline = Pipeline<AgentRunner, Option<JevOracle>, crate::tracker::Routed>;
pub async fn open_pipeline(paths: &Paths, policy: Arc<dyn ReviewPolicy>) -> anyhow::Result<RunPipeline>;
// status: "maintenance <repo> <kind>: <outcome> (<started, RFC 3339>)[  <pr url>]", the last run per repository and kind; an `ok` periodic run is not shown
```

- [ ] **Step 1: Write the failing tests**

`scheduler.rs` `mod tests` (the `use` line becomes `use super::{PeriodicClock, local_day, should_report};`):

```rust
    #[test]
    fn the_periodic_call_runs_at_startup_after_a_day_then_once_per_local_day() {
        let day = |t: i64| t.div_euclid(86_400);
        let noon = 20_000 * 86_400 + 43_200;
        let mut c = PeriodicClock::default();
        assert!(c.starting());
        assert!(c.due("a", None, noon, day), "never called");
        assert!(!c.due("b", Some(noon - 3_600), noon, day), "called an hour ago");
        assert!(c.due("c", Some(noon - 86_400), noon, day), "a day ago");
        c.ticked(noon);
        c.finished("a");
        c.finished("c");
        assert!(!c.starting());
        assert!(!c.due("a", None, noon + 5, day), "same day");
        c.ticked(noon + 5);
        let next = 20_001 * 86_400 + 5;
        assert!(c.due("a", None, next, day), "first tick after midnight");
        assert!(c.due("b", None, next, day));
        c.ticked(next);
        c.finished("b");
        // "a" is still running a day later: never twice at once.
        let after = 20_002 * 86_400 + 5;
        assert!(!c.due("a", None, after, day));
        assert!(c.due("b", None, after, day));
    }

    #[test]
    fn a_local_day_is_a_calendar_day() {
        let t = 1_790_726_400 + 43_200; // 2026-09-30T12:00:00Z
        assert_eq!(local_day(t + 86_400), local_day(t) + 1);
        assert!(local_day(t) >= 20_725 && local_day(t) <= 20_727);
    }
```

Append to `crates/provefab/tests/scheduler.rs`:

```rust
/// A policy that counts its periodic calls, uses the tools, then fails.
struct FailingPeriodic {
    calls: std::sync::atomic::AtomicU32,
}

impl provefab::policy::ReviewPolicy for FailingPeriodic {
    fn approvals_needed(&self, _: &provefab::config::RepoConfig) -> u8 {
        1
    }
    fn after_pr_opened<'a>(
        &'a self,
        _: provefab::policy::PrOpened<'a>,
    ) -> provefab::policy::BoxFuture<'a, Result<String, provefab::pipeline::PipelineError>> {
        Box::pin(async { Ok("Opened.".to_string()) })
    }
    fn periodic<'a>(
        &'a self,
        _: &'a provefab::config::RepoConfig,
        tools: &'a dyn provefab::policy::PeriodicTools,
    ) -> provefab::policy::BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tools.record_run("probe", "seen", None, None).await?;
            Err("boom".to_string())
        })
    }
}

/// Review Focus 4.
#[tokio::test]
async fn the_policy_works_once_a_day_and_a_failure_never_stops_the_queue() {
    use provefab::scheduler::PERIODIC;
    let mut f = fixture(&["test -f feature.txt"]);
    let policy = std::sync::Arc::new(FailingPeriodic {
        calls: Default::default(),
    });
    f.policy = policy.clone();
    let p = std::sync::Arc::new(
        pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await,
    );
    // The last call was 25 hours ago: due at startup.
    p.store
        .record_maintenance_run(&provefab::store::MaintenanceRun {
            id: 0,
            repo: "o/r".into(),
            kind: PERIODIC.into(),
            started_at: provefab::store::now() - 90_000,
            finished_at: None,
            model_id: None,
            cost_usd: None,
            quota_units: None,
            outcome: "ok".into(),
            pr_url: None,
            detail: None,
        })
        .await
        .unwrap();
    within(30, provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()))
        .await
        .unwrap();
    let calls = || policy.calls.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(calls(), 1);
    let id = p
        .store
        .task_by_url("https://github.com/o/r/issues/7")
        .await
        .unwrap()
        .unwrap()
        .id;
    assert_eq!(p.store.task(id).await.unwrap().unwrap().state, PrOpen);
    let last = p.store.last_maintenance_run("o/r", PERIODIC).await.unwrap().unwrap();
    assert_eq!(last.outcome, "error: boom");
    // Restarted within the day: not called again.
    within(30, provefab::scheduler::run(p.clone(), ONCE, std::future::pending::<()>()))
        .await
        .unwrap();
    assert_eq!(calls(), 1);
    let status = provefab::commands::status(&p.store).await.unwrap();
    assert!(status.contains("maintenance o/r periodic: error: boom ("), "{status}");
    assert!(status.contains("maintenance o/r probe: seen ("), "{status}");
}
```

Append to `crates/provefab/tests/rules.rs`:

```rust
#[tokio::test]
async fn status_shows_the_last_run_per_repository_and_kind() {
    let f = fixture(&["true"]);
    let p = pipeline(&f, Box::new(happy), FakeOracle::default(), FakeHub::new("x")).await;
    for run in [
        run_of("rules", None, None),
        MaintenanceRun { started_at: 5, outcome: "proposed 2 changes".into(), ..run_of("rules", Some("https://github.com/o/r/pull/90"), None) },
        MaintenanceRun { outcome: "ok".into(), ..run_of("periodic", None, None) },
    ] {
        p.store.record_maintenance_run(&run).await.unwrap();
    }
    let status = provefab::commands::status(&p.store).await.unwrap();
    assert!(
        status.contains("maintenance o/r rules: proposed 2 changes (1970-01-01T00:00:05Z)  https://github.com/o/r/pull/90\n"),
        "{status}"
    );
    assert!(!status.contains("proposed 1 change"), "only the last run: {status}");
    assert!(!status.contains("periodic"), "an ok periodic call is bookkeeping: {status}");
}
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run --all-features -p provefab the_periodic_call_runs a_local_day --test scheduler the_policy_works --test rules status_shows`
Expected: compile errors (`cannot find PeriodicClock`, `cannot find value PERIODIC`).

- [ ] **Step 3: `scheduler.rs`**

Imports: add `use crate::policy::PeriodicTools;`. After `MAX_BACKOFF`:

```rust
/// The kind of the scheduler's own maintenance row: one per periodic call
/// (repository rules plan decision 13).
pub const PERIODIC: &str = "periodic";

/// When `ReviewPolicy::periodic` runs (repository rules spec §7, plan
/// decision 14): at startup for a repository whose last call is a day old or
/// more, then on the first tick of each new local day; never while that
/// repository's previous call still runs.
#[derive(Debug, Default)]
pub struct PeriodicClock {
    prev_tick: Option<i64>,
    running: HashSet<String>,
}

impl PeriodicClock {
    /// No tick yet: `due` reads the last recorded call.
    pub fn starting(&self) -> bool {
        self.prev_tick.is_none()
    }

    /// Whether `slug`'s call is due at `now`; marks it running when it is.
    pub fn due(
        &mut self,
        slug: &str,
        last_call: Option<i64>,
        now: i64,
        day: impl Fn(i64) -> i64,
    ) -> bool {
        if self.running.contains(slug) {
            return false;
        }
        let due = match self.prev_tick {
            None => last_call.is_none_or(|at| now - at >= 86_400),
            Some(prev) => day(now) != day(prev),
        };
        if due {
            self.running.insert(slug.to_string());
        }
        due
    }

    /// Ends a tick: the next one compares its day with this one's.
    pub fn ticked(&mut self, now: i64) {
        self.prev_tick = Some(now);
    }

    pub fn finished(&mut self, slug: &str) {
        self.running.remove(slug);
    }
}

/// The calendar day of `at` (unix seconds) in the system's time zone.
pub fn local_day(at: i64) -> i64 {
    (at + utc_offset(at)).div_euclid(86_400)
}

// `time_t` and `c_long` are `i64` on 64-bit macOS and Linux.
#[allow(clippy::unnecessary_cast)]
fn utc_offset(at: i64) -> i64 {
    let t = at as libc::time_t;
    // SAFETY: `localtime_r` reads `t` and writes only into `tm`, which we
    // own; an all-zero `tm` is a valid value of this plain C struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if ok { tm.tm_gmtoff as i64 } else { 0 }
}

/// One periodic call: the policy's `Err` is logged and recorded, never fatal.
async fn periodic_call<R, O, H>(p: &Pipeline<R, O, H>, repo: &RepoConfig)
where
    R: StageRunner + Sync,
    O: Oracle + Sync,
    H: Hub + Sync,
{
    let tools = p.maintenance(repo);
    let outcome = match p.policy.periodic(repo, &tools).await {
        Ok(()) => "ok".to_string(),
        Err(e) => {
            eprintln!("provefab: periodic work for {} failed: {e}", repo.slug);
            format!("error: {e}")
        }
    };
    if let Err(e) = tools.record_run(PERIODIC, &outcome, None, None).await {
        eprintln!("provefab: could not record the periodic call for {}: {e}", repo.slug);
    }
}
```

In `run`, after the label loop and before `let workers = ...`:

```rust
    let mut clock = PeriodicClock::default();
    let mut periodic: JoinSet<()> = JoinSet::new();
    let mut periodic_of: HashMap<tokio::task::Id, String> = HashMap::new();
```

At the top of the `loop`, right after `p.refresh_prices().await;`:

```rust
        // Repository rules spec §7: the policy's daily work, beside the queue.
        while let Some(done) = periodic.try_join_next_with_id() {
            let id = match &done {
                Ok((id, ())) => *id,
                Err(e) => e.id(),
            };
            if let Some(slug) = periodic_of.remove(&id) {
                clock.finished(&slug);
            }
            if let Err(e) = done {
                eprintln!("provefab: a periodic call panicked: {}", panic_text(e));
            }
        }
        let t = crate::store::now();
        for repo in &p.config.repos {
            let last = if clock.starting() {
                match p.store.last_maintenance_run(&repo.slug, PERIODIC).await {
                    Ok(run) => run.map(|r| r.started_at),
                    Err(e) => {
                        eprintln!("provefab: could not read the last periodic call of {}: {e}", repo.slug);
                        None
                    }
                }
            } else {
                None
            };
            if clock.due(&repo.slug, last, t, local_day) {
                let (pc, rc) = (p.clone(), repo.clone());
                let handle = periodic.spawn(async move { periodic_call(&pc, &rc).await });
                periodic_of.insert(handle.id(), repo.slug.clone());
            }
        }
        clock.ticked(t);
```

Replace

```rust
        if opts.once && set.is_empty() {
            return Ok(());
        }
```

with

```rust
        if opts.once && set.is_empty() {
            // `--once` ends with the periodic calls it started.
            while periodic.join_next().await.is_some() {}
            return Ok(());
        }
```

and in the stop branch, after `set.abort_all();`, add `periodic.abort_all();`.

- [ ] **Step 4: `commands::status`**

Before `Ok(out)`:

```rust
    // Repository rules spec §7: the last maintenance run per repository and
    // kind. The scheduler's own call is shown only when it failed.
    let mut last: std::collections::BTreeMap<(String, String), crate::store::MaintenanceRun> =
        std::collections::BTreeMap::new();
    for r in store.maintenance_runs(None).await? {
        last.insert((r.repo.to_lowercase(), r.kind.clone()), r);
    }
    for r in last.into_values() {
        if r.kind == crate::scheduler::PERIODIC && r.outcome == "ok" {
            continue;
        }
        let _ = writeln!(
            out,
            "maintenance {} {}: {} ({}){}",
            r.repo,
            r.kind,
            r.outcome,
            crate::store::rfc3339(r.started_at),
            r.pr_url.map(|u| format!("  {u}")).unwrap_or_default()
        );
    }
```

- [ ] **Step 5: `app.rs`: one pipeline builder for `run` and for Pro**

Add `use crate::ports::JevOracle;` is already there; add after `fn gh()`:

```rust
/// The pipeline `provefab run` drives; Provefab Pro's commands build the same
/// one (repository rules plan decision 19).
pub type RunPipeline = Pipeline<AgentRunner, Option<JevOracle>, crate::tracker::Routed>;

/// Loads `provefab.toml` and builds the pipeline `run` uses, without the run
/// lock, for a command that works next to the service.
pub async fn open_pipeline(
    paths: &Paths,
    policy: Arc<dyn ReviewPolicy>,
) -> anyhow::Result<RunPipeline> {
    let config = load_config(paths)?;
    let oracle = oracle(&config).await?;
    let hub = routed(&config).await?;
    let store = Store::open(&paths.db()).await?;
    build_pipeline(paths, config, oracle, hub, store, policy).await
}

/// Spec §7: models whose worker is not signed in leave the catalog; then the
/// plugins, the prices and the pipeline itself.
async fn build_pipeline(
    paths: &Paths,
    mut config: Config,
    oracle: Option<JevOracle>,
    hub: crate::tracker::Routed,
    store: Store,
    policy: Arc<dyn ReviewPolicy>,
) -> anyhow::Result<RunPipeline> {
```

and move into its body, unchanged except `&paths` becoming `paths` and the final `Arc::new(Pipeline { .. })` becoming `Ok(Pipeline { .. })`, the lines of `Cmd::Run` from `let checks = commands::doctor(` through the end of the `Pipeline { ... }` literal (the doctor-based `config.models.retain`, the `config.models.is_empty()` bail, `plugins::install`, `current_exe`, `prices::load`). `Cmd::Run` then reads:

```rust
            let config = load_config(&paths)?;
            let policy = ext.policy.clone();
            for w in policy.warnings(&config) {
                eprintln!("provefab: {w}");
            }
            policy.check(&config).map_err(anyhow::Error::msg)?;
            let oracle = oracle(&config).await?;
            let hub = routed(&config).await?;
            if dry_run {
                for line in scheduler::dry_run(&config, &hub, &oracle).await? {
                    println!("{line}");
                }
                return Ok(ExitCode::SUCCESS);
            }
            let Some(_lock) = commands::lock(&paths.home.join("run.lock"))? else {
                bail!("another `provefab run` is already working on this queue");
            };
            let store = Store::open(&paths.db()).await?;
            commands::tracker_history(&store, &config).await?;
            let pipeline =
                Arc::new(build_pipeline(&paths, config, oracle, hub, store, policy).await?);
            let stop = async {
                let _ = tokio::signal::ctrl_c().await;
            };
            scheduler::run(pipeline, RunOptions { workers, once }, stop).await?;
            Ok(ExitCode::SUCCESS)
```

- [ ] **Step 6: Run the tests**

Run: `cargo nextest run --all-features -p provefab the_periodic_call_runs a_local_day --test scheduler --test rules --test app`
Expected: PASS; every existing scheduler test still returns (their policies use the default `periodic`).

- [ ] **Step 7: All checks, then commit**

Run: `cargo fmt && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green.

```bash
git add crates/provefab/src crates/provefab/tests/scheduler.rs crates/provefab/tests/rules.rs
git commit -m "scheduler: daily periodic call per repository; status shows maintenance runs

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Documentation, spec amendments, version 0.4.0, real run

**Files:**
- Create: `docs/guide/rules.md`
- Modify: `docs/guide/usage.md`, `docs/guide/configuration.md`, `docs/guide/security.md`, `README.md`, `crates/provefab/Cargo.toml:3`, `Cargo.lock`, `docs/specs/2026-10-01-repo-rules-design.md`

**Interfaces:**
- Consumes: the behaviour as built in Tasks 1-8 (read the code before writing; where this text and the code disagree, the code wins and the text is fixed).
- Produces: nothing code-facing.

- [ ] **Step 1: Write `docs/guide/rules.md`**

````markdown
# Repository rules

A repository can keep its conventions in `.provefab/rules.md`. Provefab gives them to the agents that plan, implement and review each task, and the reviewer reports a change that breaks one. Write a convention once, and the same correction is not made on every pull request.

Rules guide the agents. They do not guarantee correct code: the checks (`gates`) and your review still decide.

## The file

```markdown
# Our conventions

Anything before the first rule is an introduction for people; Provefab ignores it.

## R3: Errors in the API layer use ApiError, never anyhow
paths: src/api/**
sources: PR #41 F2 (rejected), PR #57 (closed with a change request)

Return `ApiError` from handlers; `anyhow` stays in the CLI.
```

- A rule starts with a level-2 heading `## R<number>: <summary>`. The number is a positive integer without leading zeros, unique in the file. Gaps are fine. **Never reuse a number**: earlier findings cite rules by number.
- `paths:` (optional) limits the rule to some files, as comma-separated patterns: `/`-separated from the repository root, `**` for any number of directories, `*` for any characters inside one name, everything else literal. They are the risk policy's patterns (see [Configuration](configuration.md#reposrisk-risk-aware-policy)).
- `sources:` (optional) is free text for people: where the rule comes from. Provefab never reads it.
- Then the rule's text, in Markdown, until the next `## ` heading. Use `###` headings inside a rule if you need them: after the first rule, every `## ` line starts a rule.
- Limits: 100 rules, 120 characters per summary, 2000 characters per rule text.

**An invalid file never stops a task.** A malformed heading, a number used twice, an invalid pattern, a limit exceeded or a `paths:` or `sources:` line out of place makes the whole file invalid: the task runs without rules, its log shows `rules_invalid` with the reason, and `provefab doctor` prints it. No file means no rules, and nothing changes.

## Which rules each stage gets

Provefab reads the file from the task's base commit, once per pass, never from the task's own branch: a change to the rules applies once it is merged.

- **Plan:** every rule.
- **Implementation:** rules without `paths:`, plus those matching a file the plan names.
- **Review:** rules without `paths:`, plus those matching a file the round changed.

Rules go after the issue and the diff, under the title "Repository rules (approved by the maintainers of this repository)". A prompt carries at most 12 000 characters of rules: the lowest numbers first; the rest are left out, and the prompt says how many. Keep rules short.

## The reviewer's check

The reviewer reports a change that breaks one of the rules it was given as a finding that cites it. The pull request's review notes show it as `F2 · R3 · ...`, `provefab log` and `provefab export` show the rule, and the pull request's Checks section ends with `Rules: R1, R3`, the rules the review was given. Answer such a finding like any other (`/provefab F2 rejected: ...`): your decisions are part of the record.

## Changing the rules

Edit `.provefab/rules.md` in a pull request, like any file, and merge it. Agents cannot change it: the guard refuses their writes to `.provefab/`, and Provefab never commits that directory in a task's pull request. Should a task's branch carry a change to the file anyway (pushed by other means), the round is classified in the `rules` risk category (see [Configuration](configuration.md#reposrisk-risk-aware-policy)).

Provefab Pro can propose rules from your decisions on its pull requests, at most once a week per repository, in one pull request you merge to approve or close to refuse. See its README.

## Checking

`provefab doctor` prints one `rules <owner/name>` line per repository: how many rules the base branch holds (as last fetched), `none`, or why the file is invalid.

`provefab log <id>` shows `rules_loaded` (the rule numbers, and how many the budget left out of the plan prompt) or `rules_invalid` (the reason).

## Security

Rules are instructions to the agents, placed outside the untrusted-data markers, because your maintainers merged them. They stay under the guard: a rule cannot allow a tool call the guard refuses. On a public repository, anyone can propose a change to the file, so review pull requests that change `.provefab/rules.md` closely; `provefab doctor` says so for public repositories.
````

- [ ] **Step 2: Update the other docs**

`docs/guide/usage.md`:
- In "The life of an issue", item 4 gets "The plan sees your [repository rules](rules.md), if any." and item 7 gets "The review checks the change against the rules it was given; a finding that cites one shows the rule, for example `F2 · R3`."
- Under "Reading `provefab log <id>`", add a bullet after **routes:**: "- **record:** `rules_loaded pass N: R1, R3` (the rules the pass read from its base commit, and how many the budget left out of the plan prompt) or `rules_invalid pass N: <reason>` (the task ran without rules). A finding citing a rule shows it after its key: `F2 · R3 · blocking · ...`."
- Add after "Measuring": 

```markdown
## Maintenance runs

`provefab run` gives the edition's policy a daily call per repository, the first tick after midnight local time (and at startup when the last call is a day old). The free edition does nothing then. `provefab status` ends with the last maintenance run per repository and kind, for example `maintenance acme/api rules: proposed 2 changes (2026-10-05T00:00:04Z)  <pull request>`, and with the daily call itself only when it failed.
```

`docs/guide/configuration.md`:
- In the built-in categories table, add the row `| \`rules\` | \`.provefab/rules.md\` (see [Repository rules](rules.md)) |` after `secrets-config`.
- In `[repos.risk]`, a sentence after the table: "Provefab never commits `.provefab/` in a task's pull request, so the `rules` category marks a change to the rules made by other means; disable it like any built-in."

`docs/guide/security.md`:
- In "What agents cannot do", after the guard list: "**Repository rules** (`.provefab/rules.md`, see [Repository rules](rules.md)) are instructions to the agents, read from the base branch, and stay under the guard: a rule cannot allow a tool call the guard refuses. On a public repository, review pull requests that change that file closely."
- In "Pull requests and merging": "Periodic work (Provefab Pro's rule proposals) runs one model call in an empty directory outside every checkout, with read-only tools, under the same guard. The guard filters writes and commands, not reads by absolute path, so this keeps the repository out of the call's way rather than sealing it. Its pull requests are never merged automatically."

`README.md`:
- After the risk paragraph: "A repository can keep its conventions in `.provefab/rules.md`: every stage gets them and the reviewer reports a change that breaks one (see [Repository rules](docs/guide/rules.md))."
- In the Provefab Pro sentence, after "reviewer calibration reports": ", and rule proposals drafted from your decisions".
- Documentation list, after Usage: "- [Repository rules](docs/guide/rules.md): conventions in `.provefab/rules.md`, given to every stage and checked by the reviewer."
- The install line's tag becomes `v0.4.0`.

- [ ] **Step 3: Spec amendments**

In `docs/specs/2026-10-01-repo-rules-design.md`, each marked "(amended 2026-10-01 in the plan)": §4 the `rules` stage output and `pass_rules` (plan decision 2); `rules_loaded.omitted` is the plan prompt's count (decision 4); a missing file records no event (decision 3); `Rule` has `span` (decision 7); §5 the format details of decision 6, "left out in number order" as decision 8, a `rule` must name a rule the review was given (decision 9), the `rules_given` output (decision 10); §6 the guard and `commit_all` already keep `.provefab/` out of task pull requests, so the category marks changes made by other means (decision 1); §7 `PeriodicTools` methods return `Result<_, String>` (decision 11), `highest_rule_number` and `record_run`'s `detail` with the `detail` column (decision 12), the `periodic` maintenance row (decision 13), the timing rule (decision 14), model calls and the daily budget, "no repository access" as decision 15, `propose_file`'s branch and `pr_edit` (decision 16), the signal ids and the redaction rule (decision 17), `open_pipeline` (decision 19); §8 doctor details (decision 18).

- [ ] **Step 4: Version 0.4.0**

Set `version = "0.4.0"` in `crates/provefab/Cargo.toml`, then `cargo build -p provefab` (updates `Cargo.lock`).

- [ ] **Step 5: Checks and the copy rule**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run --all-features`
Expected: green (`no_paid_code` still clean).

Run: `grep -n "—" docs/guide/*.md README.md crates/provefab/src/rules.rs crates/provefab/prompts/*.md`
Expected: no output.

Run: `grep -rn "guarantee" docs/guide/rules.md`
Expected: only the sentence "They do not guarantee correct code".

- [ ] **Step 6: Commit**

```bash
git add -A docs README.md crates/provefab/Cargo.toml Cargo.lock
git commit -m "docs: repository rules; provefab 0.4.0

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

- [ ] **Step 7: Real run before release (owner, not the implementer)**

Spec §10: on `provefab/provefab`, merge a hand-written `.provefab/rules.md` (two rules, one with `paths:`), run `provefab doctor` (the `rules provefab/provefab` line, with the public-repository note), label one issue whose change touches a scoped path, and run `provefab run --once`. Evidence to keep: the doctor line, `provefab log <id>` (`rules_loaded`), the review prompt in the session directory showing the block after the diff, and the pull request (Checks `Rules:` line, any `F<n> · R<n>` note). Then the Pro part (`provefab-pro rules propose --repo provefab/provefab`) per the Pro plan's real-run step.

---

## Self-review

**Spec coverage.** §2 decisions: file in the repo (Tasks 1, 3), Pro drafting (Pro plan), rules in plan, implementation and review with path scope and reviewer findings citing them (Tasks 3, 4), free core reading and checking (all core tasks, no Pro code), daily extension point (Tasks 6-8). §3 format, intro, numbers, limits, every validation error, invalid file runs without rules, missing file (Tasks 1, 3). §4 base commit, once per pass, `rules_loaded { pass, numbers, sha256 }`, module API (Tasks 1, 3). §5 stage selection, rendering title and placement, budget with the count, guard, review `rule` field and unknown number dropped, migration column, review notes, log, export, PR `Rules:` line (Tasks 2-4). §6 `rules` category (Task 1). §7 `periodic`, scheduler timing and concurrency, `PeriodicTools` (all six methods plus decision 12), `maintenance_runs`, status, redaction (Tasks 2, 6-8). §8 doctor line and public note, log lines (Tasks 3, 5). §9 docs (Task 9; landing and terms out of plan by the brief). §10 tests (Tasks 1-8; real run Task 9 Step 7). §11 budget: one module, one migration, no dependency (Global Constraints), 0.4.0 (Task 9).

**Placeholder scan.** No "TBD", "TODO", "similar to", or unshown code; every code step carries its code, every run step its command and expected outcome. The only moved-code step (Task 8 Step 5) names the exact lines moved and the three edits to them.

**Type consistency.** `Rule { number, summary, paths, text, span }`, `parse`, `select(rules, stage, files)`, `render(selected, budget) -> (String, usize)`, `numbers(selected, omitted)`, `rule_number` in Tasks 1, 3, 4, 6; `Event::RulesLoaded { pass, numbers, sha256, omitted }` and `RulesInvalid { pass, reason }` in Tasks 3, 6; `Finding.rule`/`FindingRow.rule: Option<String>` in Tasks 2, 4, 6; `MaintenanceRun` fields in Tasks 2, 6, 7, 8; `PeriodicTools` methods and argument orders in Tasks 6, 7, 8 and the Pro plan; `Signal { id, at, url, kind }` and `SignalKind` variants in Task 6 and the Pro plan; `PERIODIC` in Task 8; `Forge::pr_edit(slug, url, title, body)` in Task 7.

**Review Focus.** Each of the five lines has its test in the owning task (Tasks 1, 2, 3, 3, 8), named in the Review Focus section.

