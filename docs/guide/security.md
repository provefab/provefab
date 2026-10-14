# Security

Provefab runs code written by agents, in repositories whose content (issues, comments, files) may be hostile. Here is what protects it, and what stays your responsibility.

## Who can trigger Provefab

- **The label is the authorization.** Only people who can triage the repository can set `provefab`.
- **Read an issue before you label it.** The label is your approval to spend agent time on that issue's text. On a public repository, anyone can write an issue; never label one you have not read.
- **Pull request reviews** start from the `provefab:review` label, or from a `/provefab review` comment by a repository owner, organization member or collaborator. A pull request's author alone cannot ask: on a public repository, a contributor from a fork would otherwise spend your model subscriptions. Read a pull request before you ask: its title, description and diff reach a reviewer, as untrusted data. Provefab runs no gates, checks or builds on the pull request: the reviewer can only read files and use read-only commands (`cat`, `head`, `tail`, `ls`, `wc`, `grep`, `rg`, `find`, and `git show`, `diff`, `log` and a few more), runs each shell command from the worktree root (`git -C` and entering a subdirectory refused), reads nothing outside the worktree, enters no repository a pull request commits, and has no web search. Words that look like credentials are redacted from the findings Provefab posts, and Provefab never approves, requests changes on or merges a person's pull request (see [Pull request reviews](pr-review.md)).
- Only **the issue author and collaborators** can steer a task: answers to questions, comments on a closed PR, reviews. Other people's comments, and Provefab's own, are ignored.
- **On Jira and Linear**, the label is the authorization too. A question is answered by the ticket's reporter or by a workspace member: a licensed Atlassian account on Jira, any user on Linear (guests included for now). Jira Service Management customers can comment on a ticket, but they do not count as members: only the one who reported it is heard. Provefab's Jira token or Linear key stays in the macOS Keychain, in `~/.provefab/credentials.toml` on Linux (mode 600, refused when others can read it; protected by its permissions, not encrypted), or in your environment, never in `provefab.toml`, and never appears in an error, a log or a comment. Ticket text is untrusted data, like issue text.

## What agents cannot do

Every tool call of the three workers goes through the same guard, `provefab guard`, compiled into the binary. It refuses:

- any write outside the task's worktree, including through a symbolic link;
- changes to `.github/workflows/**`, `.env*`, `*.pem`, `*.key`, `.provefab/**`, and to the repository's agent config (`.claude/`, `.pi/`);
- `git` commands that write history or the remote (commit, push, tag, remote, config), `gh`, network tools (`curl`, `wget`, `nc`, `ssh`, `scp`) and package publishing commands;
- when a shell command is ambiguous (subshell, `eval`, `sh` at the end of a pipe), it refuses;
- in a review of a person's pull request, every write and every shell command but the read-only ones listed in [Pull request reviews](pr-review.md#what-a-review-does).
- in every stage, reading Provefab's own secrets: `~/.provefab/credentials.toml`, `license.key`, the database (`provefab.db`) and the workers' sign-in directories (`claude/`, `claude-api/`, `codex/`, `codex-api/`), through a read tool or a shell command, symbolic links followed (the task's worktree stays readable); and running `security` or `provefab secrets`.

The guard remains a tripwire. Native sandbox enforcement also applies to shell descendants: Codex uses named filesystem/network permissions, Claude uses its verified native sandbox, and Pi Bash and every gate use SRT. Commands can write only the worktree when the stage permits it and a private per-run temporary directory; policy, session and runtime files stay outside writable roots. Read-only stages cannot write the worktree. A required runtime or policy that cannot be verified refuses execution, with no unsandboxed fallback.

The native filesystem policy denies known Provefab credential, license and database files (including WAL, SHM and rollback journal), worker authentication directories, standard CLI/SSH/cloud authentication paths, `~/Library/Keychains` and explicitly configured `deny_read` paths. Claude and Pi expose only sandboxed Bash for file operations. Host-native file/search tools are unavailable and refused by the native-policy guard: a hook path check cannot prevent a concurrent symlink swap before an unsandboxed file operation. Read-only stages have no worktree write permission; PR reviews keep the read-only shell allowlist. Codex patches check both read and write denials and use the native filesystem sandbox context. Codex image viewing stays disabled pending operation-boundary verification. This is an explicit file deny list, not isolation from every possible host secret: arbitrary credentials stored elsewhere, atomic `.credentials.toml.*` temporary files and host credential APIs such as macOS Keychain need separate consideration. Shell environments inherit only operational values. No claim of complete host-secret isolation is made.

Validation is explicit: `[repos.sandbox].protected_paths` protects declared files and directories from commands and Codex patches, including parent replacement. Claude/Pi edits use the sandboxed shell. Empty declarations make no immutable-validation guarantee. Include every dependency that defines success. Mutable aliases are refused before execution; declare stable paths instead. See [Configuration](configuration.md#repossandbox-native-execution-policy).

Agents do not have push credentials either:
- their environment is emptied of them;
- git is configured not to push;
- a `pre-push` hook refuses every push.

The checks (`gates`) always run through SRT, including reproduction, risk and post-merge/revert checks. Network and local binding are denied by default. The configured `allow_local_network = true` request currently fails closed on every platform: the tested runtime's binding option can allow non-loopback listeners, while loopback-only enforcement is unverified.

**Repository rules** (`.provefab/rules.md`, see [Repository rules](rules.md)) are instructions to the agents, read from the base branch, and stay under the guard: a rule cannot allow a tool call the guard refuses. On a public repository, review pull requests that change that file closely.

**Only Provefab commits and pushes**, with your permissions. Its own git calls never run the repository's hooks: an agent could have written them.

Claude Code and Codex run **unmodified** in dedicated profiles. Codex never trusts the repository: its project config is ignored, and any other enabled/trusted effective hook is refused. Host Codex/git preflights use pinned executables and a trusted absolute PATH before entering a worktree. Dedicated Codex configuration accepts only basic model/auth settings and Provefab trust state. Nonempty MCP servers, executable helpers such as notify, custom providers and other unsupported settings refuse before app-server or worker startup. Plugins/apps/browser/computer-use/multi-agent/code-mode/image routes and shell snapshots are pinned off; Provefab hooks stay enabled. No user configuration is deleted automatically.

Claude support is conservative: exactly 2.1.296, a personal first-party claude.ai Pro/Max login, empty inherited user settings and no managed/cache/state policy sources. API-key helpers, Team/Enterprise and unknown modes refuse before launch. A profile with `~/.claude/state` is currently unsupported. No user state or authentication is removed automatically.

**API keys.** Codex keeps its API-key login in `~/.provefab/codex-api`. Claude API-key settings may still be configured by login commands, but this native adapter refuses their executable helper before a stage. Inherited credentials/provider overrides are removed from model worker environments, and shell children receive only operational values. The main model CLI may read its own login files; sandboxed shell descendants may not.

## What Jev can and cannot do

Jev classifies issues and routes stages. It can stop an agent going in circles, triage a failure and judge an answer. It **never approves anything**. Checks decide outcomes, but their reliability depends on declared validation protections and your review.

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
- Do not set the `provefab` label on a repository whose content you do not control. The native policy is not a VM or container boundary and does not isolate every host resource.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository (the **Security** tab, "Report a vulnerability"). Please do not open a public issue for a security problem.
