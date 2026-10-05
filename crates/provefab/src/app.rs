//! The `provefab` command line, as a library entry point: the free binary and
//! Provefab Pro both call `run` with their own `Extensions` (spec §3, D59).

use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use crate::agents::AgentRunner;
use crate::cli::{GuardFormat, run_guard, run_guard_no_tools, run_guard_review};
use crate::commands::{self, Check, Tools};
use crate::config::{Auth, Config, WorkerKind};
use crate::cooldown::Cooldowns;
use crate::forge::{Gh, Git};
use crate::paths::Paths;
use crate::pipeline::Pipeline;
use crate::policy::{OpenPrOnly, ReviewPolicy};
use crate::ports::JevOracle;
use crate::scheduler::{self, RunOptions};
use crate::secrets::Secrets;
use crate::setup::{self, SetupError};
use crate::store::Store;
use crate::{codex_setup, plugins};
use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "provefab",
    version,
    about = "Turns labelled GitHub, Jira or Linear issues into tested pull requests"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Poll the configured repos and work the queue.
    Run {
        /// Tasks worked on at the same time.
        #[arg(long, default_value_t = 1)]
        workers: usize,
        /// Classify and route open labelled issues, print the decisions, change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Poll once, work everything that can move, then exit.
        #[arg(long)]
        once: bool,
    },
    /// Queue one issue by URL (or requeue a finished or stopped one).
    Add { url: String },
    /// Every task and why it is in its state.
    Status,
    /// Per repository: issues turned into PRs, merges (automatic or by hand), reviewers.
    Stats,
    /// Everything recorded about one task.
    Log { task: i64 },
    /// The evidence record as JSON Lines (free text redacted unless --with-text).
    Export {
        #[arg(long)]
        repo: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        with_text: bool,
    },
    /// Delete the record of finished tasks and old maintenance runs from before a date (dry run without --yes).
    Prune {
        #[arg(long)]
        before: String,
        #[arg(long)]
        yes: bool,
    },
    /// Check tools, logins, the Jev key and the repos.
    #[command(after_help = setup::EXIT_CODES)]
    Doctor {
        /// One JSON object per check and line: name, ok, detail, and fix
        /// when a command fixes it.
        #[arg(long)]
        json: bool,
    },
    /// Write ~/.provefab/provefab.toml, with the models of the worker CLIs on PATH.
    #[command(after_help = setup::EXIT_CODES)]
    Init {
        /// Print the file instead of writing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// The repositories Provefab watches.
    #[command(after_help = setup::EXIT_CODES)]
    Repos {
        #[command(subcommand)]
        action: ReposAction,
    },
    /// Run `provefab run` as a background service: a launchd agent on macOS,
    /// a systemd user service on Linux (starts by itself, restarts on exit).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// One-time sign-in for a worker, in Provefab's own config directory, or
    /// the Jev key or the credentials of a Jira site or a Linear workspace
    /// (stored in the macOS Keychain, or on Linux in credentials.toml).
    Login {
        #[arg(value_enum)]
        worker: LoginWorker,
        /// Sign this worker in with your own API key instead of your subscription
        /// (used by catalog models with `auth = "api_key"`).
        #[arg(long)]
        api_key: bool,
        /// Jira only: the site host name, such as acme.atlassian.net.
        #[arg(long)]
        site: Option<String>,
    },
    /// Internal: print one stored secret for a worker's key helper.
    #[command(hide = true)]
    Secrets {
        #[command(subcommand)]
        action: SecretsAction,
    },
    /// Internal: check one agent tool call (JSON on stdin) against the guard policy.
    Guard {
        #[arg(long, value_enum)]
        format: GuardFormat,
        /// The task's worktree. Workers set PROVEFAB_WORKTREE.
        #[arg(long, env = "PROVEFAB_WORKTREE")]
        root: Option<PathBuf>,
        /// Refuse every call but the structured answer. Workers set
        /// PROVEFAB_NO_TOOLS for a stage without tools; any value but a
        /// false one (0, false, no, off) turns it on.
        #[arg(long, env = agent_workers::NO_TOOLS_ENV, value_parser = clap::builder::FalseyValueParser::new())]
        no_tools: bool,
        /// Refuse every shell command but read-only ones (`cat`, `grep`,
        /// `git diff`, ...) and every write. Workers set
        /// PROVEFAB_UNTRUSTED_REVIEW for a review of a person's pull request.
        #[arg(long, env = agent_workers::UNTRUSTED_REVIEW_ENV, value_parser = clap::builder::FalseyValueParser::new())]
        untrusted_review: bool,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Copy this binary to <home>/bin, write the service and (re)start it.
    Install {
        #[arg(long, default_value_t = 1)]
        workers: usize,
    },
    /// Stop the service and remove it.
    Uninstall,
    /// Whether the service is loaded and running.
    Status,
}

#[derive(Subcommand)]
enum SecretsAction {
    /// Print the stored secret to stdout, and nothing else.
    Get {
        #[arg(value_enum)]
        name: SecretArg,
    },
}

/// The only secret a worker asks Provefab for (plan decision 10).
#[derive(Clone, Copy, ValueEnum)]
enum SecretArg {
    Anthropic,
}

#[derive(Subcommand)]
enum ReposAction {
    /// Append one [[repos]] block to provefab.toml: base from the default
    /// branch, gates detected from the files at the repository's root.
    #[command(after_help = setup::EXIT_CODES)]
    Add {
        /// The GitHub repository, as owner/name.
        slug: String,
        /// A local clone of that repository, read instead of GitHub.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Print the block instead of adding it.
        #[arg(long)]
        dry_run: bool,
    },
}

/// A setup command's output, or its error on one stderr line with its exit
/// code (spec section 6).
fn setup_exit(r: Result<String, SetupError>) -> ExitCode {
    match r {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("provefab: {e}");
            ExitCode::from(e.code())
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum LoginWorker {
    Claude,
    Codex,
    /// The TypeSafe (Jev) key.
    Jev,
    /// Jira Cloud: account e-mail and API token, per site.
    Jira,
    /// Linear: a personal API key.
    Linear,
}

/// A command Provefab Pro adds to the CLI.
pub struct Extra {
    pub command: clap::Command,
    pub run: fn(&clap::ArgMatches) -> ExitCode,
}

/// What a binary built on the core plugs in (spec §3, D59).
pub struct Extensions {
    /// Binary name shown by `--help`, `--version` and `doctor`.
    pub name: &'static str,
    pub policy: Arc<dyn ReviewPolicy>,
    pub commands: Vec<Extra>,
}

impl Default for Extensions {
    fn default() -> Self {
        Self {
            name: "provefab",
            policy: Arc::new(OpenPrOnly),
            commands: Vec::new(),
        }
    }
}

/// The CLI with the extensions' name and extra commands.
pub fn cli_command(ext: &Extensions) -> clap::Command {
    let mut cmd = <Cli as clap::CommandFactory>::command()
        .name(ext.name)
        .bin_name(ext.name);
    for e in &ext.commands {
        cmd = cmd.subcommand(e.command.clone());
    }
    cmd
}

/// Runs the CLI on this process's arguments.
pub fn run(ext: Extensions) -> ExitCode {
    run_from(ext, std::env::args_os())
}

pub fn run_from<I, T>(ext: Extensions, args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let matches = cli_command(&ext).get_matches_from(args);
    if let Some((sub, m)) = matches.subcommand()
        && let Some(e) = ext.commands.iter().find(|e| e.command.get_name() == sub)
    {
        return (e.run)(m);
    }
    let cli = match <Cli as clap::FromArgMatches>::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    if let Cmd::Guard {
        format,
        root,
        no_tools,
        untrusted_review,
    } = cli.cmd
    {
        // Synchronous and dependency-free: runs before every agent tool call.
        let mut stdin = String::new();
        // An unreadable stdin becomes "" and is denied by run_guard.
        let _ = std::io::stdin().read_to_string(&mut stdin);
        let out = if no_tools {
            run_guard_no_tools(format, &stdin)
        } else if untrusted_review {
            run_guard_review(format, root.as_deref(), &stdin)
        } else {
            run_guard(format, root.as_deref(), &stdin)
        };
        let _ = std::io::stdout().write_all(out.stdout.as_bytes());
        let _ = std::io::stderr().write_all(out.stderr.as_bytes());
        return ExitCode::from(out.exit_code as u8);
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("provefab: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(dispatch(cli.cmd, &ext)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("provefab: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn load_config(paths: &Paths) -> anyhow::Result<Config> {
    let path = paths.config();
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    // R3: the loader quotes the offending line, which may hold a secret.
    Config::from_toml_str(&text)
        .map_err(|e| anyhow::anyhow!(crate::rules::redact_credentials(&format!("{e:#}"))))
}

/// The Jev client: `Ok(None)` without a key, the inner `Err` when the store
/// holding the key could not be read.
async fn jev_client(
    config: &Config,
    secrets: &Secrets,
) -> anyhow::Result<Result<Option<JevOracle>, crate::secrets::SecretError>> {
    let key = match commands::typesafe_key(secrets, &crate::tracker::process_env).await {
        Ok(Some(key)) => key,
        Ok(None) => return Ok(Ok(None)),
        Err(e) => return Ok(Err(e)),
    };
    let client = jev::JevClient::new(key, config.jev.model.clone(), crate::jevq::DEADLINE)?;
    Ok(Ok(Some(JevOracle { client })))
}

async fn oracle(config: &Config, secrets: &Secrets) -> anyhow::Result<Option<JevOracle>> {
    match jev_client(config, secrets).await? {
        Ok(Some(o)) => Ok(Some(o)),
        Ok(None) => {
            eprintln!(
                "provefab: no TypeSafe key; Jev fallbacks apply (standard tier, no loop detector, no triage)"
            );
            Ok(None)
        }
        Err(e) => {
            eprintln!("provefab: {e}; Jev fallbacks apply");
            Ok(None)
        }
    }
}

fn gh() -> Gh {
    Gh {
        program: "gh".into(),
    }
}

/// The hub `run` and `add` use: each repository's issues on its tracker, code
/// on GitHub. Missing Jira or Linear credentials stop the command here, with
/// the `provefab login` to run.
async fn routed(config: &Config, secrets: &Secrets) -> anyhow::Result<crate::tracker::Routed> {
    crate::tracker::Routed::from_config(config, gh(), secrets, &crate::tracker::process_env)
        .await
        .map_err(anyhow::Error::msg)
}

/// `provefab login jev|jira|linear` (spec §5, Linux spec §3): the secret is
/// typed at the Keychain's prompt on macOS, or read with echo off into
/// credentials.toml on Linux; it never passes on a command line.
fn secret_login(
    secrets: &Secrets,
    worker: LoginWorker,
    site: Option<String>,
) -> anyhow::Result<()> {
    use crate::secrets::Entry;
    use crate::tracker::is_host;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let (entry, what, done) = match worker {
        LoginWorker::Jira => {
            let Some(site) = site.map(|s| s.trim().to_lowercase()).filter(|s| is_host(s)) else {
                bail!(
                    "give the Jira site as a host name: provefab login jira --site acme.atlassian.net"
                );
            };
            print!("Atlassian account e-mail for {site}: ");
            std::io::stdout().flush()?;
            let mut email = String::new();
            input.read_line(&mut email)?;
            let email = email.trim().to_string();
            if !email.contains('@') {
                bail!("an account e-mail address is required");
            }
            let done = format!("stored; repositories with kind = \"jira\" and site = \"{site}\" use it");
            (
                Entry::Jira { site, email },
                "your Jira API token (create one at https://id.atlassian.com/manage-profile/security/api-tokens)",
                done,
            )
        }
        LoginWorker::Linear | LoginWorker::Jev if site.is_some() => {
            bail!("--site is for `provefab login jira` only")
        }
        LoginWorker::Linear => (
            Entry::Linear,
            "a Linear personal API key (from Linear's settings)",
            "stored; repositories with kind = \"linear\" use it".to_string(),
        ),
        LoginWorker::Jev => (
            Entry::Typesafe,
            "your TypeSafe (Jev) API key",
            "stored; Provefab reads it at its next start (if you run the service, `provefab service install` restarts it)".to_string(),
        ),
        LoginWorker::Claude | LoginWorker::Codex => {
            unreachable!("worker logins are handled in dispatch")
        }
    };
    secrets.store(&entry, what, &mut input, crate::secrets::stdin_is_tty())?;
    println!("{done}");
    Ok(())
}

/// The worker's sign-in command (Linux spec §5, plan decision 11).
fn login_args(worker: LoginWorker, device_code: bool) -> &'static [&'static str] {
    match worker {
        LoginWorker::Claude => &["auth", "login"],
        LoginWorker::Codex if device_code => &["login", "--device-auth"],
        LoginWorker::Codex => &["login"],
        LoginWorker::Jev | LoginWorker::Jira | LoginWorker::Linear => {
            unreachable!("secret logins are handled in secret_login")
        }
    }
}

/// The pipeline `provefab run` drives; Provefab Pro's commands build the same
/// one (repository rules plan decision 19).
pub type RunPipeline = Pipeline<AgentRunner, Option<JevOracle>, crate::tracker::Routed>;

/// Loads `provefab.toml` and builds the pipeline `run` uses. It does not
/// take the run lock: a caller that must not work beside the service (Pro's
/// `rules propose`, final review I5) takes `commands::lock_run` first.
pub async fn open_pipeline(
    paths: &Paths,
    policy: Arc<dyn ReviewPolicy>,
) -> anyhow::Result<RunPipeline> {
    let config = load_config(paths)?;
    let secrets = Secrets::system(&paths.home);
    secrets.preflight()?;
    let oracle = oracle(&config, &secrets).await?;
    let hub = routed(&config, &secrets).await?;
    let store = Store::open(&paths.db()).await?;
    build_pipeline(paths, config, oracle, hub, store, policy).await
}

/// Spec §7: models whose worker is not signed in leave the catalog; then the
/// plugins, the prices and the pipeline itself.
async fn build_pipeline(
    paths: &Paths,
    mut config: Config,
    oracle: Option<JevOracle>,
    hub: crate::tracker::Routed,
    store: Store,
    policy: Arc<dyn ReviewPolicy>,
) -> anyhow::Result<RunPipeline> {
    let tools = Tools {
        secrets: Secrets::system(&paths.home),
        ..Tools::default()
    };
    let checks = commands::doctor(&tools, &config, paths, oracle.as_ref().map(|o| &o.client)).await;
    let failed = |name: &str| checks.iter().any(|c| c.name == name && !c.ok);
    if let Some(c) = checks.iter().find(|c| c.name == "jev" && !c.ok) {
        eprintln!(
            "provefab: Jev does not answer ({}); fallbacks apply",
            c.detail
        );
    }
    // Each sign-in mode stands alone: a missing subscription login
    // never removes the same vendor's API-key models (BYOK).
    config.models.retain(|m| {
        let gone = match (m.worker, m.auth) {
            (WorkerKind::ClaudeCode, Auth::Subscription) => {
                failed("claude") || failed("claude login")
            }
            (WorkerKind::ClaudeCode, Auth::ApiKey) => failed("claude") || failed("claude api key"),
            (WorkerKind::Codex, Auth::Subscription) => {
                failed("codex") || failed("codex login") || failed("codex guard hook")
            }
            (WorkerKind::Codex, Auth::ApiKey) => {
                failed("codex") || failed("codex api login") || failed("codex api guard hook")
            }
            (WorkerKind::Pi, _) => failed("pi"),
        };
        if gone {
            eprintln!(
                "provefab: model {} removed: its worker is not ready (see `provefab doctor`)",
                m.id
            );
        }
        !gone
    });
    if config.models.is_empty() {
        bail!("no model is usable; run `provefab doctor`");
    }
    let installed = plugins::install(&paths.plugins())?;
    let exe = std::env::current_exe().context("locating Provefab binary")?;
    // Up to 2 x 10 s offline before the service starts; then the cache or snapshot.
    let prices = crate::prices::load(paths, config.routing.price_urls(), crate::store::now()).await;
    Ok(Pipeline {
        store,
        runner: AgentRunner::new(paths, &installed, exe),
        oracle,
        hub,
        git: Git {
            program: "git".into(),
        },
        paths: paths.clone(),
        config,
        cooldowns: Mutex::new(Cooldowns::default()),
        prices: std::sync::RwLock::new(prices),
        price_attempt: std::sync::atomic::AtomicI64::new(0),
        repo_locks: Mutex::new(std::collections::HashMap::new()),
        budget: tokio::sync::Mutex::new(()),
        policy,
    })
}

async fn dispatch(cmd: Cmd, ext: &Extensions) -> anyhow::Result<ExitCode> {
    let paths = Paths::from_env();
    // A home this command creates is its owner's only; `doctor` and the
    // key helper only read.
    if !matches!(cmd, Cmd::Doctor { .. } | Cmd::Secrets { .. }) {
        paths
            .ensure_home()
            .with_context(|| format!("creating {}", paths.home.display()))?;
    }
    match cmd {
        Cmd::Guard { .. } => unreachable!("handled before the runtime starts"),
        Cmd::Run {
            workers,
            dry_run,
            once,
        } => {
            let config = load_config(&paths)?;
            let secrets = Secrets::system(&paths.home);
            secrets.preflight()?;
            let policy = ext.policy.clone();
            for w in policy.warnings(&config) {
                eprintln!("provefab: {w}");
            }
            policy.check(&config).map_err(anyhow::Error::msg)?;
            let oracle = oracle(&config, &secrets).await?;
            let hub = routed(&config, &secrets).await?;
            if dry_run {
                for line in scheduler::dry_run(&config, &hub, &oracle).await? {
                    println!("{line}");
                }
                return Ok(ExitCode::SUCCESS);
            }
            let _lock = commands::lock_run_or_explain(&paths)?;
            let store = Store::open(&paths.db()).await?;
            commands::tracker_history(&store, &config).await?;
            let pipeline =
                Arc::new(build_pipeline(&paths, config, oracle, hub, store, policy).await?);
            scheduler::run(pipeline, RunOptions { workers, once }, shutdown_signal()).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Add { url } => {
            let config = load_config(&paths)?;
            let hub = routed(&config, &Secrets::system(&paths.home)).await?;
            let store = Store::open(&paths.db()).await?;
            let git = Git {
                program: "git".into(),
            };
            let out = commands::add(&store, &config, &hub, &git, &paths, &url).await?;
            println!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Status => {
            let store = Store::open(&paths.db()).await?;
            print!("{}", commands::status(&store).await?);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Stats => {
            let store = Store::open(&paths.db()).await?;
            print!("{}", commands::stats(&store).await?);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Log { task } => {
            let store = Store::open(&paths.db()).await?;
            print!("{}", commands::log(&store, task).await?);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Export {
            repo,
            since,
            with_text,
        } => {
            let store = Store::open(&paths.db()).await?;
            print!(
                "{}",
                commands::export(&store, repo.as_deref(), since.as_deref(), with_text).await?
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Prune { before, yes } => {
            let store = Store::open(&paths.db()).await?;
            print!("{}", commands::prune(&store, &before, yes).await?);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Doctor { json } => {
            let config = match load_config(&paths) {
                Ok(c) => c,
                Err(e) if json => {
                    print!(
                        "{}",
                        setup::doctor_json(&[setup::config_check(&paths, &e)], None)
                    );
                    return Ok(ExitCode::FAILURE);
                }
                Err(e) => return Err(anyhow::anyhow!(setup::one_line(&format!("{e:#}")))),
            };
            let policy = ext.policy.clone();
            let warnings = policy.warnings(&config);
            let refused = policy.check(&config).err().map(|e| Check {
                name: "merge settings".into(),
                ok: false,
                detail: e.to_string(),
            });
            if !json {
                for w in &warnings {
                    println!("warn {w}");
                }
                if let Some(c) = &refused {
                    println!("FAIL {:<22} {}", c.name, c.detail);
                }
            }
            let tools = Tools {
                secrets: Secrets::system(&paths.home),
                ..Tools::default()
            };
            let (oracle, jev_failed, store_failed) = match jev_client(&config, &tools.secrets).await
            {
                Ok(Ok(o)) => (o, None, None),
                Ok(Err(e)) => (None, None, Some(e)),
                Err(e) if json => (None, Some(e), None),
                Err(e) => return Err(e),
            };
            let mut checks =
                commands::doctor(&tools, &config, &paths, oracle.as_ref().map(|o| &o.client)).await;
            checks.extend(
                commands::tracker_checks(&tools, &config, &crate::tracker::process_env).await,
            );
            checks.extend(commands::rules_checks(&tools, &config, &paths, &gh()).await);
            checks.extend(commands::credentials_checks(&tools.secrets));
            checks.extend(
                crate::service::ServiceManager::for_user(&paths.home)
                    .checks()
                    .await,
            );
            if let Some(e) = &jev_failed {
                setup::jev_unavailable(&mut checks, e);
            }
            // The key's store could not be read: say why on the `jev key`
            // line, whose fix then is the `credentials` line's.
            if let Some(e) = &store_failed {
                for c in checks.iter_mut().filter(|c| c.name == "jev key") {
                    c.detail = e.to_string();
                }
            }
            if json {
                // Plan decision 6: every finding is a line.
                let mut all: Vec<Check> = warnings
                    .into_iter()
                    .map(|w| Check {
                        name: "warning".into(),
                        ok: true,
                        detail: w,
                    })
                    .collect();
                all.extend(refused);
                all.extend(checks);
                print!("{}", setup::doctor_json(&all, Some(&config)));
                return Ok(if setup::doctor_passed(&all) {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                });
            }
            for c in &checks {
                println!(
                    "{} {:<22} {}",
                    if c.ok { "ok  " } else { "FAIL" },
                    c.name,
                    c.detail
                );
            }
            Ok(if refused.is_none() && setup::doctor_passed(&checks) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Cmd::Init { dry_run } => Ok(setup_exit(setup::init(
            &paths,
            &std::env::var_os("PATH").unwrap_or_default(),
            dry_run,
        ))),
        Cmd::Repos {
            action:
                ReposAction::Add {
                    slug,
                    path,
                    dry_run,
                },
        } => {
            let git = Git {
                program: "git".into(),
            };
            Ok(setup_exit(
                setup::repos_add(&paths, &gh(), &git, &slug, path.as_deref(), dry_run).await,
            ))
        }
        Cmd::Service { action } => {
            let service = crate::service::ServiceManager::for_user(&paths.home);
            match action {
                ServiceAction::Install { workers } => {
                    // Fail before installing a service that could never start.
                    load_config(&paths)?;
                    let exe = std::env::current_exe().context("locating Provefab binary")?;
                    let path_env = std::env::var("PATH").unwrap_or_default();
                    let file = service.install(&exe, workers, &path_env).await?;
                    println!(
                        "installed {} (binary {}, logs {})",
                        file.display(),
                        service.binary().display(),
                        paths.home.join("logs").join("run.log").display()
                    );
                    println!("{}", service.status().await);
                    for note in service.notes().await {
                        println!("{note}");
                    }
                }
                ServiceAction::Uninstall => {
                    service.uninstall().await?;
                    println!("uninstalled");
                }
                ServiceAction::Status => println!("{}", service.status().await),
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Secrets {
            action: SecretsAction::Get {
                name: SecretArg::Anthropic,
            },
        } => match Secrets::system(&paths.home)
            .get(&crate::secrets::Name::Anthropic)
            .await?
        {
            Some(key) => {
                println!("{key}");
                Ok(ExitCode::SUCCESS)
            }
            None => bail!("no Anthropic API key is stored: run `provefab login claude --api-key`"),
        },
        Cmd::Login {
            worker: worker @ (LoginWorker::Jira | LoginWorker::Linear | LoginWorker::Jev),
            api_key,
            site,
        } => {
            if api_key {
                bail!("--api-key is for claude and codex; jev, jira and linear always store a key");
            }
            secret_login(&Secrets::system(&paths.home), worker, site)?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Login { site: Some(_), .. } => bail!("--site is for `provefab login jira` only"),
        Cmd::Login {
            worker,
            api_key: true,
            ..
        } => {
            match worker {
                LoginWorker::Claude => {
                    // The helper is checked before the key is asked for.
                    let exe = std::env::current_exe().context("locating Provefab binary")?;
                    let helper =
                        commands::api_key_helper(&exe, &paths.home).map_err(anyhow::Error::msg)?;
                    let secrets = Secrets::system(&paths.home);
                    let stdin = std::io::stdin();
                    secrets.store(
                        &crate::secrets::Entry::Anthropic,
                        "your Anthropic API key",
                        &mut stdin.lock(),
                        crate::secrets::stdin_is_tty(),
                    )?;
                    commands::claude_api_settings(&paths.claude_config_api(), &helper)?;
                    println!(
                        "stored; Claude Code models with auth = \"api_key\" use it (config dir {})",
                        paths.claude_config_api().display()
                    );
                }
                LoginWorker::Codex => {
                    let dir = paths.codex_home_api();
                    std::fs::create_dir_all(&dir)?;
                    println!("Paste your OpenAI API key, then press Enter and Ctrl-D.");
                    let status = std::process::Command::new("codex")
                        .args(["login", "--with-api-key"])
                        .env("CODEX_HOME", &dir)
                        .status()
                        .context("running codex")?;
                    if !status.success() {
                        bail!("codex login --with-api-key failed");
                    }
                    codex_setup::setup("codex".as_ref(), &dir, &dir).await?;
                    println!(
                        "signed in; Codex models with auth = \"api_key\" use it (CODEX_HOME {}, guard hook trusted)",
                        dir.display()
                    );
                }
                LoginWorker::Jev | LoginWorker::Jira | LoginWorker::Linear => {
                    unreachable!("secret logins are handled above")
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Login { worker, .. } => {
            let (program, var, dir) = match worker {
                LoginWorker::Claude => ("claude", "CLAUDE_CONFIG_DIR", paths.claude_config()),
                LoginWorker::Codex => ("codex", "CODEX_HOME", paths.codex_home()),
                LoginWorker::Jev | LoginWorker::Jira | LoginWorker::Linear => {
                    unreachable!("secret logins are handled above")
                }
            };
            std::fs::create_dir_all(&dir)?;
            let args = login_args(worker, !cfg!(target_os = "macos"));
            let status = std::process::Command::new(program)
                .args(args)
                .env(var, &dir)
                .status()
                .with_context(|| format!("running {program}"))?;
            if !status.success() {
                bail!("{program} login failed");
            }
            if let LoginWorker::Codex = worker {
                codex_setup::setup(program.as_ref(), &dir, &dir).await?;
                println!("codex guard hook trusted in {}", dir.display());
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Completes on Ctrl-C or, on unix, SIGTERM (what launchd and systemd send on stop).
/// The SIGTERM handler is registered when this is called, so a signal raised
/// right afterwards is not lost.
pub fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
    async move {
        #[cfg(unix)]
        if let Some(term) = term.as_mut() {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linux spec §5: Codex on a server signs in with a device code.
    #[test]
    fn codex_uses_a_device_code_where_there_is_no_browser() {
        assert_eq!(
            login_args(LoginWorker::Codex, true),
            ["login", "--device-auth"]
        );
        assert_eq!(login_args(LoginWorker::Codex, false), ["login"]);
        assert_eq!(login_args(LoginWorker::Claude, true), ["auth", "login"]);
        assert_eq!(login_args(LoginWorker::Claude, false), ["auth", "login"]);
    }

    #[test]
    fn jev_is_a_login_target_and_secrets_is_hidden() {
        let cmd = cli_command(&Extensions::default());
        assert!(
            cmd.clone()
                .try_get_matches_from(["provefab", "login", "jev"])
                .is_ok()
        );
        assert!(
            cmd.clone()
                .try_get_matches_from(["provefab", "secrets", "get", "anthropic"])
                .is_ok()
        );
        assert!(
            cmd.clone()
                .try_get_matches_from(["provefab", "secrets", "get", "linear"])
                .is_err()
        );
        let help = cmd.clone().render_long_help().to_string();
        assert!(!help.contains("secrets"), "{help}");
    }
}
