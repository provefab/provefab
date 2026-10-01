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
- Then the rule's text, in Markdown, until the next `## ` heading. Use `###` headings inside a rule if you need them. After the first rule, every line that starts with `## ` is a rule heading, and a line that starts with `paths:` or `sources:` after the text has begun is an error, even inside a fenced code block: Provefab does not track code fences. Indent such a line, or rephrase it.
- Limits: 100 rules, 120 characters per summary, 2000 characters per rule text.

**An invalid file never stops a task.** A malformed heading, a number used twice, an invalid pattern, a limit exceeded or a `paths:` or `sources:` line out of place makes the whole file invalid: the task runs without rules, its log shows `rules_invalid` with the reason, and `provefab doctor` prints it. No file means no rules, and nothing changes (no message, no log line).

## Which rules each stage gets

Provefab reads the file from the task's base commit, once per pass, never from the task's own branch: a change to the rules applies once it is merged. If Provefab cannot read the file from git at that moment (a transient git failure), the pass runs without rules and its log shows `rules_invalid`; the next pass reads it again.

- **Plan:** every rule.
- **Implementation:** rules without `paths:`, plus those matching a file the plan names.
- **Review:** rules without `paths:`, plus those matching a file the round changed (both sides of a rename count).

Rules go after the issue and the diff, under the title "Repository rules (approved by the maintainers of this repository)". A prompt carries at most 12 000 characters of rules: the lowest numbers first; the first rule that does not fit and every later one are left out, and the prompt says how many. Keep rules short.

## The reviewer's check

The reviewer reports a change that breaks one of the rules it was given as a finding that cites it. A citation of a rule the review was not given is dropped; the finding stays, without the rule. The pull request's review notes show it as `F2 · R3 · ...`, `provefab log` and `provefab export` show the rule, and the pull request's Checks section ends with `Rules: R1, R3`, the rules the review was given. Answer such a finding like any other (`/provefab F2 rejected: ...`): your decisions are part of the record.

## Changing the rules

Edit `.provefab/rules.md` in a pull request, like any file, and merge it. Agents cannot change it: the guard refuses their writes to `.provefab/`, and Provefab never commits that directory in a task's pull request. Should a task's branch carry a change to the file anyway (pushed by other means), the round is classified in the `rules` risk category (see [Configuration](configuration.md#reposrisk-risk-aware-policy)).

Provefab Pro can propose rules from your team's decisions, at most once a week per repository, in one pull request you merge to approve or close to refuse. See its README.

## Checking

`provefab doctor` prints one `rules <owner/name>` line per repository: how many rules the base branch holds, `none`, or why the file is invalid. It reads the base branch as Provefab last fetched it and does not fetch, so a rules file merged a minute ago shows after the next task or poll fetches the repository.

`provefab log <id>` shows `rules_loaded` (the rule numbers, and how many the budget left out of the plan prompt) or `rules_invalid` (the reason). A pass can show the line more than once when its base is pinned again; the last one is the one in force.

Known limit: a rule that was merged and removed again before any task read it leaves no trace in a task's log.

## Security

Rules are instructions to the agents, placed outside the untrusted-data markers, because your maintainers merged them. They stay under the guard: a rule cannot allow a tool call the guard refuses. On a public repository, anyone can propose a change to the file, so review pull requests that change `.provefab/rules.md` closely; `provefab doctor` says so for public repositories.
