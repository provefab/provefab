# Issue trackers: Jira Cloud and Linear

- Date: 2026-10-01
- Status: approved design (owner, 2026-10-01); released in 0.3.0 as a beta, real run pending
- Feature: issues can come from Jira Cloud or Linear; code, pull requests and `/provefab` finding commands stay on GitHub.

## 1. Intent

Teams that track work in Jira or Linear cannot use Provefab today: it only reads labelled GitHub issues. This feature lets a repository take its issues from a Jira Cloud project or a Linear team, with the same model as GitHub: a label hands a ticket to Provefab, Provefab reports progress with labels and comments, and the pull request is opened on GitHub as before.

## 2. Owner decisions

1. Trackers in v1: Jira Cloud and Linear.
2. Trigger: a label on the ticket, as on GitHub (`label` on the repository, default `provefab`).
3. Feedback: labels and comments only, as on GitHub. Provefab never changes a ticket's status.
4. Mapping: one Jira project or Linear team per repository. Several repositories may share a project, each with its own label.
5. Edition: free, in the core.
6. Architecture: split the GitHub port into an issue-tracker port and a code-forge port (approach 1 of 3).

## 3. Ports

`crates/provefab/src/ports.rs` today has one trait, `Hub`, mixing issue and pull-request calls.

- `Tracker`: `issue`, `comments`, `comment`, `edit_labels`, `ensure_label`, `issue_open`, and `open_issues` (moved from `intake::IssueSource`, which is removed; its unused `comments` goes with it).
- `Forge`: `pr_comment`, `pr_create`, `repo_clone`, `pr_status`, `pr_merge`, `repo_is_public`.
- `Hub` stays as the combination: `pub trait Hub: Tracker + Forge {}` with a blanket implementation, so the pipeline's generic bounds and Provefab Pro keep compiling.
- Signatures keep addressing an issue by repository slug and number. With one project per repository, an adapter rebuilds the ticket key from its configuration: Jira project `ENG` and number `123` give `ENG-123`; Linear team `ENG` and number `123` give `ENG-123` (Linear's `issue(id:)` accepts the identifier).
- `Gh` implements both ports, unchanged in behaviour.
- `scheduler::run` polls through the hub, `dry_run` takes a tracker, and `Routed` lands with the adapters (amended 2026-10-01 in the plan).
- `tracker::Routed` (production) implements `Tracker` by routing on the slug to the repository's tracker (`Gh`, `Jira` or `Linear`) and `Forge` by delegating to `Gh`. `app.rs` builds it from the configuration.
- `testkit::FakeHub` implements both ports; a test can give its issue a tracker key.
- `/provefab` finding commands keep being read from pull-request comments (`Forge::pr_status`), unchanged.

## 4. Identity and storage

- Migration `0006`: `ALTER TABLE tasks ADD COLUMN issue_key TEXT` (NULL for GitHub). `issue_number` and `UNIQUE (repo, issue_number)` are unchanged: the number of `ENG-123` is unique within a repository because a repository has one project or team.
- `Issue` (forge.rs) gains `key: Option<String>`; `NewIssue` and `TaskRow` carry it.
- One display function gives an issue's reference: `#123` for GitHub, `ENG-123` otherwise. It is used by `status`, `log`, `prune`, the dry-run intake list, the prompts (`Issue {{ref}}: {{title}}`), the commit message (`Provefab task N, issue ENG-123.`) and `export` (a new `issue_key` field next to `issue`).
- Branch: `provefab/ENG-123-<slug>` (the key kept in upper case so the Jira and Linear GitHub integrations link the branch); GitHub issues keep `provefab/123-<slug>`.
- Pull request title: `ENG-123: <title>` for Jira and Linear; unchanged for GitHub.
- Pull request body first line: `Closes #123.` for GitHub; for Jira `Issue: [ENG-123](<url>).`; for Linear `Issue: [ENG-123](<url>). Fixes ENG-123` (Linear's own GitHub integration closes the ticket on merge if the team enabled it; Provefab itself never changes a status).
- `provefab add` matches a Linear URL by team key and refuses a ticket from another workspace; when several repositories share a project, the ticket's label picks the repository, and none or several is an error naming the candidates (amended 2026-10-01 during implementation).
- `provefab add <url>` also accepts `https://<site>/browse/ENG-123` (Jira) and `https://linear.app/<workspace>/issue/ENG-123[/...]` (Linear): a Jira link is matched to the repository whose tracker has that site and project; a Linear link by team key, and a ticket from another workspace than the key's is refused; when several repositories share the project, the ticket's label picks among them (see the amendment above).

## 5. Configuration and credentials

```toml
[[repos]]
slug = "acme/api"
label = "provefab"

[repos.tracker]
kind = "jira"                 # "github" (default), "jira" or "linear"
site = "acme.atlassian.net"   # Jira only
project = "ENG"               # Jira project key or Linear team key
```

- Validation at load: `kind` known; `site` required for Jira and refused for others (a host name, no scheme or path); `project` required for Jira and Linear, `[A-Z][A-Z0-9_]*`; for Jira the label (and so the derived labels) contains no whitespace. Absent `[repos.tracker]` means GitHub.
- Credentials never go in `provefab.toml`. `provefab login jira --site <site>` asks for the account e-mail and an API token; `provefab login linear` asks for a personal API key. Both are stored in the macOS Keychain (`provefab-jira`, account `<site>`; `provefab-linear`, account `provefab`), like the Anthropic key. Environment variables `PROVEFAB_JIRA_EMAIL`, `PROVEFAB_JIRA_TOKEN` and `PROVEFAB_LINEAR_KEY` override them.
- The Jira e-mail is the Keychain item's comment; `project` is refused for GitHub (amended 2026-10-01 in the plan).
- If a Jira or Linear repository has no credentials, `provefab run` and `provefab add` stop at start with the `provefab login` command to run (like an invalid configuration); `provefab doctor` still runs and shows a FAIL line per repository (amended 2026-10-01 during implementation).
- `provefab doctor` prints one `tracker <slug>` line per non-GitHub repository: kind, site or workspace, project, and whether the credentials authenticate and the project or team is readable.

## 6. Jira Cloud adapter (`jira.rs`)

- REST API v3, `https://<site>/rest/api/3`, Basic authentication with e-mail and token, `reqwest` (already a dependency).
- `open_issues`: `GET /rest/api/3/search/jql` (enhanced search, paginated with `nextPageToken`) with JQL built by Provefab, never by the user: `project = "ENG" AND labels = "<label>" AND statusCategory != Done ORDER BY created ASC`, capped at 1000 like GitHub (with the same warning). Fields: key, summary, description, reporter, labels.
- Issue: number from the key, title = summary, body = description converted from Atlassian Document Format (ADF) to text, url = `https://<site>/browse/<key>`, author = the reporter's `accountId`, labels.
- Labels: Jira labels are free text and need no creation, so `ensure_label` does nothing; `edit_labels` sends one update with `add` and `remove` operations.
- Comments: written as ADF by a small converter for what Provefab writes (paragraphs, bullet lists, inline code, code blocks, links, emphasis); read back as text. Each comment's author is the `accountId`; `created_at` is normalised to UTC RFC 3339 so comparisons stay consistent.
- `issue_open`: `statusCategory.key != "done"`.

## 7. Linear adapter (`linear.rs`)

- GraphQL API `https://api.linear.app/graphql`, personal key sent as `Authorization: <key>` (no `Bearer`, per Linear's documentation), `reqwest`.
- `open_issues`: issues of team `ENG` with the label whose state type is neither `completed`, `canceled` nor `duplicate`, paginated, capped at 1000.
- Issue: number from the identifier, title, body = description (Markdown), url, author = creator id, labels.
- Labels: Linear labels are objects; `ensure_label` creates a team label with the colour when none of that name exists; `edit_labels` resolves names to ids and sends `issueUpdate` with `addedLabelIds` and `removedLabelIds`, never replacing the whole set, so a label a person adds at the same time is kept (amended 2026-10-01 during implementation).
- Comments: Markdown both ways; author = user id; `created_at` UTC RFC 3339.
- `issue_open`: state type neither `completed`, `canceled` nor `duplicate` (amended 2026-10-01 during implementation).

## 8. Behaviour common to both trackers

- Who may answer a `needs-info` question: the ticket's author and any human member of the workspace (only members can comment on Jira and Linear). Adapters map a human commenter to the association `MEMBER`. Provefab's own comments are recognised by the bot line only (emphasis removed), not by the API account: tokens belong to a person, and an operator answering from that account must be heard (amended 2026-10-01 in the plan). Non-human comments are skipped: Jira accountType `app`, Linear comments without a user or with `user.app` (amended 2026-10-01 in the plan). Jira: only accountType `atlassian` counts as a member; a Jira Service Management `customer` does not, but can still answer as the ticket's author. Linear: guests currently count as members until the real run confirms the guest field (amended 2026-10-01 during implementation). Ticket text stays untrusted data in prompts, as today.
- Bot detection compares the bot line after removing Markdown emphasis, since the ADF round trip drops the asterisks.
- `repo_is_public` is a forge question about the GitHub repository and is unchanged.
- Errors: HTTP 408, 429 and 5xx are transient whatever the body says and are retried like a failing `gh` call; other 4xx (401, 403, 404...) are permanent (the task goes to `needs_you` with a clear reason: bad token, no access, project or ticket not found). A 429 with `Retry-After` of 30 s or less is waited out once inside the call (amended 2026-10-01 in the plan). Secrets (token, e-mail, Basic value) are redacted before any message is cut (amended 2026-10-01 during implementation). A new `ForgeError` variant carries the service, status and a message that never contains the credentials.
- Pending effects keep their stored shape (`slug`, `number`); routing by slug sends them to the right tracker.
- Reopen detection after a merge uses `Tracker::issue_open`, as today.
- Post-merge comments go to the ticket (tracker) and the pull request (forge), as today.

## 9. Documentation and landing

- Core: new `docs/guide/trackers.md` (setup for Jira and Linear, labels, what Provefab writes, who may answer, limits); `configuration.md` (`[repos.tracker]`, validation), `usage.md` (references, branch and title), `security.md` (credentials, untrusted text), `README.md`, `provefab.example.toml`, the CLI description ("Turns labelled GitHub, Jira or Linear issues into tested pull requests").
- Landing copy is done at release by the controller, not in the implementation plan (amended 2026-10-01 during implementation).
- Landing: one Free line in `Pricing.astro`, one FAQ entry, the hero or feature line that says "GitHub issues". Terms: no change (free feature).
- No em-dashes; no claim that tickets are closed or moved by Provefab.

## 10. Tests

- Ports: existing pipeline, scheduler and post-merge tests pass unchanged on `FakeHub`.
- Pipeline with a Jira-like and a Linear-like tracker (FakeHub with a key): branch, PR title and body first line, labels, a needs-info question answered by a member, reopen after merge, `status`/`log`/`export` show the key.
- Adapters against the existing `wiremock` dev-dependency (amended 2026-10-01 in the plan) (no new dependency), replaying responses shaped like the official API documentation: pagination and the 1000 cap, ADF to text and back, label add and remove, Linear label creation, comment authors and bot detection, `created_at` normalisation, 401/404 permanent, 429/5xx transient, credentials absent from every error text.
- Configuration validation errors; `provefab add` URL parsing for both trackers.
- Real run before release: one Jira ticket and one Linear ticket worked end to end on the GitHub sandbox, with the owner's test site and workspace (the owner runs `provefab login`; Provefab never handles the secrets in the conversation).

## 11. Budget and scope

- Core: exactly three new modules (`tracker.rs`, `jira.rs`, `linear.rs`), one migration (`0006`), no new dependency. A fourth module, a second migration or a new dependency is a STOP: ask the owner.
- Pro: adapts to the split if needed, no new feature.
- Version: 0.3.0.

Out of v1, each with a re-open trigger:

- Changing ticket status: a pilot asks for it (owner decision 3).
- Jira Data Center: a pilot asks for it.
- GitLab as a code forge: a pilot asks for it.
- Webhooks instead of polling: polling latency or rate limits become a problem.
- A ticket choosing its repository (label or component): a pilot with a multi-repository project asks for it.

## 12. Decisions

1-6: owner decisions in section 2.
7. Keep `(slug, number)` addressing and rebuild keys from the configuration (controller). Why: one project per repository makes the number unique; it avoids changing every port signature, the stored pending effects and the unique constraint. Cost if wrong: a project key renamed in Jira breaks addressing until the configuration is updated.
8. Keep `Hub` as `Tracker + Forge` with a blanket impl, and one `FakeHub` implementing both (controller; the design discussion first proposed two fakes). Why: the pipeline and Pro stay unchanged at the type level; the port split is still explicit. Cost if wrong: none at runtime; tests can still use separate fakes later.
9. Jira and Linear commenters are trusted as `MEMBER` (controller). Why: GitHub's association exists because public repositories accept anyone; on Jira and Linear, commenters are mostly workspace members. Amended by decision 12: Jira Service Management customers can comment but are not members (only the reporter among them is heard). Cost if wrong: a Linear guest comment could answer a question; re-open with decision 12.
10. Jira comments are written as ADF through an internal converter (controller). Why: API v3 requires ADF; v2 wiki markup is legacy. Cost if wrong: rich formatting outside the converter renders as plain text.
11. Provefab's own comments are recognised by the bot line only, not by the API account (controller, amended 2026-10-01 during implementation). Why: the token belongs to a person, whose answers must be heard. Cost if wrong: a person pasting the bot line would be ignored. Re-open: never expected.
12. Jira members are `atlassian` accounts only; Linear guests count as members for now (controller, amended 2026-10-01 during implementation). Why: JSM customers are outside the workspace; the Linear guest field is unverified. Re-open trigger: the real run.
13. Linear label edits add and remove ids, never replace the set; state type `duplicate` counts as closed (controller). Why: concurrent human edits are kept; a duplicate is not work to do.
14. HTTP 408, 429 and 5xx are transient, other 4xx permanent; a `Retry-After` of 30 s or less is waited out once; secrets are redacted before any message is cut (controller).
15. A Jira or Linear repository without credentials stops `run` and `add` at start; `doctor` still runs (controller). Why: fail early with the command to run.
16. `add` matches Linear by team key, refuses another workspace, and picks a shared project's repository by label (controller).
17. Real-run checklist (owner): Jira `statusCategory.key` is `done`; Jira comment pagination `total`; the Keychain e-mail stored as the item comment and read back, including a non-ASCII e-mail (`security` may print the comment as hex); Linear filter shapes (team key, label name, state type `nin`), `IssueLabel.team`, `IssueLabelCreateInput`, `Organization.urlKey`, label name case sensitivity, the guest field name; whether Linear keeps an HTML comment (the post-merge marker) in a comment body; a Linear mutation answering `success: false` (the effect stays pending and holds the task's later effects).
18. Switching a repository's tracker is supported only without task history that could collide (controller, final review). `UNIQUE (repo, issue_number)` stays: `run` and `add` refuse to start while a task that can still touch its ticket (any state but `failed`, `needs_you`, and `pr_open` with `pr_state` archived: an open or merged PR is still watched), or a pending effect, does not match its repository's tracker (key on GitHub, no key on Jira or Linear, key of another project); intake skips, with a log line naming both, a ticket whose number is an earlier task of the repository; `add` refuses it. A task that no longer fits its repository's tracker (its repository removed, for instance) never writes to another tracker: the write is skipped and logged. Why: no second migration in this feature. Cost if wrong: a repository that changes tracker loses the tickets whose numbers it already used. Re-open trigger: a pilot needs to switch trackers on a repository with history; the fix is the key in the unique constraint, a later migration.
19. Release 0.3.0 without the real run, with Jira and Linear marked beta in the docs, the README and the landing (owner decision 2026-10-01: "On va supposer que ça fonctionne, je ne peux pas tester actuellement", then "go"). Why: the owner cannot provide test sites now; every unverified shape fails visibly (doctor FAIL line or a task in needs_you with the reason) and GitHub repositories are unaffected. Re-open: the first live Jira or Linear run; run the decision 17 checklist then, and drop "beta" when it passes.
