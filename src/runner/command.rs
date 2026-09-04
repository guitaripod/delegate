use anyhow::{Context, Result};

use super::{Job, Runner, WorkerResult};
use crate::shell;

/// Runs an arbitrary shell command as the worker; the prompt arrives on stdin.
pub struct CommandRunner;

impl Runner for CommandRunner {
    fn run(&self, job: &Job<'_>, progress: &mut dyn FnMut(String)) -> Result<WorkerResult> {
        let command = job
            .entry
            .command
            .as_deref()
            .context("command runner without a command")?;
        let mut env = job.env.clone();
        env.insert("DELEGATE_TIER".to_string(), job.tier.to_string());
        if let Some(model) = &job.entry.model {
            env.insert("DELEGATE_MODEL".to_string(), model.clone());
        }
        if let Some(level) = &job.thinking {
            env.insert("DELEGATE_THINKING".to_string(), level.clone());
        }
        for (k, v) in &job.entry.env {
            env.insert(k.clone(), v.clone());
        }
        let mut last_line = String::new();
        let outcome = shell::run_sh(
            command,
            job.cwd,
            &env,
            Some(&job.prompt),
            job.timeout,
            &mut |line| {
                last_line = line.to_string();
                progress(line.to_string());
            },
        )?;
        Ok(WorkerResult {
            exit: outcome.exit,
            timed_out: outcome.timed_out,
            tokens_in: 0,
            tokens_out: 0,
            final_text: last_line,
            log_tail: outcome.output,
        })
    }
}
