use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Result, bail};

use crate::config::{ChainEntry, Config};

pub mod claude;
pub mod command;
pub mod omp;

pub struct Job<'a> {
    pub prompt: String,
    pub cwd: &'a Path,
    pub entry: &'a ChainEntry,
    pub tier: &'a str,
    pub thinking: Option<String>,
    pub timeout: Duration,
    pub env: BTreeMap<String, String>,
}

pub struct WorkerResult {
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub final_text: String,
    pub log_tail: String,
}

pub trait Runner {
    fn run(&self, job: &Job<'_>, progress: &mut dyn FnMut(String)) -> Result<WorkerResult>;
}

pub fn for_entry(cfg: &Config, entry: &ChainEntry) -> Result<Box<dyn Runner>> {
    match entry.runner.as_str() {
        "omp" => Ok(Box::new(omp::OmpRunner::new(cfg.runners.omp.clone()))),
        "claude" => Ok(Box::new(claude::ClaudeRunner::new(
            cfg.runners.claude.clone(),
        ))),
        "command" => Ok(Box::new(command::CommandRunner)),
        other => bail!("unknown runner '{other}'"),
    }
}
