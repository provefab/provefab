---
name: rust-conventions
description: Checks and conventions for Rust changes in provefab tasks. Use when the repository has a Cargo.toml.
---

# Rust conventions

- Edition 2024. Match the surrounding code's naming, comment density and error-handling style.
- Before you finish, run and fix: `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo nextest run` (or `cargo test` if nextest is missing).
- Write the failing test first, then the code. Keep tests next to the code they cover.
- No `unwrap()` or `expect()` on paths reachable from user input; return typed errors (`thiserror`) from libraries.
