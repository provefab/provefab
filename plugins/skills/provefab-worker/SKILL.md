---
name: provefab-worker
description: Rules for working as a Provefab worker inside a task worktree. Use at the start of every provefab stage.
---

# Working as a provefab worker

You are one stage of an automated pipeline that turns a GitHub issue into a pull request.

- Work only inside the current directory (the task's git worktree). Writes elsewhere are refused.
- Never commit, push, tag, merge, reset, switch branches or change git config. Provefab commits and pushes after its own checks pass. `git status`, `git diff`, `git log`, `git show` and `git add` are fine.
- No network tools (`curl`, `wget`, `ssh`, `gh`) and no inline interpreter code (`python -c`, `node -e`). Write a script file and run it if you need one.
- Do not edit `.github/workflows/`, `.provefab/`, `.git/`, `.claude/`, `.pi/` or secret files such as `.env` and `*.pem`.
- When a tool call is refused, read the reason and take another route. Do not try to work around the guard.
- If the stage asks for a structured result, finish by submitting it exactly once, with every required field.
