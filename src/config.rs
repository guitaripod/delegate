use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;

use crate::packet::{Mode, Packet};

pub const DEFAULT_ATTEMPTS: u32 = 2;
pub const DEFAULT_TIMEOUT_SECS: u64 = 900;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub mode: Mode,
    /// Tier names from cheapest to strongest.
    pub order: Vec<String>,
    pub tiers: BTreeMap<String, Tier>,
    #[serde(default)]
    pub classes: BTreeMap<String, ClassPolicy>,
    #[serde(default)]
    pub modes: ModePolicies,
    #[serde(default)]
    pub runners: RunnersConfig,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default = "default_packets_dir")]
    pub packets_dir: String,
    #[serde(default)]
    pub data_dir: Option<String>,
    #[serde(default = "default_health_timeout")]
    pub health_timeout_ms: u64,
    /// Files exempt from the allowed-paths check (lockfiles the verifier rewrites); they still ship in the patch.
    #[serde(default = "default_scope_ignore")]
    pub scope_ignore: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Tier {
    #[serde(default)]
    pub label: Option<String>,
    pub chain: Vec<ChainEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ChainEntry {
    #[serde(default = "default_runner")]
    pub runner: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub thinking: Option<String>,
    /// URL that must answer 2xx for this entry to be selectable.
    #[serde(default)]
    pub health: Option<String>,
    /// omp config overlay written to a temp file and passed with `--config`.
    #[serde(default)]
    pub settings: Option<Value>,
    /// Shell command for the `command` runner; receives the prompt on stdin.
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub args: Vec<String>,
}

impl ChainEntry {
    pub fn display_model(&self) -> String {
        self.model
            .clone()
            .or_else(|| self.command.clone())
            .unwrap_or_else(|| self.runner.clone())
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ClassPolicy {
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub ceiling: Option<String>,
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub attempts: Option<u32>,
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Marks the class as having an objective verifier even when packets omit `verify`.
    #[serde(default)]
    pub verified: Option<bool>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub scope_ignore: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ModePolicies {
    #[serde(default = "default_conserve")]
    pub conserve: ModePolicy,
    #[serde(default = "default_rush")]
    pub rush: ModePolicy,
}

impl Default for ModePolicies {
    fn default() -> Self {
        ModePolicies {
            conserve: default_conserve(),
            rush: default_rush(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct ModePolicy {
    /// Start-tier shift in rungs; negative starts cheaper.
    #[serde(default)]
    pub shift: i32,
    /// Ceiling applied to classes that have an objective verifier.
    #[serde(default)]
    pub ceiling_verified: Option<String>,
    /// Tier that requires approval before it runs.
    #[serde(default)]
    pub ask_before: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct RunnersConfig {
    #[serde(default)]
    pub omp: OmpRunnerConfig,
    #[serde(default)]
    pub claude: ClaudeRunnerConfig,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ClaudeRunnerConfig {
    #[serde(default = "default_claude_bin")]
    pub bin: String,
    #[serde(default = "default_claude_args")]
    pub args: Vec<String>,
}

impl Default for ClaudeRunnerConfig {
    fn default() -> Self {
        ClaudeRunnerConfig {
            bin: default_claude_bin(),
            args: default_claude_args(),
        }
    }
}

fn default_claude_bin() -> String {
    "claude".to_string()
}

fn default_claude_args() -> Vec<String> {
    vec!["--strict-mcp-config".to_string()]
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct OmpRunnerConfig {
    #[serde(default = "default_omp_bin")]
    pub bin: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub no_lsp: bool,
    #[serde(default)]
    pub no_extensions: bool,
    #[serde(default)]
    pub no_skills: bool,
    #[serde(default)]
    pub no_rules: bool,
}

impl Default for OmpRunnerConfig {
    fn default() -> Self {
        OmpRunnerConfig {
            bin: default_omp_bin(),
            args: Vec::new(),
            no_lsp: true,
            no_extensions: false,
            no_skills: false,
            no_rules: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_password_env")]
    pub password_env: String,
    #[serde(default = "default_env_file")]
    pub env_file: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig {
            listen: default_listen(),
            user: default_user(),
            password_env: default_password_env(),
            env_file: default_env_file(),
        }
    }
}

fn default_packets_dir() -> String {
    ".delegate/packets".to_string()
}

fn default_health_timeout() -> u64 {
    3000
}

fn default_scope_ignore() -> Vec<String> {
    [
        "Cargo.lock",
        "package-lock.json",
        "bun.lock",
        "bun.lockb",
        "yarn.lock",
        "pnpm-lock.yaml",
        "Package.resolved",
        "poetry.lock",
        "uv.lock",
        "Gemfile.lock",
        "go.sum",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn default_runner() -> String {
    "omp".to_string()
}

fn default_omp_bin() -> String {
    "omp".to_string()
}

fn default_true() -> bool {
    true
}

fn default_listen() -> String {
    "0.0.0.0:4100".to_string()
}

fn default_user() -> String {
    "delegate".to_string()
}

fn default_password_env() -> String {
    "DELEGATE_PASSWORD".to_string()
}

fn default_env_file() -> Option<String> {
    Some("~/.config/delegate/serve.env".to_string())
}

fn default_conserve() -> ModePolicy {
    ModePolicy {
        shift: -1,
        ceiling_verified: None,
        ask_before: None,
    }
}

fn default_rush() -> ModePolicy {
    ModePolicy {
        shift: 1,
        ceiling_verified: None,
        ask_before: None,
    }
}

/// KEY=VALUE lines; later entries win, `export` prefixes and quotes are tolerated.
pub fn read_env_file(path: &std::path::Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((k, v)) = line.split_once('=') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            map.insert(k.trim().to_string(), v.to_string());
        }
    }
    map
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn expand_home(s: &str) -> PathBuf {
    match s.strip_prefix("~/") {
        Some(rest) => home_dir().join(rest),
        None if s == "~" => home_dir(),
        None => PathBuf::from(s),
    }
}

pub fn config_dir() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("delegate"),
        _ => home_dir().join(".config").join("delegate"),
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.yml")
}

pub fn host_overlay_path() -> PathBuf {
    config_dir().join("host.yml")
}

/// Configuration layers in load order: base file, host overlay, `DELEGATE_CONFIG` entries, CLI overlays.
pub fn layer_paths(extra: &[PathBuf]) -> Vec<(PathBuf, bool)> {
    let mut layers = vec![(config_path(), true), (host_overlay_path(), false)];
    if let Some(list) = std::env::var_os("DELEGATE_CONFIG") {
        for item in list.to_string_lossy().split(':').filter(|s| !s.is_empty()) {
            layers.push((expand_home(item), true));
        }
    }
    for path in extra {
        layers.push((path.clone(), true));
    }
    layers
}

fn deep_merge(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Mapping(mut b), Value::Mapping(o)) => {
            for (k, v) in o {
                let merged = match b.remove(&k) {
                    Some(existing) => deep_merge(existing, v),
                    None => v,
                };
                b.insert(k, merged);
            }
            Value::Mapping(b)
        }
        (_, o) => o,
    }
}

pub fn load(extra: &[PathBuf]) -> Result<Config> {
    let mut merged = Value::Mapping(Default::default());
    let mut found_base = false;
    for (path, required) in layer_paths(extra) {
        if !path.exists() {
            if required {
                bail!(
                    "config file not found: {} (run `delegate config init` to create one)",
                    path.display()
                );
            }
            continue;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let value: Value = serde_yaml_ng::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        if !value.is_null() {
            merged = deep_merge(merged, value);
        }
        found_base = true;
    }
    if !found_base {
        bail!("no configuration found");
    }
    let config: Config = serde_yaml_ng::from_value(merged).context("configuration is invalid")?;
    config.validate()?;
    Ok(config)
}

pub struct Plan {
    pub start: usize,
    pub ceiling: usize,
    pub attempts: u32,
    pub timeout_secs: u64,
    pub verify: Option<String>,
    pub mode: Mode,
    pub ask_before: Option<usize>,
    pub env: BTreeMap<String, String>,
    pub scope_ignore: Vec<String>,
}

#[derive(Default, Clone, Debug)]
pub struct Overrides {
    pub tier: Option<String>,
    pub ceiling: Option<String>,
    pub mode: Option<Mode>,
    pub attempts: Option<u32>,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.order.is_empty() {
            bail!("config `order` must list at least one tier");
        }
        for name in &self.order {
            let tier = self.tiers.get(name).with_context(|| {
                format!("tier '{name}' is in `order` but not defined under `tiers`")
            })?;
            if tier.chain.is_empty() {
                bail!("tier '{name}' has an empty chain");
            }
            for (i, entry) in tier.chain.iter().enumerate() {
                match entry.runner.as_str() {
                    "omp" => {
                        if entry.model.is_none() {
                            bail!(
                                "tier '{name}' chain[{i}] uses the omp runner but has no `model`"
                            );
                        }
                    }
                    "command" => {
                        if entry.command.is_none() {
                            bail!(
                                "tier '{name}' chain[{i}] uses the command runner but has no `command`"
                            );
                        }
                    }
                    "claude" => {}
                    other => bail!(
                        "tier '{name}' chain[{i}] has unknown runner '{other}' (omp, claude, command)"
                    ),
                }
            }
        }
        for name in self.tiers.keys() {
            if !self.order.contains(name) {
                bail!("tier '{name}' is defined but missing from `order`");
            }
        }
        for (class, policy) in &self.classes {
            for tier in [&policy.tier, &policy.ceiling].into_iter().flatten() {
                self.tier_index(tier)
                    .with_context(|| format!("class '{class}' references unknown tier '{tier}'"))?;
            }
        }
        for (mode, policy) in [
            ("conserve", &self.modes.conserve),
            ("rush", &self.modes.rush),
        ] {
            for tier in [&policy.ceiling_verified, &policy.ask_before]
                .into_iter()
                .flatten()
            {
                self.tier_index(tier)
                    .with_context(|| format!("mode '{mode}' references unknown tier '{tier}'"))?;
            }
        }
        Ok(())
    }

    pub fn tier_index(&self, name: &str) -> Result<usize> {
        self.order
            .iter()
            .position(|t| t == name)
            .with_context(|| format!("unknown tier '{name}' (known: {})", self.order.join(", ")))
    }

    pub fn tier(&self, index: usize) -> (&str, &Tier) {
        let name = &self.order[index];
        (name.as_str(), &self.tiers[name])
    }

    pub fn class_policy(&self, class: &str) -> Option<&ClassPolicy> {
        self.classes
            .get(class)
            .or_else(|| self.classes.get("default"))
    }

    pub fn data_dir(&self) -> PathBuf {
        if let Some(dir) = &self.data_dir {
            return expand_home(dir);
        }
        match std::env::var_os("XDG_DATA_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("delegate"),
            _ => home_dir().join(".local").join("share").join("delegate"),
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir().join("delegate.db")
    }

    pub fn plan(&self, packet: &Packet, overrides: &Overrides) -> Result<Plan> {
        let policy = self.class_policy(&packet.class);
        let last = self.order.len() - 1;
        let start_name = overrides
            .tier
            .clone()
            .or_else(|| packet.tier.clone())
            .or_else(|| policy.and_then(|p| p.tier.clone()));
        let ceiling_name = overrides
            .ceiling
            .clone()
            .or_else(|| packet.ceiling.clone())
            .or_else(|| policy.and_then(|p| p.ceiling.clone()));
        let mut start = match start_name {
            Some(name) => self.tier_index(&name)?,
            None => 0,
        };
        let mut ceiling = match ceiling_name {
            Some(name) => self.tier_index(&name)?,
            None => last,
        };
        if start > ceiling {
            bail!(
                "start tier {} is above ceiling {}",
                self.order[start],
                self.order[ceiling]
            );
        }
        let verify = packet
            .verify
            .clone()
            .or_else(|| policy.and_then(|p| p.verify.clone()));
        let verified = policy.and_then(|p| p.verified).unwrap_or(verify.is_some());
        let mode = overrides.mode.or(packet.mode).unwrap_or(self.mode);
        let mode_policy = match mode {
            Mode::Normal => None,
            Mode::Conserve => Some(&self.modes.conserve),
            Mode::Rush => Some(&self.modes.rush),
        };
        let mut ask_before = None;
        if let Some(mp) = mode_policy {
            if verified && let Some(cap) = &mp.ceiling_verified {
                ceiling = ceiling.min(self.tier_index(cap)?);
            }
            let shifted = start as i64 + mp.shift as i64;
            start = shifted.clamp(0, ceiling as i64) as usize;
            if let Some(ask) = &mp.ask_before {
                let idx = self.tier_index(ask)?;
                if idx > start && idx <= ceiling {
                    ask_before = Some(idx);
                }
            }
        }
        let attempts = overrides
            .attempts
            .or(packet.attempts)
            .or_else(|| policy.and_then(|p| p.attempts))
            .unwrap_or(DEFAULT_ATTEMPTS)
            .max(1);
        let timeout_secs = packet
            .timeout
            .or_else(|| policy.and_then(|p| p.timeout))
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .max(1);
        let env = policy.map(|p| p.env.clone()).unwrap_or_default();
        let mut scope_ignore = self.scope_ignore.clone();
        if let Some(p) = policy {
            scope_ignore.extend(p.scope_ignore.iter().cloned());
        }
        Ok(Plan {
            start,
            ceiling,
            attempts,
            timeout_secs,
            verify,
            mode,
            ask_before,
            env,
            scope_ignore,
        })
    }

    /// Extra environment for workers from the server env file (provider keys), minus the daemon password itself.
    pub fn env_file_entries(&self) -> BTreeMap<String, String> {
        let Some(file) = &self.server.env_file else {
            return BTreeMap::new();
        };
        let mut map = read_env_file(&expand_home(file));
        map.remove(&self.server.password_env);
        map
    }

    pub fn packets_dir(&self, repo: &Path) -> PathBuf {
        repo.join(&self.packets_dir)
    }
}

pub const DEFAULT_CONFIG: &str = r#"# delegate configuration (shared across machines).
# Per-machine differences go in host.yml next to this file; it is deep-merged on top.
mode: normal

order: [t1, t2, t3]

tiers:
  t1:
    label: local
    chain:
      - runner: omp
        model: llama-swap/qwen38-nvfp4
        thinking: low
        health: http://127.0.0.1:8081/v1/models
        settings:
          temperature: 0
  t2:
    label: cheap cloud
    chain:
      - runner: omp
        model: kimi-code/k3
        thinking: medium
  t3:
    label: frontier
    chain:
      - runner: omp
        model: anthropic/claude-opus-5
        thinking: high

classes:
  default:
    tier: t2
    ceiling: t3
  rust-impl:
    tier: t1
    ceiling: t3
    verify: cargo build && cargo clippy --all-targets -- -D warnings && cargo test
  rust-mech:
    tier: t1
    ceiling: t2
    verify: cargo build && cargo test
  swift-impl:
    tier: t2
    ceiling: t3
  strings:
    tier: t1
    ceiling: t2
  docs:
    tier: t1
    ceiling: t2
    verified: false

modes:
  conserve:
    shift: -1
    ceiling_verified: t2
    ask_before: t3
  rush:
    shift: 1

# Lockfiles the verifier rewrites are exempt from the allowed-paths check by default
# (Cargo.lock, package-lock.json, Package.resolved, ...). Add more here or per class.
# scope_ignore: [Cargo.lock, Package.resolved]

runners:
  omp:
    bin: omp
    no_lsp: true

server:
  listen: 0.0.0.0:4100
  user: delegate
  password_env: DELEGATE_PASSWORD
  env_file: ~/.config/delegate/serve.env
"#;

pub const DEFAULT_HOST_OVERLAY: &str = r#"# Per-machine overlay, deep-merged over config.yml. Keep it out of the shared repo.
# Example for a laptop that should prefer the GPU box over the tailnet, then its own local model:
# tiers:
#   t1:
#     chain:
#       - runner: omp
#         model: arch-tailnet/qwen38-nvfp4
#         health: http://100.91.211.44:8081/v1/models
#       - runner: omp
#         model: ollama/qwen3.8:27b-mlx
#         health: http://127.0.0.1:11434/v1/models
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const THREE_TIERS: &str = "
order: [t1, t2, t3]
tiers:
  t1:
    chain:
      - runner: omp
        model: local-model
  t2:
    chain:
      - runner: omp
        model: mid-model
  t3:
    chain:
      - runner: omp
        model: big-model
";

    const BASE_CONFIG: &str = "
order: [t1, t2]
tiers:
  t1:
    label: local
    chain:
      - runner: omp
        model: base-model
  t2:
    chain:
      - runner: omp
        model: cloud-model
server:
  listen: 0.0.0.0:4100
";

    fn cfg_from(yaml: &str) -> Config {
        serde_yaml_ng::from_str(yaml).expect("test config parses")
    }

    fn packet_in(class: &str) -> Packet {
        Packet::new(class, "do the thing")
    }

    #[test]
    fn plan_defaults_to_first_tier_through_last_with_default_attempts_and_timeout() {
        let cfg = cfg_from(THREE_TIERS);
        let packet = packet_in("default");
        let plan = cfg
            .plan(&packet, &Overrides::default())
            .expect("plan resolves");
        assert_eq!(plan.start, 0);
        assert_eq!(plan.ceiling, 2);
        assert_eq!(plan.attempts, DEFAULT_ATTEMPTS);
        assert_eq!(plan.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(plan.verify.is_none());
        assert_eq!(plan.mode, Mode::Normal);
        assert!(plan.ask_before.is_none());
    }

    #[test]
    fn class_policy_supplies_defaults_when_packet_omits_fields() {
        let yaml = format!(
            "{THREE_TIERS}classes:\n  rust-impl:\n    tier: t2\n    ceiling: t3\n    verify: cargo test\n    attempts: 3\n    timeout: 120\n"
        );
        let cfg = cfg_from(&yaml);
        let packet = packet_in("rust-impl");
        let plan = cfg
            .plan(&packet, &Overrides::default())
            .expect("plan resolves");
        assert_eq!(plan.start, 1);
        assert_eq!(plan.ceiling, 2);
        assert_eq!(plan.verify.as_deref(), Some("cargo test"));
        assert_eq!(plan.attempts, 3);
        assert_eq!(plan.timeout_secs, 120);
    }

    #[test]
    fn packet_fields_take_priority_over_class_policy() {
        let yaml = format!(
            "{THREE_TIERS}classes:\n  rust-impl:\n    tier: t1\n    ceiling: t2\n    verify: cargo test\n    attempts: 3\n    timeout: 120\n"
        );
        let cfg = cfg_from(&yaml);
        let mut packet = packet_in("rust-impl");
        packet.tier = Some("t2".to_string());
        packet.ceiling = Some("t3".to_string());
        packet.verify = Some("cargo clippy".to_string());
        packet.attempts = Some(5);
        packet.timeout = Some(30);
        let plan = cfg
            .plan(&packet, &Overrides::default())
            .expect("plan resolves");
        assert_eq!(plan.start, 1);
        assert_eq!(plan.ceiling, 2);
        assert_eq!(plan.verify.as_deref(), Some("cargo clippy"));
        assert_eq!(plan.attempts, 5);
        assert_eq!(plan.timeout_secs, 30);
        assert_eq!(plan.mode, Mode::Normal);
    }

    #[test]
    fn cli_overrides_win_over_packet_and_class_policy() {
        let yaml = format!("{THREE_TIERS}classes:\n  rust-impl:\n    tier: t1\n    ceiling: t2\n");
        let cfg = cfg_from(&yaml);
        let mut packet = packet_in("rust-impl");
        packet.tier = Some("t1".to_string());
        packet.ceiling = Some("t2".to_string());
        packet.attempts = Some(3);
        packet.mode = Some(Mode::Conserve);
        let overrides = Overrides {
            tier: Some("t3".to_string()),
            ceiling: Some("t3".to_string()),
            mode: Some(Mode::Normal),
            attempts: Some(7),
        };
        let plan = cfg.plan(&packet, &overrides).expect("plan resolves");
        assert_eq!(plan.start, 2);
        assert_eq!(plan.ceiling, 2);
        assert_eq!(plan.attempts, 7);
        assert_eq!(plan.mode, Mode::Normal);
    }

    #[test]
    fn conserve_mode_shifts_start_caps_verified_ceiling_and_flags_approval() {
        let yaml = "
order: [t1, t2, t3, t4]
tiers:
  t1:
    chain: [{runner: omp, model: m1}]
  t2:
    chain: [{runner: omp, model: m2}]
  t3:
    chain: [{runner: omp, model: m3}]
  t4:
    chain: [{runner: omp, model: m4}]
classes:
  rust-impl:
    tier: t3
    ceiling: t4
    verify: cargo test
modes:
  conserve:
    shift: -2
    ceiling_verified: t3
    ask_before: t3
";
        let cfg = cfg_from(yaml);
        let mut packet = packet_in("rust-impl");
        packet.mode = Some(Mode::Conserve);
        let plan = cfg
            .plan(&packet, &Overrides::default())
            .expect("plan resolves");
        assert_eq!(plan.start, 0, "shift of -2 from t3 lands on t1");
        assert_eq!(
            plan.ceiling, 2,
            "verified class is capped at ceiling_verified (t3)"
        );
        assert_eq!(
            plan.ask_before,
            Some(2),
            "t3 sits above start and within the capped ceiling"
        );
    }

    #[test]
    fn class_verified_false_disables_ceiling_verified_even_with_a_verify_command() {
        let yaml = "
order: [t1, t2, t3]
tiers:
  t1:
    chain: [{runner: omp, model: m1}]
  t2:
    chain: [{runner: omp, model: m2}]
  t3:
    chain: [{runner: omp, model: m3}]
classes:
  docs:
    tier: t1
    ceiling: t3
    verify: cargo test
    verified: false
modes:
  conserve:
    shift: 0
    ceiling_verified: t2
";
        let cfg = cfg_from(yaml);
        let mut packet = packet_in("docs");
        packet.mode = Some(Mode::Conserve);
        let plan = cfg
            .plan(&packet, &Overrides::default())
            .expect("plan resolves");
        assert_eq!(
            plan.ceiling, 2,
            "verified: false keeps the full ceiling despite ceiling_verified"
        );
    }

    #[test]
    fn rush_mode_shift_is_clamped_to_the_ceiling() {
        let yaml = format!(
            "{THREE_TIERS}classes:\n  strings:\n    tier: t2\n    ceiling: t2\nmodes:\n  rush:\n    shift: 5\n"
        );
        let cfg = cfg_from(&yaml);
        let mut packet = packet_in("strings");
        packet.mode = Some(Mode::Rush);
        let plan = cfg
            .plan(&packet, &Overrides::default())
            .expect("plan resolves");
        assert_eq!(
            plan.start, 1,
            "a shift of +5 cannot push start past the ceiling (t2)"
        );
        assert_eq!(plan.ceiling, 1);
    }

    #[test]
    fn plan_errors_when_start_tier_is_above_ceiling() {
        let cfg = cfg_from(THREE_TIERS);
        let packet = packet_in("default");
        let overrides = Overrides {
            tier: Some("t3".to_string()),
            ceiling: Some("t1".to_string()),
            ..Overrides::default()
        };
        let err = match cfg.plan(&packet, &overrides) {
            Ok(_) => panic!("expected start-above-ceiling to be rejected"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("above ceiling"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn plan_errors_on_unknown_override_tier() {
        let cfg = cfg_from(THREE_TIERS);
        let packet = packet_in("default");
        let overrides = Overrides {
            tier: Some("nope".to_string()),
            ..Overrides::default()
        };
        let err = match cfg.plan(&packet, &overrides) {
            Ok(_) => panic!("expected an unknown override tier to be rejected"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("unknown tier"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn deep_merge_keeps_untouched_sibling_keys_across_layers() {
        let base: Value = serde_yaml_ng::from_str(BASE_CONFIG).unwrap();
        let overlay: Value =
            serde_yaml_ng::from_str("server:\n  listen: 127.0.0.1:9000\n").unwrap();
        let merged = deep_merge(base, overlay);
        let cfg: Config =
            serde_yaml_ng::from_value(merged).expect("merged value is a valid config");
        assert_eq!(cfg.server.listen, "127.0.0.1:9000");
        assert_eq!(
            cfg.server.user,
            default_user(),
            "keys the overlay never mentioned keep their default"
        );
        assert_eq!(
            cfg.tiers["t1"].label.as_deref(),
            Some("local"),
            "sibling top-level keys survive the merge"
        );
    }

    #[test]
    fn deep_merge_replaces_a_tier_chain_wholesale_instead_of_concatenating() {
        let base: Value = serde_yaml_ng::from_str(BASE_CONFIG).unwrap();
        let overlay: Value = serde_yaml_ng::from_str(
            "tiers:\n  t1:\n    chain:\n      - runner: omp\n        model: overlay-model\n      - runner: omp\n        model: overlay-fallback\n",
        )
        .unwrap();
        let merged = deep_merge(base, overlay);
        let cfg: Config =
            serde_yaml_ng::from_value(merged).expect("merged value is a valid config");
        let t1 = &cfg.tiers["t1"];
        assert_eq!(
            t1.label.as_deref(),
            Some("local"),
            "the label key was never touched by the overlay"
        );
        assert_eq!(
            t1.chain.len(),
            2,
            "overlay chain replaces the base chain rather than appending to it"
        );
        assert_eq!(t1.chain[0].model.as_deref(), Some("overlay-model"));
        assert_eq!(t1.chain[1].model.as_deref(), Some("overlay-fallback"));
    }

    #[test]
    fn validate_accepts_a_well_formed_config() {
        let cfg = cfg_from(THREE_TIERS);
        cfg.validate()
            .expect("a minimal well-formed config should validate");
    }

    #[test]
    fn validate_rejects_a_tier_with_an_empty_chain() {
        let cfg = cfg_from("order: [t1]\ntiers:\n  t1:\n    chain: []\n");
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("empty chain"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn validate_rejects_an_omp_chain_entry_without_a_model() {
        let cfg = cfg_from("order: [t1]\ntiers:\n  t1:\n    chain:\n      - runner: omp\n");
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("no `model`"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn validate_rejects_a_command_chain_entry_without_a_command() {
        let cfg = cfg_from("order: [t1]\ntiers:\n  t1:\n    chain:\n      - runner: command\n");
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("no `command`"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn validate_rejects_an_unknown_runner() {
        let cfg = cfg_from("order: [t1]\ntiers:\n  t1:\n    chain:\n      - runner: magic\n");
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("unknown runner"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn validate_rejects_a_tier_defined_but_missing_from_order() {
        let cfg = cfg_from(
            "order: [t1]\ntiers:\n  t1:\n    chain:\n      - runner: omp\n        model: m\n  t2:\n    chain:\n      - runner: omp\n        model: m2\n",
        );
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("missing from `order`"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn validate_rejects_an_order_entry_not_defined_under_tiers() {
        let cfg = cfg_from(
            "order: [t1, t2]\ntiers:\n  t1:\n    chain:\n      - runner: omp\n        model: m\n",
        );
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("not defined under `tiers`"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn validate_rejects_a_class_referencing_an_unknown_tier() {
        let yaml = format!("{THREE_TIERS}classes:\n  default:\n    tier: nope\n");
        let cfg = cfg_from(&yaml);
        let err = cfg.validate().unwrap_err();
        assert!(
            err.to_string().contains("references unknown tier"),
            "unexpected message: {err}"
        );
    }
}
