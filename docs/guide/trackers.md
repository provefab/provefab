# Jira and Linear

> **Beta.** Jira and Linear support has been tested against recorded API responses, not yet against live Jira and Linear sites. If something fails, `provefab doctor` and the task's reason say what; please report it in an issue.

A repository can take its issues from a Jira Cloud project or a Linear team instead of GitHub issues. The code, the branches and the pull requests stay on GitHub, and so do the `/provefab` commands on review findings.

It works like GitHub issues: a label hands a ticket to Provefab, Provefab reports progress with labels and comments, and the pull request is opened on GitHub. **Provefab never changes a ticket's status.**

## Setup

One Jira project or one Linear team per repository. Several repositories may share a project, each with its own label: a ticket must carry only one of their labels (with both, it is queued in both repositories).

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

If a Jira or Linear repository has no credentials, `provefab run` and `provefab add` stop at start and print the `provefab login` command to run, as they do for an invalid configuration. `provefab doctor` still runs and shows a FAIL line for that repository.

## Labels

Put the repository's `label` (`provefab` by default) on a ticket. Provefab polls for open tickets of the project or team with that label (Jira: status category not Done; Linear: state not completed, canceled or duplicate), up to 1000 per poll. Then it moves the ticket between the same labels as on GitHub: `provefab:needs-info`, `provefab:in-pr`, `provefab:failed`, `provefab:merged`, and the risk labels `provefab:risk-<category>`.

- Jira labels are free text: nothing is created. A Jira label cannot contain a space, so the repository's `label` cannot either.
- Linear labels are created on the team at startup, with their colour, when none of that name exists in the team or the workspace.
- When listing Linear issues stops early, the log line says why: `linear issues: stopped after N pages (page cap)` when the page limit is reached, or `linear issues: stopped after N pages (repeated cursor)` when the server returns a cursor already seen. The line starts with `linear comments:` when it is a ticket's comments that stop early.
- Provefab adds and removes only its own labels. A label a person adds to the ticket at the same moment is kept.

## What Provefab writes

- **Comments** on the ticket, each starting with "Posted by Provefab (automated), not typed by a person." On Jira they are converted to Atlassian Document Format (paragraphs, lists, code, links, emphasis); on Linear they stay Markdown.
- **The branch** `provefab/ENG-123-<title>` and **the pull request title** `ENG-123: <title>`, so the Jira and Linear GitHub integrations link them to the ticket.
- **The pull request body** starts with `Issue: [ENG-123](<link>).` On Linear it adds `Fixes ENG-123`: if your team enabled Linear's GitHub integration, Linear may close the ticket when the pull request merges. That is Linear's setting, not Provefab's.

## Who may answer

When Provefab asks for details, the ticket's reporter (Jira) or creator (Linear) and any member of the workspace may answer in a comment.

- Provefab recognises its own comments by the "Posted by Provefab" line, not by the account that wrote them. The token belongs to a person: if you answer from that same account, you are heard.
- Comments posted by apps or integrations are ignored.
- Jira: only Atlassian accounts count as members. A Jira Service Management customer is not a member, but can still answer as the ticket's reporter.
- Linear: guests currently count as members.

A ticket's text is data for the agents, never instructions.

## Adding a ticket by hand

`provefab add <url>` accepts `https://<site>/browse/ENG-123` (Jira) and `https://linear.app/<workspace>/issue/ENG-123` (Linear).

- A Linear URL is matched by team key. A ticket from another workspace than the one your key belongs to is refused.
- When several repositories share a project or team, the ticket's label picks the repository (the match ignores case, so `Web` matches `web`). A ticket with no matching label, or with the labels of several repositories, is an error that names the candidates.

## Limits

- Jira Cloud only (not Jira Data Center), polling only (no webhooks).
- A ticket cannot choose its repository by itself: the project or team is configured per repository, and a shared project is split by label.
- Renaming a Jira project key or a Linear team key needs `project` updated in `provefab.toml`.
- Switching a repository's tracker (or its project) is supported only for a repository with no task history that could collide. `provefab run` and `provefab add` refuse to start while a task in progress (a task whose pull request is open, or merged less than 14 days ago, counts), or an update still to send, belongs to the previous tracker: restore it until those tasks finish. Afterwards, a ticket whose number matches an earlier task of that repository (ENG-12 after GitHub issue #12) is skipped with a log line naming both, and `provefab add` refuses it. Keeping both apart needs a change of the task store, planned for a later version.
- A bad token, missing access or a ticket that no longer exists stops the task (`needs_you`, with the reason in `provefab log`). Timeouts (HTTP 408), rate limits (429) and server errors (5xx) are retried like a failing `gh` call; a rate limit that asks to wait 30 seconds or less is waited out once. Other 4xx errors are permanent.
- Tokens, e-mails and keys are removed from every error message before it is shown or stored.
