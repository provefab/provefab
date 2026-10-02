# Set up Provefab with a coding agent

This page is for a coding agent, such as Claude Code or Codex, that a person asked to set up Provefab on the repository they are in. The agent writes and checks Provefab's configuration with three commands that need no editor. The person keeps the installation, the sign-ins, the keys and the service, and runs those commands themselves.

## Rules

- Never handle a secret: do not ask for, type, read, print or store a token, an API key or a password.
- Never install Provefab, a worker CLI or the Provefab service. Never sign anything in. Never put a label on an issue. Never run `provefab run`, `provefab login`, `provefab service`, `gh auth login` or `security`.
- Run only the commands on this page. When one fails, report its error to the person instead of working around it.
- Change `provefab.toml` only through `provefab init` and `provefab repos add`, unless the person asks you to edit it.

## Steps

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
   1. Run `git remote get-url origin`. Take `<owner/name>` from it: the two parts after `github.com/` or `github.com:`, without a trailing `.git`. The URL can hold a token before an `@`: never repeat the URL, only `<owner/name>`. When the URL is not on `github.com`, tell the person that Provefab works with GitHub repositories, and go to step 4.
   2. Run, with that `<owner/name>`:

      ```bash
      provefab repos add <owner/name> --path "$(git rev-parse --show-toplevel)"
      ```

   - Exit code 0: one `[[repos]]` block was added at the end of the file, with the repository's default branch as `base` and the checks detected from the files at its root as `gates`. The output names both. Keep them for the summary. When the output has a line starting with `note:`, show it to the person, and change `gates` only if they ask you to.
   - Exit code 3: the repository is already configured. Go to step 4.
   - Exit code 4: the repository was not recognised: no Rust, Node, Python or Go project at its root, several of them, or a `package.json` without a `lint`, `typecheck` or `test` script. Report the error, tell the person that its `[[repos]]` block is written by hand (see the [configuration guide](configuration.md)), and go to step 4.
   - Exit code 2: report the error. It names the cause: `<owner/name>` is not in that form; there is no configuration yet (go back to step 2); the directory is not a git clone, or its `origin` is not that repository (check `<owner/name>`); the default branch or one of its files cannot be read from the clone (ask the person to run `git fetch origin` in the clone, then run this step again).
   - Exit code 5: `gh` or the network failed (`gh` is used when the clone does not say which branch is the default). Report the error. When it says to check `gh auth status`, ask the person to run `gh auth login` in their own terminal, then run this step again.
   - Exit code 1: the file does not load, or cannot be read or written. Report the error, and go to step 4.

4. **Check everything:** run `provefab doctor --json`. It prints one JSON object per line: `name`, `ok`, `detail`, and `fix` when a command fixes the problem.
   - For each line with `"ok": false` and a `fix`, show the person the `fix` command to run in their own terminal. Do not run it yourself: these commands sign in or store a key. The one exception is `provefab init`, the `fix` of a missing configuration: run step 2.
   - For each line with `"ok": false` and no `fix`, show the person its `name` and `detail`. A missing tool (`git`, `gh`, `claude`, `codex`) is theirs to install.
   - Lines named `warning` have `"ok": true`: show their `detail`, they need no command.
   - The `jev key` line is optional: without a TypeSafe key, Provefab runs with cautious defaults, and the exit code ignores that line.
   - Exit code 0: every other check passed. Exit code 1: at least one failed.
   - When the person says they ran the commands, run `provefab doctor --json` again, and repeat this step. Stop when the exit code is 0, or when the person does not want to go on.

5. **End with a short summary for the person:**
   - what is configured: the file's path, the models, the repository with its `base` and `gates` (or why it was not added);
   - the commands left for them, to run in their own terminal: each `fix` that still fails, `provefab run --dry-run` to preview how the open issues would be handled, `provefab service install --workers 1` to start the service, and the `provefab` label on an issue when they want Provefab to work on it.

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
