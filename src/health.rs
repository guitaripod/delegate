use std::time::Duration;

/// GET the URL and treat any 2xx as healthy; used to pick the first reachable chain entry.
pub fn check(url: &str, timeout_ms: u64) -> Result<(), String> {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(timeout_ms)))
        .build();
    let agent: ureq::Agent = config.into();
    match agent.get(url).call() {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(code)) => Err(format!("HTTP {code}")),
        Err(e) => Err(short_error(&e.to_string())),
    }
}

fn short_error(text: &str) -> String {
    let first = text.lines().next().unwrap_or(text);
    if first.len() > 120 {
        format!("{}…", &first[..120])
    } else {
        first.to_string()
    }
}
