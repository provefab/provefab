# Native Sandbox Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox syntax for tracking.

**Goal:** Enforce native OS restrictions for worker commands and validation gates.
**Architecture:** Shared Rust sandbox policy feeds native Codex/Claude permissions and SRT gate/Pi wrappers. Trusted configuration stays outside task worktrees; Claude/Pi file operations use sandboxed Bash; guards remain defense in depth.
**Tech Stack:** Rust, Tokio, serde_json, TOML, native Codex and Claude CLI, Anthropic SRT, Pi TypeScript extension.
**Spec:** docs/superpowers/specs/2026-10-13-native-sandbox.md

## Global Constraints
- Network denied; the user explicitly chose network-free gates until stricter local isolation is available. The local option currently refuses on every platform.
- No unsandboxed fallback and no silent platform downgrade.
- No global package installation, publishing, deployment or real credential probes.
- Current session permissions are unrestricted with approval never; omit sandbox_permissions.
- Core work stays in /Users/antoinehoriot/Projects/provefab/provefab-sandbox, branch feat/native-sandbox. User-authorized landing changes use a separate landing worktree, preserving existing untracked docs/distribution.
- Native runtime evidence is required before completion; Linux unverified unless actually tested.

### Task 1: Shared policy and worker enforcement
**Files:** Create crates/agent-workers/src/sandbox.rs; modify lib.rs, types.rs, codex.rs, claude.rs, pi.rs and plugins/pi-provefab/extensions/provefab.ts; migrate StageRequest literals mechanically.
**Interface:** StageRequest gains sandbox: SandboxPolicy. SandboxPolicy has allow_local_network: bool, deny_read: Vec<PathBuf>, deny_write: Vec<PathBuf>, runtime: PathBuf (default srt). Workers always use strict network even if allow_local_network is true. The core supplies sensitive and validation paths. Add preparation methods returning io::Result; preparation must canonicalize worktree/session roots, reject policy/runtime paths under writable scope, and fail before agent launch on invalid configuration. Native config generation must be deterministic and safely quote TOML/JSON/shell paths. Export a SRT gate command builder consuming worktree, scratch and policy; gates need the same protection. Add defense in depth environment sanitation without erasing main CLI authentication from protected login files.
- [x] Add focused failing behavior tests before implementation. Start with a worker argv assertion that approval_policy=never and named network=false are present, and legacy --sandbox is absent. Example: assert!(!args.iter().any(|s| s == "--sandbox")); this fails on baseline.
- [x] Implement native adapter settings. Codex uses a whole inline TOML filesystem table passed with -c (individual dotted path keys fail). Claude must reject widening user sandbox settings, require failIfUnavailable and disallow excluded/unsandboxed commands. Pi wraps Bash through SRT with the installed createBashToolDefinition API, retaining timeout and cancellation; missing policy blocks the call.
- [x] Add tests for serialized path quoting, read-only/full writes, missing runtime/config and config under worktree rejection. Do not add a production bypass just for existing tests. Use explicit fake CLI programs in tests when observing argv/environment.
- [x] Run focused tests and self-review. Commit task changes; record red/green commands and output in task report.

### Task 2: Repository policy, gates and immutable validation integration
**Files:** config.rs, pipeline.rs, pr_review.rs, agents.rs, gates.rs, guard paths and CLI guard entry, docs/config examples and affected fixtures. Create one focused core sandbox policy module if needed.
**Interface:** Per-repo SandboxConfig deserializes strict defaults and refuses unknown security keys. Pipeline derives SandboxPolicy for every stage and gate, including post-merge checks and reproduction. SandboxConfig fields: allow_local_network (false), protected_paths (empty Vec<PathBuf>), deny_read (empty Vec<PathBuf>), runtime (PathBuf default srt). protected_paths entries may be worktree-relative or absolute; deny_read must be absolute. Empty protected_paths does not provide immutable validation. Do not silently assert complete dependency protection. Unknown fields fail deserialization. Protected lexical paths and resolved targets both need enforcement; reject ambiguous/missing protected path declarations rather than silently dropping them. Runtime is resolved from trusted PATH, never worktree PATH entries. Keep run_gates API consistently sandboxed; tests may inject a fake SRT executable but production must have no unsandboxed branch.
- [x] Add failing config default/unknown-field tests, and a gate test with a missing executable that attempts to create a marker: assert!(!report.passed()); assert!(!marker.exists()). Verify failure on old implementation.
- [x] Wire policy, with secret paths for explicit Paths.home plus actual worker auth homes and inherited HOME. Canonicalize protected paths and reject symlink escapes, worktree root replacement and session overlap. Ensure direct Write/Edit tool paths cannot bypass validation protections.
- [x] Add gate argv/env and all pipeline entrypoint regression tests. Capture errors in existing GateReport, preserving timeout/log behavior. Document configuration and runtime provisioning, localhost scope and deny-list limitations.
- [x] Run focused tests; commit with report containing evidence.

### Task 3: Adversarial native validation and final verification
**Files:** integration tests/probe harness and docs/superpowers/native-sandbox-ledger.md.
- [x] Reuse the audit's fake-secret/local-remote probes against the actual integrated code, with an unsandboxed positive control. Test missing runtime/config before marker execution, HTTP through Python/curl, child and symlink reads, validation edits and normal allowed work.
- [x] Run native strict gates and verify local-option refusal, plus actual Codex worker launch and Claude launch or fail-closed refusal according to verified support. Exercise Pi's wrapped tool directly without a model if possible. Record unsupported surfaces honestly.
- [ ] Run cargo fmt -- --check, cargo clippy -- -D warnings, cargo nextest run. Reopen edited files. Run an independent whole-branch security review naming child escape, mutable config/harness, environment leaks, fallback and lifecycle risks; fix important findings and rerun covering checks.
- [ ] Leave a reviewable branch and concise French results. No merge/push/deployment requested.

### Task 4: Documentation, landing and final build (user added)
**Scope budget:** documentation and existing landing feature/security copy only. No redesign, deployment, release publishing or pricing changes. Preserve existing layout, palette and typography.
- [x] Align documentation and landing claims with actual verified support and limitations. Core documentation remains the source of generated site docs; update the landing submodule to the reviewed local core commit.
- [x] Use a separate clean landing worktree; do not touch original untracked docs/distribution.
- [x] Run site tests/build/link/content checks and inspect rendered pages in the browser. Run final release build of core CLI. Desktop checked; mobile navigation remains an explicit pre-publication follow-up after CUA interruption. No dark theme exists.
- [x] Record build artifacts and unresolved platform/runtime limitations; leave reviewable local branches.
