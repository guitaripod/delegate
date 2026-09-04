use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use wait_timeout::ChildExt;

use crate::config::home_dir;

pub const TAIL_BYTES: usize = 8000;

pub struct CommandOutcome {
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub output: String,
}

/// Keeps the last `TAIL_BYTES` of a growing log so failures stay readable without storing everything.
pub struct Tail {
    buf: String,
}

impl Tail {
    pub fn new() -> Tail {
        Tail { buf: String::new() }
    }

    pub fn push_line(&mut self, line: &str) {
        self.buf.push_str(line);
        self.buf.push('\n');
        if self.buf.len() > TAIL_BYTES * 2 {
            let cut = self.buf.len() - TAIL_BYTES;
            let boundary = self
                .buf
                .char_indices()
                .map(|(i, _)| i)
                .find(|&i| i >= cut)
                .unwrap_or(cut);
            self.buf.drain(..boundary);
        }
    }

    pub fn finish(mut self) -> String {
        if self.buf.len() > TAIL_BYTES {
            let cut = self.buf.len() - TAIL_BYTES;
            let boundary = self
                .buf
                .char_indices()
                .map(|(i, _)| i)
                .find(|&i| i >= cut)
                .unwrap_or(cut);
            self.buf.drain(..boundary);
        }
        self.buf
    }
}

/// PATH for child processes: the daemon may start with a minimal environment, and omp needs bun.
pub fn child_path() -> String {
    let home = home_dir();
    let mut parts: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    for extra in [
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ] {
        let s = extra.to_string_lossy().to_string();
        if !parts.contains(&s) {
            parts.push(s);
        }
    }
    parts.join(":")
}

pub fn resolve_bin(name: &str) -> PathBuf {
    if name.contains('/') {
        return crate::config::expand_home(name);
    }
    for dir in child_path().split(':') {
        let candidate = Path::new(dir).join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

pub struct ShellJob<'a> {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: &'a Path,
    pub env: &'a BTreeMap<String, String>,
    pub stdin: Option<&'a str>,
    pub timeout: Duration,
}

/// Runs a process, streaming merged stdout/stderr lines to `on_line`, killing it at the timeout.
pub fn run(job: ShellJob<'_>, on_line: &mut dyn FnMut(&str)) -> Result<CommandOutcome> {
    let started = Instant::now();
    let mut command = Command::new(&job.program);
    command
        .args(&job.args)
        .current_dir(job.cwd)
        .env("PATH", child_path())
        .stdin(if job.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (k, v) in job.env {
        command.env(k, v);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("spawning {}", job.program.display()))?;
    if let (Some(text), Some(mut stdin)) = (job.stdin, child.stdin.take()) {
        let owned = text.to_string();
        std::thread::spawn(move || {
            let _ = stdin.write_all(owned.as_bytes());
        });
    }
    let (tx, rx) = mpsc::channel::<String>();
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        }));
    }
    if let Some(stderr) = child.stderr.take() {
        let tx = tx.clone();
        readers.push(std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        }));
    }
    drop(tx);
    let deadline = started + job.timeout;
    let mut tail = Tail::new();
    let mut timed_out = false;
    let mut exit = None;
    loop {
        let now = Instant::now();
        if now >= deadline {
            timed_out = true;
            let _ = child.kill();
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(line) => {
                on_line(&line);
                tail.push_line(&line);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                let _ = child.kill();
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    for reader in readers {
        let _ = reader.join();
    }
    for line in rx.try_iter() {
        on_line(&line);
        tail.push_line(&line);
    }
    if timed_out {
        let _ = child.wait();
    } else {
        let status = child
            .wait_timeout(Duration::from_secs(30))
            .context("waiting for child")?;
        exit = match status {
            Some(s) => s.code(),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                None
            }
        };
    }
    Ok(CommandOutcome {
        exit,
        timed_out,
        output: tail.finish(),
    })
}

/// Runs `sh -c <command>` in `cwd`, merging both output streams.
pub fn run_sh(
    command: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    stdin: Option<&str>,
    timeout: Duration,
    on_line: &mut dyn FnMut(&str),
) -> Result<CommandOutcome> {
    run(
        ShellJob {
            program: PathBuf::from("sh"),
            args: vec!["-c".to_string(), command.to_string()],
            cwd,
            env,
            stdin,
            timeout,
        },
        on_line,
    )
}
