//! Runs a stage on whichever worker the routed model belongs to (spec §5).

use std::future::Future;
use std::path::PathBuf;

use agent_workers::{
    ClaudeCodeWorker, CodexWorker, PiWorker, StageRequest, StageResult, Worker, WorkerError,
    WorkerEvent,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::codex_setup;
use crate::config::{ModelEntry, WorkerKind};
use crate::paths::Paths;
use crate::plugins::InstalledPlugins;

/// What the pipeline needs from the agents; tests substitute a fake.
pub trait StageRunner {
    fn run(
        &self,
        model: &ModelEntry,
        req: StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> impl Future<Output = Result<StageResult, WorkerError>> + Send;
}

pub struct AgentRunner {
    pub pi: PiWorker,
    pub claude: ClaudeCodeWorker,
    pub codex: CodexWorker,
}

impl AgentRunner {
    /// Real workers using Provefab's own config dirs and installed plugins.
    pub fn new(paths: &Paths, plugins: &InstalledPlugins, provefab_bin: PathBuf) -> Self {
        Self {
            pi: PiWorker {
                program: "pi".into(),
                package: plugins.pi_package.clone(),
                provefab_bin: provefab_bin.clone(),
            },
            claude: ClaudeCodeWorker {
                program: "claude".into(),
                plugin_dir: plugins.cc_plugin.clone(),
                config_dir: paths.claude_config(),
                provefab_bin: provefab_bin.clone(),
            },
            codex: CodexWorker {
                program: "codex".into(),
                codex_home: paths.codex_home(),
                provefab_bin,
            },
        }
    }
}

/// The catalog entry decides the worker, the model name and (for Pi) the provider.
pub fn apply_model(model: &ModelEntry, mut req: StageRequest) -> StageRequest {
    req.model = model.model.clone();
    req.provider = (model.worker == WorkerKind::Pi).then(|| model.provider.clone());
    req
}

impl StageRunner for AgentRunner {
    async fn run(
        &self,
        model: &ModelEntry,
        req: StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> Result<StageResult, WorkerError> {
        let req = apply_model(model, req);
        match model.worker {
            WorkerKind::Pi => self.pi.run(&req, events).await,
            WorkerKind::ClaudeCode => self.claude.run(&req, events).await,
            WorkerKind::Codex => {
                // Spec D28, D31: pin the worktree untrusted, then refuse to run
                // unless our guard hook is trusted and no project is.
                let setup = |e: codex_setup::CodexSetupError| WorkerError::Io(e.to_string());
                codex_setup::pin_untrusted(&self.codex.program, &self.codex.codex_home, &req.cwd)
                    .await
                    .map_err(setup)?;
                codex_setup::check(&self.codex.program, &self.codex.codex_home, &req.cwd)
                    .await
                    .map_err(setup)?;
                self.codex.run(&req, events).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Tier;
    use agent_workers::ToolProfile;
    use std::time::Duration;

    fn entry(worker: WorkerKind, model: &str, provider: &str) -> ModelEntry {
        ModelEntry {
            id: "m".into(),
            worker,
            model: model.into(),
            provider: provider.into(),
            tier: Tier::Standard,
            max_concurrency: 1,
        }
    }

    fn req() -> StageRequest {
        StageRequest {
            cwd: "/w".into(),
            prompt: "p".into(),
            model: String::new(),
            provider: None,
            tools: ToolProfile::ReadOnly,
            system_prompt_file: None,
            output_schema: None,
            max_turns: 10,
            timeout: Duration::from_secs(60),
            session_dir: "/s".into(),
        }
    }

    #[test]
    fn the_catalog_entry_sets_model_and_provider() {
        let pi = apply_model(&entry(WorkerKind::Pi, "sonnet-4", "anthropic"), req());
        assert_eq!(
            (pi.model.as_str(), pi.provider.as_deref()),
            ("sonnet-4", Some("anthropic"))
        );
        let cc = apply_model(&entry(WorkerKind::ClaudeCode, "opus", "ignored"), req());
        assert_eq!((cc.model.as_str(), cc.provider), ("opus", None));
        let cx = apply_model(&entry(WorkerKind::Codex, "gpt-5.5", ""), req());
        assert_eq!((cx.model.as_str(), cx.provider), ("gpt-5.5", None));
    }
}
