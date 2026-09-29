//! Fakes and fixtures for end-to-end pipeline runs (spec §8), shared with
//! Provefab Pro: fake Jev, fake workers and a fake GitHub, against a real
//! temporary git repository with a bare `origin`. Enabled by the `testkit`
//! feature.

pub use std::future::Future;
pub use std::path::{Path, PathBuf};
pub use std::process::Command;
pub use std::sync::Mutex;

pub use crate::agents::StageRunner;
pub use crate::config::{Config, ModelEntry};
pub use crate::cooldown::Cooldowns;
pub use crate::forge::{Comment, ForgeError, Git, Issue};
pub use crate::jevq::{IssueContext, Triage};
pub use crate::paths::Paths;
pub use crate::pipeline::Pipeline;
pub use crate::ports::{Hub, Oracle};
pub use crate::store::{NewIssue, Store};
pub use crate::task::{TaskKind, TaskState, Verdict};
pub use agent_workers::{ExitReason, StageRequest, StageResult, Usage, WorkerError, WorkerEvent};
pub use serde_json::{Value, json};
pub use tokio::sync::mpsc::UnboundedSender;

// ---------- fakes ----------

pub type Script = dyn Fn(&ModelEntry, &StageRequest, &UnboundedSender<WorkerEvent>) -> Option<StageResult>
    + Send
    + Sync;

pub struct FakeRunner {
    pub script: Box<Script>,
    /// (model id, stage, prompt)
    pub calls: Mutex<Vec<(String, String, String)>>,
    /// Runs in flight per model, and the most ever seen at once.
    pub active: std::sync::Arc<Mutex<(std::collections::HashMap<String, u32>, u32)>>,
}

pub fn stage_of(prompt: &str) -> &'static str {
    if prompt.starts_with("You are the planning") {
        "plan"
    } else if prompt.starts_with("You are the implementation") {
        "implement"
    } else {
        "review"
    }
}

impl StageRunner for FakeRunner {
    fn run(
        &self,
        model: &ModelEntry,
        req: StageRequest,
        events: UnboundedSender<WorkerEvent>,
    ) -> impl Future<Output = Result<StageResult, WorkerError>> + Send {
        self.calls.lock().unwrap().push((
            model.id.clone(),
            stage_of(&req.prompt).to_string(),
            req.prompt.clone(),
        ));
        let r = (self.script)(model, &req, &events);
        let active = self.active.clone();
        let id = model.id.clone();
        {
            let mut a = active.lock().unwrap();
            let n = a.0.entry(id.clone()).or_insert(0);
            *n += 1;
            let n = *n;
            a.1 = a.1.max(n);
        }
        async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            *active.lock().unwrap().0.get_mut(&id).unwrap() -= 1;
            match r {
                Some(r) => Ok(r),
                None => std::future::pending().await,
            }
        }
    }
}

impl FakeRunner {
    pub fn calls(&self) -> Vec<(String, String, String)> {
        self.calls.lock().unwrap().clone()
    }
    pub fn stages(&self) -> Vec<(String, String)> {
        self.calls().into_iter().map(|(m, s, _)| (m, s)).collect()
    }
}

pub fn done(output: Option<Value>) -> Option<StageResult> {
    Some(StageResult {
        exit: ExitReason::Completed,
        structured_output: output,
        final_text: None,
        usage: Usage::default(),
        turns: 3,
    })
}

pub fn exit(e: ExitReason) -> Option<StageResult> {
    Some(StageResult {
        exit: e,
        structured_output: None,
        final_text: None,
        usage: Usage::default(),
        turns: 1,
    })
}

pub fn plan_json(repro: Option<&str>) -> Value {
    json!({
        "summary": "Add feature.txt",
        "steps": ["write feature.txt"],
        "files": ["feature.txt"],
        "risks": [],
        "repro_command": repro,
    })
}

pub fn approve() -> Value {
    json!({"verdict": "approve", "findings": []})
}

/// A worker that plans, appends to `feature.txt`, and gets `verdicts` from
/// the reviewer in order (then approvals).
pub fn reviewed(
    verdicts: Vec<&'static str>,
) -> impl Fn(&ModelEntry, &StageRequest, &UnboundedSender<WorkerEvent>) -> Option<StageResult>
+ Send
+ Sync {
    let n = Mutex::new(0usize);
    move |_: &ModelEntry, req: &StageRequest, _: &UnboundedSender<WorkerEvent>| match stage_of(
        &req.prompt,
    ) {
        "plan" => done(Some(plan_json(None))),
        "implement" => {
            let p = req.cwd.join("feature.txt");
            let old = std::fs::read_to_string(&p).unwrap_or_default();
            std::fs::write(&p, format!("{old}x")).unwrap();
            done(None)
        }
        _ => {
            let mut i = n.lock().unwrap();
            let v = verdicts.get(*i).copied().unwrap_or("approve");
            *i += 1;
            if v == "changes" {
                done(Some(json!({"verdict": "changes", "findings": [
                    {"file": "src/lib.rs", "line": 1, "severity": "blocking", "text": format!("finding {i}")}
                ]})))
            } else {
                done(Some(approve()))
            }
        }
    }
}

/// A worker that plans, writes `feature.txt`, and approves.
pub fn happy(
    _: &ModelEntry,
    req: &StageRequest,
    _: &UnboundedSender<WorkerEvent>,
) -> Option<StageResult> {
    match stage_of(&req.prompt) {
        "plan" => done(Some(plan_json(None))),
        "implement" => {
            std::fs::write(req.cwd.join("feature.txt"), "done\n").unwrap();
            done(None)
        }
        _ => done(Some(approve())),
    }
}

#[derive(Default)]
pub struct FakeOracle {
    pub verdict: Option<Verdict>,
    pub loop_p: Option<f64>,
    pub triage: Option<Triage>,
    pub reply: Option<f64>,
    pub triaged: Mutex<Vec<String>>,
    /// Issue bodies classify was asked about, in order.
    pub classified: Mutex<Vec<String>>,
}

impl Oracle for FakeOracle {
    async fn classify(&self, issue: &IssueContext) -> Option<Verdict> {
        self.classified.lock().unwrap().push(issue.body.clone());
        self.verdict.clone()
    }
    async fn loop_probability(&self, _: &[WorkerEvent]) -> Option<f64> {
        self.loop_p
    }
    async fn triage(&self, command: &str, _: &str) -> Option<Triage> {
        self.triaged.lock().unwrap().push(command.to_string());
        self.triage
    }
    async fn reply_answers(&self, _: &str, _: &str) -> Option<f64> {
        self.reply
    }
}

pub struct FakeHub {
    pub issue: Issue,
    pub comments: Mutex<Vec<Comment>>,
    /// While set, reading comments fails (GitHub unreachable).
    pub comments_down: std::sync::atomic::AtomicBool,
    pub posted: Mutex<Vec<String>>,
    pub labels: Mutex<Vec<(Vec<String>, Vec<String>)>>,
    pub prs: Mutex<Vec<(String, String, String, String)>>,
    /// The next this-many `pr_create` calls fail (GitHub unreachable).
    pub pr_create_failures: std::sync::atomic::AtomicU32,
    /// The next this-many `comment` calls fail (GitHub unreachable).
    pub comment_failures: std::sync::atomic::AtomicU32,
    /// The next this-many `edit_labels` calls fail (GitHub unreachable).
    pub label_failures: std::sync::atomic::AtomicU32,
    pub ensured: Mutex<Vec<String>>,
    /// Where `repo_clone` clones from (a local path standing in for GitHub).
    pub clone_from: Mutex<Option<PathBuf>>,
    /// What `pr_status` answers (open, no comments, until a test changes it).
    pub pr_status: Mutex<crate::forge::PrStatus>,
    pub issue_is_open: std::sync::atomic::AtomicBool,
    /// Merges requested through `pr_merge` (`url@head`).
    pub merged: Mutex<Vec<String>>,
    /// A file `pr_create` overwrites with garbage (breaks git in a worktree).
    pub break_on_pr_create: Mutex<Option<PathBuf>>,
    /// How many times `issue_open` was asked (the watcher's GitHub cost).
    pub issue_open_calls: std::sync::atomic::AtomicU32,
    /// While set, `issue` fails as gh does for an issue that does not exist.
    pub issue_missing: std::sync::atomic::AtomicBool,
    /// While set, `edit_labels` fails (GitHub unreachable).
    pub labels_down: std::sync::atomic::AtomicBool,
    /// The repository is public (merge policies refuse by default).
    pub public: std::sync::atomic::AtomicBool,
}

impl FakeHub {
    pub fn new(body: &str) -> Self {
        Self {
            issue: Issue {
                number: 7,
                title: "Add a feature file".into(),
                body: body.into(),
                url: "https://github.com/o/r/issues/7".into(),
                author: "alice".into(),
                labels: vec!["provefab".into()],
            },
            comments: Mutex::new(Vec::new()),
            posted: Mutex::new(Vec::new()),
            labels: Mutex::new(Vec::new()),
            prs: Mutex::new(Vec::new()),
            pr_create_failures: Default::default(),
            comment_failures: Default::default(),
            label_failures: Default::default(),
            ensured: Mutex::new(Vec::new()),
            comments_down: Default::default(),
            clone_from: Mutex::new(None),
            pr_status: Mutex::new(crate::forge::PrStatus {
                state: crate::forge::PrState::Open,
                comments: Vec::new(),
            }),
            issue_is_open: Default::default(),
            merged: Mutex::new(Vec::new()),
            break_on_pr_create: Mutex::new(None),
            issue_open_calls: Default::default(),
            issue_missing: Default::default(),
            labels_down: Default::default(),
            public: Default::default(),
        }
    }

    pub fn add_comment(&self, author: &str, body: &str) {
        let mut c = self.comments.lock().unwrap();
        let n = c.len();
        c.push(Comment {
            author: author.into(),
            association: "NONE".into(),
            body: body.into(),
            // Real time, one second apart: Provefab compares them with its own clock.
            created_at: crate::store::rfc3339(crate::store::now() + n as i64),
        });
    }

    pub fn last_labels(&self) -> (Vec<String>, Vec<String>) {
        self.labels
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

impl Hub for FakeHub {
    async fn issue(&self, _: &str, _: u64) -> Result<Issue, ForgeError> {
        if self.issue_missing.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ForgeError::Failed {
                program: "gh".into(),
                args: "issue view 7".into(),
                code: Some(1),
                stderr: "GraphQL: Could not resolve to an issue or pull request with the number of 7. (repository.issue) token=ghp_SECRET".into(),
            });
        }
        Ok(self.issue.clone())
    }
    async fn comments(&self, _: &str, _: u64) -> Result<Vec<Comment>, ForgeError> {
        if self.comments_down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ForgeError::Parse("gh".into(), "down".into()));
        }
        Ok(self.comments.lock().unwrap().clone())
    }
    async fn comment(&self, _: &str, _: u64, body: &str) -> Result<(), ForgeError> {
        use std::sync::atomic::Ordering;
        let left = self.comment_failures.load(Ordering::SeqCst);
        if left > 0 {
            self.comment_failures.store(left - 1, Ordering::SeqCst);
            return Err(ForgeError::Parse("gh comment".into(), "HTTP 502".into()));
        }
        self.posted.lock().unwrap().push(body.to_string());
        self.add_comment("me", &crate::forge::with_prefix(body));
        Ok(())
    }
    async fn edit_labels(
        &self,
        _: &str,
        _: u64,
        add: &[&str],
        remove: &[&str],
    ) -> Result<(), ForgeError> {
        if self.labels_down.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ForgeError::Parse("gh".into(), "down".into()));
        }
        use std::sync::atomic::Ordering;
        let left = self.label_failures.load(Ordering::SeqCst);
        if left > 0 {
            self.label_failures.store(left - 1, Ordering::SeqCst);
            return Err(ForgeError::Parse("gh label".into(), "HTTP 502".into()));
        }
        self.labels.lock().unwrap().push((
            add.iter().map(|s| s.to_string()).collect(),
            remove.iter().map(|s| s.to_string()).collect(),
        ));
        Ok(())
    }
    async fn pr_create(
        &self,
        _: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<String, ForgeError> {
        use std::sync::atomic::Ordering;
        if let Some(path) = self.break_on_pr_create.lock().unwrap().clone() {
            std::fs::write(path, "gitdir: /nonexistent\n").unwrap();
        }
        let left = self.pr_create_failures.load(Ordering::SeqCst);
        if left > 0 {
            self.pr_create_failures.store(left - 1, Ordering::SeqCst);
            return Err(ForgeError::Parse(
                "gh pr create".into(),
                "HTTP 502 token=ghp_SECRET".into(),
            ));
        }
        self.prs
            .lock()
            .unwrap()
            .push((head.into(), base.into(), title.into(), body.into()));
        Ok("https://github.com/o/r/pull/8".into())
    }
    async fn ensure_label(&self, _: &str, name: &str, _: &str, _: &str) -> Result<(), ForgeError> {
        self.ensured.lock().unwrap().push(name.to_string());
        Ok(())
    }
    async fn pr_status(&self, _: &str, _: &str) -> Result<crate::forge::PrStatus, ForgeError> {
        Ok(self.pr_status.lock().unwrap().clone())
    }
    async fn pr_merge(&self, _: &str, url: &str, head: &str) -> Result<(), ForgeError> {
        self.merged.lock().unwrap().push(format!("{url}@{head}"));
        Ok(())
    }
    async fn repo_is_public(&self, _: &str) -> Result<bool, ForgeError> {
        Ok(self.public.load(std::sync::atomic::Ordering::SeqCst))
    }
    async fn issue_open(&self, _: &str, _: u64) -> Result<bool, ForgeError> {
        self.issue_open_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.issue_is_open.load(std::sync::atomic::Ordering::SeqCst))
    }
    async fn repo_clone(&self, _: &str, dest: &Path) -> Result<(), ForgeError> {
        let Some(src) = self.clone_from.lock().unwrap().clone() else {
            return Err(ForgeError::Parse(
                "gh repo clone".into(),
                "no source".into(),
            ));
        };
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        git(
            parent_of(dest),
            &["clone", "-q", src.to_str().unwrap(), dest.to_str().unwrap()],
        );
        git(dest, &["config", "user.name", "t"]);
        git(dest, &["config", "user.email", "t@t"]);
        Ok(())
    }
}

// ---------- fixture ----------

pub fn parent_of(p: &Path) -> &Path {
    p.parent().unwrap_or(Path::new("/"))
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        // Hermetic: the gates' no-push `GIT_CONFIG_*` must not reach test repos.
        .env_remove("GIT_CONFIG_COUNT")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub origin: PathBuf,
    pub home: PathBuf,
    pub config: Config,
    pub policy: std::sync::Arc<dyn crate::policy::ReviewPolicy>,
}

pub const MODELS: &str = r#"
[jev]
model = "jev-1.13"

[[models]]
id = "std-claude"
worker = "claude-code"
model = "sonnet"
tier = "standard"

[[models]]
id = "std-codex"
worker = "codex"
model = "gpt-5.5"
tier = "standard"

[[models]]
id = "top-claude"
worker = "claude-code"
model = "opus"
tier = "frontier"
"#;

pub fn fixture(gates: &[&str]) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let origin = dir.path().join("origin.git");
    let local = dir.path().join("local");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&local).unwrap();
    git(
        dir.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(&local, &["init", "-q", "-b", "main"]);
    git(&local, &["config", "user.name", "t"]);
    git(&local, &["config", "user.email", "t@t"]);
    std::fs::write(local.join("README.md"), "hello\n").unwrap();
    git(&local, &["add", "-A"]);
    git(&local, &["commit", "-q", "-m", "init"]);
    git(
        &local,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&local, &["push", "-q", "origin", "main"]);
    let gates: Vec<String> = gates.iter().map(|g| format!("{g:?}")).collect();
    let toml = format!(
        "{MODELS}\n[[repos]]\nslug = \"o/r\"\nlocal_path = {:?}\ngates = [{}]\n\n[limits]\nstage_timeout = \"60s\"\ngate_timeout = \"60s\"\n",
        local.display().to_string(),
        gates.join(", ")
    );
    let config = Config::from_toml_str(&toml).unwrap();
    Fixture {
        _dir: dir,
        origin,
        home,
        config,
        policy: std::sync::Arc::new(crate::policy::OpenPrOnly),
    }
}

pub async fn pipeline<O: Oracle>(
    f: &Fixture,
    script: Box<Script>,
    oracle: O,
    hub: FakeHub,
) -> Pipeline<FakeRunner, O, FakeHub> {
    std::fs::create_dir_all(&f.home).unwrap();
    let paths = Paths::new(&f.home);
    Pipeline {
        store: Store::open(&paths.db()).await.unwrap(),
        runner: FakeRunner {
            script,
            calls: Mutex::new(Vec::new()),
            active: Default::default(),
        },
        oracle,
        hub,
        git: Git {
            program: "git".into(),
        },
        paths,
        config: f.config.clone(),
        cooldowns: Mutex::new(Cooldowns::default()),
        prices: std::sync::RwLock::new(
            crate::prices::PriceTable::from_models_dev(
                include_str!("../tests/fixtures/prices/models_dev.json"),
                1,
            )
            .unwrap(),
        ),
        repo_locks: Mutex::new(std::collections::HashMap::new()),
        budget: tokio::sync::Mutex::new(()),
        policy: f.policy.clone(),
    }
}

pub async fn queue<R: StageRunner + Sync, O: Oracle + Sync, H: Hub + Sync>(
    p: &Pipeline<R, O, H>,
) -> i64 {
    queue_n(p, 7, "Add a feature file").await
}

pub async fn queue_n<R: StageRunner + Sync, O: Oracle + Sync, H: Hub + Sync>(
    p: &Pipeline<R, O, H>,
    number: u64,
    title: &str,
) -> i64 {
    p.store
        .add_issue(&NewIssue {
            repo: "o/r".into(),
            number,
            url: format!("https://github.com/o/r/issues/{number}"),
            title: title.into(),
            author: "alice".into(),
        })
        .await
        .unwrap()
        .unwrap()
}

pub async fn states<R: StageRunner + Sync, O: Oracle + Sync, H: Hub + Sync>(
    p: &Pipeline<R, O, H>,
    id: i64,
) -> Vec<TaskState> {
    p.store
        .transitions(id)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.to)
        .collect()
}

pub fn verdict(kind: TaskKind, underspecified: f64) -> Verdict {
    Verdict {
        task_kind: kind,
        difficulty: 2.0,
        difficulty_confidence: 0.9,
        scope: 1.0,
        underspecified,
        jev_model: "jev-1.13.0".into(),
        plan_depth: None,
        review_risk: None,
    }
}

pub use TaskState::*;
