//! The `provefab` command line, as a library entry point: the free binary and
//! Provefab Pro both call `run` with their own `Extensions` (spec §3, D59).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use crate::agents::AgentRunner;
use crate::cli::{GuardFormat, run_guard};
use crate::commands::{self, Tools};
use crate::config::{Auth, Config, WorkerKind};
use crate::cooldown::Cooldowns;
use crate::forge::{Gh, Git};
use crate::intake::GhLabelPoller;
use crate::paths::Paths;
use crate::pipeline::Pipeline;
use crate::policy::{OpenPrOnly, ReviewPolicy};
use crate::ports::JevOracle;
use crate::scheduler::{self, RunOptions};
use crate::store::Store;
use crate::{codex_setup, plugins};
use anyhow::{Context, bail};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "provefab",
    version,
    about = "Turns labelled GitHub issues into tested pull requests"
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
    /// Check tools, logins, the Jev key and the repos.
    Doctor,
    /// Run `provefab run` as a launchd agent (start at login, restart on exit).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// One-time sign-in for a worker, in Provefab's own config directory.
    Login {
        #[arg(value_enum)]
        worker: LoginWorker,
        /// Sign this worker in with your own API key instead of your subscription
        /// (used by catalog models with `auth = "api_key"`).
        #[arg(long)]
        api_key: bool,
    },
    /// Internal: check one agent tool call (JSON on stdin) against the guard policy.
    Guard {
        #[arg(long, value_enum)]
        format: GuardFormat,
        /// The task's worktree. Workers set PROVEFAB_WORKTREE.
        #[arg(long, env = "PROVEFAB_WORKTREE")]
        root: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum ServiceAction {
    /// Copy this binary to <home>/bin, write the agent and load it.
    Install {
        #[arg(long, default_value_t = 1)]
        workers: usize,
    },
    /// Unload the agent and remove it.
    Uninstall,
    /// Whether the agent is loaded and running.
    Status,
}

#[derive(Clone, Copy, ValueEnum)]
enum LoginWorker {
    Claude,
    Codex,
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
    if let Cmd::Guard { format, root } = cli.cmd {
        // Synchronous and dependency-free: runs before every agent tool call.
        let mut stdin = String::new();
        // An unreadable stdin becomes "" and is denied by run_guard.
        let _ = std::io::stdin().read_to_string(&mut stdin);
        let out = run_guard(format, root.as_deref(), &stdin);
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
    Ok(Config::from_toml_str(&text)?)
}

async fn oracle(config: &Config) -> anyhow::Result<Option<JevOracle>> {
    let Some(key) = commands::typesafe_key().await else {
        eprintln!(
            "provefab: no TypeSafe key; Jev fallbacks apply (standard tier, no loop detector, no triage)"
        );
        return Ok(None);
    };
    let client = jev::JevClient::new(key, config.jev.model.clone(), crate::jevq::DEADLINE)?;
    Ok(Some(JevOracle { client }))
}

fn gh() -> Gh {
    Gh {
        program: "gh".into(),
    }
}

async fn dispatch(cmd: Cmd, ext: &Extensions) -> anyhow::Result<ExitCode> {
    let paths = Paths::from_env();
    match cmd {
        Cmd::Guard { .. } => unreachable!("handled before the runtime starts"),
        Cmd::Run {
            workers,
            dry_run,
            once,
        } => {
            let mut config = load_config(&paths)?;
            let policy = ext.policy.clone();
            for w in policy.warnings(&config) {
                eprintln!("provefab: {w}");
            }
            policy.check(&config).map_err(anyhow::Error::msg)?;
            let oracle = oracle(&config).await?;
            let source = GhLabelPoller { gh: gh() };
            if dry_run {
                for line in scheduler::dry_run(&config, &source, &oracle).await? {
                    println!("{line}");
                }
                return Ok(ExitCode::SUCCESS);
            }
            let Some(_lock) = commands::lock(&paths.home.join("run.lock"))? else {
                bail!("another `provefab run` is already working on this queue");
            };
            // Spec §7: models whose worker is not signed in leave the catalog.
            let checks = commands::doctor(
                &Tools::default(),
                &config,
                &paths,
                oracle.as_ref().map(|o| &o.client),
            )
            .await;
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
                    (WorkerKind::ClaudeCode, Auth::ApiKey) => {
                        failed("claude") || failed("claude api key")
                    }
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
            let pipeline = Arc::new(Pipeline {
                store: Store::open(&paths.db()).await?,
                runner: AgentRunner::new(&paths, &installed, exe),
                oracle,
                hub: gh(),
                git: Git {
                    program: "git".into(),
                },
                paths: paths.clone(),
                config,
                cooldowns: Mutex::new(Cooldowns::default()),
                repo_locks: Mutex::new(std::collections::HashMap::new()),
                budget: tokio::sync::Mutex::new(()),
                policy,
            });
            let stop = async {
                let _ = tokio::signal::ctrl_c().await;
            };
            scheduler::run(pipeline, &source, RunOptions { workers, once }, stop).await?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Add { url } => {
            let config = load_config(&paths)?;
            let store = Store::open(&paths.db()).await?;
            let git = Git {
                program: "git".into(),
            };
            let out = commands::add(&store, &config, &gh(), &git, &paths, &url).await?;
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
        Cmd::Doctor => {
            let config = load_config(&paths)?;
            let policy = ext.policy.clone();
            for w in policy.warnings(&config) {
                println!("warn {w}");
            }
            let policy_ok = match policy.check(&config) {
                Ok(()) => true,
                Err(e) => {
                    println!("FAIL {:<22} {e}", "merge settings");
                    false
                }
            };
            let oracle = oracle(&config).await?;
            let checks = commands::doctor(
                &Tools::default(),
                &config,
                &paths,
                oracle.as_ref().map(|o| &o.client),
            )
            .await;
            let mut ok = policy_ok;
            for c in &checks {
                // The Jev key is optional: its absence is reported, not fatal.
                ok &= c.ok || c.name == "jev key";
                println!(
                    "{} {:<22} {}",
                    if c.ok { "ok  " } else { "FAIL" },
                    c.name,
                    c.detail
                );
            }
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        Cmd::Service { action } => {
            let service = crate::service::Service::for_user(&paths.home);
            match action {
                ServiceAction::Install { workers } => {
                    // Fail before installing an agent that could never start.
                    load_config(&paths)?;
                    let exe = std::env::current_exe().context("locating Provefab binary")?;
                    let path_env = std::env::var("PATH").unwrap_or_default();
                    let plist = service.install(&exe, workers, &path_env).await?;
                    println!(
                        "installed {} (binary {}, logs {})",
                        plist.display(),
                        service.binary().display(),
                        paths.home.join("logs").join("run.log").display()
                    );
                    println!("{}", service.status().await);
                }
                ServiceAction::Uninstall => {
                    service.uninstall().await?;
                    println!("uninstalled");
                }
                ServiceAction::Status => println!("{}", service.status().await),
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Login {
            worker,
            api_key: true,
        } => {
            match worker {
                LoginWorker::Claude => {
                    // The key goes to the Keychain (prompted without echo);
                    // Claude Code reads it there through `apiKeyHelper`.
                    println!("Enter your Anthropic API key at the Keychain prompt.");
                    let status = std::process::Command::new("security")
                        .args([
                            "add-generic-password",
                            "-U",
                            "-s",
                            commands::ANTHROPIC_KEYCHAIN_SERVICE,
                            "-a",
                            "provefab",
                            "-w",
                        ])
                        .status()
                        .context("running security")?;
                    if !status.success() {
                        bail!("could not store the key in the Keychain");
                    }
                    commands::claude_api_settings(&paths.claude_config_api())?;
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
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Login { worker, .. } => {
            let (program, var, dir) = match worker {
                LoginWorker::Claude => ("claude", "CLAUDE_CONFIG_DIR", paths.claude_config()),
                LoginWorker::Codex => ("codex", "CODEX_HOME", paths.codex_home()),
            };
            std::fs::create_dir_all(&dir)?;
            let args: &[&str] = match worker {
                LoginWorker::Claude => &["auth", "login"],
                LoginWorker::Codex => &["login"],
            };
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
