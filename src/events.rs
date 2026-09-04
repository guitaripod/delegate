use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttemptStatus {
    Pass,
    Fail,
    Timeout,
    Scope,
    Error,
}

impl AttemptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AttemptStatus::Pass => "pass",
            AttemptStatus::Fail => "fail",
            AttemptStatus::Timeout => "timeout",
            AttemptStatus::Scope => "scope",
            AttemptStatus::Error => "error",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Passed,
    Failed,
    Held,
    Cancelled,
    Error,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Passed => "passed",
            RunStatus::Failed => "failed",
            RunStatus::Held => "held",
            RunStatus::Cancelled => "cancelled",
            RunStatus::Error => "error",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
    RunStarted {
        packet_id: String,
        class: String,
        start_tier: String,
        ceiling: String,
        mode: String,
        host: String,
        repo: String,
    },
    TierSelected {
        tier: String,
        label: String,
        runner: String,
        model: String,
        chain_index: usize,
    },
    TierSkipped {
        tier: String,
        reason: String,
    },
    AttemptStarted {
        tier: String,
        attempt: u32,
        model: String,
    },
    Progress {
        tier: String,
        attempt: u32,
        text: String,
    },
    AttemptFinished {
        tier: String,
        attempt: u32,
        status: AttemptStatus,
        verify_exit: Option<i32>,
        duration_ms: u64,
        tokens_in: u64,
        tokens_out: u64,
        changed_files: Vec<String>,
        scope_violations: Vec<String>,
        verify_tail: String,
        worker_summary: String,
    },
    ApprovalRequired {
        tier: String,
        reason: String,
    },
    ApprovalResolved {
        tier: String,
        approved: bool,
    },
    Escalated {
        from: String,
        to: String,
        reason: String,
    },
    Applied {
        files: Vec<String>,
        patch_bytes: usize,
    },
    RunFinished {
        status: RunStatus,
        passed_tier: Option<String>,
        escalations: u32,
        duration_ms: u64,
        summary: String,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Envelope {
    pub run_id: String,
    pub seq: u64,
    pub ts: DateTime<Utc>,
    #[serde(flatten)]
    pub event: RunEvent,
}

fn seconds(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

impl RunEvent {
    /// One terminal line per event, or nothing for events that are noise in human mode.
    pub fn human(&self) -> Option<String> {
        Some(match self {
            RunEvent::RunStarted {
                class,
                start_tier,
                ceiling,
                mode,
                packet_id,
                ..
            } => {
                format!("packet {packet_id} · class {class} · {start_tier}→{ceiling} · mode {mode}")
            }
            RunEvent::TierSelected { tier, model, .. } => format!("{tier} = {model}"),
            RunEvent::TierSkipped { tier, reason } => format!("{tier} skipped: {reason}"),
            RunEvent::AttemptStarted { tier, attempt, .. } => {
                format!("{tier} attempt {attempt} running")
            }
            RunEvent::Progress { tier, text, .. } => format!("{tier}   {text}"),
            RunEvent::AttemptFinished {
                tier,
                attempt,
                status,
                verify_exit,
                duration_ms,
                changed_files,
                scope_violations,
                ..
            } => {
                let mark = match status {
                    AttemptStatus::Pass => "✓",
                    _ => "✗",
                };
                let detail = match status {
                    AttemptStatus::Pass => format!("{} file(s)", changed_files.len()),
                    AttemptStatus::Fail => match verify_exit {
                        Some(code) => format!("verify exit {code}"),
                        None => "worker failed".to_string(),
                    },
                    AttemptStatus::Timeout => "timed out".to_string(),
                    AttemptStatus::Scope => {
                        format!("out of scope: {}", scope_violations.join(", "))
                    }
                    AttemptStatus::Error => "runner error".to_string(),
                };
                format!(
                    "{tier} {mark} attempt {attempt} {detail} ({})",
                    seconds(*duration_ms)
                )
            }
            RunEvent::ApprovalRequired { tier, reason } => {
                format!("{tier} needs approval: {reason}")
            }
            RunEvent::ApprovalResolved { tier, approved } => {
                if *approved {
                    format!("{tier} approved")
                } else {
                    format!("{tier} held")
                }
            }
            RunEvent::Escalated { from, to, reason } => format!("{from} → {to} ({reason})"),
            RunEvent::Applied { files, patch_bytes } => {
                format!("applied {} file(s), {} bytes", files.len(), patch_bytes)
            }
            RunEvent::RunFinished {
                status,
                passed_tier,
                escalations,
                duration_ms,
                summary,
            } => {
                let tier = passed_tier.clone().unwrap_or_else(|| "-".to_string());
                let mut line = format!(
                    "{} at {tier} · {escalations} escalation(s) · {}",
                    status.as_str(),
                    seconds(*duration_ms)
                );
                if !summary.is_empty() {
                    line.push_str(" · ");
                    line.push_str(summary);
                }
                line
            }
        })
    }
}
