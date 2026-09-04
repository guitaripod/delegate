use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// Isolated git worktree mirroring the repository's committed and uncommitted state at creation time.
pub struct Workspace {
    top: PathBuf,
    pub dir: PathBuf,
    base: String,
    keep: bool,
}

fn git_output(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("spawning git")?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        pipe.write_all(bytes).context("writing to git stdin")?;
    }
    let output = child.wait_with_output().context("waiting for git")?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    Ok(String::from_utf8_lossy(&git_output(cwd, args, None)?)
        .trim()
        .to_string())
}

pub fn toplevel(repo: &Path) -> Result<PathBuf> {
    let top = git(repo, &["rev-parse", "--show-toplevel"])
        .with_context(|| format!("{} is not inside a git repository", repo.display()))?;
    Ok(PathBuf::from(top))
}

impl Workspace {
    pub fn create(repo: &Path, keep: bool) -> Result<Workspace> {
        let top = toplevel(repo)?;
        git(&top, &["rev-parse", "--verify", "HEAD"])
            .context("repository has no commits yet; commit once before delegating")?;
        let dir = std::env::temp_dir().join(format!(
            "delegate-{}",
            ulid::Ulid::generate().to_string().to_lowercase()
        ));
        let dir_str = dir.to_string_lossy().to_string();
        git(
            &top,
            &["worktree", "add", "--detach", "-q", &dir_str, "HEAD"],
        )?;
        let mut ws = Workspace {
            top: top.clone(),
            dir: dir.clone(),
            base: String::new(),
            keep,
        };
        ws.mirror_uncommitted()?;
        git(&dir, &["add", "-A"])?;
        git(
            &dir,
            &[
                "-c",
                "user.name=delegate",
                "-c",
                "user.email=delegate@localhost",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "--no-verify",
                "-m",
                "delegate base",
            ],
        )?;
        ws.base = git(&dir, &["rev-parse", "HEAD"])?;
        Ok(ws)
    }

    /// Copies the tracked diff against HEAD and every untracked, non-ignored file into the worktree.
    fn mirror_uncommitted(&self) -> Result<()> {
        let diff = git_output(
            &self.top,
            &[
                "diff",
                "HEAD",
                "--binary",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
            ],
            None,
        )?;
        if !diff.is_empty() {
            git_output(
                &self.dir,
                &["apply", "--whitespace=nowarn", "--allow-empty"],
                Some(&diff),
            )
            .context("mirroring uncommitted changes into the worktree")?;
        }
        let untracked = git_output(
            &self.top,
            &["ls-files", "--others", "--exclude-standard", "-z"],
            None,
        )?;
        for rel in untracked.split(|b| *b == 0).filter(|s| !s.is_empty()) {
            let rel = String::from_utf8_lossy(rel).to_string();
            let src = self.top.join(&rel);
            let dst = self.dir.join(&rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if src.is_file() {
                std::fs::copy(&src, &dst).with_context(|| format!("copying untracked {rel}"))?;
            }
        }
        Ok(())
    }

    pub fn changed_files(&self) -> Result<Vec<String>> {
        git(&self.dir, &["add", "-A"])?;
        let out = git(&self.dir, &["diff", "--cached", "--name-only", &self.base])?;
        Ok(out
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    pub fn patch(&self) -> Result<Vec<u8>> {
        git(&self.dir, &["add", "-A"])?;
        git_output(
            &self.dir,
            &["diff", "--cached", "--binary", "--no-color", &self.base],
            None,
        )
    }

    /// Applies a worker patch to the real working tree, three-way so unrelated edits made meanwhile survive,
    /// then leaves the touched files unstaged so the result is reviewed before it is committed.
    pub fn apply(&self, patch: &[u8], files: &[String]) -> Result<()> {
        if patch.is_empty() {
            return Ok(());
        }
        let file = self.dir.join(".delegate-result.patch");
        std::fs::write(&file, patch)?;
        let file_str = file.to_string_lossy().to_string();
        git(
            &self.top,
            &["apply", "--3way", "--whitespace=nowarn", &file_str],
        )
        .context("applying the worker's patch to the repository")?;
        let _ = std::fs::remove_file(&file);
        let mut reset = vec!["reset", "-q", "--"];
        reset.extend(files.iter().map(String::as_str));
        let _ = git(&self.top, &reset);
        Ok(())
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        let dir = self.dir.to_string_lossy().to_string();
        let _ = git(&self.top, &["worktree", "remove", "--force", &dir]);
        let _ = git(&self.top, &["worktree", "prune"]);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
