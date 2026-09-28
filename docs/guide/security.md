# Security

Provefab runs code written by agents, in repositories whose content (issues, comments, files) may be hostile. Here is what protects it, and what stays your responsibility.

## Who can trigger Provefab

- **The label is the authorization.** Only people who can triage the repository can set `provefab`.
- **Read an issue before you label it.** The label is your approval to spend agent time on that issue's text. On a public repository, anyone can write an issue; never label one you have not read.
- Only **the issue author and collaborators** can steer a task: answers to questions, comments on a closed PR, reviews. Other people's comments, and Provefab's own, are ignored.

## What agents cannot do

Every tool call of the three workers goes through the same guard, `provefab guard`, compiled into the binary. It refuses:

- any write outside the task's worktree, including through a symbolic link;
- changes to `.github/workflows/**`, `.env*`, `*.pem`, `*.key`, `.provefab/**`, and to the repository's agent config (`.claude/`, `.pi/`);
- `git` commands that write history or the remote (commit, push, tag, remote, config), `gh`, network tools (`curl`, `wget`, `nc`, `ssh`, `scp`) and package publishing commands;
- when a shell command is ambiguous (subshell, `eval`, `sh` at the end of a pipe), it refuses.

Agents do not have push credentials either:
- their environment is emptied of them;
- git is configured not to push;
- a `pre-push` hook refuses every push.

The checks (`gates`) run with the same protections.

**Only Provefab commits and pushes**, with your permissions. Its own git calls never run the repository's hooks: an agent could have written them.

Claude Code and Codex run **unmodified**, with your own login, in separate config directories. Codex never trusts the repository: its project config is ignored.

## What Jev can and cannot do

Jev classifies issues and routes stages. It can stop an agent going in circles, triage a failure and judge an answer. It **never approves anything**. Malicious text in an issue can at worst waste a pass; it cannot get code past the checks.

## Pull requests and merging

This binary never merges. Every pull request waits for a person, and its text lists the checks that ran and any test the change deletes or disables.

Provefab Pro adds guarded auto-merge: a second reviewer on a different model, then conditions that all fail closed (checks passed, reproduction really failed for a bugfix, no test weakened, a size cap, an unchanged base, and a merge pinned to the checked commit). Keep auto-merge off on repositories where strangers write the issues: a second model is not a defence against a crafted issue.

**Limits to know:**
- An AI reviewer can be wrong. Keep your own review for anything that matters.
- Provefab does not see your GitHub CI. Require status checks on the base branch if the repository matters.

## Your responsibility

- Review the pull requests.
- Keep `gates` strong: Provefab cannot be more reliable than your tests.
- Do not set the `provefab` label on a repository whose content you do not control. Container isolation does not exist yet.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting on this repository (the **Security** tab, "Report a vulnerability"). Please do not open a public issue for a security problem.
