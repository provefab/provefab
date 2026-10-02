# Security

Provefab runs code written by agents, in repositories whose content (issues, comments, files) may be hostile. Here is what protects it, and what stays your responsibility.

## Who can trigger Provefab

- **The label is the authorization.** Only people who can triage the repository can set `provefab`.
- **Read an issue before you label it.** The label is your approval to spend agent time on that issue's text. On a public repository, anyone can write an issue; never label one you have not read.
- **Pull request reviews** start from the `provefab:review` label, or from a `/provefab review` comment by a repository owner, organization member or collaborator. A pull request's author alone cannot ask: on a public repository, a contributor from a fork would otherwise spend your model subscriptions. Read a pull request before you ask: its title, description and diff reach a reviewer, as untrusted data. Provefab runs no gates, checks or builds on the pull request: the reviewer can only read files and use read-only commands (`cat`, `head`, `tail`, `ls`, `wc`, `grep`, `rg`, `find`, and `git show`, `diff`, `log` and a few more), and reads nothing outside the review's worktree, with no web search. Words that look like credentials are redacted from the findings Provefab posts, and Provefab never approves, requests changes on or merges a person's pull request (see [Pull request reviews](pr-review.md)).
- Only **the issue author and collaborators** can steer a task: answers to questions, comments on a closed PR, reviews. Other people's comments, and Provefab's own, are ignored.
- **On Jira and Linear**, the label is the authorization too. A question is answered by the ticket's reporter or by a workspace member: a licensed Atlassian account on Jira, any user on Linear (guests included for now). Jira Service Management customers can comment on a ticket, but they do not count as members: only the one who reported it is heard. Provefab's Jira token or Linear key stays in the Keychain or your environment, never in `provefab.toml`, and never appears in an error, a log or a comment. Ticket text is untrusted data, like issue text.

## What agents cannot do

Every tool call of the three workers goes through the same guard, `provefab guard`, compiled into the binary. It refuses:

- any write outside the task's worktree, including through a symbolic link;
- changes to `.github/workflows/**`, `.env*`, `*.pem`, `*.key`, `.provefab/**`, and to the repository's agent config (`.claude/`, `.pi/`);
- `git` commands that write history or the remote (commit, push, tag, remote, config), `gh`, network tools (`curl`, `wget`, `nc`, `ssh`, `scp`) and package publishing commands;
- when a shell command is ambiguous (subshell, `eval`, `sh` at the end of a pipe), it refuses;
- in a review of a person's pull request, every write and every shell command but the read-only ones listed in [Pull request reviews](pr-review.md#what-a-review-does).

Agents do not have push credentials either:
- their environment is emptied of them;
- git is configured not to push;
- a `pre-push` hook refuses every push.

The checks (`gates`) run with the same protections.

**Repository rules** (`.provefab/rules.md`, see [Repository rules](rules.md)) are instructions to the agents, read from the base branch, and stay under the guard: a rule cannot allow a tool call the guard refuses. On a public repository, review pull requests that change that file closely.

**Only Provefab commits and pushes**, with your permissions. Its own git calls never run the repository's hooks: an agent could have written them.

Claude Code and Codex run **unmodified**, with your own plan login or your own API key, in separate config directories per sign-in mode. Codex never trusts the repository: its project config is ignored.

**API keys.** The Anthropic key lives in the macOS Keychain (`provefab-anthropic`); Claude Code fetches it through `apiKeyHelper`, so it is never placed in the agent's environment. Codex keeps its API-key login in `~/.provefab/codex-api`. Provefab removes inherited key and provider variables from the workers (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`, the Bedrock and Vertex switches, and every `OPENAI_*` and `CODEX_*` except `CODEX_HOME`), so a key in your shell never leaks into a stage by accident.

## What Jev can and cannot do

Jev classifies issues and routes stages. It can stop an agent going in circles, triage a failure and judge an answer. It **never approves anything**. Malicious text in an issue can at worst waste a pass; it cannot get code past the checks.

## Pull requests and merging

This binary never merges. Every pull request waits for a person, and its text lists the checks that ran and any test the change deletes or disables.

Periodic work (Provefab Pro's rule proposals) runs one model call in an empty directory outside every checkout, with no tools: the guard refuses every tool call (`provefab guard --no-tools`), so the model gets only its prompt. That prompt can carry what people wrote on the repository, which is why it gets no tools. What goes to the model provider is the comment and review text, with credential-looking strings removed, and never command output. Its pull requests are never merged automatically. Provefab pushes a proposal branch only while the branch is as Provefab last left it (a leased push): if a person commits to it, updates pause while the branch holds that commit, until the branch is deleted (GitHub can delete it on merge) or the commit is merged into the base, so a maintainer's edit is never overwritten. Provefab recognises its own proposal commit by its trailers (`Provefab-Proposal`, `Provefab-Content-Sha256`), so a push it made but did not record (an interrupted run) does not pause updates. A proposal is built from the rules file as Provefab read it; if that file changes on the base branch during the model call, Provefab pushes nothing.

Provefab Pro adds merge policies. Ready to merge is the default; auto-merge is opt-in, needs a second reviewer on a different model, and conditions that all fail closed: checks passed, reproduction really failed for a bugfix, no test weakened, a test added or changed, no excluded path touched (`.github/**` by default), no detected risk category unless `allow_risk` lists it (never `unknown`), a size cap, an unchanged base, and a merge pinned to the checked commit. **Public repositories are never auto-merged by default**, since anyone can write their issues; a second model is not a defence against a crafted issue.

**Limits to know:**
- An AI reviewer can be wrong. Keep your own review for anything that matters.
- Provefab does not see your GitHub CI. Require status checks on the base branch if the repository matters.

## Your responsibility

- Review the pull requests.
- Keep `gates` strong: Provefab cannot be more reliable than your tests.
- Do not set the `provefab` label on a repository whose content you do not control. Container isolation does not exist yet.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository (the **Security** tab, "Report a vulnerability"). Please do not open a public issue for a security problem.
