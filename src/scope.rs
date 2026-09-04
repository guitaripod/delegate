use globset::{Glob, GlobSet, GlobSetBuilder};

/// Files outside the packet's allowed paths. An empty allow-list permits everything;
/// `ignore` patterns (a bare name matches at any depth) are exempt from the check.
pub fn violations(changed: &[String], allowed: &[String], ignore: &[String]) -> Vec<String> {
    if allowed.is_empty() {
        return Vec::new();
    }
    let globs = build_globs(allowed);
    let ignored = build_globs(ignore);
    let prefixes: Vec<String> = allowed
        .iter()
        .map(|p| p.trim_end_matches('/').to_string())
        .filter(|p| !p.is_empty())
        .collect();
    changed
        .iter()
        .filter(|file| !is_ignored(file, &ignored))
        .filter(|file| !permitted(file, &globs, &prefixes))
        .cloned()
        .collect()
}

fn is_ignored(file: &str, ignored: &GlobSet) -> bool {
    if ignored.is_match(file) {
        return true;
    }
    file.rsplit('/')
        .next()
        .map(|base| ignored.is_match(base))
        .unwrap_or(false)
}

fn build_globs(allowed: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in allowed {
        if let Ok(glob) = Glob::new(pattern.trim_end_matches('/')) {
            builder.add(glob);
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

fn permitted(file: &str, globs: &GlobSet, prefixes: &[String]) -> bool {
    if globs.is_match(file) {
        return true;
    }
    prefixes
        .iter()
        .any(|p| file == p || file.starts_with(&format!("{p}/")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|i| i.to_string()).collect()
    }

    #[test]
    fn empty_allow_list_permits_everything() {
        assert!(violations(&s(&["a.rs", "dir/b.rs"]), &[], &[]).is_empty());
    }

    #[test]
    fn directory_prefix_and_globs() {
        let allowed = s(&["src/", "tests/**/*.rs", "README.md"]);
        let changed = s(&[
            "src/main.rs",
            "tests/x/y.rs",
            "README.md",
            "Cargo.toml",
            "srcx/a.rs",
        ]);
        assert_eq!(
            violations(&changed, &allowed, &[]),
            s(&["Cargo.toml", "srcx/a.rs"])
        );
    }

    #[test]
    fn ignored_lockfiles_at_any_depth() {
        let allowed = s(&["src/"]);
        let changed = s(&[
            "src/lib.rs",
            "Cargo.lock",
            "sub/crate/Cargo.lock",
            "Cargo.toml",
        ]);
        let ignore = s(&["Cargo.lock"]);
        assert_eq!(violations(&changed, &allowed, &ignore), s(&["Cargo.toml"]));
    }
}
