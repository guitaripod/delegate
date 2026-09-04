use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{Job, Runner, WorkerResult};
use crate::config::OmpRunnerConfig;
use crate::shell::{self, ShellJob};

/// Drives `omp -p --mode json` as the worker and reads usage from its event stream.
pub struct OmpRunner {
    cfg: OmpRunnerConfig,
}

impl OmpRunner {
    pub fn new(cfg: OmpRunnerConfig) -> OmpRunner {
        OmpRunner { cfg }
    }

    fn overlay_file(&self, job: &Job<'_>) -> Result<Option<PathBuf>> {
        let Some(settings) = &job.entry.settings else {
            return Ok(None);
        };
        let path = std::env::temp_dir().join(format!(
            "delegate-overlay-{}.yml",
            ulid::Ulid::generate().to_string().to_lowercase()
        ));
        std::fs::write(&path, serde_yaml_ng::to_string(settings)?)
            .context("writing omp overlay")?;
        Ok(Some(path))
    }

    fn args(&self, job: &Job<'_>, overlay: Option<&PathBuf>) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--mode".to_string(),
            "json".to_string(),
            "--no-session".to_string(),
            "--auto-approve".to_string(),
            "--cwd".to_string(),
            job.cwd.to_string_lossy().to_string(),
        ];
        if let Some(model) = &job.entry.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(level) = &job.thinking {
            args.push("--thinking".to_string());
            args.push(level.clone());
        }
        args.push("--max-time".to_string());
        args.push(job.timeout.as_secs().to_string());
        if self.cfg.no_lsp {
            args.push("--no-lsp".to_string());
        }
        if self.cfg.no_extensions {
            args.push("--no-extensions".to_string());
        }
        if self.cfg.no_skills {
            args.push("--no-skills".to_string());
        }
        if self.cfg.no_rules {
            args.push("--no-rules".to_string());
        }
        if let Some(path) = overlay {
            args.push("--config".to_string());
            args.push(path.to_string_lossy().to_string());
        }
        args.extend(self.cfg.args.iter().cloned());
        args.extend(job.entry.args.iter().cloned());
        args
    }
}

#[derive(Default)]
struct Usage {
    tokens_in: u64,
    tokens_out: u64,
    final_text: String,
}

fn usage_from_message(message: &Value, usage: &mut Usage) {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    if let Some(u) = message.get("usage") {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        usage.tokens_in += n("input") + n("cacheRead") + n("cacheWrite");
        usage.tokens_out += n("output");
    }
    let text: Vec<&str> = message
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let joined = text.join("").trim().to_string();
    if !joined.is_empty() {
        usage.final_text = joined;
    }
}

fn tool_summary(event: &Value) -> Option<String> {
    let name = event
        .get("toolName")
        .or_else(|| event.get("name"))
        .and_then(Value::as_str)?;
    let args = event.get("args").or_else(|| event.get("arguments"));
    let detail = args
        .and_then(|a| {
            a.get("path")
                .or_else(|| a.get("command"))
                .or_else(|| a.get("pattern"))
                .and_then(Value::as_str)
        })
        .map(|d| shorten(d, 100))
        .unwrap_or_default();
    Some(format!("{name} {detail}").trim().to_string())
}

fn shorten(text: &str, max: usize) -> String {
    let single = text.lines().next().unwrap_or("");
    if single.chars().count() > max {
        let cut: String = single.chars().take(max).collect();
        format!("{cut}…")
    } else {
        single.to_string()
    }
}

impl Runner for OmpRunner {
    fn run(&self, job: &Job<'_>, progress: &mut dyn FnMut(String)) -> Result<WorkerResult> {
        let overlay = self.overlay_file(job)?;
        let args = self.args(job, overlay.as_ref());
        let mut env = job.env.clone();
        for (k, v) in &job.entry.env {
            env.insert(k.clone(), v.clone());
        }
        let mut usage = Usage::default();
        let outcome = shell::run(
            ShellJob {
                program: shell::resolve_bin(&self.cfg.bin),
                args,
                cwd: job.cwd,
                env: &env,
                stdin: Some(&job.prompt),
                timeout: job.timeout + std::time::Duration::from_secs(30),
            },
            &mut |line| {
                let Ok(event) = serde_json::from_str::<Value>(line) else {
                    if !line.trim().is_empty() {
                        progress(shorten(line, 160));
                    }
                    return;
                };
                match event.get("type").and_then(Value::as_str) {
                    Some("message_end") => {
                        if let Some(message) = event.get("message") {
                            usage_from_message(message, &mut usage);
                        }
                    }
                    Some("tool_execution_start") => {
                        if let Some(summary) = tool_summary(&event) {
                            progress(summary);
                        }
                    }
                    _ => {}
                }
            },
        )?;
        if let Some(path) = overlay {
            let _ = std::fs::remove_file(path);
        }
        Ok(WorkerResult {
            exit: outcome.exit,
            timed_out: outcome.timed_out,
            tokens_in: usage.tokens_in,
            tokens_out: usage.tokens_out,
            final_text: usage.final_text,
            log_tail: outcome.output,
        })
    }
}
