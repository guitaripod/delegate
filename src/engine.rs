use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::config::{ChainEntry, Config, Overrides, Plan};
use crate::events::{AttemptStatus, Envelope, RunEvent, RunStatus};
use crate::packet::Packet;
use crate::prompt::{self, Failure};
use crate::runner::{self, Job};
use crate::scope;
use crate::shell;
use crate::store::{AttemptRow, RunRow, Store};
use crate::workspace::{self, Workspace};

pub trait EventSink {
    fn emit(&mut self, env: &Envelope);
}

pub trait Approver {
    fn approve(&mut self, tier: &str, reason: &str) -> bool;
}

pub struct RunRequest {
    pub run_id: String,
    pub packet: Packet,
    pub overrides: Overrides,
    pub keep_worktree: bool,
}

pub struct RunOutcome {
    pub status: RunStatus,
}

struct Emitter<'a> {
    run_id: String,
    seq: u64,
    store: &'a Mutex<Store>,
    sink: &'a mut dyn EventSink,
}

impl Emitter<'_> {
    fn emit(&mut self, event: RunEvent) {
        self.seq += 1;
        let env = Envelope {
            run_id: self.run_id.clone(),
            seq: self.seq,
            ts: Utc::now(),
            event,
        };
        if let Ok(store) = self.store.lock() {
            let _ = store.append_event(&env);
        }
        self.sink.emit(&env);
    }
}

fn hostname() -> String {
    gethostname::gethostname().to_string_lossy().to_string()
}

pub fn resolve_repo(packet: &Packet) -> Result<PathBuf> {
    let base = match &packet.repo {
        Some(repo) => crate::config::expand_home(repo),
        None => std::env::current_dir()?,
    };
    workspace::toplevel(&base)
}

/// Runs one packet through the tier ladder and persists every step; never panics the caller on worker failure.
pub fn execute(
    cfg: &Config,
    store: &Mutex<Store>,
    req: RunRequest,
    sink: &mut dyn EventSink,
    approver: &mut dyn Approver,
    cancel: &AtomicBool,
) -> Result<RunOutcome> {
    let plan = cfg.plan(&req.packet, &req.overrides)?;
    let repo = resolve_repo(&req.packet)?;
    let mut emitter = Emitter {
        run_id: req.run_id.clone(),
        seq: 0,
        store,
        sink,
    };
    {
        let store = store
            .lock()
            .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
        store.insert_run(&RunRow {
            id: req.run_id.clone(),
            packet_id: req.packet.id.clone(),
            class: req.packet.class.clone(),
            repo: repo.to_string_lossy().to_string(),
            host: hostname(),
            mode: plan.mode.to_string(),
            start_tier: cfg.order[plan.start].clone(),
            ceiling: cfg.order[plan.ceiling].clone(),
            status: RunStatus::Running.as_str().to_string(),
            created_at: Utc::now().to_rfc3339(),
            finished_at: None,
            passed_tier: None,
            escalations: 0,
            summary: String::new(),
            packet: req.packet.clone(),
        })?;
    }
    let started = Instant::now();
    emitter.emit(RunEvent::RunStarted {
        packet_id: req.packet.id.clone(),
        class: req.packet.class.clone(),
        start_tier: cfg.order[plan.start].clone(),
        ceiling: cfg.order[plan.ceiling].clone(),
        mode: plan.mode.to_string(),
        host: hostname(),
        repo: repo.to_string_lossy().to_string(),
    });
    let result = run_ladder(cfg, &plan, &req, &repo, &mut emitter, approver, cancel);
    let (status, passed_tier, escalations, summary) = match result {
        Ok(ladder) => (
            ladder.status,
            ladder.passed_tier,
            ladder.escalations,
            ladder.summary,
        ),
        Err(e) => (RunStatus::Error, None, 0, format!("{e:#}")),
    };
    if let Ok(store) = store.lock() {
        let _ = store.finish_run(
            &req.run_id,
            status.as_str(),
            passed_tier.as_deref(),
            escalations,
            &summary,
        );
    }
    emitter.emit(RunEvent::RunFinished {
        status,
        passed_tier: passed_tier.clone(),
        escalations,
        duration_ms: started.elapsed().as_millis() as u64,
        summary,
    });
    Ok(RunOutcome { status })
}

struct LadderOutcome {
    status: RunStatus,
    passed_tier: Option<String>,
    escalations: u32,
    summary: String,
}

/// Index of the first chain entry at or after `from` whose health URL answers, or every reason why none did.
fn first_healthy(cfg: &Config, entries: &[ChainEntry], from: usize) -> Result<usize, String> {
    let mut reasons = Vec::new();
    for (i, entry) in entries.iter().enumerate().skip(from) {
        match &entry.health {
            None => return Ok(i),
            Some(url) => match crate::health::check(url, cfg.health_timeout_ms) {
                Ok(()) => return Ok(i),
                Err(reason) => {
                    reasons.push(format!("{} unreachable ({reason})", entry.display_model()))
                }
            },
        }
    }
    if reasons.is_empty() {
        reasons.push("no chain entries left".to_string());
    }
    Err(reasons.join("; "))
}

fn worker_env(cfg: &Config, plan: &Plan, repo: &Path) -> BTreeMap<String, String> {
    let mut env = plan.env.clone();
    for (k, v) in cfg.env_file_entries() {
        env.entry(k).or_insert(v);
    }
    if repo.join("Cargo.toml").exists() && !env.contains_key("CARGO_TARGET_DIR") {
        env.insert(
            "CARGO_TARGET_DIR".to_string(),
            repo.join("target")
                .join("delegate")
                .to_string_lossy()
                .to_string(),
        );
    }
    env
}

fn run_ladder(
    cfg: &Config,
    plan: &Plan,
    req: &RunRequest,
    repo: &Path,
    emitter: &mut Emitter<'_>,
    approver: &mut dyn Approver,
    cancel: &AtomicBool,
) -> Result<LadderOutcome> {
    let env = worker_env(cfg, plan, repo);
    let timeout = Duration::from_secs(plan.timeout_secs);
    let mut previous: Option<Failure> = None;
    let mut escalations = 0u32;
    let mut last_tier: Option<String> = None;
    let mut idx = plan.start;
    while idx <= plan.ceiling {
        if cancel.load(Ordering::Relaxed) {
            return Ok(cancelled(escalations));
        }
        let (tier_name, tier) = cfg.tier(idx);
        if let Some(prev) = &last_tier {
            escalations += 1;
            let reason = previous
                .as_ref()
                .map(|f| format!("{} failed at {}", f.tier, f.attempt))
                .unwrap_or_else(|| "tier unavailable".to_string());
            emitter.emit(RunEvent::Escalated {
                from: prev.clone(),
                to: tier_name.to_string(),
                reason,
            });
        }
        if plan.ask_before == Some(idx) {
            let reason = format!("mode {} requires approval before {tier_name}", plan.mode);
            emitter.emit(RunEvent::ApprovalRequired {
                tier: tier_name.to_string(),
                reason: reason.clone(),
            });
            let approved = approver.approve(tier_name, &reason);
            emitter.emit(RunEvent::ApprovalResolved {
                tier: tier_name.to_string(),
                approved,
            });
            if !approved {
                if cancel.load(Ordering::Relaxed) {
                    return Ok(cancelled(escalations));
                }
                return Ok(LadderOutcome {
                    status: RunStatus::Held,
                    passed_tier: None,
                    escalations,
                    summary: format!("held before {tier_name}"),
                });
            }
        }
        let mut chain_index = match first_healthy(cfg, &tier.chain, 0) {
            Ok(index) => index,
            Err(reason) => {
                emitter.emit(RunEvent::TierSkipped {
                    tier: tier_name.to_string(),
                    reason,
                });
                last_tier = Some(tier_name.to_string());
                idx += 1;
                continue;
            }
        };
        let mut announced = None;
        let mut attempt = 1u32;
        while attempt <= plan.attempts {
            if cancel.load(Ordering::Relaxed) {
                return Ok(cancelled(escalations));
            }
            let entry = &tier.chain[chain_index];
            if announced != Some(chain_index) {
                emitter.emit(RunEvent::TierSelected {
                    tier: tier_name.to_string(),
                    label: tier.label.clone().unwrap_or_default(),
                    runner: entry.runner.clone(),
                    model: entry.display_model(),
                    chain_index,
                });
                announced = Some(chain_index);
            }
            let runner = runner::for_entry(cfg, entry)?;
            let thinking = req
                .packet
                .effort
                .map(|e| e.thinking_level().to_string())
                .or_else(|| entry.thinking.clone());
            let outcome = run_attempt(AttemptContext {
                cfg,
                plan,
                req,
                repo,
                tier_name,
                entry,
                chain_index,
                runner: runner.as_ref(),
                thinking,
                attempt,
                env: &env,
                timeout,
                previous: previous.as_ref(),
                emitter,
            })?;
            match outcome {
                AttemptOutcome::Passed { files, summary } => {
                    return Ok(LadderOutcome {
                        status: RunStatus::Passed,
                        passed_tier: Some(tier_name.to_string()),
                        escalations,
                        summary: format!("{} file(s): {}", files.len(), summary),
                    });
                }
                AttemptOutcome::Failed(failure) => {
                    previous = Some(failure);
                    attempt += 1;
                }
                AttemptOutcome::ProviderFailed(reason) => {
                    match first_healthy(cfg, &tier.chain, chain_index + 1) {
                        Ok(next) => {
                            emitter.emit(RunEvent::ChainFailover {
                                tier: tier_name.to_string(),
                                from: entry.display_model(),
                                to: tier.chain[next].display_model(),
                                reason,
                            });
                            chain_index = next;
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        last_tier = Some(tier_name.to_string());
        idx += 1;
    }
    let summary = previous
        .as_ref()
        .map(|f| {
            format!(
                "exhausted ladder; last failure at {} attempt {}",
                f.tier, f.attempt
            )
        })
        .unwrap_or_else(|| "no tier was reachable".to_string());
    Ok(LadderOutcome {
        status: RunStatus::Failed,
        passed_tier: None,
        escalations,
        summary,
    })
}

fn cancelled(escalations: u32) -> LadderOutcome {
    LadderOutcome {
        status: RunStatus::Cancelled,
        passed_tier: None,
        escalations,
        summary: "cancelled".to_string(),
    }
}

enum AttemptOutcome {
    Passed {
        files: Vec<String>,
        summary: String,
    },
    Failed(Failure),
    /// The worker never got going (auth, credits, unknown model, runner error): try the next chain entry.
    ProviderFailed(String),
}

struct AttemptContext<'a, 'e> {
    cfg: &'a Config,
    plan: &'a Plan,
    req: &'a RunRequest,
    repo: &'a Path,
    tier_name: &'a str,
    entry: &'a ChainEntry,
    chain_index: usize,
    runner: &'a dyn runner::Runner,
    thinking: Option<String>,
    attempt: u32,
    env: &'a BTreeMap<String, String>,
    timeout: Duration,
    previous: Option<&'a Failure>,
    emitter: &'a mut Emitter<'e>,
}

fn run_attempt(ctx: AttemptContext<'_, '_>) -> Result<AttemptOutcome> {
    let AttemptContext {
        cfg,
        plan,
        req,
        repo,
        tier_name,
        entry,
        chain_index,
        runner,
        thinking,
        attempt,
        env,
        timeout,
        previous,
        emitter,
    } = ctx;
    let started_at = Utc::now();
    let started = Instant::now();
    let model = entry.display_model();
    emitter.emit(RunEvent::AttemptStarted {
        tier: tier_name.to_string(),
        attempt,
        model: model.clone(),
    });
    let ws = Workspace::create(repo, req.keep_worktree)?;
    let prompt = prompt::build(
        &req.packet,
        tier_name,
        plan.verify.as_deref(),
        previous,
        &ws.dir,
    );
    let job = Job {
        prompt,
        cwd: &ws.dir,
        entry,
        tier: tier_name,
        thinking,
        timeout,
        env: env.clone(),
    };
    let worker = runner.run(&job, &mut |text| {
        emitter.emit(RunEvent::Progress {
            tier: tier_name.to_string(),
            attempt,
            text,
        });
    });
    let (worker_ok, worker_summary, tokens_in, tokens_out, worker_timed_out, worker_exit, log_tail) =
        match worker {
            Ok(w) => (
                true,
                w.final_text,
                w.tokens_in,
                w.tokens_out,
                w.timed_out,
                w.exit,
                w.log_tail,
            ),
            Err(e) => (
                false,
                format!("runner error: {e:#}"),
                0,
                0,
                false,
                None,
                String::new(),
            ),
        };
    let changed = ws.changed_files().unwrap_or_default();
    let violations = scope::violations(&changed, &req.packet.paths, &plan.scope_ignore);
    let mut verify_exit = None;
    let mut verify_tail = String::new();
    let mut verify_timed_out = false;
    let status = if !worker_ok {
        AttemptStatus::Error
    } else if !violations.is_empty() {
        AttemptStatus::Scope
    } else if let Some(cmd) = plan.verify.as_deref() {
        let mut verify_env = env.clone();
        for (k, v) in &entry.env {
            verify_env.entry(k.clone()).or_insert_with(|| v.clone());
        }
        let outcome = shell::run_sh(cmd, &ws.dir, &verify_env, None, timeout, &mut |_| {})?;
        verify_exit = outcome.exit;
        verify_tail = outcome.output;
        verify_timed_out = outcome.timed_out;
        if outcome.timed_out {
            AttemptStatus::Timeout
        } else if outcome.exit == Some(0) {
            AttemptStatus::Pass
        } else {
            AttemptStatus::Fail
        }
    } else if worker_timed_out {
        AttemptStatus::Timeout
    } else if worker_exit == Some(0) {
        AttemptStatus::Pass
    } else {
        AttemptStatus::Fail
    };
    if status != AttemptStatus::Pass && verify_tail.is_empty() {
        verify_tail = if worker_timed_out || verify_timed_out {
            format!("timed out after {}s\n{}", timeout.as_secs(), log_tail)
        } else {
            log_tail.clone()
        };
    }
    let provider_failed = status != AttemptStatus::Pass
        && (!worker_ok
            || (worker_exit != Some(0)
                && !worker_timed_out
                && tokens_out == 0
                && changed.is_empty()));
    let status = if provider_failed {
        AttemptStatus::Error
    } else {
        status
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    emitter.emit(RunEvent::AttemptFinished {
        tier: tier_name.to_string(),
        attempt,
        status,
        verify_exit,
        duration_ms,
        tokens_in,
        tokens_out,
        changed_files: changed.clone(),
        scope_violations: violations.clone(),
        verify_tail: verify_tail.clone(),
        worker_summary: worker_summary.clone(),
    });
    let patch = if status == AttemptStatus::Pass {
        Some(ws.patch()?)
    } else {
        None
    };
    {
        let store = emitter
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("store lock poisoned"))?;
        store.insert_attempt(
            &AttemptRow {
                run_id: req.run_id.clone(),
                tier: tier_name.to_string(),
                chain_index,
                runner: entry.runner.clone(),
                model: model.clone(),
                attempt,
                status: status.as_str().to_string(),
                verify_exit,
                duration_ms,
                tokens_in,
                tokens_out,
                changed_files: changed.clone(),
                scope_violations: violations.clone(),
                verify_tail: verify_tail.clone(),
                worker_summary: worker_summary.clone(),
                started_at: started_at.to_rfc3339(),
                finished_at: Utc::now().to_rfc3339(),
            },
            patch.as_deref(),
        )?;
    }
    let _ = cfg;
    if let Some(patch) = patch {
        ws.apply(&patch, &changed)
            .context("applying the passing patch")?;
        emitter.emit(RunEvent::Applied {
            files: changed.clone(),
            patch_bytes: patch.len(),
        });
        return Ok(AttemptOutcome::Passed {
            files: changed,
            summary: worker_summary,
        });
    }
    if provider_failed {
        return Ok(AttemptOutcome::ProviderFailed(failure_reason(
            &worker_summary,
            &verify_tail,
        )));
    }
    Ok(AttemptOutcome::Failed(Failure {
        tier: tier_name.to_string(),
        attempt,
        verify_tail,
        scope_violations: violations,
        worker_summary,
    }))
}

/// Last meaningful line the failed worker printed, short enough for one event line.
fn failure_reason(worker_summary: &str, log_tail: &str) -> String {
    let line = [worker_summary, log_tail]
        .iter()
        .flat_map(|text| text.lines().rev())
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("Working"))
        .unwrap_or("no output");
    let shortened: String = line.chars().take(160).collect();
    shortened
}
