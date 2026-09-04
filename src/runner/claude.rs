use anyhow::Result;
use serde_json::Value;

use super::{Job, Runner, WorkerResult};
use crate::config::ClaudeRunnerConfig;
use crate::shell::{self, ShellJob};

/// Drives Claude Code headless (`claude -p --output-format stream-json`) as the worker, on the user's own subscription.
pub struct ClaudeRunner {
    cfg: ClaudeRunnerConfig,
}

impl ClaudeRunner {
    pub fn new(cfg: ClaudeRunnerConfig) -> ClaudeRunner {
        ClaudeRunner { cfg }
    }

    fn args(&self, job: &Job<'_>) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
            "--no-session-persistence".to_string(),
            "--dangerously-skip-permissions".to_string(),
        ];
        if let Some(model) = &job.entry.model {
            args.push("--model".to_string());
            args.push(model.clone());
        }
        if let Some(level) = &job.thinking {
            args.push("--effort".to_string());
            args.push(level.clone());
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

fn add_usage(usage: &mut Usage, value: &Value) {
    let n = |k: &str| value.get(k).and_then(Value::as_u64).unwrap_or(0);
    usage.tokens_in +=
        n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
    usage.tokens_out += n("output_tokens");
}

fn tool_summary(block: &Value) -> Option<String> {
    let name = block.get("name").and_then(Value::as_str)?;
    let input = block.get("input");
    let detail = input
        .and_then(|i| {
            i.get("file_path")
                .or_else(|| i.get("command"))
                .or_else(|| i.get("pattern"))
                .or_else(|| i.get("path"))
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

impl Runner for ClaudeRunner {
    fn run(&self, job: &Job<'_>, progress: &mut dyn FnMut(String)) -> Result<WorkerResult> {
        let mut env = job.env.clone();
        for (k, v) in &job.entry.env {
            env.insert(k.clone(), v.clone());
        }
        let mut usage = Usage::default();
        let mut is_error = false;
        let outcome = shell::run(
            ShellJob {
                program: shell::resolve_bin(&self.cfg.bin),
                args: self.args(job),
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
                    Some("assistant") => {
                        let blocks = event
                            .get("message")
                            .and_then(|m| m.get("content"))
                            .and_then(Value::as_array);
                        for block in blocks.into_iter().flatten() {
                            if block.get("type").and_then(Value::as_str) == Some("tool_use")
                                && let Some(summary) = tool_summary(block)
                            {
                                progress(summary);
                            }
                        }
                    }
                    Some("result") => {
                        if let Some(u) = event.get("usage") {
                            add_usage(&mut usage, u);
                        }
                        if let Some(text) = event.get("result").and_then(Value::as_str) {
                            usage.final_text = text.trim().to_string();
                        }
                        is_error = event
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                    }
                    _ => {}
                }
            },
        )?;
        let exit = match (outcome.exit, is_error) {
            (Some(0), true) => Some(1),
            (code, _) => code,
        };
        Ok(WorkerResult {
            exit,
            timed_out: outcome.timed_out,
            tokens_in: usage.tokens_in,
            tokens_out: usage.tokens_out,
            final_text: usage.final_text,
            log_tail: outcome.output,
        })
    }
}
