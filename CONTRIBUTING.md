# Contributing

Thank you for your interest in Provefab.

- **Issues are welcome**: bug reports, questions and ideas. For a bug, include what happened, what you expected, and `provefab doctor` output with secrets removed.
- **Pull requests are not accepted yet.** The contribution terms for a project licensed under the FSL alongside a commercial edition are still being decided. This will change; until then, please open an issue instead.
- **Security problems** go through GitHub's private vulnerability reporting, never a public issue. See [Security](docs/guide/security.md#reporting-a-vulnerability).

Before any local change, run the checks:

```bash
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo nextest run
```
