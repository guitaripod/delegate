use std::path::Path;

use crate::packet::Packet;

pub struct Failure {
    pub tier: String,
    pub attempt: u32,
    pub verify_tail: String,
    pub scope_violations: Vec<String>,
    pub worker_summary: String,
}

/// The worker prompt: a self-contained brief that never mentions other models or the tier map.
pub fn build(
    packet: &Packet,
    tier: &str,
    verify: Option<&str>,
    previous: Option<&Failure>,
    cwd: &Path,
) -> String {
    let mut p = String::new();
    p.push_str(&format!(
        "You are a delegated worker executing exactly one task packet in the repository at {}.\n",
        cwd.display()
    ));
    p.push_str(&format!(
        "Packet {} · class {} · tier {}\n\n",
        packet.id, packet.class, tier
    ));
    p.push_str("GOAL\n");
    p.push_str(packet.goal.trim());
    p.push_str("\n\n");
    if let Some(notes) = packet
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        p.push_str("CONTEXT\n");
        p.push_str(notes);
        p.push_str("\n\n");
    }
    if !packet.read.is_empty() {
        p.push_str("READ FIRST\n");
        for path in &packet.read {
            p.push_str(&format!("- {path}\n"));
        }
        p.push('\n');
    }
    if packet.paths.is_empty() {
        p.push_str("ALLOWED PATHS\nAny file in the repository, but change only what the goal requires.\n\n");
    } else {
        p.push_str("ALLOWED PATHS (create or modify only files matching these; anything else fails the packet)\n");
        for path in &packet.paths {
            p.push_str(&format!("- {path}\n"));
        }
        p.push('\n');
    }
    match verify {
        Some(cmd) => {
            p.push_str("VERIFY\nRun this command before you finish; it must exit 0:\n    ");
            p.push_str(cmd);
            p.push_str("\nRun it yourself, fix what it reports, and only finish when it passes. If it cannot pass, say exactly why in your final message.\n\n");
        }
        None => {
            p.push_str("VERIFY\nThere is no automatic verifier. Check your own work carefully before finishing.\n\n");
        }
    }
    p.push_str(RULES);
    p.push_str("\nFINAL MESSAGE\nEnd with a short summary: what changed, what the verify command reported, and any decisions you made.\n");
    if let Some(prev) = previous {
        p.push_str(&format!(
            "\nPREVIOUS ATTEMPT (tier {}, attempt {}) FAILED. Fix these before anything else.\n",
            prev.tier, prev.attempt
        ));
        if !prev.scope_violations.is_empty() {
            p.push_str("Files changed outside the allowed paths:\n");
            for f in &prev.scope_violations {
                p.push_str(&format!("- {f}\n"));
            }
        }
        if !prev.verify_tail.trim().is_empty() {
            p.push_str("Verifier output:\n```\n");
            p.push_str(prev.verify_tail.trim());
            p.push_str("\n```\n");
        }
        if !prev.worker_summary.trim().is_empty() {
            p.push_str("The previous worker reported:\n");
            p.push_str(prev.worker_summary.trim());
            p.push('\n');
        }
    }
    p
}

const RULES: &str = "RULES
- Do not commit, stage, push, tag, or create branches; the dispatcher applies your changes.
- Do not modify files outside the allowed paths.
- Never write inline code comments. If something needs explaining, extract it into a well-named function with a doc comment.
- Never add \"Co-Authored-By\" or \"Generated with\" text anywhere.
- Never start, stop, restart, or kill system services.
- Do not ask questions; nobody will answer. Make the reasonable choice and state it in your final message.
- Do not touch files the goal does not require, even inside the allowed paths.
";
