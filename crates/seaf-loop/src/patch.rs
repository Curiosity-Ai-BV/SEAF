use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use crate::policy::normalize_repo_path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParsedPatch {
    pub changed_paths: Vec<String>,
    pub files: Vec<PatchFile>,
    pub contains_binary_patch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchFile {
    pub paths: Vec<String>,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub contains_binary_patch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchParseError {
    EmptyPatch,
    MalformedGitHeader(String),
    MissingPath(String),
    UnsafePath(String),
}

pub fn parse_unified_diff(patch: &str) -> Result<ParsedPatch, PatchParseError> {
    let mut files = Vec::new();
    let mut current: Option<PatchFileBuilder> = None;
    let mut contains_binary_patch = false;

    for line in patch.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish_file(&mut files, current.take())?;
            let mut file = PatchFileBuilder::default();
            let paths = parse_git_header_paths(rest)?;
            for path in paths {
                file.add_path(path);
            }
            current = Some(file);
            continue;
        }

        if let Some(rest) = line.strip_prefix("--- ") {
            if current.is_none() {
                current = Some(PatchFileBuilder::default());
            }
            if let Some(path) = parse_file_header_path(rest)? {
                let file = current.as_mut().expect("current file");
                file.old_path = Some(path.clone());
                file.add_path(path);
            }
            continue;
        }

        if let Some(rest) = line.strip_prefix("+++ ") {
            if current.is_none() {
                current = Some(PatchFileBuilder::default());
            }
            if let Some(path) = parse_file_header_path(rest)? {
                let file = current.as_mut().expect("current file");
                file.new_path = Some(path.clone());
                file.add_path(path);
            }
            continue;
        }

        if line == "GIT binary patch" || line.starts_with("Binary files ") {
            contains_binary_patch = true;
            if let Some(file) = current.as_mut() {
                file.contains_binary_patch = true;
            }
        }
    }

    finish_file(&mut files, current)?;

    if files.is_empty() {
        return Err(PatchParseError::EmptyPatch);
    }

    let mut changed_paths = Vec::new();
    for file in &files {
        for path in &file.paths {
            push_unique(&mut changed_paths, path.clone());
        }
    }

    if changed_paths.is_empty() {
        return Err(PatchParseError::MissingPath(
            "patch did not name a repository path".to_string(),
        ));
    }

    Ok(ParsedPatch {
        changed_paths,
        files,
        contains_binary_patch,
    })
}

fn finish_file(
    files: &mut Vec<PatchFile>,
    current: Option<PatchFileBuilder>,
) -> Result<(), PatchParseError> {
    let Some(current) = current else {
        return Ok(());
    };

    if current.paths.is_empty() {
        return Err(PatchParseError::MissingPath(
            "file diff did not name a repository path".to_string(),
        ));
    }

    files.push(PatchFile {
        paths: current.paths,
        old_path: current.old_path,
        new_path: current.new_path,
        contains_binary_patch: current.contains_binary_patch,
    });
    Ok(())
}

fn parse_git_header_paths(rest: &str) -> Result<Vec<String>, PatchParseError> {
    let tokens = parse_tokens(rest)?;
    if tokens.len() != 2 {
        return Err(PatchParseError::MalformedGitHeader(rest.to_string()));
    }

    let mut paths = Vec::new();
    for token in tokens {
        let Some(path) = normalize_patch_path(&token, false)? else {
            return Err(PatchParseError::UnsafePath(token));
        };
        push_unique(&mut paths, path);
    }

    Ok(paths)
}

fn parse_file_header_path(rest: &str) -> Result<Option<String>, PatchParseError> {
    let token = parse_file_header_token(rest)?;
    normalize_patch_path(&token, true)
}

fn parse_file_header_token(rest: &str) -> Result<String, PatchParseError> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(PatchParseError::MissingPath(
            "file header did not include a path".to_string(),
        ));
    }

    if rest.starts_with('"') {
        let tokens = parse_tokens(rest)?;
        return tokens
            .into_iter()
            .next()
            .ok_or_else(|| PatchParseError::MissingPath("quoted file header path".to_string()));
    }

    let path = rest.split_once('\t').map_or(rest, |(path, _)| path);
    Ok(path.to_string())
}

fn normalize_patch_path(
    raw_path: &str,
    allow_dev_null: bool,
) -> Result<Option<String>, PatchParseError> {
    let path = raw_path.trim();
    if path == "/dev/null" {
        return if allow_dev_null {
            Ok(None)
        } else {
            Err(PatchParseError::UnsafePath(path.to_string()))
        };
    }

    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);

    normalize_repo_path(path)
        .map(Some)
        .ok_or_else(|| PatchParseError::UnsafePath(path.to_string()))
}

fn parse_tokens(input: &str) -> Result<Vec<String>, PatchParseError> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_quotes = false;

    while let Some(character) = chars.next() {
        match character {
            '"' => {
                in_quotes = !in_quotes;
            }
            '\\' if in_quotes => {
                let Some(escaped) = chars.next() else {
                    return Err(PatchParseError::MalformedGitHeader(input.to_string()));
                };
                current.push(escaped);
            }
            character if character.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            character => current.push(character),
        }
    }

    if in_quotes {
        return Err(PatchParseError::MalformedGitHeader(input.to_string()));
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    Ok(tokens)
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[derive(Default)]
struct PatchFileBuilder {
    paths: Vec<String>,
    old_path: Option<String>,
    new_path: Option<String>,
    contains_binary_patch: bool,
}

impl PatchFileBuilder {
    fn add_path(&mut self, path: String) {
        push_unique(&mut self.paths, path);
    }
}

impl fmt::Display for PatchParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPatch => write!(formatter, "patch is empty or contains no file diffs"),
            Self::MalformedGitHeader(header) => {
                write!(formatter, "malformed git diff header: {header}")
            }
            Self::MissingPath(message) => write!(formatter, "patch path missing: {message}"),
            Self::UnsafePath(path) => write!(formatter, "unsafe patch path: {path}"),
        }
    }
}

impl Error for PatchParseError {}
