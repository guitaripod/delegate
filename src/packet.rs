use std::fmt;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    pub fn thinking_level(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

impl FromStr for Effort {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "low" => Ok(Effort::Low),
            "medium" | "med" => Ok(Effort::Medium),
            "high" => Ok(Effort::High),
            other => bail!("unknown effort '{other}' (low, medium, high)"),
        }
    }
}

#[derive(Serialize, Deserialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Normal,
    Conserve,
    Rush,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Mode::Normal => "normal",
            Mode::Conserve => "conserve",
            Mode::Rush => "rush",
        };
        f.write_str(s)
    }
}

impl FromStr for Mode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "normal" => Ok(Mode::Normal),
            "conserve" => Ok(Mode::Conserve),
            "rush" => Ok(Mode::Rush),
            other => bail!("unknown mode '{other}' (normal, conserve, rush)"),
        }
    }
}

/// One delegated unit of work: what to do, where it may write, and how the result is judged.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Packet {
    /// ULID assigned when the packet is created.
    pub id: String,
    /// Task class key; selects defaults from the `classes` table in the config.
    pub class: String,
    /// What the worker must achieve, written for a reader with no other context.
    pub goal: String,
    /// Paths or globs the worker may create or modify. Empty means unrestricted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Shell command run in the isolated worktree after the worker finishes; exit 0 passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<String>,
    /// Files the worker should read before starting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read: Vec<String>,
    /// Free-form context appended to the worker prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Start tier; overrides the class default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// Highest tier escalation may reach; overrides the class default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling: Option<String>,
    /// Reasoning effort passed to the worker model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// Seconds allowed per attempt for the worker and the verifier each.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
    /// Attempts per tier before escalating.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
    /// Dispatch mode override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    /// Repository root the packet applies to; defaults to the current directory when run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// RFC 3339 creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
}

impl Packet {
    pub fn new(class: &str, goal: &str) -> Packet {
        Packet {
            id: ulid::Ulid::generate().to_string(),
            class: class.to_string(),
            goal: goal.to_string(),
            paths: Vec::new(),
            verify: None,
            read: Vec::new(),
            notes: None,
            tier: None,
            ceiling: None,
            effort: None,
            timeout: None,
            attempts: None,
            mode: None,
            repo: None,
            created: Some(chrono::Utc::now().to_rfc3339()),
        }
    }

    pub fn parse(text: &str) -> Result<Packet> {
        let packet: Packet =
            serde_yaml_ng::from_str(text).context("packet is not valid YAML/JSON")?;
        packet.validate()?;
        Ok(packet)
    }

    pub fn load(path: &Path) -> Result<Packet> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading packet {}", path.display()))?;
        Packet::parse(&text).with_context(|| format!("parsing packet {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(path, self.to_yaml()?)
            .with_context(|| format!("writing packet {}", path.display()))
    }

    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yaml_ng::to_string(self)?)
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            bail!("packet id is empty");
        }
        if self.class.trim().is_empty() {
            bail!("packet class is empty");
        }
        if self.goal.trim().is_empty() {
            bail!("packet goal is empty");
        }
        if self.attempts == Some(0) {
            bail!("attempts must be at least 1");
        }
        if self.timeout == Some(0) {
            bail!("timeout must be at least 1 second");
        }
        Ok(())
    }

    pub fn schema_json() -> Result<String> {
        let schema = schemars::schema_for!(Packet);
        Ok(serde_json::to_string_pretty(&schema)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "id: 01ARZ3NDEKTSV4RRFFQ69G5FAV\nclass: rust-impl\ngoal: fix the bug\n";

    #[test]
    fn parse_accepts_the_minimal_required_fields() {
        let packet = Packet::parse(MINIMAL).expect("minimal packet parses");
        assert_eq!(packet.id, "01ARZ3NDEKTSV4RRFFQ69G5FAV");
        assert_eq!(packet.class, "rust-impl");
        assert_eq!(packet.goal, "fix the bug");
        assert!(packet.paths.is_empty());
        assert!(packet.read.is_empty());
        assert!(packet.verify.is_none());
        assert!(packet.tier.is_none());
    }

    #[test]
    fn parse_fails_when_goal_is_missing() {
        let yaml = "id: 01ARZ3NDEKTSV4RRFFQ69G5FAV\nclass: rust-impl\n";
        assert!(Packet::parse(yaml).is_err());
    }

    #[test]
    fn parse_fails_when_goal_is_blank() {
        let yaml = "id: 01ARZ3NDEKTSV4RRFFQ69G5FAV\nclass: rust-impl\ngoal: \"   \"\n";
        let err = Packet::parse(yaml).unwrap_err();
        assert!(
            err.to_string().contains("goal is empty"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn parse_fails_on_an_unknown_field() {
        let yaml = format!("{MINIMAL}bogus_field: 1\n");
        assert!(Packet::parse(&yaml).is_err());
    }

    #[test]
    fn parse_fails_when_attempts_is_zero() {
        let yaml = format!("{MINIMAL}attempts: 0\n");
        let err = Packet::parse(&yaml).unwrap_err();
        assert!(
            err.to_string().contains("attempts must be at least 1"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn parse_fails_when_timeout_is_zero() {
        let yaml = format!("{MINIMAL}timeout: 0\n");
        let err = Packet::parse(&yaml).unwrap_err();
        assert!(
            err.to_string()
                .contains("timeout must be at least 1 second"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn yaml_round_trip_omits_absent_optional_fields() {
        let packet = Packet::new("rust-impl", "fix the bug");
        let yaml = packet.to_yaml().expect("packet serializes");
        for key in [
            "paths", "verify", "read", "notes", "tier", "ceiling", "effort", "timeout", "attempts",
            "mode", "repo",
        ] {
            assert!(
                !yaml.contains(key),
                "expected '{key}' to be absent from:\n{yaml}"
            );
        }
        let round_tripped = Packet::parse(&yaml).expect("round tripped packet parses");
        assert_eq!(round_tripped.id, packet.id);
        assert_eq!(round_tripped.class, packet.class);
        assert_eq!(round_tripped.goal, packet.goal);
        assert_eq!(round_tripped.created, packet.created);
        assert!(round_tripped.tier.is_none());
        assert!(round_tripped.ceiling.is_none());
        assert!(round_tripped.effort.is_none());
        assert!(round_tripped.verify.is_none());
        assert!(round_tripped.notes.is_none());
        assert!(round_tripped.mode.is_none());
        assert!(round_tripped.repo.is_none());
        assert!(round_tripped.paths.is_empty());
        assert!(round_tripped.read.is_empty());
    }

    #[test]
    fn effort_from_str_accepts_known_levels_and_rejects_others() {
        assert_eq!(Effort::from_str("low").unwrap(), Effort::Low);
        assert_eq!(Effort::from_str("MEDIUM").unwrap(), Effort::Medium);
        assert_eq!(Effort::from_str("med").unwrap(), Effort::Medium);
        assert_eq!(Effort::from_str("high").unwrap(), Effort::High);
        assert!(Effort::from_str("extreme").is_err());
    }

    #[test]
    fn mode_from_str_accepts_known_modes_and_rejects_others() {
        assert_eq!(Mode::from_str("normal").unwrap(), Mode::Normal);
        assert_eq!(Mode::from_str("Conserve").unwrap(), Mode::Conserve);
        assert_eq!(Mode::from_str("rush").unwrap(), Mode::Rush);
        assert!(Mode::from_str("turbo").is_err());
    }

    #[test]
    fn mode_default_is_normal() {
        assert_eq!(Mode::default(), Mode::Normal);
    }

    #[test]
    fn schema_json_is_valid_json_and_names_the_required_fields() {
        let schema = Packet::schema_json().expect("schema renders");
        let value: serde_json::Value = serde_json::from_str(&schema).expect("schema is valid json");
        let required: Vec<&str> = value["required"]
            .as_array()
            .expect("schema lists required fields")
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        assert!(required.contains(&"id"));
        assert!(required.contains(&"class"));
        assert!(required.contains(&"goal"));
        assert!(!required.contains(&"notes"));
    }
}
