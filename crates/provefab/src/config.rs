//! `provefab.toml`: `[jev]`, the `[[models]]` catalog, `[[repos]]` and `[limits]`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::task::Tier;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkerKind {
    Pi,
    ClaudeCode,
    /// The unmodified `codex` CLI, signed in with the user's own ChatGPT plan (spec D27).
    Codex,
}

/// How a Claude Code or Codex model signs in: the user's own subscription
/// login, or the user's own API key (bring your own key, switchable per model).
/// Pi reads its own provider credentials and ignores this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    #[default]
    Subscription,
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    pub worker: WorkerKind,
    /// Worker-specific: a Claude Code alias (`opus`) or a Pi `--model` pattern.
    pub model: String,
    /// Pi only: the `--provider` name.
    #[serde(default)]
    pub provider: String,
    pub tier: Tier,
    #[serde(default = "one")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub auth: Auth,
    /// `provider/model` key in the price table when the automatic match is wrong.
    #[serde(default)]
    pub price_id: Option<String>,
    /// Manual prices, USD per million tokens; override the fetched ones.
    #[serde(default)]
    pub price_in: Option<f64>,
    #[serde(default)]
    pub price_out: Option<f64>,
    #[serde(default)]
    pub price_cache_read: Option<f64>,
    #[serde(default)]
    pub price_cache_write: Option<f64>,
    /// Subscription models: how much quota a stage uses, relative to the
    /// vendor's cheapest model (default derived from prices, D72).
    #[serde(default)]
    pub quota_weight: Option<f64>,
}

fn one() -> u32 {
    1
}

impl ModelEntry {
    /// Rate limits hit an account, not a model, so cooldowns are keyed on this.
    /// What a rate limit pauses: the family, per sign-in mode. A subscription
    /// hitting its limit leaves the same vendor's API key usable.
    pub fn cooldown_key(&self) -> String {
        match (self.worker, self.auth) {
            (WorkerKind::Pi, _) | (_, Auth::Subscription) => self.provider_key(),
            (_, Auth::ApiKey) => format!("{}:api-key", self.provider_key()),
        }
    }

    pub fn provider_key(&self) -> String {
        match self.worker {
            WorkerKind::ClaudeCode => "claude-code".to_string(),
            WorkerKind::Codex => "codex".to_string(),
            WorkerKind::Pi => format!("pi:{}", self.provider),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JevConfig {
    pub model: String,
    #[serde(default = "default_underspecified")]
    pub underspecified_threshold: f64,
    #[serde(default = "default_loop")]
    pub loop_threshold: f64,
}

fn default_underspecified() -> f64 {
    0.7
}

fn default_loop() -> f64 {
    0.8
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Config {
    pub jev: JevConfig,
    pub models: Vec<ModelEntry>,
    #[serde(default)]
    pub repos: Vec<RepoConfig>,
    #[serde(default)]
    pub limits: Limits,
    /// Order inside a tier (D73).
    #[serde(default)]
    pub routing: crate::routing::Routing,
}

/// One repository Provefab watches (spec §2.3).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RepoConfig {
    /// `owner/name` on GitHub.
    pub slug: String,
    /// The user's checkout; `~` is expanded by `path()`. Worktrees are created from it.
    /// Absent: Provefab keeps its own clone under `<home>/repos` (D48).
    #[serde(default)]
    pub local_path: Option<PathBuf>,
    /// Read by Provefab Pro only (D61); the core keeps it uninterpreted.
    #[serde(default)]
    pub merge: Option<toml::Table>,
    /// Retired keys, kept only to refuse them with a pointer to `[repos.merge]` (D62).
    #[serde(default, rename = "auto_merge")]
    pub(crate) retired_auto_merge: Option<toml::Value>,
    #[serde(default, rename = "auto_merge_max_lines")]
    pub(crate) retired_auto_merge_max_lines: Option<toml::Value>,
    #[serde(default = "default_label")]
    pub label: String,
    /// Branch worktrees start from and pull requests target.
    #[serde(default = "default_base")]
    pub base: String,
    #[serde(default = "default_poll", with = "humantime_serde")]
    pub poll_interval: Duration,
    #[serde(default)]
    pub trust_pi_project: bool,
    /// Commands run in the worktree after every implement stage. At least one.
    pub gates: Vec<String>,
}

impl RepoConfig {
    /// Provefab clones this repo itself (no `local_path`, D48).
    pub fn managed(&self) -> bool {
        self.local_path.is_none()
    }

    /// The checkout worktrees come from: `local_path` with `~` expanded, or the
    /// provefab's own clone under `<provefab_home>/repos/<owner>/<name>`.
    pub fn path_in(&self, provefab_home: &Path) -> PathBuf {
        let Some(local) = &self.local_path else {
            return provefab_home.join("repos").join(&self.slug);
        };
        match local.strip_prefix("~") {
            Ok(rest) => std::env::var_os("HOME")
                .map(|h| PathBuf::from(h).join(rest))
                .unwrap_or_else(|| local.clone()),
            Err(_) => local.clone(),
        }
    }

    /// `path_in` the default provefab home (`PROVEFAB_HOME`, else `~/.provefab`).
    pub fn path(&self) -> PathBuf {
        self.path_in(&crate::paths::Paths::from_env().home)
    }
}

fn default_label() -> String {
    "provefab".into()
}

fn default_base() -> String {
    "main".into()
}

fn default_poll() -> Duration {
    Duration::from_secs(180)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Limits {
    #[serde(default = "default_stage_timeout", with = "humantime_serde")]
    pub stage_timeout: Duration,
    #[serde(default = "default_gate_timeout", with = "humantime_serde")]
    pub gate_timeout: Duration,
    #[serde(default)]
    pub max_turns: MaxTurns,
    #[serde(default = "default_review_rounds")]
    pub review_rounds: u32,
    /// Automatic passes per issue before Provefab stops (D53).
    #[serde(default = "default_max_auto_passes")]
    pub max_auto_passes: u32,
    /// Worker runs over any rolling 24 hours (D53).
    #[serde(default = "default_max_stage_runs_per_day")]
    pub max_stage_runs_per_day: u32,
    /// Steps one `drive` may take before the task is parked for the user (D53).
    #[serde(default = "default_max_drive_steps")]
    pub max_drive_steps: u32,
    /// Waits after each transient failure in a pass; one more failure needs the user (D51).
    #[serde(default = "default_retry_delays", deserialize_with = "durations")]
    pub retry_delays: Vec<Duration>,
}

/// A list of humantime durations such as `["5m", "15m"]`.
fn durations<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Duration>, D::Error> {
    let raw: Vec<humantime_serde::Serde<Duration>> = Deserialize::deserialize(d)?;
    Ok(raw.into_iter().map(|s| s.into_inner()).collect())
}

fn default_max_drive_steps() -> u32 {
    200
}

fn default_retry_delays() -> Vec<Duration> {
    [5, 15, 45]
        .into_iter()
        .map(|m| Duration::from_secs(m * 60))
        .collect()
}

fn default_max_auto_passes() -> u32 {
    3
}

fn default_max_stage_runs_per_day() -> u32 {
    60
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            stage_timeout: default_stage_timeout(),
            gate_timeout: default_gate_timeout(),
            max_turns: MaxTurns::default(),
            review_rounds: default_review_rounds(),
            max_auto_passes: default_max_auto_passes(),
            max_stage_runs_per_day: default_max_stage_runs_per_day(),
            retry_delays: default_retry_delays(),
            max_drive_steps: default_max_drive_steps(),
        }
    }
}

fn default_stage_timeout() -> Duration {
    Duration::from_secs(30 * 60)
}

fn default_gate_timeout() -> Duration {
    Duration::from_secs(20 * 60)
}

fn default_review_rounds() -> u32 {
    2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct MaxTurns {
    #[serde(default = "turns_plan")]
    pub plan: u32,
    #[serde(default = "turns_implement")]
    pub implement: u32,
    #[serde(default = "turns_review")]
    pub review: u32,
}

impl Default for MaxTurns {
    fn default() -> Self {
        Self {
            plan: turns_plan(),
            implement: turns_implement(),
            review: turns_review(),
        }
    }
}

fn turns_plan() -> u32 {
    40
}

fn turns_implement() -> u32 {
    150
}

fn turns_review() -> u32 {
    40
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("provefab.toml: {0}")]
    Parse(String),
    #[error(
        "provefab.toml: jev.model must be a pinned version such as `jev-1.13.0`, not `jev-latest`"
    )]
    UnpinnedJev,
    #[error("provefab.toml: model id `{0}` is used twice")]
    DuplicateModel(String),
    #[error("provefab.toml: model `{0}` runs on Pi but has no `provider`")]
    MissingProvider(String),
    #[error("provefab.toml: model `{0}` has max_concurrency = 0, so it could never run")]
    ZeroConcurrency(String),
    #[error("provefab.toml: repo `{0}` is not `owner/name`")]
    BadSlug(String),
    #[error("provefab.toml: repo `{0}` is listed twice")]
    DuplicateRepo(String),
    #[error("provefab.toml: repo `{0}` has no gates, so nothing would check the agents' work")]
    NoGates(String),
    #[error("provefab.toml: repo `{0}` has an empty or zero `{1}`")]
    EmptyField(String, &'static str),
    #[error(
        "provefab.toml: repo `{0}` uses `{1}`, which moved: write `[repos.merge]` with `auto` and `max_lines` instead (read by Provefab Pro)"
    )]
    RetiredKey(String, &'static str),
}

impl Config {
    pub fn from_toml_str(s: &str) -> Result<Self, ConfigError> {
        let cfg: Config = toml::from_str(s).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        // Spec D9: new Jev releases must not change routing silently.
        if self.jev.model == "jev-latest" {
            return Err(ConfigError::UnpinnedJev);
        }
        let mut seen = HashSet::new();
        for m in &self.models {
            if !seen.insert(m.id.as_str()) {
                return Err(ConfigError::DuplicateModel(m.id.clone()));
            }
            if m.worker == WorkerKind::Pi && m.provider.is_empty() {
                return Err(ConfigError::MissingProvider(m.id.clone()));
            }
            if m.max_concurrency == 0 {
                return Err(ConfigError::ZeroConcurrency(m.id.clone()));
            }
        }
        let mut slugs = HashSet::new();
        for r in &self.repos {
            if r.retired_auto_merge.is_some() {
                return Err(ConfigError::RetiredKey(r.slug.clone(), "auto_merge"));
            }
            if r.retired_auto_merge_max_lines.is_some() {
                return Err(ConfigError::RetiredKey(
                    r.slug.clone(),
                    "auto_merge_max_lines",
                ));
            }
            let parts: Vec<&str> = r.slug.split('/').collect();
            if parts.len() != 2 || parts.iter().any(|p| p.is_empty()) {
                return Err(ConfigError::BadSlug(r.slug.clone()));
            }
            if !slugs.insert(r.slug.as_str()) {
                return Err(ConfigError::DuplicateRepo(r.slug.clone()));
            }
            if r.gates.iter().all(|g| g.trim().is_empty()) {
                return Err(ConfigError::NoGates(r.slug.clone()));
            }
            // An empty label could match every open issue (the label is the authorization).
            if r.label.trim().is_empty() {
                return Err(ConfigError::EmptyField(r.slug.clone(), "label"));
            }
            if r.base.trim().is_empty() {
                return Err(ConfigError::EmptyField(r.slug.clone(), "base"));
            }
            if r.poll_interval.is_zero() {
                return Err(ConfigError::EmptyField(r.slug.clone(), "poll_interval"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
[jev]
model = "jev-1.13"
underspecified_threshold = 0.7
loop_threshold = 0.8

[[models]]
id = "claude-opus"
worker = "claude-code"
model = "opus"
provider = ""
tier = "frontier"
max_concurrency = 1

[[models]]
id = "codex-standard"
worker = "pi"
provider = "openai-codex"
model = "gpt-5"
tier = "standard"

[[repos]]
slug = "owner/name"
local_path = "~/code/name"
label = "provefab"
poll_interval = "3m"
trust_pi_project = false
gates = ["cargo fmt -- --check"]

[limits]
stage_timeout = "30m"
review_rounds = 2
"#;

    #[test]
    fn parses_the_spec_example() {
        let cfg = Config::from_toml_str(SPEC_EXAMPLE).unwrap();
        assert_eq!(cfg.jev.model, "jev-1.13");
        assert_eq!(cfg.models.len(), 2);
        assert_eq!(cfg.models[0].worker, WorkerKind::ClaudeCode);
        assert_eq!(cfg.models[1].max_concurrency, 1, "defaults to 1");
        assert_eq!(cfg.models[0].provider_key(), "claude-code");
        assert_eq!(cfg.models[1].provider_key(), "pi:openai-codex");
    }

    #[test]
    fn thresholds_default_when_omitted() {
        let cfg = Config::from_toml_str("models = []\n[jev]\nmodel = \"jev-1.13\"\n").unwrap();
        assert_eq!(cfg.jev.underspecified_threshold, 0.7);
        assert_eq!(cfg.jev.loop_threshold, 0.8);
    }

    #[test]
    fn rejects_unpinned_jev() {
        let err =
            Config::from_toml_str("models = []\n[jev]\nmodel = \"jev-latest\"\n").unwrap_err();
        assert_eq!(err, ConfigError::UnpinnedJev);
    }

    #[test]
    fn rejects_duplicate_ids_missing_provider_and_zero_concurrency() {
        let base = "[jev]\nmodel = \"jev-1.13\"\n";
        let dup = format!(
            "{base}[[models]]\nid=\"a\"\nworker=\"claude-code\"\nmodel=\"opus\"\ntier=\"fast\"\n[[models]]\nid=\"a\"\nworker=\"claude-code\"\nmodel=\"opus\"\ntier=\"fast\"\n"
        );
        assert_eq!(
            Config::from_toml_str(&dup).unwrap_err(),
            ConfigError::DuplicateModel("a".into())
        );

        let no_provider =
            format!("{base}[[models]]\nid=\"p\"\nworker=\"pi\"\nmodel=\"m\"\ntier=\"fast\"\n");
        assert_eq!(
            Config::from_toml_str(&no_provider).unwrap_err(),
            ConfigError::MissingProvider("p".into())
        );

        let zero = format!(
            "{base}[[models]]\nid=\"z\"\nworker=\"claude-code\"\nmodel=\"opus\"\ntier=\"fast\"\nmax_concurrency=0\n"
        );
        assert_eq!(
            Config::from_toml_str(&zero).unwrap_err(),
            ConfigError::ZeroConcurrency("z".into())
        );
    }

    #[test]
    fn unknown_tier_is_a_parse_error() {
        let bad = "[jev]\nmodel = \"jev-1.13\"\n[[models]]\nid=\"a\"\nworker=\"claude-code\"\nmodel=\"opus\"\ntier=\"ultra\"\n";
        assert!(matches!(
            Config::from_toml_str(bad),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn codex_worker_parses_and_keys_its_own_cooldown() {
        let cfg = Config::from_toml_str(
            "[jev]\nmodel = \"jev-1.13\"\n[[models]]\nid=\"codex-hi\"\nworker=\"codex\"\nmodel=\"gpt-5.5\"\ntier=\"frontier\"\n",
        )
        .unwrap();
        assert_eq!(cfg.models[0].worker, WorkerKind::Codex);
        assert_eq!(cfg.models[0].provider_key(), "codex");
    }

    const BASE: &str = "models = []\n[jev]\nmodel = \"jev-1.13.0\"\n";

    /// Each model runs on the user's subscription or an API key, and the two
    /// modes pause separately when a limit hits (BYOK, switchable per model).
    #[test]
    fn auth_defaults_to_subscription_and_api_keys_cool_down_separately() {
        let cfg = Config::from_toml_str("[jev]\nmodel = \"jev-1.13.0\"\n[[models]]\nid=\"sub\"\nworker=\"claude-code\"\nmodel=\"sonnet\"\ntier=\"standard\"\n[[models]]\nid=\"key\"\nworker=\"claude-code\"\nmodel=\"sonnet\"\ntier=\"standard\"\nauth=\"api_key\"\n")
        .unwrap();
        let (sub, key) = (&cfg.models[0], &cfg.models[1]);
        assert_eq!(sub.auth, Auth::Subscription);
        assert_eq!(key.auth, Auth::ApiKey);
        // Same family for cross-review...
        assert_eq!(sub.provider_key(), key.provider_key());
        // ...but separate cooldowns.
        assert_ne!(sub.cooldown_key(), key.cooldown_key());
        assert_eq!(sub.cooldown_key(), "claude-code");
        assert_eq!(key.cooldown_key(), "claude-code:api-key");
        assert!(Config::from_toml_str("[jev]\nmodel = \"jev-1.13.0\"\n[[models]]\nid=\"x\"\nworker=\"codex\"\nmodel=\"gpt\"\ntier=\"standard\"\nauth=\"password\"\n")
        .is_err());
    }

    #[test]
    fn the_merge_table_is_kept_uninterpreted() {
        let c = Config::from_toml_str(&format!(
            "{BASE}\n[[repos]]\nslug = \"o/r\"\ngates = [\"make\"]\n\n[repos.merge]\nauto = true\nmax_lines = 10\n"
        ))
        .unwrap();
        let m = c.repos[0].merge.as_ref().unwrap();
        assert_eq!(m["auto"].as_bool(), Some(true));
        assert_eq!(m["max_lines"].as_integer(), Some(10));
    }

    /// Plan 5 review focus 5: a retired key must never be silently ignored.
    #[test]
    fn retired_auto_merge_keys_fail_with_the_new_table_named() {
        for key in ["auto_merge = true", "auto_merge_max_lines = 10"] {
            let e = Config::from_toml_str(&format!(
                "{BASE}\n[[repos]]\nslug = \"o/r\"\ngates = [\"make\"]\n{key}\n"
            ))
            .unwrap_err();
            assert!(
                matches!(e, ConfigError::RetiredKey(ref s, _) if s == "o/r"),
                "{e:?}"
            );
            assert!(e.to_string().contains("[repos.merge]"), "{e}");
        }
    }

    #[test]
    fn repos_and_limits_parse_with_defaults() {
        let cfg = Config::from_toml_str(SPEC_EXAMPLE).unwrap();
        let r = &cfg.repos[0];
        assert_eq!(r.slug, "owner/name");
        assert_eq!(r.label, "provefab");
        assert_eq!(r.base, "main");
        assert_eq!(r.poll_interval, Duration::from_secs(180));
        assert_eq!(r.gates, vec!["cargo fmt -- --check"]);
        assert_eq!(cfg.limits.stage_timeout, Duration::from_secs(1800));
        assert_eq!(cfg.limits.gate_timeout, Duration::from_secs(1200));
        assert_eq!(cfg.limits.review_rounds, 2);
        assert_eq!(
            cfg.limits.max_turns,
            MaxTurns {
                plan: 40,
                implement: 150,
                review: 40
            }
        );
        let bare = Config::from_toml_str("models = []\n[jev]\nmodel = \"jev-1.13\"\n").unwrap();
        assert!(bare.repos.is_empty());
        assert_eq!(bare.limits, Limits::default());
    }

    #[test]
    fn repo_path_expands_home() {
        let cfg = Config::from_toml_str(SPEC_EXAMPLE).unwrap();
        let home = std::env::var("HOME").unwrap();
        assert_eq!(cfg.repos[0].path(), PathBuf::from(home).join("code/name"));
    }

    #[test]
    fn local_path_is_optional_and_resolves_under_home() {
        let cfg = Config::from_toml_str(
            "models = []\n[jev]\nmodel = \"jev-1.13.0\"\n[[repos]]\nslug = \"o/r\"\ngates = [\"make\"]\n",
        )
        .unwrap();
        let r = &cfg.repos[0];
        assert!(r.managed() && r.merge.is_none());
        assert_eq!(r.path_in(Path::new("/f")), PathBuf::from("/f/repos/o/r"));
        assert_eq!(
            (
                cfg.limits.max_auto_passes,
                cfg.limits.max_stage_runs_per_day
            ),
            (3, 60)
        );
        let own = Config::from_toml_str(SPEC_EXAMPLE).unwrap();
        assert!(!own.repos[0].managed());
        assert_eq!(own.repos[0].path_in(Path::new("/f")), own.repos[0].path());
    }

    #[test]
    fn rejects_bad_slug_duplicate_repo_and_missing_gates() {
        let base = "models = []\n[jev]\nmodel = \"jev-1.13\"\n";
        let repo = |slug: &str, gates: &str| {
            format!("[[repos]]\nslug = \"{slug}\"\nlocal_path = \"/r\"\ngates = [{gates}]\n")
        };
        let bad = format!("{base}{}", repo("noslash", "\"make\""));
        assert_eq!(
            Config::from_toml_str(&bad).unwrap_err(),
            ConfigError::BadSlug("noslash".into())
        );
        let dup = format!(
            "{base}{}{}",
            repo("a/b", "\"make\""),
            repo("a/b", "\"make\"")
        );
        assert_eq!(
            Config::from_toml_str(&dup).unwrap_err(),
            ConfigError::DuplicateRepo("a/b".into())
        );
        let none = format!("{base}{}", repo("a/b", ""));
        assert_eq!(
            Config::from_toml_str(&none).unwrap_err(),
            ConfigError::NoGates("a/b".into())
        );
    }

    /// Final review (minor, promoted): an empty label could make `gh issue list --label ""`
    /// match every open issue, which would bypass the label authorization (spec §3.3).
    #[test]
    fn review_rejects_empty_label_empty_base_and_zero_poll() {
        let base = "models = []\n[jev]\nmodel = \"jev-1.13\"\n";
        let with = |extra: &str| {
            format!(
                "{base}[[repos]]\nslug = \"a/b\"\nlocal_path = \"/r\"\ngates = [\"make\"]\n{extra}\n"
            )
        };
        assert_eq!(
            Config::from_toml_str(&with("label = \" \"")).unwrap_err(),
            ConfigError::EmptyField("a/b".into(), "label")
        );
        assert_eq!(
            Config::from_toml_str(&with("base = \"\"")).unwrap_err(),
            ConfigError::EmptyField("a/b".into(), "base")
        );
        assert_eq!(
            Config::from_toml_str(&with("poll_interval = \"0s\"")).unwrap_err(),
            ConfigError::EmptyField("a/b".into(), "poll_interval")
        );
    }
}
