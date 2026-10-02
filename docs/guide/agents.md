# Set up Provefab with a coding agent

This page is for a coding agent, such as Claude Code or Codex, that a person asked to set up Provefab on the repository they are in. The agent writes and checks Provefab's configuration with Provefab commands that need no editor (`provefab init`, `provefab repos add`, `provefab doctor --json`). The person keeps the installation, the sign-ins, the keys and the service, and runs those commands themselves.

## Rules

- Never handle a secret: do not ask for, type, read, print or store a token, an API key or a password.
- Never install Provefab, a worker CLI or the Provefab service. Never sign anything in. Never put a label on an issue. Never run `provefab run`, `provefab login`, `provefab service`, `gh auth login` or `security`.
- Run only the commands on this page. When one fails, report its error to the person instead of working around it.
- Change `provefab.toml` only through `provefab init` and `provefab repos add`, unless the person asks you to edit it.

## Steps

`provefab --version`, `provefab init` and `provefab doctor --json` run from any directory. Only step 3 needs the person's clone: run it from inside that clone.

`provefab init` and `provefab repos add` end with a line starting with `next:`, which names the next command. It is information only: the steps below already cover it.

1. **Check that Provefab is installed:** run `provefab --version`.
   - It prints `provefab <version>`: go to step 2.
   - The command is not found: ask the person to run this line in their own terminal, and stop.

     ```bash
     curl -fsSL https://provefab.com/install.sh | sh
     ```

2. **Write the configuration:** run `provefab init`.
   - Exit code 0: the file is written (`~/.provefab/provefab.toml`, or `provefab.toml` in `$PROVEFAB_HOME` when that variable is set). The output names the file and the models of the worker CLIs found on `PATH`. Keep both for the summary.
   - Exit code 3: a configuration already exists. Keep it: never delete or replace it. Go to step 3.
   - Exit code 4: no worker CLI is on `PATH`. Ask the person to install Claude Code (`claude`) or the Codex CLI (`codex`), and stop.
   - Exit code 1: report the error, and stop.

3. **Add the repository the person is in.**
   1. In the clone, run `gh repo view --json nameWithOwner -q .nameWithOwner`. It prints `<owner/name>` and no URL (do not print the remote's URL yourself: it can hold a token). When it fails because the clone is not on GitHub (the error says that none of the git remotes point to a known GitHub host), tell the person that Provefab works with GitHub repositories, and go to step 4. When it fails because `gh` is not signed in, ask the person to run `gh auth login` in their own terminal, then run this step again. On any other failure, report its error and go to step 4.
   2. Run, with that `<owner/name>`:

      ```bash
      provefab repos add <owner/name> --path "$(git rev-parse --show-toplevel)"
      ```

   - Exit code 0: one `[[repos]]` block was added at the end of the file, with the repository's default branch as `base` and the checks detected from the files at its root as `gates`. The output names both. Keep them for the summary. It also says that Provefab keeps its own clone in its directory (`set local_path by hand to use yours`): Provefab works in that clone, not in the person's, which `--path` only read. Tell the person, and that they can set `local_path` in the block by hand to use theirs. When the output has a line starting with `note:`, show it to the person, and change `gates` only if they ask you to.
   - Exit code 3: the repository is already configured. Go to step 4.
   - Exit code 4: the repository was not recognised: no Rust, Node, Python or Go project at its root, several of them, or a `package.json` without a `lint`, `typecheck` or `test` script. Report the error, tell the person that its `[[repos]]` block is written by hand (see the [configuration guide](configuration.md)), and go to step 4.
   - Exit code 2: report the error. It names the cause: `<owner/name>` is not in that form; there is no configuration yet (go back to step 2); the directory is not a git clone; its `origin` is not that repository (the error says `its origin is not github.com/<owner/name>`): in a fork, `gh repo view` can name the upstream repository instead of the fork. Then take `<owner/name>` from `origin` with `git remote get-url origin | sed -E 's#^.*github\.com[:/]##; s#\.git$##'`, which prints only `<owner/name>` and never the rest of the URL, or ask the person which repository it is, and run step 3.2 again; the default branch or one of its files cannot be read from the clone (ask the person to run `git fetch origin` in the clone, then run this step again).
   - Exit code 5: `gh` or the network failed (`gh` is used when the clone does not say which branch is the default). Report the error. When it says to check `gh auth status`, ask the person to run `gh auth login` in their own terminal, then run this step again.
   - Exit code 1: the file does not load, or cannot be read or written. Report the error, and go to step 4.

4. **Check everything:** run `provefab doctor --json`. It prints one JSON object per line: `name`, `ok`, `detail`, and `fix` when a command fixes the problem.
   - For the lines with `"ok": false` and a `fix`, show the person each distinct `fix` command once, in the order of the lines, to run in their own terminal. Several lines can share one `fix`: `provefab login codex` signs Codex in and also sets up the `codex guard hook` (the hook Provefab installs in its own Codex directory). Do not run a `fix` yourself: these commands sign in or store a key. The one exception is `provefab init`, the `fix` of a missing configuration: run step 2.
   - For each line with `"ok": false` and no `fix`, show the person its `name` and `detail`. A missing tool (`git`, `gh`, `claude`, `codex`) is theirs to install.
   - Lines with `"ok": true` need nothing: they report what was found (tools and versions, gates, rules, prices, the repository's risk policy, and so on).
   - Show the person the `detail` of every line whose `detail` contains the word "warning" in any case, on `"ok": true` lines too, and of every line named `warning`. They need no command, and they are not failures.
   - The `risk <owner/name>` line can end with `warning: risky changes implemented on <providers> keep a standard reviewer`. It means that a risky change (to CI, dependencies, migrations and so on) is reviewed by the usual model from another provider, because the catalog has no `frontier` model from another provider. Tell the person, as information: if they want a stronger reviewer for those changes, they can add such a model to the catalog in `provefab.toml` (see the catalog and `[repos.risk]` sections of the [configuration guide](configuration.md)).
   - The `codex` lines appear only when the catalog has a Codex model: `provefab init` adds one when `codex` is on `PATH`. A person who does not want Codex can delete that model's `[[models]]` block (the one with `worker = "codex"`) from `provefab.toml` by hand, or ask you to, then run `provefab doctor --json` again.
   - Jev is the service Provefab asks to rate each issue, to choose how strong a model to use, to ask a question when an issue is unclear, and to stop an agent that goes in circles; it needs a TypeSafe key. With a key, the line is named `jev`: `"ok": true` when Jev answered, `"ok": false` with the reason in `detail` when it did not (show it to the person). Without a key, the line is named `jev key` with `"ok": false`: it is optional, Provefab then runs every task on the standard models, and the exit code ignores that line.
   - Exit code 0: every other check passed. Exit code 1: at least one failed.
   - When the person says they ran the commands, run `provefab doctor --json` again, and repeat this step. Stop when the exit code is 0, or when the person does not want to go on.

5. **End with a short summary for the person:**
   - what is configured: the file's path, the models, the repository with its `base` and `gates` (or why it was not added), and that Provefab keeps its own clone;
   - the commands left for them, to run in their own terminal: each distinct `fix` that still fails, `provefab run --dry-run` to preview how the open issues would be handled, `provefab service install --workers 1` to start the service, and the `provefab` label on an issue when they want Provefab to work on it;
   - when `doctor` still fails, what that means: at startup, Provefab drops each model whose worker (`claude`, `codex`) fails its `doctor` lines, and stops when no model is left; Provefab reads issues and opens pull requests through `gh`, so the `gh login` line has to pass first.

## Exit codes

`provefab init`, `provefab repos add` and `provefab doctor` exit with:

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | a `doctor` check failed (the Jev key is optional and never fails it), or another error (the message says which) |
| 2 | usage error, or `repos add` before `provefab init` |
| 3 | the configuration or the repository already exists |
| 4 | stack or worker not recognised |
| 5 | `gh` or network error |

A wrong argument or option prints the command's usage, starting with `error:`, and exits 2. Every other error is one line on stderr starting with `provefab:`.
