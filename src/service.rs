use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use base64::Engine as _;

use crate::config::{Config, expand_home, home_dir};

fn random_password() -> Result<String> {
    let mut bytes = [0u8; 24];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// Creates the env file with a fresh password when it does not exist; returns the path and whether it was created.
pub fn ensure_env_file(cfg: &Config) -> Result<(PathBuf, bool)> {
    let path = expand_home(
        cfg.server
            .env_file
            .as_deref()
            .context("server.env_file is unset")?,
    );
    if path.exists() {
        return Ok((path, false));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let password = random_password()?;
    std::fs::write(&path, format!("{}={}\n", cfg.server.password_env, password))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok((path, true))
}

fn binary_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| home_dir().join(".cargo/bin/delegate"))
}

fn child_path_literal() -> String {
    let home = home_dir().to_string_lossy().to_string();
    format!(
        "{home}/.bun/bin:{home}/.cargo/bin:{home}/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
    )
}

pub fn install(cfg: &Config) -> Result<()> {
    let (env_path, created) = ensure_env_file(cfg)?;
    if created {
        println!("created {} with a new password", env_path.display());
    }
    if cfg!(target_os = "macos") {
        install_launchd(cfg)
    } else {
        install_systemd(cfg, &env_path)
    }
}

fn install_systemd(cfg: &Config, env_path: &std::path::Path) -> Result<()> {
    let dir = home_dir().join(".config/systemd/user");
    std::fs::create_dir_all(&dir)?;
    let unit = dir.join("delegate.service");
    let text = format!(
        "[Unit]
Description=delegate tiered dispatcher daemon
After=network-online.target

[Service]
ExecStart={bin} serve
EnvironmentFile=-{env}
Environment=PATH={path}
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
",
        bin = binary_path().display(),
        env = env_path.display(),
        path = child_path_literal(),
    );
    std::fs::write(&unit, text)?;
    run("systemctl", &["--user", "daemon-reload"])?;
    run(
        "systemctl",
        &["--user", "enable", "--now", "delegate.service"],
    )?;
    run("systemctl", &["--user", "restart", "delegate.service"])?;
    println!(
        "installed {} (listening on {})",
        unit.display(),
        cfg.server.listen
    );
    Ok(())
}

fn install_launchd(cfg: &Config) -> Result<()> {
    let dir = home_dir().join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    let plist = dir.join("cc.midgarcorp.delegate.plist");
    let logs = home_dir().join("Library/Logs");
    std::fs::create_dir_all(&logs)?;
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>cc.midgarcorp.delegate</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>serve</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>{path}</string>
  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{logs}/delegate.log</string>
  <key>StandardErrorPath</key><string>{logs}/delegate.log</string>
</dict>
</plist>
"#,
        bin = binary_path().display(),
        path = child_path_literal(),
        logs = logs.display(),
    );
    std::fs::write(&plist, text)?;
    let uid = Command::new("id").arg("-u").output()?;
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_string();
    let domain = format!("gui/{uid}");
    let plist_str = plist.to_string_lossy().to_string();
    let _ = Command::new("launchctl")
        .args(["bootout", &domain, &plist_str])
        .output();
    run("launchctl", &["bootstrap", &domain, &plist_str])?;
    run(
        "launchctl",
        &[
            "kickstart",
            "-k",
            &format!("{domain}/cc.midgarcorp.delegate"),
        ],
    )?;
    println!(
        "installed {} (listening on {})",
        plist.display(),
        cfg.server.listen
    );
    Ok(())
}

fn run(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("running {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
