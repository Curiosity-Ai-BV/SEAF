use std::path::{Component, Path};

pub fn default_exclude_patterns() -> Vec<String> {
    [
        ".env*",
        "secrets/**",
        "infra/signing/**",
        "*.pem",
        "*.key",
        "*.p12",
        "node_modules/**",
        "target/**",
        "dist/**",
        ".git/**",
        ".seaf/**",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub fn normalize_repo_path(path: &str) -> Option<String> {
    if path.trim().is_empty() || path.contains('\\') {
        return None;
    }

    let mut components = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(value) => components.push(value.to_str()?.to_string()),
            Component::CurDir => (),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    if components.is_empty() {
        None
    } else {
        Some(components.join("/"))
    }
}

pub fn matching_pattern<'patterns>(
    repo_path: &str,
    patterns: &'patterns [String],
) -> Option<&'patterns str> {
    patterns
        .iter()
        .map(String::as_str)
        .find(|pattern| pattern_matches(repo_path, pattern))
}

fn pattern_matches(repo_path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }

    if let Some(prefix) = pattern.strip_suffix("/**") {
        return normalize_repo_path(prefix)
            .map(|prefix| contains_component_sequence(repo_path, &prefix))
            .unwrap_or(true);
    }

    if let Some(suffix) = pattern.strip_prefix("*.") {
        let suffix = format!(".{suffix}");
        return file_name(repo_path).ends_with(&suffix);
    }

    if let Some(prefix) = pattern.strip_suffix('*') {
        if !prefix.contains('/') && !prefix.contains('*') {
            return file_name(repo_path).starts_with(prefix);
        }
    }

    // Supported globs are the fixed safety patterns this slice needs: foo/**,
    // *.ext, and basename*. Unsupported wildcards exclude conservatively.
    if pattern.contains('*') {
        return true;
    }

    normalize_repo_path(pattern)
        .map(|pattern| repo_path == pattern)
        .unwrap_or(true)
}

fn contains_component_sequence(repo_path: &str, pattern_prefix: &str) -> bool {
    let path_components: Vec<&str> = repo_path.split('/').collect();
    let prefix_components: Vec<&str> = pattern_prefix.split('/').collect();

    path_components
        .windows(prefix_components.len())
        .any(|window| window == prefix_components.as_slice())
}

fn file_name(repo_path: &str) -> &str {
    repo_path.rsplit('/').next().unwrap_or(repo_path)
}
