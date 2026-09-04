mod config;
mod engine;
mod events;
mod health;
mod packet;
mod prompt;
mod runner;
mod scope;
mod server;
mod service;
mod shell;
mod store;
mod workspace;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::config::{Config, Overrides};
use crate::engine::{Approver, EventSink, RunRequest};
use crate::events::{Envelope, RunStatus};
use crate::packet::{Effort, Mode, Packet};
use crate::store::Store;

#[derive(Parser)]
#[command(
    name = "delegate",
    version,
    about = "Tiered task dispatcher: packets in, verified patches out"
)]
struct Cli {
    /// Extra config overlay, deep-merged last (repeatable)
    #[arg(long = "config", global = true, value_name = "FILE")]
    config: Vec<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write a new packet file
    New(NewArgs),
    /// Run a packet through the tier ladder
    Run(RunArgs),
    /// Re-run a stored run's packet, optionally on another tier
    Replay(ReplayArgs),
    /// List recent runs
    Log {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Show one run with its attempts
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Pass rates per class and tier
    Stats {
        #[arg(long)]
        class: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Resolved tier chains on this host, with live health
    Tiers {
        #[arg(long)]
        json: bool,
    },
    /// Print the packet JSON schema
    Schema,
    /// Manage configuration files
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Run the HTTP daemon
    Serve {
        #[arg(long)]
        listen: Option<String>,
    },
    /// Install and start the daemon as a user service (systemd or launchd)
    InstallService,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Write default config.yml and host.yml if they do not exist
    Init,
    /// Load and validate the merged configuration
    Check,
    /// Print the configuration layer paths
    Path,
}

#[derive(Args, Clone)]
struct Dispatch {
    #[arg(long)]
    tier: Option<String>,
    #[arg(long)]
    ceiling: Option<String>,
    #[arg(long)]
    mode: Option<Mode>,
    #[arg(long)]
    attempts: Option<u32>,
    /// Emit JSON event lines instead of human output
    #[arg(long)]
    json: bool,
    /// Approve escalations without asking
    #[arg(long, short = 'y')]
    yes: bool,
    /// Leave the temporary worktrees in place for inspection
    #[arg(long)]
    keep_worktree: bool,
}

impl Dispatch {
    fn overrides(&self) -> Overrides {
        Overrides {
            tier: self.tier.clone(),
            ceiling: self.ceiling.clone(),
            mode: self.mode,
            attempts: self.attempts,
        }
    }
}

#[derive(Args)]
struct NewArgs {
    #[arg(long)]
    class: String,
    #[arg(long)]
    goal: String,
    /// Allowed path or glob (repeatable)
    #[arg(long = "path")]
    paths: Vec<String>,
    #[arg(long)]
    verify: Option<String>,
    /// File the worker should read first (repeatable)
    #[arg(long = "read")]
    read: Vec<String>,
    #[arg(long)]
    notes: Option<String>,
    #[arg(long)]
    effort: Option<Effort>,
    #[arg(long)]
    timeout: Option<u64>,
    #[arg(long)]
    repo: Option<PathBuf>,
    /// Output file (default: <repo>/.delegate/packets/<id>.yml)
    #[arg(long, short = 'o')]
    out: Option<PathBuf>,
    /// Open the packet in $EDITOR before finishing
    #[arg(long)]
    edit: bool,
    /// Run the packet immediately
    #[arg(long)]
    run: bool,
    #[command(flatten)]
    dispatch: Dispatch,
}

#[derive(Args)]
struct RunArgs {
    packet: PathBuf,
    #[command(flatten)]
    dispatch: Dispatch,
}

#[derive(Args)]
struct ReplayArgs {
    id: String,
    #[command(flatten)]
    dispatch: Dispatch,
}

struct HumanSink {
    json: bool,
}

impl EventSink for HumanSink {
    fn emit(&mut self, env: &Envelope) {
        if self.json {
            if let Ok(line) = serde_json::to_string(env) {
                println!("{line}");
            }
        } else if let Some(line) = env.event.human() {
            println!("{line}");
        }
        let _ = std::io::stdout().flush();
    }
}

struct TtyApprover {
    yes: bool,
}

impl Approver for TtyApprover {
    fn approve(&mut self, tier: &str, reason: &str) -> bool {
        if self.yes {
            return true;
        }
        if !std::io::stdin().is_terminal() {
            eprintln!("{reason}; no terminal to ask, holding (use --yes to approve)");
            return false;
        }
        eprint!("Escalate to {tier}? [y/N] ");
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn dispatch(cli: Cli) -> Result<ExitCode> {
    match cli.cmd {
        Cmd::Schema => {
            println!("{}", Packet::schema_json()?);
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Config { cmd } => config_cmd(cmd, &cli.config),
        Cmd::New(args) => {
            let cfg = config::load(&cli.config)?;
            new_packet(&cfg, args)
        }
        Cmd::Run(args) => {
            let cfg = config::load(&cli.config)?;
            let packet = Packet::load(&args.packet)?;
            run_packet(&cfg, packet, &args.dispatch)
        }
        Cmd::Replay(args) => {
            let cfg = config::load(&cli.config)?;
            let packet = {
                let store = Store::open(&cfg.db_path())?;
                store.get_run(&args.id)?.packet
            };
            run_packet(&cfg, packet, &args.dispatch)
        }
        Cmd::Log { limit, json } => {
            let cfg = config::load(&cli.config)?;
            let store = Store::open(&cfg.db_path())?;
            let rows = store.list_runs(limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                for r in rows {
                    let passed = r.passed_tier.clone().unwrap_or_else(|| "-".to_string());
                    println!(
                        "{:<10} {:<16} {:<12} {:<9} {}→{} esc {} {}",
                        &r.id[..10.min(r.id.len())],
                        &r.created_at[..16.min(r.created_at.len())],
                        truncate(&r.class, 12),
                        r.status,
                        r.start_tier,
                        passed,
                        r.escalations,
                        truncate(&r.summary, 60)
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Show { id, json } => {
            let cfg = config::load(&cli.config)?;
            let store = Store::open(&cfg.db_path())?;
            let run = store.get_run(&id)?;
            let attempts = store.attempts(&run.id)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "run": run, "attempts": attempts })
                    )?
                );
            } else {
                println!(
                    "run {} · {} · {} · {}",
                    run.id, run.status, run.class, run.created_at
                );
                println!("repo {} · host {} · mode {}", run.repo, run.host, run.mode);
                println!(
                    "{}→{} · passed {} · {} escalation(s)",
                    run.start_tier,
                    run.ceiling,
                    run.passed_tier.clone().unwrap_or_else(|| "-".to_string()),
                    run.escalations
                );
                println!("goal: {}", truncate(run.packet.goal.trim(), 200));
                for a in &attempts {
                    println!(
                        "  {} #{} {} {:<7} {:.1}s in {} out {} files {}",
                        a.tier,
                        a.attempt,
                        a.model,
                        a.status,
                        a.duration_ms as f64 / 1000.0,
                        a.tokens_in,
                        a.tokens_out,
                        a.changed_files.len()
                    );
                    if a.status != "pass" && !a.verify_tail.trim().is_empty() {
                        for line in a
                            .verify_tail
                            .trim()
                            .lines()
                            .rev()
                            .take(12)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                        {
                            println!("      {line}");
                        }
                    }
                }
                if !run.summary.is_empty() {
                    println!("summary: {}", run.summary);
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Stats { class, json } => {
            let cfg = config::load(&cli.config)?;
            let store = Store::open(&cfg.db_path())?;
            let rows = store.stats(class.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                println!(
                    "{:<16} {:<5} {:>8} {:>7} {:>6} {:>8} {:>10} {:>9}",
                    "class", "tier", "attempts", "passes", "rate", "avg", "tokens_in", "tokens_out"
                );
                for r in rows {
                    println!(
                        "{:<16} {:<5} {:>8} {:>7} {:>5.0}% {:>7.1}s {:>10} {:>9}",
                        truncate(&r.class, 16),
                        r.tier,
                        r.attempts,
                        r.passes,
                        r.pass_rate * 100.0,
                        r.avg_ms / 1000.0,
                        r.tokens_in,
                        r.tokens_out
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Tiers { json } => {
            let cfg = config::load(&cli.config)?;
            let views = server::tier_views(&cfg, true);
            if json {
                println!("{}", serde_json::to_string_pretty(&views)?);
            } else {
                for t in views {
                    println!("{} {}", t.tier, t.label);
                    for c in t.chain {
                        let state = match (c.healthy, c.reason) {
                            (Some(true), _) => "up".to_string(),
                            (Some(false), Some(r)) => format!("down: {r}"),
                            (Some(false), None) => "down".to_string(),
                            (None, _) => "unchecked".to_string(),
                        };
                        let thinking = c
                            .thinking
                            .map(|t| format!(" thinking={t}"))
                            .unwrap_or_default();
                        println!("  {} {}{} [{}]", c.runner, c.model, thinking, state);
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Serve { listen } => {
            let cfg = config::load(&cli.config)?;
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                        tracing_subscriber::EnvFilter::new("info,tower_http=warn")
                    }),
                )
                .init();
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(server::serve(cfg, listen))?;
            Ok(ExitCode::SUCCESS)
        }
        Cmd::InstallService => {
            let cfg = config::load(&cli.config)?;
            service::install(&cfg)?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn config_cmd(cmd: ConfigCmd, extra: &[PathBuf]) -> Result<ExitCode> {
    match cmd {
        ConfigCmd::Init => {
            let base = config::config_path();
            let host = config::host_overlay_path();
            std::fs::create_dir_all(config::config_dir())?;
            if base.exists() {
                println!("exists: {}", base.display());
            } else {
                std::fs::write(&base, config::DEFAULT_CONFIG)?;
                println!("wrote {}", base.display());
            }
            if host.exists() {
                println!("exists: {}", host.display());
            } else {
                std::fs::write(&host, config::DEFAULT_HOST_OVERLAY)?;
                println!("wrote {}", host.display());
            }
            config::load(extra)?;
            Ok(ExitCode::SUCCESS)
        }
        ConfigCmd::Check => {
            let cfg = config::load(extra)?;
            println!(
                "ok: {} tier(s) [{}], {} class(es), mode {}, db {}",
                cfg.order.len(),
                cfg.order.join(" → "),
                cfg.classes.len(),
                cfg.mode,
                cfg.db_path().display()
            );
            Ok(ExitCode::SUCCESS)
        }
        ConfigCmd::Path => {
            for (path, required) in config::layer_paths(extra) {
                let state = if path.exists() {
                    "present"
                } else if required {
                    "missing"
                } else {
                    "absent (optional)"
                };
                println!("{} [{state}]", path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn new_packet(cfg: &Config, args: NewArgs) -> Result<ExitCode> {
    let repo_base = match &args.repo {
        Some(r) => r.clone(),
        None => std::env::current_dir()?,
    };
    let repo = workspace::toplevel(&repo_base)?;
    let mut packet = Packet::new(&args.class, &args.goal);
    packet.paths = args.paths;
    packet.verify = args.verify;
    packet.read = args.read;
    packet.notes = args.notes;
    packet.tier = args.dispatch.tier.clone();
    packet.ceiling = args.dispatch.ceiling.clone();
    packet.effort = args.effort;
    packet.timeout = args.timeout;
    packet.attempts = args.dispatch.attempts;
    packet.mode = args.dispatch.mode;
    packet.repo = Some(repo.to_string_lossy().to_string());
    packet.validate()?;
    let out = args
        .out
        .unwrap_or_else(|| cfg.packets_dir(&repo).join(format!("{}.yml", packet.id)));
    packet.save(&out)?;
    if args.edit {
        open_editor(&out)?;
        packet = Packet::load(&out)?;
    }
    if !args.dispatch.json {
        println!("{}", out.display());
    }
    if args.run {
        let dispatch = Dispatch {
            tier: None,
            ceiling: None,
            mode: None,
            attempts: None,
            ..args.dispatch
        };
        return run_packet(cfg, packet, &dispatch);
    }
    Ok(ExitCode::SUCCESS)
}

fn open_editor(path: &std::path::Path) -> Result<()> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("delegate-edit")
        .arg(path)
        .status()
        .with_context(|| format!("launching editor {editor}"))?;
    if !status.success() {
        bail!("editor exited with {status}");
    }
    Ok(())
}

fn run_packet(cfg: &Config, packet: Packet, dispatch: &Dispatch) -> Result<ExitCode> {
    let store = Mutex::new(Store::open(&cfg.db_path())?);
    let run_id = ulid::Ulid::generate().to_string();
    if !dispatch.json {
        println!("run {run_id}");
    }
    let mut sink = HumanSink {
        json: dispatch.json,
    };
    let mut approver = TtyApprover { yes: dispatch.yes };
    let cancel = AtomicBool::new(false);
    let outcome = engine::execute(
        cfg,
        &store,
        RunRequest {
            run_id,
            packet,
            overrides: dispatch.overrides(),
            keep_worktree: dispatch.keep_worktree,
        },
        &mut sink,
        &mut approver,
        &cancel,
    )?;
    Ok(match outcome.status {
        RunStatus::Passed => ExitCode::SUCCESS,
        RunStatus::Error => ExitCode::from(2),
        _ => ExitCode::from(1),
    })
}

fn truncate(text: &str, max: usize) -> String {
    let single = text.lines().next().unwrap_or("");
    if single.chars().count() > max {
        let cut: String = single.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        single.to_string()
    }
}
