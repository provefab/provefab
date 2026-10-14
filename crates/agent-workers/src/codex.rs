use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

use crate::process::{Flow, Transcript, apply_worker_env, prepare_git_hooks, run_jsonl};
use crate::types::{digest, looks_rate_limited};
use crate::{
    ExitReason, StageRequest, StageResult, ToolProfile, Usage, Worker, WorkerError, WorkerEvent,
};

/// Runs a stage with the unmodified `codex` CLI (`codex exec --json`), signed in
/// with the user's own ChatGPT plan from a dedicated `CODEX_HOME` (spec §5.5, D27).
///
/// The guard hook lives in `CODEX_HOME/hooks.json` and must already be trusted
/// (see Provefab's `codex_setup`). This worker never passes
/// `--dangerously-bypass-hook-trust`: that flag also runs the target repo's own
/// `.codex/` hooks (spec D28).
pub struct CodexWorker {
    pub program: PathBuf,
    /// `CODEX_HOME`, logged in once with `codex login`.
    pub codex_home: PathBuf,
    /// The `provefab` binary the guard hook calls for `provefab guard`.
    pub provefab_bin: PathBuf,
}

/// Credentials and endpoints that would move Codex off the ChatGPT sign-in.
const API_KEY_ENV: [&str; 3] = ["OPENAI_API_KEY", "CODEX_API_KEY", "OPENAI_BASE_URL"];

/// Inherited variables that could move Codex off Provefab's ChatGPT sign-in
/// (`CODEX_ACCESS_TOKEN` takes precedence over the stored login, for example):
/// every `CODEX_*` except `CODEX_HOME`, and every `OPENAI_*`.
fn inherited_overrides(keys: impl Iterator<Item = String>) -> Vec<String> {
    keys.filter(|k| (k.starts_with("CODEX_") && k != "CODEX_HOME") || k.starts_with("OPENAI_"))
        .collect()
}

impl CodexWorker {
    /// `prompt` is the full text sent to the agent (Codex has no separate
    /// system-prompt flag, so `run` puts the stage instructions first).
    pub fn args(&self, req: &StageRequest, prompt: &str) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec!["exec".into(), "--json".into(), "-C".into()];
        a.push(req.cwd.clone().into_os_string());
        let mut push = |s: &str| a.push(s.into());
        push("-m");
        push(&req.model);
        for setting in [
            "approval_policy=\"never\"".to_string(),
            "web_search=\"disabled\"".to_string(),
            "default_permissions=\"provefab\"".to_string(),
            "permissions.provefab.network.enabled=false".to_string(),
            req.sandbox.codex_filesystem(req, &self.codex_home),
            crate::sandbox::codex_shell_environment(req),
        ] {
            push("-c");
            push(&setting);
        }
        // Execpolicy `.rules` files from the user or the repo must not widen what runs.
        push("--ignore-rules");
        // Without tools, Codex's built-in web search and image viewer stay
        // off as well (codex-rs: `web_search = "disabled"` registers no
        // search tool; `features.view_image = false` no image viewer).
        if req.tools == ToolProfile::NoTools {
            // Its empty scratch directory is no git repository, which Codex
            // refuses without this flag; stages run in worktrees.
            push("--skip-git-repo-check");
        }
        for setting in crate::CODEX_TOOL_BOUNDARY_OVERRIDES {
            push("-c");
            push(setting);
        }
        if req.output_schema.is_some() {
            a.push("--output-schema".into());
            a.push(req.session_dir.join("output-schema.json").into_os_string());
        }
        a.push(prompt.into());
        a
    }

    /// The full command, environment included, for one stage.
    fn command(&self, program: &std::path::Path, req: &StageRequest, prompt: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(self.args(req, prompt)).current_dir(&req.cwd);
        crate::sanitize_environment(&mut cmd);
        apply_worker_env(
            &mut cmd,
            &req.cwd,
            &req.session_dir.join("git-hooks"),
            req.tools,
        );
        for key in API_KEY_ENV {
            cmd.env_remove(key);
        }
        for key in inherited_overrides(std::env::vars().map(|(k, _)| k)) {
            cmd.env_remove(key);
        }
        cmd.env("CODEX_HOME", &self.codex_home);
        cmd.env("PROVEFAB_BIN", &self.provefab_bin);
        cmd
    }
    pub fn prepare_command(&self, req: &StageRequest, prompt: &str) -> std::io::Result<Command> {
        let mut req = crate::sandbox::prepare_request(
            req,
            std::slice::from_ref(&self.provefab_bin),
            std::slice::from_ref(&self.codex_home),
        )?;
        crate::check_codex_host_settings(&self.codex_home)?;
        req.sandbox
            .deny_write
            .extend(crate::check_codex_context_sources(
                &self.codex_home,
                &req.cwd,
                &req.session_dir,
            )?);
        let program = crate::sandbox::executable(&self.program, &req.cwd, &req.session_dir)?;
        crate::check_codex_version(&program, &req.cwd, &req.session_dir)?;
        req.sandbox.deny_write.push(program.clone());
        let policy = crate::sandbox::write_policy(&req, false)?;
        let mut cmd = self.command(&program, &req, prompt);
        crate::sandbox::apply_temporary_env(&mut cmd, &req);
        cmd.env(crate::SANDBOX_POLICY_ENV, policy);
        Ok(cmd)
    }
}

impl Worker for CodexWorker {
    async fn run(
        &self,
        req: &StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> Result<StageResult, WorkerError> {
        let io = |e: std::io::Error| WorkerError::Io(e.to_string());
        let normalized = crate::sandbox::prepare_request(
            req,
            std::slice::from_ref(&self.provefab_bin),
            std::slice::from_ref(&self.codex_home),
        )
        .map_err(io)?;
        let req = &normalized;
        if let Some(schema) = &req.output_schema {
            std::fs::write(
                req.session_dir.join("output-schema.json"),
                schema.to_string(),
            )
            .map_err(io)?;
        }
        let prompt = match &req.system_prompt_file {
            Some(f) => format!(
                "{}\n\n{}",
                std::fs::read_to_string(f).map_err(io)?.trim_end(),
                req.prompt
            ),
            None => req.prompt.clone(),
        };
        prepare_git_hooks(&req.session_dir).map_err(io)?;
        let cmd = self.prepare_command(req, &prompt).map_err(io)?;
        let mut transcript = Transcript::open(&req.session_dir)?;
        let mut state = CodexState {
            wants_output: req.output_schema.is_some(),
            ..CodexState::default()
        };
        let finished = run_jsonl(cmd, req.timeout, |v| {
            transcript.write(&v);
            state.on_record(&v, &events);
            Flow::Continue
        })
        .await?;
        let exit = if finished.timed_out {
            ExitReason::Timeout
        } else if state.completed && finished.status.is_some_and(|s| s.success()) {
            // Errors Codex retried before the turn completed do not decide the outcome.
            ExitReason::Completed
        } else if let Some(err) = state.error.clone() {
            if looks_rate_limited(&err) {
                ExitReason::RateLimited(err)
            } else {
                ExitReason::ProviderError(err)
            }
        } else if !state.completed || !finished.status.is_some_and(|s| s.success()) {
            // No `turn.completed`: Codex never finished the turn.
            ExitReason::Crashed {
                code: finished.status.and_then(|s| s.code()),
                stderr_tail: finished.stderr_tail,
            }
        } else {
            ExitReason::Completed
        };
        Ok(StageResult {
            exit,
            structured_output: state.output,
            final_text: state.final_text,
            usage: state.usage,
            turns: state.turns,
            actual_model: None,
        })
    }
}

#[derive(Default)]
struct CodexState {
    wants_output: bool,
    completed: bool,
    error: Option<String>,
    output: Option<Value>,
    final_text: Option<String>,
    usage: Usage,
    turns: u32,
}

impl CodexState {
    fn on_record(&mut self, v: &Value, events: &UnboundedSender<WorkerEvent>) {
        let emit = |e| {
            let _ = events.send(e);
        };
        let item = v.get("item").unwrap_or(&Value::Null);
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match v.get("type").and_then(Value::as_str).unwrap_or_default() {
            "item.started" => match item_type {
                "command_execution" => emit(WorkerEvent::ToolStart {
                    name: "Bash".into(),
                    input: item
                        .get("command")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }),
                "file_change" => emit(WorkerEvent::ToolStart {
                    name: "apply_patch".into(),
                    input: digest(item.get("changes").unwrap_or(&Value::Null)),
                }),
                _ => {}
            },
            "item.completed" => match item_type {
                "command_execution" => emit(WorkerEvent::ToolEnd {
                    name: "Bash".into(),
                    is_error: item.get("exit_code").and_then(Value::as_i64) != Some(0)
                        || status != "completed",
                }),
                "file_change" => emit(WorkerEvent::ToolEnd {
                    name: "apply_patch".into(),
                    is_error: status != "completed",
                }),
                // A call the guard hook refused arrives as an error item.
                // A call the guard hook refused. Other error items (config
                // warnings, reroute notices) are not tool calls; they stay in the transcript.
                "error"
                    if item
                        .get("message")
                        .and_then(Value::as_str)
                        .is_some_and(|m| m.starts_with("Command blocked by PreToolUse hook")) =>
                {
                    emit(WorkerEvent::ToolEnd {
                        name: "blocked".into(),
                        is_error: true,
                    })
                }
                "agent_message" => {
                    let text = item
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if self.wants_output {
                        self.output = serde_json::from_str(&text).ok();
                    }
                    emit(WorkerEvent::Text(text.clone()));
                    self.final_text = Some(text);
                }
                _ => {}
            },
            "turn.completed" => {
                self.completed = true;
                self.turns += 1;
                emit(WorkerEvent::TurnEnd);
                let n = |p: &str| v.pointer(p).and_then(Value::as_u64).unwrap_or(0);
                // Codex's input_tokens include the cached ones (D74).
                let cached = n("/usage/cached_input_tokens");
                self.usage.input_tokens += n("/usage/input_tokens").saturating_sub(cached);
                self.usage.cache_read_tokens += cached;
                self.usage.output_tokens += n("/usage/output_tokens");
            }
            "turn.failed" => {
                self.error = Some(
                    v.pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("turn failed")
                        .to_string(),
                );
            }
            "error" => {
                let message = v
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error")
                    .to_string();
                // "Reconnecting... n/5" is a stream error Codex is retrying, not a limit.
                if message.starts_with("Reconnecting") {
                    emit(WorkerEvent::Retry {
                        message: message.clone(),
                    });
                } else if looks_rate_limited(&message) {
                    emit(WorkerEvent::RateLimited {
                        message: message.clone(),
                    });
                }
                self.error = Some(message);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::NO_TOOLS_ENV;
    use serde_json::json;
    use std::time::Duration;

    fn worker() -> CodexWorker {
        CodexWorker {
            program: "codex".into(),
            codex_home: "/home/.provefab/codex".into(),
            provefab_bin: "/x/provefab".into(),
        }
    }

    fn req(tools: ToolProfile, schema: bool) -> StageRequest {
        StageRequest {
            sandbox: Default::default(),
            cwd: "/w".into(),
            prompt: "Fix it".into(),
            model: "gpt-5.5".into(),
            provider: None,
            tools,
            system_prompt_file: None,
            output_schema: schema.then(|| json!({"type": "object"})),
            max_turns: 40,
            timeout: Duration::from_secs(60),
            session_dir: "/w/.s".into(),
        }
    }

    fn joined(args: Vec<OsString>) -> String {
        args.into_iter()
            .map(|s| s.into_string().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn inherited_codex_host_helpers_refuse_before_version_or_worker_start() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let home = dir.path().join("home");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&home).unwrap();
        let marker = dir.path().join("outside-started");
        let program = dir.path().join("codex-host-helper");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ntouch '{}'\necho 'codex-cli 0.162.1'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut worker = worker();
        worker.program = program;
        worker.codex_home = home.clone();
        worker.provefab_bin = "/bin/false".into();
        let mut request = req(ToolProfile::Full, false);
        request.cwd = work;
        request.session_dir = dir.path().join("session");
        std::fs::write(home.join("cloud-config-bundle-cache.json"), "{}").unwrap();
        assert!(worker.prepare_command(&request, "prompt").is_err());
        assert!(
            !marker.exists(),
            "external cloud policy checked after startup"
        );
        std::fs::remove_file(home.join("cloud-config-bundle-cache.json")).unwrap();
        for config in [
            "[mcp_servers.probe]\ncommand='/bin/false'\n",
            "notify=['/bin/false']\n",
            "cli_auth_credentials_store='keyring'\n",
            "cli_auth_credentials_store='auto'\n",
            "cli_auth_credentials_store='ephemeral'\n",
        ] {
            std::fs::write(home.join("config.toml"), config).unwrap();
            assert!(
                worker.prepare_command(&request, "prompt").is_err(),
                "host helpers accepted"
            );
            assert!(
                !marker.exists(),
                "host helper settings were checked after launch"
            );
        }
        std::fs::write(
            home.join("config.toml"),
            "cli_auth_credentials_store='file'\n",
        )
        .unwrap();
        std::fs::write(home.join("auth.json"), r#"{"auth_mode":"agent_identity"}"#).unwrap();
        assert!(worker.prepare_command(&request, "prompt").is_err());
        assert!(
            !marker.exists(),
            "uninspectable authentication launched Codex"
        );
    }

    #[test]
    fn every_native_profile_disables_external_tool_routes() {
        for tools in [
            ToolProfile::Full,
            ToolProfile::ReadOnly,
            ToolProfile::UntrustedReadOnly,
            ToolProfile::NoTools,
        ] {
            let args = worker().args(&req(tools, false), "prompt");
            for setting in [
                "project_doc_max_bytes=0",
                "project_root_markers=[]",
                "skills.bundled.enabled=false",
                "cloud.skills.enabled=false",
                "cli_auth_credentials_store=\"file\"",
                "features.plugins=false",
                "features.apps=false",
                "features.remote_plugin=false",
                "features.multi_agent=false",
                "features.computer_use=false",
                "features.browser_use=false",
                "features.code_mode_host=false",
                "features.shell_snapshot=false",
                "features.shell_snapshot_v2=false",
            ] {
                assert!(
                    args.iter().any(|a| a == setting),
                    "unsafe inherited route: {setting}"
                );
            }
        }
    }

    #[test]
    fn implicit_codex_context_refuses_before_executable_probe() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        let home = root.path().join("home");
        std::fs::create_dir(&work).unwrap();
        std::fs::create_dir(&home).unwrap();
        let marker = root.path().join("started");
        let program = root.path().join("codex");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ntouch '{}'\necho 'codex-cli 0.162.1'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let worker = CodexWorker {
            program,
            codex_home: home.clone(),
            provefab_bin: "/bin/false".into(),
        };
        let mut request = req(ToolProfile::Full, false);
        request.cwd = work.clone();
        request.session_dir = root.path().join("session");
        for path in [
            home.join("AGENTS.md"),
            home.join("AGENTS.override.md"),
            home.join("skills/custom/SKILL.md"),
            work.join(".agents/skills/custom/SKILL.md"),
            work.join(".codex/skills/custom/SKILL.md"),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "dummy context").unwrap();
            assert!(
                worker.prepare_command(&request, "trusted prompt").is_err(),
                "{} accepted",
                path.display()
            );
            assert!(!marker.exists(), "context inspected after executable probe");
            std::fs::remove_file(&path).unwrap();
            if path.file_name().unwrap() == "SKILL.md" {
                std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap()).unwrap();
            }
        }
        std::fs::create_dir_all(home.join("skills/.system")).unwrap();
        let command = worker.prepare_command(&request, "trusted prompt").unwrap();
        assert!(
            marker.exists(),
            "supported stock cache prevented the normal version probe"
        );
        let policy_file = command
            .as_std()
            .get_envs()
            .find(|(key, _)| *key == crate::SANDBOX_POLICY_ENV)
            .unwrap()
            .1
            .unwrap();
        let policy: Value = serde_json::from_slice(&std::fs::read(policy_file).unwrap()).unwrap();
        for path in [
            work.canonicalize().unwrap().join(".agents"),
            work.canonicalize().unwrap().join(".codex"),
        ] {
            assert!(
                policy["filesystem"]["denyWrite"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!(path))
            );
        }
        let permissions = command
            .as_std()
            .get_args()
            .find(|arg| {
                arg.to_string_lossy()
                    .starts_with("permissions.provefab.filesystem=")
            })
            .unwrap();
        let permissions: toml::Table = permissions.to_str().unwrap().parse().unwrap();
        assert_eq!(
            permissions["permissions"]["provefab"]["filesystem"][work
                .canonicalize()
                .unwrap()
                .join(".agents")
                .to_str()
                .unwrap()]
            .as_str(),
            Some("read")
        );
    }

    #[test]
    fn every_native_profile_disables_host_image_reads() {
        for tools in [
            ToolProfile::Full,
            ToolProfile::ReadOnly,
            ToolProfile::UntrustedReadOnly,
            ToolProfile::NoTools,
        ] {
            let request = req(tools, false);
            assert!(
                worker()
                    .args(&request, "prompt")
                    .iter()
                    .any(|a| a == "features.view_image=false")
            );
        }
    }

    #[test]
    fn native_permissions_never_offer_an_unsandboxed_approval() {
        let args = worker().args(&req(ToolProfile::Full, false), "Fix it");
        assert!(!args.iter().any(|s| s == "--sandbox"));
        assert!(args.iter().any(|s| s == "approval_policy=\"never\""));
        assert!(
            args.iter()
                .any(|s| s == "permissions.provefab.network.enabled=false")
        );
    }

    #[test]
    fn args_pick_the_sandbox_from_the_tool_profile_and_never_bypass_hook_trust() {
        let full = joined(worker().args(&req(ToolProfile::Full, false), "Fix it"));
        assert!(
            full.starts_with("exec --json -C /w -m gpt-5.5 -c approval_policy="),
            "{full}"
        );
        assert!(full.ends_with(" Fix it"), "{full}");
        assert!(!full.contains("--output-schema"), "{full}");
        let review = joined(worker().args(&req(ToolProfile::ReadOnly, true), "Review"));
        assert!(!review.contains("\"/w\"=\"write\""), "{review}");
        assert!(
            review.contains("--output-schema /w/.s/output-schema.json"),
            "{review}"
        );
        for a in [&full, &review] {
            assert!(
                !a.contains("bypass"),
                "hook trust must never be bypassed: {a}"
            );
            assert!(!a.contains("danger-full-access"), "{a}");
        }
    }

    /// Repository rules pre-flight S2: Codex always has a shell, so the guard
    /// refuses it; its sandbox stays read-only behind that.
    #[test]
    fn no_tools_is_read_only_and_tells_the_guard() {
        let none = req(ToolProfile::NoTools, true);
        let args = joined(worker().args(&none, "Answer"));
        assert!(!args.contains("\"/w\"=\"write\""), "{args}");
        // Final review: Codex's own web search and image viewer stay off too.
        assert!(
            args.contains("-c web_search=\"disabled\"")
                && args.contains("-c features.view_image=false"),
            "{args}"
        );
        let review = joined(worker().args(&req(ToolProfile::ReadOnly, true), "R"));
        assert!(review.contains("web_search=\"disabled\""), "{review}");
        // The scratch directory is no git repository: Codex refuses it
        // without this flag. Stages run in git worktrees and never pass it.
        assert!(args.contains("--skip-git-repo-check"), "{args}");
        let full = joined(worker().args(&req(ToolProfile::Full, false), "F"));
        for a in [&review, &full] {
            assert!(a.contains("web_search=\"disabled\""), "{a}");
            assert!(!a.contains("--skip-git-repo-check"), "{a}");
        }
        let cmd = worker().command(&worker().program, &none, "Answer");
        let set = cmd
            .as_std()
            .get_envs()
            .find(|(k, _)| *k == NO_TOOLS_ENV)
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().to_string()));
        assert_eq!(set.as_deref(), Some("1"));
    }

    /// Final review I1 (pull request reviews): Codex keeps its shell, in a
    /// read-only sandbox, and the guard is told to allow read-only commands only.
    #[test]
    fn an_untrusted_review_is_read_only_and_tells_the_guard() {
        let review = req(ToolProfile::UntrustedReadOnly, true);
        let args = joined(worker().args(&review, "Review"));
        assert!(!args.contains("\"/w\"=\"write\""), "{args}");
        // No web search or image viewer either: nothing read leaves through them.
        assert!(
            args.contains("-c web_search=\"disabled\"")
                && args.contains("-c features.view_image=false"),
            "{args}"
        );
        assert!(!args.contains("--skip-git-repo-check"), "{args}");
        let marker = worker()
            .command(&worker().program, &review, "Review")
            .as_std()
            .get_envs()
            .find(|(k, _)| *k == crate::UNTRUSTED_REVIEW_ENV)
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().to_string()));
        assert_eq!(marker.as_deref(), Some("1"));
    }

    #[test]
    fn command_uses_the_provefab_codex_home_and_drops_api_keys() {
        let cmd = worker().command(&worker().program, &req(ToolProfile::Full, false), "Fix it");
        let envs: Vec<(String, Option<String>)> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into(),
                    v.map(|v| v.to_string_lossy().into()),
                )
            })
            .collect();
        for key in [
            "OPENAI_API_KEY",
            "CODEX_API_KEY",
            "OPENAI_BASE_URL",
            "SSH_AUTH_SOCK",
        ] {
            assert!(envs.contains(&(key.into(), None)), "{key} kept: {envs:?}");
        }
        assert!(envs.contains(&("CODEX_HOME".into(), Some("/home/.provefab/codex".into()))));
        assert!(envs.contains(&("PROVEFAB_BIN".into(), Some("/x/provefab".into()))));
        assert!(envs.contains(&("PROVEFAB_WORKTREE".into(), Some("/w".into()))));
    }

    fn feed(s: &mut CodexState, records: &[Value]) -> Vec<WorkerEvent> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for r in records {
            s.on_record(r, &tx);
        }
        drop(tx);
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    #[test]
    fn parses_commands_patches_refusals_output_and_usage() {
        let mut s = CodexState {
            wants_output: true,
            ..CodexState::default()
        };
        let events = feed(
            &mut s,
            &[
                json!({"type":"thread.started","thread_id":"t"}),
                json!({"type":"turn.started"}),
                json!({"type":"item.started","item":{"id":"1","type":"command_execution","command":"/bin/bash -lc 'ls'","status":"in_progress"}}),
                json!({"type":"item.completed","item":{"id":"1","type":"command_execution","command":"/bin/bash -lc 'ls'","exit_code":0,"status":"completed"}}),
                json!({"type":"item.completed","item":{"id":"2","type":"error","message":"Command blocked by PreToolUse hook: `git tag` is reserved for Provefab"}}),
                json!({"type":"item.started","item":{"id":"3","type":"file_change","changes":[{"path":"/w/a.rs","kind":"add"}],"status":"in_progress"}}),
                json!({"type":"item.completed","item":{"id":"3","type":"file_change","changes":[{"path":"/w/a.rs","kind":"add"}],"status":"completed"}}),
                json!({"type":"item.completed","item":{"id":"4","type":"agent_message","text":"{\"verdict\":\"approve\"}"}}),
                json!({"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":7}}),
            ],
        );
        assert_eq!(
            events[..4],
            [
                WorkerEvent::ToolStart {
                    name: "Bash".into(),
                    input: "/bin/bash -lc 'ls'".into()
                },
                WorkerEvent::ToolEnd {
                    name: "Bash".into(),
                    is_error: false
                },
                WorkerEvent::ToolEnd {
                    name: "blocked".into(),
                    is_error: true
                },
                WorkerEvent::ToolStart {
                    name: "apply_patch".into(),
                    input: r#"[{"kind":"add","path":"/w/a.rs"}]"#.into()
                },
            ]
        );
        assert!(s.completed);
        assert_eq!(s.turns, 1);
        assert_eq!(s.output, Some(json!({"verdict": "approve"})));
        assert_eq!(
            s.usage,
            // Codex's input_tokens include the cached ones: split them.
            Usage {
                input_tokens: 200,
                output_tokens: 7,
                cache_read_tokens: 800,
                cache_write_tokens: 0,
            }
        );
    }

    #[test]
    fn failed_commands_turn_failures_and_usage_limits() {
        let mut s = CodexState::default();
        let events = feed(
            &mut s,
            &[
                json!({"type":"item.completed","item":{"id":"1","type":"command_execution","command":"cargo test","exit_code":101,"status":"completed"}}),
                json!({"type":"error","message":"You've hit your usage limit. Try again later."}),
            ],
        );
        assert_eq!(
            events,
            vec![
                WorkerEvent::ToolEnd {
                    name: "Bash".into(),
                    is_error: true
                },
                WorkerEvent::RateLimited {
                    message: "You've hit your usage limit. Try again later.".into()
                },
            ]
        );
        let mut s = CodexState::default();
        feed(
            &mut s,
            &[json!({"type":"turn.failed","error":{"message":"model not found"}})],
        );
        assert_eq!(s.error.as_deref(), Some("model not found"));
        assert!(!s.completed);
    }

    #[test]
    fn free_text_is_not_structured_output() {
        let mut s = CodexState {
            wants_output: true,
            ..CodexState::default()
        };
        feed(
            &mut s,
            &[
                json!({"type":"item.completed","item":{"id":"1","type":"agent_message","text":"Looks good to me."}}),
            ],
        );
        assert_eq!(s.output, None);
    }

    /// Final review #6: any CODEX_* or OPENAI_* override (an access token, a refresh URL,
    /// a base URL) could move the run off Provefab's ChatGPT sign-in.
    #[test]
    fn review_6_codex_and_openai_overrides_are_removed() {
        let inherited = [
            "PATH",
            "HOME",
            "CODEX_HOME",
            "CODEX_ACCESS_TOKEN",
            "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
            "OPENAI_API_KEY",
            "OPENAI_ORG_ID",
        ];
        let mut removed = inherited_overrides(inherited.iter().map(|k| k.to_string()));
        removed.sort();
        assert_eq!(
            removed,
            vec![
                "CODEX_ACCESS_TOKEN",
                "CODEX_REFRESH_TOKEN_URL_OVERRIDE",
                "OPENAI_API_KEY",
                "OPENAI_ORG_ID"
            ]
        );
    }

    /// Final review #8: only hook refusals are "blocked" tool events; other error items
    /// (config warnings, reroute notices) are not tool calls.
    #[test]
    fn review_8_only_hook_refusals_are_blocked_events() {
        let mut s = CodexState::default();
        let events = feed(
            &mut s,
            &[
                json!({"type":"item.completed","item":{"id":"1","type":"error","message":"config: unknown key `foo` ignored"}}),
                json!({"type":"item.completed","item":{"id":"2","type":"error","message":"Command blocked by PreToolUse hook: no"}}),
            ],
        );
        assert_eq!(
            events,
            vec![WorkerEvent::ToolEnd {
                name: "blocked".into(),
                is_error: true
            }]
        );
    }
}
